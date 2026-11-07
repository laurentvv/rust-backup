//! Encryption: AES-256-GCM per-chunk AEAD, Argon2id-wrapped master key.
//!
//! Design (restic/borg-style encrypt-then-store):
//! - one random 256-bit master key per repository;
//! - the master key is stored sealed under a key-encryption key derived
//!   from the user passphrase with **Argon2id** (params persisted in the
//!   key file so they can be raised later);
//! - every chunk is sealed independently with AES-256-GCM, a fresh random
//!   96-bit nonce, and associated data binding the ciphertext to the chunk
//!   hash and plaintext length — swapping or truncating chunks then fails
//!   authentication instead of silently restoring wrong data.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::hash::ChunkHash;

/// AEAD tag size in bytes (GCM-128).
pub const TAG_SIZE: usize = 16;

/// Nonce size in bytes (96-bit GCM nonce).
pub const NONCE_SIZE: usize = 12;

/// Salt size in bytes for the Argon2id key wrap.
pub const SALT_SIZE: usize = 16;

/// Associated-data domain separator for the master-key envelope.
const AD_MASTERKEY: &[u8] = b"rustbackup/masterkey/v1";

/// Argon2id parameters used to derive the key-encryption key.
///
/// Defaults follow the RFC 9106 "low-memory" recommendation
/// (64 MiB, t=3, p=4), a good balance for a desktop application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Time cost (iterations).
    pub t_cost: u32,
    /// Parallelism.
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            m_cost: 65_536, // 64 MiB
            t_cost: 3,
            p_cost: 4,
        }
    }
}

/// The 256-bit repository master key. Zeroized when dropped.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct MasterKey(pub [u8; 32]);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material, even in debug logs.
        f.write_str("MasterKey(***)")
    }
}

impl MasterKey {
    /// Generates a fresh random master key.
    #[must_use]
    pub fn generate() -> Self {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Self(key)
    }
}

/// The passphrase-wrapped master key, persisted as `keys/<id>.json` in the
/// repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKey {
    /// Algorithm identifier, checked on unlock.
    pub algorithm: String,
    /// Argon2id parameters that must be repeated on unlock.
    pub kdf: KdfParams,
    /// Hex-encoded Argon2id salt.
    pub salt: String,
    /// Hex-encoded per-envelope nonce.
    pub nonce: String,
    /// Hex-encoded AES-256-GCM ciphertext (sealed master key + tag).
    pub ciphertext: String,
}

impl EncryptedKey {
    /// Seals `master` under a key derived from `passphrase`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Crypto`](crate::Error::Crypto) when Argon2id
    /// rejects the parameters or sealing fails.
    pub fn create(passphrase: &str, kdf: KdfParams, master: &MasterKey) -> crate::Result<Self> {
        let mut salt = [0u8; SALT_SIZE];
        let mut nonce = [0u8; NONCE_SIZE];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut salt);
        rng.fill_bytes(&mut nonce);

        let kek = derive_kek(passphrase, &salt, &kdf)?;
        let cipher = Aes256Gcm::new_from_slice(&kek)
            .map_err(|e| crate::Error::Crypto(format!("kek has invalid length: {e}")))?;
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &master.0,
                    aad: AD_MASTERKEY,
                },
            )
            .map_err(|_| crate::Error::Crypto("failed to seal master key".into()))?;

        Ok(Self {
            algorithm: "AES-256-GCM + Argon2id".into(),
            kdf,
            salt: to_hex(&salt),
            nonce: to_hex(&nonce),
            ciphertext: to_hex(&sealed),
        })
    }

    /// Derives the KEK from `passphrase` and opens the master key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Crypto`](crate::Error::Crypto) with a deliberately
    /// generic message ("wrong passphrase or corrupted key file") on any
    /// failure — wrong passphrase, tampered envelope, changed params.
    pub fn unlock(&self, passphrase: &str) -> crate::Result<MasterKey> {
        let salt: [u8; SALT_SIZE] = parse_hex(&self.salt, "salt")?;
        let nonce: [u8; NONCE_SIZE] = parse_hex(&self.nonce, "nonce")?;
        let ciphertext = parse_hex_loose(&self.ciphertext, "ciphertext")?;

        let kek = derive_kek(passphrase, &salt, &self.kdf)?;
        let cipher = Aes256Gcm::new_from_slice(&kek)
            .map_err(|_| crate::Error::Crypto("wrong passphrase or corrupted key file".into()))?;
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: AD_MASTERKEY,
                },
            )
            .map_err(|_| crate::Error::Crypto("wrong passphrase or corrupted key file".into()))?;
        let master: [u8; 32] = plain
            .try_into()
            .map_err(|_| crate::Error::Crypto("wrong passphrase or corrupted key file".into()))?;
        Ok(MasterKey(master))
    }

    /// Serializes the envelope to its on-disk JSON form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`](crate::Error::Json) on serialization
    /// failure.
    pub fn to_json(&self) -> crate::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parses an envelope written by [`EncryptedKey::to_json`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`](crate::Error::Json) on parse failure.
    pub fn from_json(data: &str) -> crate::Result<Self> {
        Ok(serde_json::from_str(data)?)
    }
}

/// Per-run chunk cipher bound to the master key.
#[derive(Clone)]
pub struct ChunkCipher {
    cipher: Aes256Gcm,
}

impl ChunkCipher {
    /// Builds a cipher from the repository master key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Crypto`](crate::Error::Crypto) if the master key
    /// is not 32 bytes (cannot happen for keys built by
    /// [`MasterKey::generate`]).
    pub fn new(master: &MasterKey) -> crate::Result<Self> {
        let cipher = Aes256Gcm::new_from_slice(&master.0)
            .map_err(|_| crate::Error::Crypto("master key must be 32 bytes".into()))?;
        Ok(Self { cipher })
    }

    /// Associated data binding a chunk blob to its identity.
    fn ad(hash: &ChunkHash, plain_len: u64) -> [u8; 40] {
        let mut ad = [0u8; 40];
        ad[..32].copy_from_slice(hash.as_bytes());
        ad[32..].copy_from_slice(&plain_len.to_be_bytes());
        ad
    }

    /// Seals (compresses-then-encrypted data) `plaintext` under a fresh
    /// random nonce. Returns `nonce || ciphertext||tag`.
    ///
    /// `hash` must be the BLAKE3 hash of `plaintext` and `plain_len` its
    /// length; both become AEAD associated data.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Crypto`](crate::Error::Crypto) on encryption
    /// failure.
    pub fn seal(
        &self,
        plaintext: &[u8],
        hash: &ChunkHash,
        plain_len: u64,
    ) -> crate::Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let ad = Self::ad(hash, plain_len);
        let sealed = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: &ad,
                },
            )
            .map_err(|_| crate::Error::Crypto("failed to seal chunk".into()))?;
        let mut out = Vec::with_capacity(NONCE_SIZE + sealed.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// Opens a blob produced by [`ChunkCipher::seal`], verifying its MAC,
    /// the chunk identity (`hash`) and the plaintext length (`plain_len`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Crypto`](crate::Error::Crypto) when the blob is
    /// truncated, tampered with, or bound to another chunk hash/length.
    pub fn open(&self, blob: &[u8], hash: &ChunkHash, plain_len: u64) -> crate::Result<Vec<u8>> {
        if blob.len() < NONCE_SIZE + TAG_SIZE {
            return Err(crate::Error::Crypto("chunk blob too short".into()));
        }
        let (nonce_bytes, sealed) = blob.split_at(NONCE_SIZE);
        let ad = Self::ad(hash, plain_len);
        self.cipher
            .decrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: sealed,
                    aad: &ad,
                },
            )
            .map_err(|_| {
                crate::Error::Crypto(format!(
                    "authentication failed for chunk {hash} (corrupted or wrong repository key)"
                ))
            })
    }
}

/// Derives the 32-byte key-encryption key from the passphrase.
fn derive_kek(
    passphrase: &str,
    salt: &[u8; SALT_SIZE],
    kdf: &KdfParams,
) -> crate::Result<[u8; 32]> {
    let params = argon2::Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|e| crate::Error::Crypto(format!("invalid Argon2id params: {e}")))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| crate::Error::Crypto(format!("Argon2id failed: {e}")))?;
    Ok(out)
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn parse_hex<const N: usize>(value: &str, what: &str) -> crate::Result<[u8; N]> {
    let bytes = crate::hash::hex::from_hex(value)?;
    bytes.try_into().map_err(|v: Vec<u8>| {
        crate::Error::Crypto(format!("{what} must be {N} bytes, got {}", v.len()))
    })
}

fn parse_hex_loose(value: &str, what: &str) -> crate::Result<Vec<u8>> {
    crate::hash::hex::from_hex(value)
        .map_err(|e| crate::Error::Crypto(format!("invalid {what} hex: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deliberately light Argon2id params so unit tests stay fast; the
    // production default lives in KdfParams::default.
    fn fast_kdf() -> KdfParams {
        KdfParams {
            m_cost: 1024,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn master_key_envelope_roundtrip_should_return_the_same_key() {
        let master = MasterKey::generate();
        let envelope = EncryptedKey::create("correct horse battery staple", fast_kdf(), &master)
            .expect("create envelope");
        let json = envelope.to_json().expect("to_json");
        let parsed = EncryptedKey::from_json(&json).expect("from_json");
        let unlocked = parsed
            .unlock("correct horse battery staple")
            .expect("unlock");
        assert_eq!(unlocked.0, master.0);
    }

    #[test]
    fn wrong_passphrase_should_fail_with_a_clean_error() {
        let master = MasterKey::generate();
        let envelope =
            EncryptedKey::create("right passphrase", fast_kdf(), &master).expect("create envelope");
        let err = envelope.unlock("wrong passphrase").unwrap_err();
        match err {
            crate::Error::Crypto(msg) => {
                assert!(!msg.contains("right passphrase"), "no secret leakage");
            }
            other => panic!("expected Crypto error, got {other:?}"),
        }
    }

    #[test]
    fn full_pipeline_compress_seal_open_decompress_should_roundtrip() {
        // Exact production order: compress, then seal; open, then decompress.
        let cipher = ChunkCipher::new(&MasterKey::generate()).expect("cipher");
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 241) as u8).collect();
        let hash = ChunkHash::from_data(&payload);
        let packed = crate::compress::compress(&payload, 5).expect("compress");
        let blob = cipher
            .seal(&packed, &hash, payload.len() as u64)
            .expect("seal");
        let opened = cipher
            .open(&blob, &hash, payload.len() as u64)
            .expect("open");
        let unpacked = crate::compress::decompress(&opened, payload.len()).expect("decompress");
        assert_eq!(unpacked, payload);
    }

    #[test]
    fn chunk_seal_open_roundtrip_should_preserve_plaintext() {
        let cipher = ChunkCipher::new(&MasterKey::generate()).expect("cipher");
        let payload = b"the quick brown fox jumps over the lazy dog".as_slice();
        let hash = ChunkHash::from_data(payload);
        let blob = cipher
            .seal(payload, &hash, payload.len() as u64)
            .expect("seal");
        let opened = cipher
            .open(&blob, &hash, payload.len() as u64)
            .expect("open");
        assert_eq!(opened, payload);
    }

    #[test]
    fn tampered_blob_should_fail_authentication() {
        let cipher = ChunkCipher::new(&MasterKey::generate()).expect("cipher");
        let payload = vec![7u8; 1024];
        let hash = ChunkHash::from_data(&payload);
        let mut blob = cipher
            .seal(&payload, &hash, payload.len() as u64)
            .expect("seal");
        let mid = blob.len() / 2;
        blob[mid] ^= 0x01;
        assert!(cipher.open(&blob, &hash, payload.len() as u64).is_err());
    }

    #[test]
    fn swapped_hash_or_length_should_fail_authentication() {
        let cipher = ChunkCipher::new(&MasterKey::generate()).expect("cipher");
        let payload = vec![9u8; 512];
        let hash = ChunkHash::from_data(&payload);
        let blob = cipher
            .seal(&payload, &hash, payload.len() as u64)
            .expect("seal");

        let other_hash = ChunkHash::from_data(b"different content");
        assert!(
            cipher
                .open(&blob, &other_hash, payload.len() as u64)
                .is_err()
        );
        assert!(cipher.open(&blob, &hash, payload.len() as u64 + 1).is_err());
    }

    #[test]
    fn truncated_blob_should_fail_cleanly() {
        let cipher = ChunkCipher::new(&MasterKey::generate()).expect("cipher");
        let payload = b"0123456789".as_slice();
        let hash = ChunkHash::from_data(payload);
        let blob = cipher
            .seal(payload, &hash, payload.len() as u64)
            .expect("seal");
        assert!(
            cipher
                .open(&blob[..5], &hash, payload.len() as u64)
                .is_err()
        );
    }

    #[test]
    fn debug_of_master_key_should_never_leak_material() {
        let master = MasterKey::generate();
        let rendered = format!("{master:?}");
        assert_eq!(rendered, "MasterKey(***)");
        assert!(!rendered.contains(&master.0[0].to_string()));
    }
}

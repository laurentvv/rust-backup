//! The backup repository: on-disk format v1, self-contained on any
//! [`StorageBackend`].
//!
//! Layout (repo-relative paths):
//! - `config.json` — public repository parameters (chunking, compression)
//! - `keys/<id>.json` — master key sealed under the passphrase (Argon2id)
//! - `chunks/<2-hex>/<hash>.chunk` — sealed chunks (see below)
//! - `snapshots/<id>.snap` — sealed snapshot manifests, written LAST
//! - `locks/` — reserved
//!
//! Chunk file layout: `RBCH` magic | version u8 | plain_len u64 LE |
//! AES-256-GCM(nonce || zstd(plaintext) || tag), with the chunk hash and
//! plain_len as associated data. Snapshot layout: `RBSN` magic | version
//! u8 | plain_len u64 LE | sealed manifest, AD = snapshot id + plain_len.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::StorageBackend;
use crate::chunker::ChunkerConfig;
use crate::crypto::{ChunkCipher, EncryptedKey, KdfParams, MasterKey};
use crate::hash::ChunkHash;
use crate::manifest::SnapshotManifest;

/// Chunk blob magic.
const CHUNK_MAGIC: &[u8; 4] = b"RBCH";
/// Manifest blob magic.
const MANIFEST_MAGIC: &[u8; 4] = b"RBSN";
/// On-disk format version for both blob kinds.
const BLOB_VERSION: u8 = 1;
/// Header size: magic(4) + version(1) + plain_len(8).
const HEADER_SIZE: usize = 13;

/// Public repository parameters, stored in `config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoConfig {
    /// On-disk format version (1).
    pub version: u32,
    /// Chunker tuning; must stay stable for meaningful dedup.
    pub chunker: ChunkerConfig,
    /// zstd level applied to chunk plaintext before encryption.
    pub compression_level: i32,
    /// When the repository was created (UTC).
    pub created_at: DateTime<Utc>,
}

/// Report returned by [`Repository::prune_orphans`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    /// Number of orphan chunks deleted.
    pub chunks_deleted: u64,
    /// Sum of bytes freed (stored compressed/sealed chunk blob sizes).
    pub bytes_freed: u64,
    /// Number of chunks preserved (referenced by at least one snapshot).
    pub chunks_kept: u64,
    /// Leftover temp files (`*.tmp-*`) from interrupted writes, removed.
    #[serde(default)]
    pub temp_files_removed: u64,
}

/// Exclusive lock information stored under `locks/<id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    /// Unique identifier for this lock acquisition.
    pub id: Uuid,
    /// Hostname where the process is running.
    pub hostname: String,
    /// Operating system process ID holding the lock.
    pub pid: u32,
    /// When the lock was acquired (UTC).
    pub created_at: DateTime<Utc>,
    /// High-level operation ("backup", "prune", "delete_snapshot").
    pub operation: String,
}

/// An exclusive repository lock handle. When dropped, the lock file on the
/// backend is removed automatically.
pub struct RepositoryLock {
    backend: std::sync::Arc<dyn StorageBackend>,
    lock_id: Uuid,
    released: bool,
}

impl RepositoryLock {
    /// Explicitly release the lock early.
    pub fn release(mut self) -> crate::Result<()> {
        if !self.released {
            self.released = true;
            let rel = format!("locks/{}.json", self.lock_id.simple());
            self.backend.remove(&rel)?;
        }
        Ok(())
    }

    /// Unique lock identifier.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.lock_id
    }
}

impl Drop for RepositoryLock {
    fn drop(&mut self) {
        if !self.released {
            let rel = format!("locks/{}.json", self.lock_id.simple());
            let _ = self.backend.remove(&rel);
        }
    }
}

/// An opened backup repository with the master key derived.
#[derive(Clone)]
pub struct Repository {
    backend: std::sync::Arc<dyn StorageBackend>,
    config: RepoConfig,
    cipher: ChunkCipher,
    key_id: String,
}

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("key_id", &self.key_id)
            .field("config", &self.config)
            .field("backend", &self.backend.kind())
            .finish()
    }
}

impl Repository {
    /// Creates a new repository: writes the sealed key and `config.json`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) when the destination is
    /// already a repository, and crypto/IO errors on failure.
    pub fn init(
        backend: std::sync::Arc<dyn StorageBackend>,
        passphrase: &str,
        chunker: ChunkerConfig,
        compression_level: i32,
        kdf: KdfParams,
    ) -> crate::Result<Self> {
        chunker.validate()?;
        if backend.exists("config.json")? {
            return Err(crate::Error::Repo(
                "destination already contains a repository".into(),
            ));
        }
        let master = MasterKey::generate();
        let key_id = Uuid::new_v4().simple().to_string();
        let envelope = EncryptedKey::create(passphrase, kdf, &master)?;
        backend.write_atomic(
            &format!("keys/{key_id}.json"),
            envelope.to_json()?.as_bytes(),
        )?;

        let config = RepoConfig {
            version: 1,
            chunker,
            compression_level,
            created_at: Utc::now(),
        };
        backend.write_atomic(
            "config.json",
            serde_json::to_string_pretty(&config)?.as_bytes(),
        )?;
        backend.mkdirs("locks")?;

        let cipher = ChunkCipher::new(&master)?;
        Ok(Self {
            backend,
            config,
            cipher,
            key_id,
        })
    }

    /// Opens an existing repository and unlocks the master key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) when the destination is
    /// not a repository (or an unsupported version) and
    /// [`Error::Crypto`](crate::Error::Crypto) on a wrong passphrase.
    pub fn open(
        backend: std::sync::Arc<dyn StorageBackend>,
        passphrase: &str,
    ) -> crate::Result<Self> {
        if !backend.exists("config.json")? {
            return Err(crate::Error::Repo(
                "no repository at destination (config.json missing)".into(),
            ));
        }
        let config: RepoConfig = serde_json::from_slice(&backend.read("config.json")?)
            .map_err(|e| crate::Error::Repo(format!("invalid config.json: {e}")))?;
        if config.version != 1 {
            return Err(crate::Error::Repo(format!(
                "unsupported repository version {}",
                config.version
            )));
        }
        let key_files = backend.list("keys")?;
        let key_path = key_files.first().ok_or_else(|| {
            crate::Error::Repo("repository has no key file (keys/ is empty)".into())
        })?;
        let envelope = EncryptedKey::from_json(&String::from_utf8_lossy(&backend.read(key_path)?))?;
        let master = envelope.unlock(passphrase)?;
        let key_id = key_path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".json")
            .to_owned();

        Ok(Self {
            backend,
            config,
            cipher: ChunkCipher::new(&master)?,
            key_id,
        })
    }

    /// Public repository parameters.
    #[must_use]
    pub fn config(&self) -> &RepoConfig {
        &self.config
    }

    /// Id of the active key file (for `rbackup` diagnostics).
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Backend description (location of the repository).
    #[must_use]
    pub fn describe(&self) -> String {
        self.backend.describe()
    }

    fn chunk_rel(hash: &ChunkHash) -> String {
        let hex = hash.to_hex();
        format!("chunks/{}/{}.chunk", &hex[..2], hex)
    }

    /// Stores a chunk if not already present (dedup).
    ///
    /// Returns `true` when the chunk was newly written, `false` when an
    /// identical chunk (same hash) already existed.
    ///
    /// # Errors
    ///
    /// Returns compression/crypto/IO errors as usual; a storage failure
    /// never leaves a partially-written chunk behind (atomic writes).
    pub fn store_chunk(&self, hash: &ChunkHash, plaintext: &[u8]) -> crate::Result<bool> {
        let rel = Self::chunk_rel(hash);
        if self.backend.exists(&rel)? {
            return Ok(false);
        }
        let packed = crate::compress::compress(plaintext, self.config.compression_level)?;
        let sealed = self
            .cipher
            .seal(packed.as_slice(), hash, plaintext.len() as u64)?;
        let mut blob = Vec::with_capacity(HEADER_SIZE + sealed.len());
        blob.extend_from_slice(CHUNK_MAGIC);
        blob.push(BLOB_VERSION);
        blob.extend_from_slice(&(plaintext.len() as u64).to_le_bytes());
        blob.extend_from_slice(&sealed);
        self.backend.write_atomic(&rel, &blob)?;
        Ok(true)
    }

    /// Reads a chunk, verifying MAC, hash binding and length.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) on header/format
    /// violations and [`Error::Crypto`](crate::Error::Crypto) on
    /// authentication failure.
    pub fn read_chunk(&self, hash: &ChunkHash, plain_len: u64) -> crate::Result<Vec<u8>> {
        let blob = self.backend.read(&Self::chunk_rel(hash))?;
        let (packed, actual_len) = Self::unpack(&blob, CHUNK_MAGIC)?;
        if actual_len != plain_len {
            return Err(crate::Error::Repo(format!(
                "chunk {hash} declares {actual_len} bytes, caller expected {plain_len}"
            )));
        }
        let opened = self.cipher.open(packed, hash, plain_len)?;
        crate::compress::decompress(&opened, plain_len as usize)
    }

    /// Whether a chunk with this hash exists in the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when the check fails.
    pub fn chunk_exists(&self, hash: &ChunkHash) -> crate::Result<bool> {
        self.backend.exists(&Self::chunk_rel(hash))
    }

    /// Lists every chunk file in the repository (repo-relative paths).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when listing fails.
    pub fn list_chunks(&self) -> crate::Result<Vec<String>> {
        self.backend.list("chunks")
    }

    /// Finalizes a snapshot by writing its sealed manifest — this call is
    /// what makes the snapshot exist, and it happens LAST by construction.
    ///
    /// # Errors
    ///
    /// Returns serialization/crypto/IO errors; on failure the snapshot
    /// simply does not exist yet.
    pub fn write_manifest(&self, manifest: &SnapshotManifest) -> crate::Result<()> {
        let json = manifest.to_json()?;
        let ad = Self::manifest_ad(manifest.id, json.len() as u64);
        let sealed = self.cipher.seal_with_ad(&json, &ad)?;
        let mut blob = Vec::with_capacity(HEADER_SIZE + sealed.len());
        blob.extend_from_slice(MANIFEST_MAGIC);
        blob.push(BLOB_VERSION);
        blob.extend_from_slice(&(json.len() as u64).to_le_bytes());
        blob.extend_from_slice(&sealed);
        self.backend
            .write_atomic(&format!("snapshots/{}.snap", manifest.id.simple()), &blob)
    }

    /// Reads and parses a snapshot manifest by id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) when the snapshot does
    /// not exist or is malformed, and [`Error::Crypto`](crate::Error::Crypto)
    /// on authentication failure.
    pub fn read_manifest(&self, snapshot_id: &Uuid) -> crate::Result<SnapshotManifest> {
        let rel = format!("snapshots/{}.snap", snapshot_id.simple());
        let blob = self.backend.read(&rel)?;
        let (sealed, plain_len) = Self::unpack(&blob, MANIFEST_MAGIC)?;
        let json = self
            .cipher
            .open_with_ad(sealed, &Self::manifest_ad(*snapshot_id, plain_len))?;
        SnapshotManifest::from_json(&json)
    }

    /// Lists snapshot ids present in the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when listing fails.
    pub fn list_snapshot_ids(&self) -> crate::Result<Vec<Uuid>> {
        let mut ids = Vec::new();
        for rel in self.backend.list("snapshots")? {
            let stem = rel
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .trim_end_matches(".snap");
            if let Ok(id) = Uuid::parse_str(stem) {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Loads all manifests, newest first.
    ///
    /// # Errors
    ///
    /// Propagates read/parse errors of any manifest.
    pub fn all_manifests(&self) -> crate::Result<Vec<SnapshotManifest>> {
        let mut manifests: Vec<SnapshotManifest> = Vec::new();
        for id in self.list_snapshot_ids()? {
            manifests.push(self.read_manifest(&id)?);
        }
        manifests.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        Ok(manifests)
    }

    /// Newest manifest of a job, if the job has any snapshot.
    ///
    /// # Errors
    ///
    /// Propagates manifest read errors.
    pub fn latest_manifest_for_job(
        &self,
        job_id: &uuid::Uuid,
    ) -> crate::Result<Option<SnapshotManifest>> {
        Ok(self
            .all_manifests()?
            .into_iter()
            .find(|m| &m.job_id == job_id))
    }

    /// All manifests for a given job, newest first.
    ///
    /// # Errors
    ///
    /// Propagates manifest read errors.
    pub fn manifests_for_job(&self, job_id: &uuid::Uuid) -> crate::Result<Vec<SnapshotManifest>> {
        Ok(self
            .all_manifests()?
            .into_iter()
            .filter(|m| &m.job_id == job_id)
            .collect())
    }

    /// Counts how many consecutive incremental snapshots exist since the latest
    /// full snapshot for this job.
    ///
    /// # Errors
    ///
    /// Propagates manifest read errors.
    pub fn count_consecutive_incrementals_for_job(
        &self,
        job_id: &uuid::Uuid,
    ) -> crate::Result<u32> {
        let manifests = self.manifests_for_job(job_id)?;
        let mut count = 0;
        for m in manifests {
            if m.kind == crate::domain::BackupKind::Full {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// Acquires an exclusive write lock on the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RepositoryLocked`](crate::Error::RepositoryLocked) if an
    /// active lock is held by another process or operation. Stale locks (> 24
    /// hours) are broken automatically.
    pub fn acquire_lock(&self, operation: &str) -> crate::Result<RepositoryLock> {
        let _ = self.backend.mkdirs("locks");
        let existing = self.backend.list("locks")?;
        let now = Utc::now();
        let stale_threshold = chrono::Duration::hours(24);

        for rel in existing {
            if !rel.ends_with(".json") {
                continue;
            }
            if let Ok(data) = self.backend.read(&rel)
                && let Ok(info) = serde_json::from_slice::<LockInfo>(&data)
            {
                let age = now.signed_duration_since(info.created_at);
                if age < stale_threshold {
                    return Err(crate::Error::RepositoryLocked(format!(
                        "operation '{}' by PID {} on host '{}' since {}",
                        info.operation, info.pid, info.hostname, info.created_at
                    )));
                }
                // Stale lock: break it.
                let _ = self.backend.remove(&rel);
            }
        }

        let lock_id = Uuid::new_v4();
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "localhost".to_string());
        let info = LockInfo {
            id: lock_id,
            hostname,
            pid: std::process::id(),
            created_at: now,
            operation: operation.to_string(),
        };
        let rel = format!("locks/{}.json", lock_id.simple());
        let json = serde_json::to_string_pretty(&info)?;
        self.backend.write_atomic(&rel, json.as_bytes())?;

        Ok(RepositoryLock {
            backend: std::sync::Arc::clone(&self.backend),
            lock_id,
            released: false,
        })
    }

    /// Deletes a snapshot manifest from the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RepositoryLocked`](crate::Error::RepositoryLocked) if
    /// locked, or IO errors.
    pub fn delete_snapshot(&self, snapshot_id: &Uuid) -> crate::Result<()> {
        let _lock = self.acquire_lock("delete_snapshot")?;
        let rel = format!("snapshots/{}.snap", snapshot_id.simple());
        self.backend.remove(&rel)?;
        Ok(())
    }

    /// Scans the repository for chunks that are no longer referenced by ANY
    /// snapshot manifest, and unlinks them to reclaim storage.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RepositoryLocked`](crate::Error::RepositoryLocked),
    /// [`Error::Cancelled`](crate::Error::Cancelled) if cooperative cancellation
    /// is triggered, or IO errors.
    pub fn prune_orphans(
        &self,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        mut on_progress: impl FnMut(u64, u64),
    ) -> crate::Result<PruneReport> {
        let _lock = self.acquire_lock("prune")?;

        // 1. Collect all referenced chunk hashes from all existing manifests.
        let manifests = self.all_manifests()?;
        let mut referenced: HashSet<ChunkHash> = HashSet::new();
        for m in &manifests {
            for s in &m.sources {
                for f in &s.entries {
                    for c in &f.chunks {
                        referenced.insert(c.hash);
                    }
                }
            }
        }

        // 2. Scan every stored chunk file on the backend.
        let chunk_files = self.list_chunks()?;
        let total = chunk_files.len() as u64;
        let mut report = PruneReport::default();

        for (ix, rel) in chunk_files.into_iter().enumerate() {
            if let Some(c) = cancel
                && c.load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(crate::Error::Cancelled);
            }
            on_progress(ix as u64, total);

            if is_temp_file(&rel) {
                // Leftover of an interrupted write. Safe to delete: we hold
                // the exclusive lock, so no writer is active right now.
                self.backend.remove(&rel)?;
                report.temp_files_removed += 1;
                continue;
            }
            if !rel.ends_with(".chunk") {
                continue;
            }
            let stem = rel
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .trim_end_matches(".chunk");
            if let Ok(hash) = ChunkHash::from_hex(stem) {
                if referenced.contains(&hash) {
                    report.chunks_kept += 1;
                } else {
                    let len = self.backend.read(&rel).map(|b| b.len() as u64).unwrap_or(0);
                    self.backend.remove(&rel)?;
                    report.chunks_deleted += 1;
                    report.bytes_freed += len;
                }
            }
        }
        // 3. Interrupted snapshot writes leave temp files next to manifests.
        for rel in self.backend.list("snapshots")? {
            if is_temp_file(&rel) {
                self.backend.remove(&rel)?;
                report.temp_files_removed += 1;
            }
        }
        on_progress(total, total);
        Ok(report)
    }

    fn unpack<'a>(blob: &'a [u8], magic: &[u8; 4]) -> crate::Result<(&'a [u8], u64)> {
        if blob.len() < HEADER_SIZE || &blob[..4] != magic {
            return Err(crate::Error::Repo("corrupt blob header".into()));
        }
        if blob[4] != BLOB_VERSION {
            return Err(crate::Error::Repo(format!(
                "unsupported blob version {}",
                blob[4]
            )));
        }
        let plain_len = u64::from_le_bytes(
            blob[5..HEADER_SIZE]
                .try_into()
                .map_err(|_| crate::Error::Repo("short header".into()))?,
        );
        Ok((&blob[HEADER_SIZE..], plain_len))
    }

    fn manifest_ad(snapshot_id: Uuid, plain_len: u64) -> Vec<u8> {
        let mut ad = Vec::with_capacity(28);
        ad.extend_from_slice(b"RBSN");
        ad.extend_from_slice(snapshot_id.as_bytes());
        ad.extend_from_slice(&plain_len.to_be_bytes());
        ad
    }
}

/// True for temp names produced by `StorageBackend::write_atomic`
/// (`<stem>.tmp-<uuid>` locally, `<name>.tmp-<uuid>` over SFTP).
fn is_temp_file(rel: &str) -> bool {
    rel.rsplit('/')
        .next()
        .is_some_and(|name| name.contains(".tmp-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::LocalFs;

    fn temp_repo() -> (std::path::PathBuf, Repository) {
        let dir = std::env::temp_dir().join(format!("rb-repo-{}", Uuid::new_v4()));
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        let repo = Repository::init(
            backend,
            "pass phrase",
            ChunkerConfig::default(),
            5,
            fast_kdf(),
        )
        .expect("init");
        (dir, repo)
    }

    fn fast_kdf() -> KdfParams {
        KdfParams {
            m_cost: 1024,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn init_twice_on_same_destination_should_fail() {
        let (dir, _repo) = temp_repo();
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        assert!(
            Repository::init(backend, "other", ChunkerConfig::default(), 5, fast_kdf()).is_err()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_with_wrong_passphrase_should_fail_cleanly() {
        let (dir, _repo) = temp_repo();
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        let err = Repository::open(backend, "not the passphrase").unwrap_err();
        assert!(matches!(err, crate::Error::Crypto(_)), "got {err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chunk_store_read_roundtrip_and_dedup_flag() {
        let (_dir, repo) = temp_repo();
        let data = vec![5u8; 300_000];
        let hash = ChunkHash::from_data(&data);
        assert!(repo.store_chunk(&hash, &data).expect("store"));
        assert!(!repo.store_chunk(&hash, &data).expect("store again"));
        let back = repo.read_chunk(&hash, data.len() as u64).expect("read");
        assert_eq!(back, data);
    }

    #[test]
    fn manifest_write_read_roundtrip() {
        let (_dir, repo) = temp_repo();
        let mut manifest = SnapshotManifest::new(Uuid::new_v4(), "job");
        manifest.sources.push(crate::manifest::SourceSnapshot {
            root: std::path::PathBuf::from(r"C:\X"),
            entries: Vec::new(),
        });
        repo.write_manifest(&manifest).expect("write manifest");
        let ids = repo.list_snapshot_ids().expect("ids");
        assert_eq!(ids, vec![manifest.id]);
        let back = repo.read_manifest(&manifest.id).expect("read manifest");
        assert_eq!(back.id, manifest.id);
        assert_eq!(back.job_name, "job");
        assert!(
            repo.latest_manifest_for_job(&manifest.job_id)
                .expect("latest")
                .is_some()
        );
    }

    #[test]
    fn chunk_with_declared_length_mismatch_should_error() {
        let (_dir, repo) = temp_repo();
        let data = vec![1u8; 1000];
        let hash = ChunkHash::from_data(&data);
        repo.store_chunk(&hash, &data).expect("store");
        assert!(repo.read_chunk(&hash, 999).is_err());
    }

    #[test]
    fn open_on_empty_folder_should_report_missing_repo() {
        let dir = std::env::temp_dir().join(format!("rb-empty-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).ok();
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        let err = Repository::open(backend, "whatever").unwrap_err();
        assert!(matches!(err, crate::Error::Repo(_)), "got {err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exclusive_lock_prevents_concurrent_access_and_releases_on_drop() {
        let (dir, repo) = temp_repo();
        let lock1 = repo.acquire_lock("first_op").expect("acquire lock 1");
        assert!(
            repo.acquire_lock("second_op").is_err(),
            "concurrent lock must fail"
        );
        drop(lock1);
        let lock2 = repo
            .acquire_lock("third_op")
            .expect("acquire lock 2 after drop");
        drop(lock2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_snapshot_removes_manifest_and_leaves_others_intact() {
        let (dir, repo) = temp_repo();
        let m1 = SnapshotManifest::new(Uuid::new_v4(), "job1");
        let m2 = SnapshotManifest::new(Uuid::new_v4(), "job2");
        repo.write_manifest(&m1).expect("write m1");
        repo.write_manifest(&m2).expect("write m2");
        assert_eq!(repo.list_snapshot_ids().expect("ids").len(), 2);

        repo.delete_snapshot(&m1.id).expect("delete m1");
        let ids = repo.list_snapshot_ids().expect("ids after delete");
        assert_eq!(ids, vec![m2.id]);
        assert!(repo.read_manifest(&m1.id).is_err());
        assert!(repo.read_manifest(&m2.id).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_orphans_cleans_unreferenced_chunks_and_keeps_live_ones() {
        let (dir, repo) = temp_repo();
        // 1. Store two chunks
        let data1 = vec![10u8; 1000];
        let hash1 = ChunkHash::from_data(&data1);
        repo.store_chunk(&hash1, &data1).expect("store 1");

        let data2 = vec![20u8; 2000];
        let hash2 = ChunkHash::from_data(&data2);
        repo.store_chunk(&hash2, &data2).expect("store 2");

        // 2. Snapshot references ONLY chunk1
        let mut m = SnapshotManifest::new(Uuid::new_v4(), "job");
        m.sources.push(crate::manifest::SourceSnapshot {
            root: std::path::PathBuf::from(r"C:\test"),
            entries: vec![crate::manifest::FileEntry {
                path: std::path::PathBuf::from("file1.bin"),
                size: 1000,
                mtime_ns: 0,
                chunks: vec![crate::manifest::ChunkRef {
                    hash: hash1,
                    plain_len: 1000,
                }],
                kind: crate::manifest::EntryKind::File,
            }],
        });
        repo.write_manifest(&m).expect("write m");

        // 3. Prune: chunk2 should be deleted, chunk1 kept
        let report = repo.prune_orphans(None, |_, _| {}).expect("prune");
        assert_eq!(report.chunks_deleted, 1);
        assert_eq!(report.chunks_kept, 1);
        assert!(repo.chunk_exists(&hash1).expect("chunk1 exists"));
        assert!(!repo.chunk_exists(&hash2).expect("chunk2 pruned"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_removes_stale_temp_files() {
        let (dir, repo) = temp_repo();
        let data = vec![7u8; 500];
        let hash = ChunkHash::from_data(&data);
        repo.store_chunk(&hash, &data).expect("store");
        let mut m = SnapshotManifest::new(Uuid::new_v4(), "job");
        m.sources.push(crate::manifest::SourceSnapshot {
            root: std::path::PathBuf::from(r"C:\test"),
            entries: vec![crate::manifest::FileEntry {
                path: std::path::PathBuf::from("f.bin"),
                size: 500,
                mtime_ns: 0,
                chunks: vec![crate::manifest::ChunkRef {
                    hash,
                    plain_len: 500,
                }],
                kind: crate::manifest::EntryKind::File,
            }],
        });
        repo.write_manifest(&m).expect("write m");

        // Leftovers of interrupted writes, in both naming styles
        // (LocalFs replaces the extension, SFTP appends it).
        let backend = &repo.backend;
        backend
            .write_atomic("chunks/ab/abcd.tmp-0123", b"partial")
            .expect("tmp chunk");
        backend
            .write_atomic("snapshots/dead.snap.tmp-4567", b"partial")
            .expect("tmp snap");

        let report = repo.prune_orphans(None, |_, _| {}).expect("prune");
        assert_eq!(report.temp_files_removed, 2);
        assert_eq!(report.chunks_kept, 1, "live chunk untouched");
        assert!(!backend.exists("chunks/ab/abcd.tmp-0123").expect("exists"));
        assert!(
            !backend
                .exists("snapshots/dead.snap.tmp-4567")
                .expect("exists")
        );
        assert_eq!(repo.list_snapshot_ids().expect("ids"), vec![m.id]);
        std::fs::remove_dir_all(&dir).ok();
    }
}

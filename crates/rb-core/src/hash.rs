//! BLAKE3 content hashes identifying chunks.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 32-byte BLAKE3 digest identifying a chunk's plaintext content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkHash(pub [u8; 32]);

impl ChunkHash {
    /// Hashes `data` with BLAKE3.
    #[must_use]
    pub fn from_data(data: &[u8]) -> Self {
        Self(blake3::hash(data).into())
    }

    /// Raw 32 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex encoding (64 chars).
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Parses a 64-char lowercase-hex hash.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidHash`](crate::Error::InvalidHash) on length
    /// or character errors.
    pub fn from_hex(value: &str) -> crate::Result<Self> {
        let bytes = hex::from_hex(value)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|e: Vec<u8>| {
            crate::Error::InvalidHash(format!("expected 32 bytes, got {}", e.len()))
        })?;
        Ok(Self(arr))
    }
}

impl std::fmt::Display for ChunkHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for ChunkHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ChunkHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_hex(&value).map_err(serde::de::Error::custom)
    }
}

/// Hex helpers shared with the [`ChunkHash`] implementation.
pub(crate) mod hex {
    /// Decodes lowercase-or-uppercase hex into bytes.
    pub(crate) fn from_hex(value: &str) -> crate::Result<Vec<u8>> {
        if !value.len().is_multiple_of(2) {
            return Err(crate::Error::InvalidHash("odd-length hex string".into()));
        }
        let mut out = Vec::with_capacity(value.len() / 2);
        let bytes = value.as_bytes();
        for pair in bytes.chunks(2) {
            let hi = hex_digit(pair[0])?;
            let lo = hex_digit(pair[1])?;
            out.push(hi << 4 | lo);
        }
        Ok(out)
    }

    fn hex_digit(c: u8) -> crate::Result<u8> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            other => Err(crate::Error::InvalidHash(format!(
                "invalid hex digit: 0x{other:02x}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_should_preserve_the_hash() {
        let hash = ChunkHash::from_data(b"rustbackup");
        let parsed = ChunkHash::from_hex(&hash.to_hex()).expect("parse hex");
        assert_eq!(hash, parsed);
    }

    #[test]
    fn from_hex_should_reject_garbage() {
        assert!(ChunkHash::from_hex("zz").is_err());
        assert!(ChunkHash::from_hex("abc").is_err());
        assert!(ChunkHash::from_hex(&"a".repeat(66)).is_err());
    }

    #[test]
    fn serde_should_roundtrip_as_hex_string() {
        let hash = ChunkHash::from_data(b"payload");
        let json = serde_json::to_string(&hash).expect("serialize");
        assert!(json.starts_with('"') && json.len() == 66);
        let back: ChunkHash = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(hash, back);
    }
}

//! Per-chunk zstd compression.
//!
//! Compression happens BEFORE encryption (encrypt-the-compressed-data):
//! compressed data has high entropy, which is what AEAD ciphertext wants,
//! and compressing after encryption would be useless.

/// Compresses `data` with zstd at `level` (0..=22).
///
/// # Errors
///
/// Returns [`Error::Compression`](crate::Error::Compression) when zstd
/// fails (level out of range, allocation failure).
pub fn compress(data: &[u8], level: i32) -> crate::Result<Vec<u8>> {
    zstd::bulk::compress(data, level).map_err(|e| crate::Error::Compression(e.to_string()))
}

/// Decompresses `data`, which must contain exactly `plain_len` decompressed
/// bytes. The exact length comes from the sealed chunk header — this avoids
/// guessing a capacity and lets zstd fail loudly on truncation.
///
/// # Errors
///
/// Returns [`Error::Compression`](crate::Error::Compression) on corrupt or
/// mismatched input.
pub fn decompress(data: &[u8], plain_len: usize) -> crate::Result<Vec<u8>> {
    let out = zstd::bulk::decompress(data, plain_len)
        .map_err(|e| crate::Error::Compression(e.to_string()))?;
    // zstd tolerates an oversized capacity hint and returns fewer bytes;
    // the chunk format requires the exact plaintext length, so enforce it.
    if out.len() != plain_len {
        return Err(crate::Error::Compression(format!(
            "decompressed {} bytes, expected {plain_len}",
            out.len()
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_decompress_roundtrip_should_be_lossless() {
        // Repetitive payload so compression actually shrinks something.
        let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let packed = compress(&payload, 5).expect("compress");
        assert!(packed.len() < payload.len() / 10, "should shrink well");
        let unpacked = decompress(&packed, payload.len()).expect("decompress");
        assert_eq!(unpacked, payload);
    }

    #[test]
    fn decompress_should_fail_on_wrong_length_or_corruption() {
        let packed = compress(b"hello world hello world", 5).expect("compress");
        assert!(decompress(&packed, 999).is_err(), "wrong plain_len");
        let mut corrupted = packed.clone();
        let mid = corrupted.len() / 2;
        corrupted[mid] ^= 0xff;
        assert!(decompress(&corrupted, 24).is_err(), "corrupted stream");
    }

    #[test]
    fn empty_input_roundtrips() {
        let packed = compress(&[], 3).expect("compress empty");
        let unpacked = decompress(&packed, 0).expect("decompress empty");
        assert!(unpacked.is_empty());
    }
}

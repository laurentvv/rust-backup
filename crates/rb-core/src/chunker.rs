//! Content-defined chunking (FastCDC, 2020 variant).
//!
//! Splitting data at content-dependent boundaries makes chunk identities
//! stable under insertions and deletions, which is what gives the repository
//! deduplication across snapshots (restic/rustic-style incremental-forever).

use fastcdc::v2020;

/// Tuning of the content-defined chunker.
///
/// The average is the geometric mean of min and max in the default config
/// (256 KiB / 1 MiB / 4 MiB), which spreads chunk sizes evenly on a log
/// scale as recommended by the FastCDC paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChunkerConfig {
    /// Minimum chunk size in bytes.
    pub min_size: usize,
    /// Expected (average) chunk size in bytes.
    pub avg_size: usize,
    /// Maximum chunk size in bytes.
    pub max_size: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            min_size: 256 * 1024,
            avg_size: 1024 * 1024,
            max_size: 4 * 1024 * 1024,
        }
    }
}

impl ChunkerConfig {
    /// Guards against nonsensical chunk sizes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidChunker`](crate::Error::InvalidChunker) when
    /// the sizes are not strictly increasing or out of the accepted range.
    pub fn validate(&self) -> crate::Result<()> {
        if self.min_size < 1024 {
            return Err(crate::Error::InvalidChunker(
                "min_size must be at least 1024 bytes".into(),
            ));
        }
        if self.min_size >= self.avg_size || self.avg_size >= self.max_size {
            return Err(crate::Error::InvalidChunker(format!(
                "sizes must satisfy min < avg < max, got {} < {} < {}",
                self.min_size, self.avg_size, self.max_size
            )));
        }
        if self.max_size > 64 * 1024 * 1024 {
            return Err(crate::Error::InvalidChunker(
                "max_size must not exceed 64 MiB".into(),
            ));
        }
        Ok(())
    }
}

/// One content-defined chunk of a byte stream, as an offset/length pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSpan {
    /// Byte offset of the chunk in the source stream.
    pub offset: usize,
    /// Chunk length in bytes.
    pub length: usize,
}

/// Splits `data` into content-defined chunks per `config`.
///
/// Chunk boundaries depend only on the bytes between them: the same bytes
/// produce the same boundary regardless of where they sit in the stream.
///
/// # Errors
///
/// Returns [`Error::InvalidChunker`](crate::Error::InvalidChunker) when the
/// configuration is invalid.
pub fn chunk_data(data: &[u8], config: &ChunkerConfig) -> crate::Result<Vec<ChunkSpan>> {
    config.validate()?;
    Ok(
        v2020::FastCDC::new(data, config.min_size, config.avg_size, config.max_size)
            .map(|entry| ChunkSpan {
                offset: entry.offset,
                length: entry.length,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        // xorshift is enough for distribution tests, no rand needed here.
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn chunk_data_should_be_deterministic_for_identical_input() {
        let data = pseudo_random(8 * 1024 * 1024, 42);
        let cfg = ChunkerConfig::default();
        let a = chunk_data(&data, &cfg).expect("chunk a");
        let b = chunk_data(&data, &cfg).expect("chunk b");
        assert_eq!(a, b);
    }

    #[test]
    fn chunk_data_should_respect_min_max_bounds() {
        let data = pseudo_random(16 * 1024 * 1024, 7);
        let cfg = ChunkerConfig::default();
        let chunks = chunk_data(&data, &cfg).expect("chunks");
        assert!(chunks.len() > 1, "expected several chunks");
        let total: usize = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, data.len(), "chunks must cover the input exactly");
        for (i, c) in chunks.iter().enumerate() {
            assert!(
                c.length <= cfg.max_size,
                "chunk {i} exceeds max: {}",
                c.length
            );
            // The final chunk may be shorter than min (end of stream).
            let is_last = c.offset + c.length == data.len();
            if !is_last {
                assert!(
                    c.length >= cfg.min_size,
                    "non-final chunk {i} below min: {}",
                    c.length
                );
            }
        }
    }

    #[test]
    fn chunk_data_should_produce_averages_near_the_target() {
        let data = pseudo_random(32 * 1024 * 1024, 99);
        let cfg = ChunkerConfig::default();
        let chunks = chunk_data(&data, &cfg).expect("chunks");
        let avg = data.len() / chunks.len();
        // Generous band: normalized FastCDC on random data stays close to
        // the target; a factor-of-4 deviation would signal broken sizing.
        assert!(
            avg > cfg.avg_size / 4 && avg < cfg.avg_size * 4,
            "average chunk size {avg} too far from target {}",
            cfg.avg_size
        );
    }

    #[test]
    fn chunk_boundaries_should_stabilize_after_an_insertion() {
        let cfg = ChunkerConfig::default();
        let data = pseudo_random(8 * 1024 * 1024, 5);
        // Real prepend: every byte offset shifts, only content-defined
        // boundaries can bring the two streams back in sync.
        let inserted = [b"PREFIX-DISRUPTION-BYTES".as_slice(), data.as_slice()].concat();
        let before = chunk_data(&data, &cfg).expect("before");
        let after = chunk_data(&inserted, &cfg).expect("after");

        let hash_of = |buf: &[u8], span: &ChunkSpan| {
            blake3::hash(&buf[span.offset..span.offset + span.length])
        };
        // Every original chunk must reappear after the disruption except at
        // most the two around the shifted prefix boundary.
        let all_before: std::collections::HashSet<_> =
            before.iter().map(|c| hash_of(&data, c)).collect();
        let disturbed: usize = after
            .iter()
            .map(|c| hash_of(&inserted, c))
            .filter(|h| !all_before.contains(h))
            .count();
        assert!(
            disturbed <= 2,
            "prefix insertion disturbed {disturbed} chunks, expected <= 2"
        );
    }

    #[test]
    fn config_should_reject_inverted_sizes() {
        let bad = ChunkerConfig {
            min_size: 4096,
            avg_size: 2048,
            max_size: 1024,
        };
        assert!(chunk_data(b"x", &bad).is_err());
    }
}

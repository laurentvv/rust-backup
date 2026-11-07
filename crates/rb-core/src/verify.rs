//! Repository integrity verification ("validité des sauvegardes").
//!
//! - **Quick**: every chunk referenced by every manifest exists, and every
//!   manifest authenticates.
//! - **Full**: additionally read + authenticate + re-hash every referenced
//!   chunk, and report orphan chunk files (stored but unreferenced —
//!   harmless, they only waste space).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::{DateTime, Utc};

use crate::hash::ChunkHash;
use crate::repo::Repository;

/// Depth of the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Existence + manifest authentication only (fast).
    Quick,
    /// Read, authenticate and re-hash every referenced chunk (slower,
    /// catches bit rot inside stored chunks).
    Full,
}

impl VerifyMode {
    /// Stable keyword for the catalog.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Full => "full",
        }
    }
}

/// One problem found during verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyIssue {
    /// What is wrong, in user-facing terms.
    pub message: String,
}

/// Result of a verification run.
#[derive(Debug, Clone)]
pub struct VerifyReport {
    /// Mode that ran.
    pub mode: VerifyMode,
    /// When the run started (UTC).
    pub started_at: DateTime<Utc>,
    /// When the run finished (UTC).
    pub finished_at: DateTime<Utc>,
    /// Snapshots checked.
    pub manifests_checked: usize,
    /// Distinct chunks checked.
    pub chunks_checked: usize,
    /// Chunks present in the store but referenced by no manifest.
    pub orphan_chunks: usize,
    /// Problems found (empty when the repository is healthy).
    pub issues: Vec<VerifyIssue>,
}

impl VerifyReport {
    /// Whether the repository passed the check.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.issues.is_empty()
    }
}

/// A stored verification outcome (catalog row form of a [`VerifyReport`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// Repository location that was checked.
    pub repo: String,
    /// Mode that ran.
    pub mode: VerifyMode,
    /// Whether the repository passed.
    pub ok: bool,
    /// Distinct chunks checked.
    pub chunks_checked: u64,
    /// Problems found (empty when healthy).
    pub issues: Vec<String>,
    /// When the run finished (UTC).
    pub finished_at: DateTime<Utc>,
}

/// Runs a verification pass over the repository.
///
/// # Errors
///
/// Returns IO/catalog-style errors when the repository cannot be read at
/// all (missing config, unreadable files abort the pass); individual
/// integrity problems are collected in the report instead.
pub fn verify(
    repo: &Repository,
    mode: VerifyMode,
    cancel: Option<Arc<AtomicBool>>,
    mut on_progress: impl FnMut(usize, usize) -> crate::Result<()>,
) -> crate::Result<VerifyReport> {
    let started_at = Utc::now();
    let mut report = VerifyReport {
        mode,
        started_at,
        finished_at: started_at,
        manifests_checked: 0,
        chunks_checked: 0,
        orphan_chunks: 0,
        issues: Vec::new(),
    };

    // 1. Load and authenticate every manifest, collecting references.
    let mut referenced: BTreeMap<String, u64> = BTreeMap::new();
    let snapshots = repo.list_snapshot_ids()?;
    for id in &snapshots {
        if let Some(flag) = &cancel
            && flag.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(crate::Error::Cancelled);
        }
        match repo.read_manifest(id) {
            Ok(manifest) => {
                report.manifests_checked += 1;
                for (_, entry) in manifest.iter_files() {
                    for chunk in &entry.chunks {
                        referenced.insert(chunk.hash.to_hex(), chunk.plain_len);
                    }
                }
            }
            Err(e) => report.issues.push(VerifyIssue {
                message: format!("snapshot {id} failed to load: {e}"),
            }),
        }
    }
    if snapshots.is_empty() {
        report.issues.push(VerifyIssue {
            message: "repository contains no snapshots".into(),
        });
    }

    // 2. Check every referenced chunk.
    let total = referenced.len();
    for (index, (hex, plain_len)) in referenced.iter().enumerate() {
        if let Some(flag) = &cancel
            && flag.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(crate::Error::Cancelled);
        }
        on_progress(index, total)?;
        let hash = ChunkHash::from_hex(hex)?;
        if !repo.chunk_exists(&hash)? {
            report.issues.push(VerifyIssue {
                message: format!("chunk {hex} referenced by a snapshot is MISSING"),
            });
            continue;
        }
        if mode == VerifyMode::Full {
            match repo.read_chunk(&hash, *plain_len) {
                Ok(_) => {}
                Err(e) => report.issues.push(VerifyIssue {
                    message: format!("chunk {hex} failed verification: {e}"),
                }),
            }
        }
        report.chunks_checked += 1;
    }

    // 3. Orphan chunks: stored but referenced nowhere (informational).
    let stored = repo.list_chunks()?;
    for rel in &stored {
        let hex = rel
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".chunk");
        if !referenced.contains_key(hex) {
            report.orphan_chunks += 1;
        }
    }

    report.finished_at = Utc::now();
    Ok(report)
}

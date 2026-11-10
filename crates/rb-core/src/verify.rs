//! Repository integrity verification ("validité des sauvegardes").
//!
//! - **Quick**: every chunk referenced by every manifest exists, and every
//!   manifest authenticates.
//! - **Full**: additionally read + authenticate + re-hash every referenced
//!   chunk, check every pack trailer against the index, and report orphan
//!   chunks (loose files or pack blobs stored but unreferenced — harmless,
//!   they only waste space) and unindexed packs (left by a crash between
//!   pack write and index write; `prune` removes them).

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
    /// Chunks present in the store (loose or in a pack) but referenced by
    /// no manifest.
    pub orphan_chunks: usize,
    /// Pack files present on the backend but listed by no index file.
    pub unindexed_packs: usize,
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
        unindexed_packs: 0,
        issues: Vec::new(),
    };

    // Shared lock: concurrent prune would make live chunks look missing.
    let _lock = repo.acquire_shared_lock("verify")?;
    repo.invalidate_pack_index();

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

    // 3. Packs: in full mode every trailer must match the index (detects
    // index/pack drift and a damaged trailer).
    let index = repo.pack_index()?;
    if mode == VerifyMode::Full {
        for (pack_id, expected) in &index.packs {
            if let Some(flag) = &cancel
                && flag.load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(crate::Error::Cancelled);
            }
            if !index.existing_packs.contains(pack_id) {
                continue; // already reported MISSING per referenced chunk
            }
            for message in repo.check_pack_against_index(pack_id, expected) {
                report.issues.push(VerifyIssue { message });
            }
        }
    }
    report.unindexed_packs = index
        .existing_packs
        .iter()
        .filter(|id| !index.packs.contains_key(*id))
        .count();

    // 4. Orphan chunks: stored but referenced nowhere (informational).
    let mut stored: std::collections::BTreeSet<String> =
        index.blobs.keys().map(ChunkHash::to_hex).collect();
    for rel in repo.list_chunks()? {
        let hex = rel
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".chunk");
        if rel.ends_with(".chunk") {
            stored.insert(hex.to_owned());
        }
    }
    report.orphan_chunks = stored
        .iter()
        .filter(|hex| !referenced.contains_key(*hex))
        .count();

    report.finished_at = Utc::now();
    Ok(report)
}

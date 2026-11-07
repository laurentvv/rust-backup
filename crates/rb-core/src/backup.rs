//! The backup engine: walk sources, chunk + dedup + store, finalize the
//! snapshot manifest LAST.
//!
//! Incremental behavior falls out of content-defined dedup: the first run
//! stores every chunk (a full backup), later runs store only chunks whose
//! content has never been seen — with a size+mtime fast path that avoids
//! re-reading unchanged files entirely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use walkdir::WalkDir;

use crate::chunker::ChunkerConfig;
use crate::domain::JobConfig;
use crate::hash::ChunkHash;
use crate::manifest::{
    ChunkRef, EntryKind, FileEntry, SkippedItem, SnapshotManifest, SourceSnapshot,
};
use crate::repo::Repository;

/// Live progress events emitted during a backup run.
#[derive(Debug, Clone)]
pub enum BackupEvent {
    /// A source file is being processed.
    FileStarted {
        /// Absolute path of the file.
        path: PathBuf,
        /// Size in bytes.
        size: u64,
    },
    /// A source file has been processed.
    FileDone {
        /// Absolute path of the file.
        path: PathBuf,
        /// Size in bytes.
        size: u64,
        /// Chunks newly stored for this file.
        new_chunks: u64,
        /// Uncompressed bytes of those new chunks.
        new_bytes: u64,
    },
    /// A file could not be read (locked, permissions); the run continues.
    FileSkipped {
        /// Absolute path of the file.
        path: PathBuf,
        /// Why it was skipped.
        reason: String,
    },
    /// The snapshot manifest has been finalized — the snapshot now exists.
    SnapshotFinalized {
        /// Id of the new snapshot.
        snapshot_id: uuid::Uuid,
    },
}

/// Aggregate counters of one backup run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackupStats {
    /// Files captured in the snapshot.
    pub files_total: u64,
    /// Bytes of captured files.
    pub bytes_total: u64,
    /// Chunks newly written to the repository.
    pub chunks_new: u64,
    /// Uncompressed bytes of new chunks.
    pub bytes_new: u64,
    /// Files skipped (locked/unreadable).
    pub files_skipped: u64,
}

/// Options for a backup run.
#[derive(Debug, Clone, Default)]
pub struct BackupOptions {
    /// Cooperative cancellation flag; checked between files. When set, the
    /// run aborts WITHOUT finalizing a snapshot.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Runs one backup of `job` into `repo`.
///
/// The snapshot manifest is written only after every source has been
/// processed successfully — an aborted or failed run leaves chunks behind
/// (harmless, deduplicated later) but never a half-complete snapshot.
///
/// # Errors
///
/// Returns [`Error::Cancelled`](crate::Error::Cancelled) when the cancel
/// flag was raised; IO errors while walking directories abort the run;
/// unreadable FILES are skipped with a [`BackupEvent::FileSkipped`].
pub fn run_backup(
    repo: &Repository,
    job: &JobConfig,
    options: &BackupOptions,
    mut on_event: impl FnMut(BackupEvent) -> crate::Result<()>,
) -> crate::Result<(SnapshotManifest, BackupStats)> {
    job.validate()?;
    let chunker_cfg: ChunkerConfig = repo.config().chunker;
    let cancel = options.cancel.clone();

    // Fast path: reuse chunk lists of unchanged files (size + mtime) from
    // the previous snapshot of this job.
    let previous = repo.latest_manifest_for_job(&job.id)?;
    let mut prev_index: HashMap<(String, String), FileEntry> = HashMap::new();
    if let Some(prev) = &previous {
        for source in &prev.sources {
            let root_key = source.root.to_string_lossy().to_string();
            for entry in &source.entries {
                prev_index.insert(
                    (root_key.clone(), entry.path.to_string_lossy().to_string()),
                    entry.clone(),
                );
            }
        }
    }

    let mut manifest = SnapshotManifest::new(job.id, &job.name);
    let mut stats = BackupStats::default();

    for source_root in &job.sources {
        let root_key = source_root.to_string_lossy().to_string();
        let mut entries: Vec<FileEntry> = Vec::new();

        for walk_entry in WalkDir::new(source_root)
            .follow_links(false)
            .sort_by_file_name()
        {
            if is_cancelled(&cancel) {
                return Err(crate::Error::Cancelled);
            }
            let walk_entry = walk_entry.map_err(|e| {
                crate::Error::Io(std::io::Error::other(format!(
                    "walking {}: {e}",
                    source_root.display()
                )))
            })?;
            let abs = walk_entry.path();
            let rel = abs
                .strip_prefix(source_root)
                .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;
            if rel.as_os_str().is_empty() {
                continue; // the root itself
            }
            let meta = walk_entry.metadata().map_err(|e| {
                crate::Error::Io(std::io::Error::other(format!(
                    "metadata {}: {e}",
                    abs.display()
                )))
            })?;

            if walk_entry.file_type().is_dir() {
                entries.push(FileEntry {
                    path: rel.to_path_buf(),
                    size: 0,
                    mtime_ns: mtime_ns(&meta),
                    kind: EntryKind::Dir,
                    chunks: Vec::new(),
                });
                continue;
            }
            if walk_entry.file_type().is_symlink() {
                entries.push(FileEntry {
                    path: rel.to_path_buf(),
                    size: 0,
                    mtime_ns: 0,
                    kind: EntryKind::Symlink,
                    chunks: Vec::new(),
                });
                continue;
            }

            // Regular file.
            let size = meta.len();
            let mtime = mtime_ns(&meta);
            on_event(BackupEvent::FileStarted {
                path: abs.to_path_buf(),
                size,
            })?;

            let key = (root_key.clone(), rel.to_string_lossy().to_string());
            if let Some(prev_entry) = prev_index.get(&key)
                && prev_entry.size == size
                && prev_entry.mtime_ns == mtime
            {
                entries.push(prev_entry.clone());
                stats.files_total += 1;
                stats.bytes_total += size;
                on_event(BackupEvent::FileDone {
                    path: abs.to_path_buf(),
                    size,
                    new_chunks: 0,
                    new_bytes: 0,
                })?;
                continue;
            }

            match backup_file(repo, abs, &chunker_cfg) {
                Ok(mut result) => {
                    result.entry.path = rel.to_path_buf();
                    entries.push(result.entry);
                    stats.files_total += 1;
                    stats.bytes_total += size;
                    stats.chunks_new += result.new_chunks;
                    stats.bytes_new += result.new_bytes;
                    on_event(BackupEvent::FileDone {
                        path: abs.to_path_buf(),
                        size,
                        new_chunks: result.new_chunks,
                        new_bytes: result.new_bytes,
                    })?;
                }
                Err(crate::Error::Io(e)) if is_sharing_violation(&e) => {
                    let reason = format!("locked or unreadable: {e}");
                    on_event(BackupEvent::FileSkipped {
                        path: abs.to_path_buf(),
                        reason: reason.clone(),
                    })?;
                    manifest.skipped.push(SkippedItem {
                        path: rel.to_path_buf(),
                        reason,
                    });
                    stats.files_skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        manifest.sources.push(SourceSnapshot {
            root: source_root.clone(),
            entries,
        });
    }

    // Finalize LAST: until this atomic write lands, the snapshot does not
    // exist for any reader (restores, verifies, listings).
    repo.write_manifest(&manifest)?;
    on_event(BackupEvent::SnapshotFinalized {
        snapshot_id: manifest.id,
    })?;

    Ok((manifest, stats))
}

/// Result of backing up a single file.
struct FileBackup {
    entry: FileEntry,
    new_chunks: u64,
    new_bytes: u64,
}

/// Reads, chunks, dedups and stores one file.
fn backup_file(
    repo: &Repository,
    abs: &Path,
    chunker_cfg: &ChunkerConfig,
) -> crate::Result<FileBackup> {
    let file = std::fs::File::open(abs)?;
    let meta = file.metadata()?;
    let reader = std::io::BufReader::with_capacity(2 * 1024 * 1024, file);
    let chunker = fastcdc::v2020::StreamCDC::new(
        reader,
        chunker_cfg.min_size,
        chunker_cfg.avg_size,
        chunker_cfg.max_size,
    );

    let mut chunks: Vec<ChunkRef> = Vec::new();
    let mut size = 0u64;
    let mut new_chunks = 0u64;
    let mut new_bytes = 0u64;

    for entry in chunker {
        let entry = entry.map_err(|e| {
            crate::Error::Io(std::io::Error::other(format!(
                "chunking {}: {e}",
                abs.display()
            )))
        })?;
        let data = entry.data;
        size += data.len() as u64;
        let hash = ChunkHash::from_data(&data);
        let plain_len = data.len() as u64;
        if repo.store_chunk(&hash, &data)? {
            new_chunks += 1;
            new_bytes += plain_len;
        }
        chunks.push(ChunkRef { hash, plain_len });
    }

    Ok(FileBackup {
        entry: FileEntry {
            path: PathBuf::new(), // the caller sets the relative path
            size,
            mtime_ns: mtime_ns(&meta),
            kind: EntryKind::File,
            chunks,
        },
        new_chunks,
        new_bytes,
    })
}

fn is_cancelled(cancel: &Option<Arc<AtomicBool>>) -> bool {
    cancel
        .as_ref()
        .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64)
}

/// Windows sharing violation (ERROR_SHARING_VIOLATION, 32) or access
/// denied (5) on an exclusively locked file: skip instead of failing.
#[cfg(windows)]
fn is_sharing_violation(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(32) | Some(5))
}

/// POSIX equivalents (EACCES/EPERM): skip instead of failing.
#[cfg(not(windows))]
fn is_sharing_violation(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(13) | Some(1))
}

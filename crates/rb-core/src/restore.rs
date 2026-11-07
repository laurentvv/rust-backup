//! The restore engine: bring files and directories from a snapshot back
//! to their original location, or to a destination of the user's choice.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use uuid::Uuid;

use crate::manifest::{EntryKind, FileEntry, SnapshotManifest, SourceSnapshot};
use crate::repo::Repository;

/// What to do when a restore target already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverwritePolicy {
    /// Keep the existing file, count it as skipped.
    #[default]
    Skip,
    /// Replace the existing file with the snapshot version.
    Overwrite,
}

/// Where restored data lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreDestination {
    /// Back to the absolute paths recorded in the snapshot manifest.
    Original,
    /// Under `base`, preserving each source's subtree:
    /// `base/<source-root-name>/<relative-path>`.
    Target(PathBuf),
}

/// One selected item to restore: a path relative to its source root.
/// Selecting a directory restores it and everything below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSelection {
    /// Absolute source root as recorded in the manifest.
    pub source_root: PathBuf,
    /// Path relative to that root (a file or a directory).
    pub rel_path: PathBuf,
}

/// Options for a restore run.
#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    /// Existing-file policy (default: skip).
    pub overwrite: OverwritePolicy,
    /// Cooperative cancellation flag.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Live progress events of a restore run.
#[derive(Debug, Clone)]
pub enum RestoreEvent {
    /// A file is being restored.
    FileStarted {
        /// Destination path.
        dest: PathBuf,
        /// Bytes expected.
        size: u64,
    },
    /// A file was restored successfully.
    FileDone {
        /// Destination path.
        dest: PathBuf,
        /// Bytes written.
        bytes: u64,
    },
    /// A file was skipped (already exists under the skip policy).
    FileSkipped {
        /// Destination path.
        dest: PathBuf,
        /// Why.
        reason: String,
    },
}

/// Aggregate counters of a restore run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestoreStats {
    /// Files restored.
    pub files_restored: u64,
    /// Bytes restored.
    pub bytes_restored: u64,
    /// Files skipped (exists + skip policy, or not writable).
    pub files_skipped: u64,
    /// Directories created.
    pub dirs_created: u64,
}

/// Restores the `selection` from snapshot `snapshot_id`.
///
/// Files are written to a temporary name and renamed into place, so an
/// interrupted restore never leaves half-written files at their final
/// names (under the overwrite policy, the previous file survives).
///
/// # Errors
///
/// Returns [`Error::Repo`](crate::Error::Repo) when the snapshot or a
/// selected path does not exist, [`Error::Cancelled`] via the flag, and
/// chunk/crypto errors when repository data fails authentication.
#[allow(clippy::too_many_lines)]
pub fn restore(
    repo: &Repository,
    snapshot_id: &Uuid,
    selection: &[RestoreSelection],
    destination: &RestoreDestination,
    options: &RestoreOptions,
    mut on_event: impl FnMut(RestoreEvent) -> crate::Result<()>,
) -> crate::Result<RestoreStats> {
    let manifest = repo.read_manifest(snapshot_id)?;
    let plan = build_plan(&manifest, selection, destination)?;

    let mut stats = RestoreStats::default();
    for (_source, entry, dest) in &plan {
        if is_cancelled(&options.cancel) {
            return Err(crate::Error::Cancelled);
        }
        match entry.kind {
            EntryKind::Dir => {
                std::fs::create_dir_all(dest)?;
                stats.dirs_created += 1;
            }
            EntryKind::Symlink => {
                // Recreating links needs privileges on Windows; record and
                // continue rather than failing the whole restore.
                stats.files_skipped += 1;
                on_event(RestoreEvent::FileSkipped {
                    dest: dest.clone(),
                    reason: "symlink recreation not supported yet".into(),
                })?;
            }
            EntryKind::File => {
                if dest.exists() && options.overwrite == OverwritePolicy::Skip {
                    stats.files_skipped += 1;
                    on_event(RestoreEvent::FileSkipped {
                        dest: dest.clone(),
                        reason: "already exists (skip policy)".into(),
                    })?;
                    continue;
                }
                on_event(RestoreEvent::FileStarted {
                    dest: dest.clone(),
                    size: entry.size,
                })?;
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                restore_file(repo, &entry.chunks, dest)?;
                set_mtime(dest, entry.mtime_ns);
                stats.files_restored += 1;
                stats.bytes_restored += entry.size;
                on_event(RestoreEvent::FileDone {
                    dest: dest.clone(),
                    bytes: entry.size,
                })?;
            }
        }
    }
    Ok(stats)
}

/// The concrete work list: `(source snapshot, entry, destination path)`.
/// Expands the selection (files and directory prefixes) against the
/// manifest and computes destination paths.
fn build_plan<'a>(
    manifest: &'a SnapshotManifest,
    selection: &[RestoreSelection],
    destination: &RestoreDestination,
) -> crate::Result<Vec<(&'a SourceSnapshot, &'a FileEntry, PathBuf)>> {
    // Group selected paths per source root for prefix expansion.
    let mut selected: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for item in selection {
        selected
            .entry(item.source_root.to_string_lossy().to_string())
            .or_default()
            .push(item.rel_path.clone());
    }

    let mut plan: Vec<(&SourceSnapshot, &FileEntry, PathBuf)> = Vec::new();
    let mut matched_any = false;
    for source in &manifest.sources {
        let root_key = source.root.to_string_lossy().to_string();
        let Some(wanted) = selected.get(&root_key) else {
            continue;
        };
        if wanted.is_empty() {
            continue;
        }
        let source_name = source_dir_name(source);
        for entry in &source.entries {
            let hit = wanted.iter().any(|w| {
                let w_norm = normalize(w);
                let p_norm = normalize(&entry.path);
                // An empty selection path selects the whole source.
                w_norm.is_empty() || p_norm == w_norm || p_norm.starts_with(&format!("{w_norm}/"))
            });
            if !hit {
                continue;
            }
            matched_any = true;
            let dest = match destination {
                RestoreDestination::Original => source.root.join(&entry.path),
                RestoreDestination::Target(base) => base.join(&source_name).join(&entry.path),
            };
            plan.push((source, entry, dest));
        }
    }
    if !matched_any {
        return Err(crate::Error::Repo(
            "selection matches nothing in this snapshot".into(),
        ));
    }
    Ok(plan)
}

/// Restores one file: stream chunks in order, verify each chunk's content
/// hash, write via temp file + atomic rename.
fn restore_file(
    repo: &Repository,
    chunks: &[crate::manifest::ChunkRef],
    dest: &Path,
) -> crate::Result<()> {
    let tmp =
        TempFileGuard(dest.with_extension(format!("rbrestore-{}", uuid::Uuid::new_v4().simple())));
    {
        use std::io::Write as _;
        let mut out = std::fs::File::create(&tmp.0)?;
        for chunk in chunks {
            let plaintext = repo.read_chunk(&chunk.hash, chunk.plain_len)?;
            let actual = crate::hash::ChunkHash::from_data(&plaintext);
            if actual != chunk.hash {
                return Err(crate::Error::Repo(format!(
                    "chunk {} content does not match its address",
                    chunk.hash
                )));
            }
            out.write_all(&plaintext)?;
        }
        out.sync_all()?;
    }
    // `rename` replaces an existing file atomically (Windows and POSIX):
    // an interrupted restore leaves either the old file or the new one.
    std::fs::rename(&tmp.0, dest)?;
    tmp.disarm();
    Ok(())
}

/// Removes its temp file on drop unless disarmed, so that EVERY early
/// return of [`restore_file`] (I/O error, AEAD failure, hash mismatch)
/// leaves no `.rbrestore-*` file behind.
struct TempFileGuard(std::path::PathBuf);

impl TempFileGuard {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn set_mtime(path: &Path, mtime_ns: i64) {
    let secs = mtime_ns.div_euclid(1_000_000_000);
    let nanos = mtime_ns.rem_euclid(1_000_000_000);
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(secs, nanos as u32)).ok();
}

/// Directory name used under a [`RestoreDestination::Target`] base for
/// each source root.
fn source_dir_name(source: &SourceSnapshot) -> String {
    source
        .root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty() && n != "/" && n != "\\")
        .unwrap_or_else(|| "source".into())
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_cancelled(cancel: &Option<Arc<AtomicBool>>) -> bool {
    cancel
        .as_ref()
        .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
}

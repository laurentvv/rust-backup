//! The backup engine: walk sources, chunk + dedup + store, finalize the
//! snapshot manifest LAST.
//!
//! Storage runs as a pipeline: the walk thread only reads, chunks and
//! hashes; worker threads compress/seal in parallel; upload writers with
//! dedicated backend channels land the chunks. Dedup membership comes from
//! an in-memory index built once per run, so the hot path never pays a
//! network round trip to ask "does this chunk already exist?".
//!
//! Incremental behavior falls out of content-defined dedup: the first run
//! stores every chunk (a full backup), later runs store only chunks whose
//! content has never been seen — with a size+mtime fast path that avoids
//! re-reading unchanged files entirely.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
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
    /// A source file has been processed (read, chunked, hashed and handed
    /// to the storage pipeline — the chunks may still be in flight; they
    /// are guaranteed landed when [`BackupEvent::SnapshotFinalized`]
    /// fires).
    FileDone {
        /// Absolute path of the file.
        path: PathBuf,
        /// Size in bytes.
        size: u64,
        /// Chunks newly queued for storage for this file.
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
    /// Kind of backup run performed (full or incremental).
    pub kind: crate::domain::BackupKind,
}

/// Options for a backup run.
#[derive(Debug, Clone, Default)]
pub struct BackupOptions {
    /// Cooperative cancellation flag; checked between files. When set, the
    /// run aborts WITHOUT finalizing a snapshot.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Explicit override for backup kind.
    /// `None` lets the engine decide automatically based on `job.max_incrementals`
    /// and the history of snapshots for this job.
    pub force_kind: Option<crate::domain::BackupKind>,
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
    let _lock = repo.acquire_lock("backup")?;
    let chunker_cfg: ChunkerConfig = repo.config().chunker;
    let cancel = options.cancel.clone();

    let previous = repo.latest_manifest_for_job(&job.id)?;

    // Determine whether this run is Full or Incremental:
    let kind = if let Some(forced) = options.force_kind {
        forced
    } else if previous.is_none() {
        // Initial backup for a job is always full.
        crate::domain::BackupKind::Full
    } else if job.max_incrementals == 0 {
        // Cadence disabled: always incremental after initial full.
        crate::domain::BackupKind::Incremental
    } else {
        let consec = repo.count_consecutive_incrementals_for_job(&job.id)?;
        if consec >= job.max_incrementals {
            crate::domain::BackupKind::Full
        } else {
            crate::domain::BackupKind::Incremental
        }
    };

    // Fast path: reuse chunk lists of unchanged files (size + mtime) from
    // the previous snapshot of this job IF this is an incremental run.
    let mut prev_index: HashMap<(String, String), FileEntry> = HashMap::new();
    if kind == crate::domain::BackupKind::Incremental
        && let Some(prev) = &previous
    {
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
    manifest.kind = kind;
    let mut stats = BackupStats {
        kind,
        ..Default::default()
    };

    // One pipeline per run: in-memory chunk index + parallel compress/seal
    // workers + parallel upload writers on dedicated backend channels.
    let pipeline = ChunkPipeline::spawn(repo)?;

    for source_root in &job.sources {
        let root_key = source_root.to_string_lossy().to_string();
        let mut entries: Vec<FileEntry> = Vec::new();
        let excludes = &job.excludes;

        for walk_entry in WalkDir::new(source_root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                if let Ok(rel) = entry.path().strip_prefix(source_root)
                    && !rel.as_os_str().is_empty()
                {
                    for pat in excludes {
                        if crate::domain::matches_exclude(rel, pat) {
                            return false;
                        }
                    }
                }
                true
            })
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

            match backup_file(&pipeline, abs, &chunker_cfg) {
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

    // Every queued chunk must have landed before the manifest can exist.
    pipeline.finish()?;

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
    pipeline: &ChunkPipeline,
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
        if pipeline.submit(hash, data)? {
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

/// Default parallel upload writers opened per run.
const UPLOAD_WORKERS: usize = 4;

/// Parallel upload writers opened per run. Several channels overlap the
/// server-side fsync latency that dominates slow destinations; tunable
/// through `RBACKUP_UPLOAD_WORKERS` (clamped to 1..=16). OpenSSH caps a
/// single SSH connection at `MaxSessions` (default 10) channels including
/// the control session, so values above ~8 need a raised server limit.
fn upload_workers() -> usize {
    std::env::var("RBACKUP_UPLOAD_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(UPLOAD_WORKERS, |n| n.clamp(1, 16))
}
/// Chunks buffered between the walk thread and the compress/seal workers.
/// Bounded so RAM stays sane even at the 4 MiB max chunk size.
const RAW_QUEUE: usize = 24;
/// Sealed blobs buffered between the compress/seal workers and the upload
/// writers.
const SEALED_QUEUE: usize = 12;
/// Cap on compress/seal workers: compression is rarely the bottleneck past
/// a dozen cores, and each worker holds one chunk of RAM.
const MAX_COMPRESS_WORKERS: usize = 16;

/// One chunk as cut by the walk thread, before compression.
struct RawChunk {
    hash: ChunkHash,
    data: Vec<u8>,
}

/// One sealed chunk blob ready for upload.
struct SealedChunk {
    rel: String,
    blob: Vec<u8>,
}

/// The storage pipeline of one backup run.
///
/// Topology: walk thread → [`RAW_QUEUE`] → N compress/seal workers →
/// [`SEALED_QUEUE`] → [`UPLOAD_WORKERS`] upload writers (each with a
/// dedicated backend channel). All queues are bounded: a slow destination
/// applies backpressure to the walk instead of ballooning RAM.
///
/// Failure semantics: the first worker error is remembered and the worker
/// exits; channel disconnects then cascade (senders observe the loss), so
/// the walk thread aborts promptly with that error. Chunks already landed
/// stay (harmless orphans, deduplicated later); no manifest is ever
/// written for a failed run.
struct ChunkPipeline {
    raw_tx: flume::Sender<RawChunk>,
    sealed_tx: flume::Sender<SealedChunk>,
    /// Chunks known to exist (run-start index) or already submitted this
    /// run — also the within-run dedup guard.
    known: Arc<Mutex<HashSet<ChunkHash>>>,
    first_error: Arc<Mutex<Option<crate::Error>>>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl ChunkPipeline {
    /// Builds the run-start chunk index and spawns the workers. Fails fast
    /// when the backend cannot open the upload writers.
    ///
    /// # Errors
    ///
    /// Returns listing errors from the index build and connection errors
    /// from writer creation.
    fn spawn(repo: &Repository) -> crate::Result<Self> {
        let known = Arc::new(Mutex::new(load_chunk_index(repo)?));
        let first_error: Arc<Mutex<Option<crate::Error>>> = Arc::new(Mutex::new(None));
        let (raw_tx, raw_rx) = flume::bounded(RAW_QUEUE);
        let (sealed_tx, sealed_rx) = flume::bounded(SEALED_QUEUE);
        let mut workers = Vec::new();

        // Upload writers first: a backend that cannot open them fails the
        // run before anything is read.
        let backend = Arc::clone(repo.backend());
        let wanted = upload_workers();
        for worker_id in 0..wanted {
            let mut writer = backend.open_writer().map_err(|e| {
                crate::Error::Repo(format!(
                    "opening upload writer {}/{}: {e} — the server may cap concurrent SFTP \
                     channels (OpenSSH MaxSessions, default 10); lower RBACKUP_UPLOAD_WORKERS",
                    worker_id + 1,
                    wanted
                ))
            })?;
            let rx: flume::Receiver<SealedChunk> = sealed_rx.clone();
            let errors = Arc::clone(&first_error);
            workers.push(std::thread::spawn(move || {
                while let Ok(chunk) = rx.recv() {
                    if let Err(e) = writer.write_new(&chunk.rel, &chunk.blob) {
                        record_error(&errors, e);
                        break;
                    }
                }
            }));
        }

        let compress_workers = std::thread::available_parallelism().map_or(4, |n| n.get());
        let compress_workers = compress_workers.min(MAX_COMPRESS_WORKERS);
        for _ in 0..compress_workers {
            let repo = repo.clone();
            let rx: flume::Receiver<RawChunk> = raw_rx.clone();
            let tx: flume::Sender<SealedChunk> = sealed_tx.clone();
            let errors = Arc::clone(&first_error);
            workers.push(std::thread::spawn(move || {
                while let Ok(chunk) = rx.recv() {
                    let blob = match repo.seal_chunk_blob(&chunk.hash, &chunk.data) {
                        Ok(blob) => blob,
                        Err(e) => {
                            record_error(&errors, e);
                            break;
                        }
                    };
                    let rel = Repository::chunk_rel(&chunk.hash);
                    if tx.send(SealedChunk { rel, blob }).is_err() {
                        break; // upload side gone; its error is recorded
                    }
                }
            }));
        }

        Ok(Self {
            raw_tx,
            sealed_tx,
            known,
            first_error,
            workers,
        })
    }

    /// Offers one chunk to the pipeline.
    ///
    /// Returns `Ok(true)` when the chunk is new (queued for upload —
    /// counting it as stored is safe because a later failure aborts the
    /// whole run before any manifest exists) and `Ok(false)` when the chunk
    /// was already known, in which case nothing is sent.
    ///
    /// # Errors
    ///
    /// Returns the first worker error (or a disconnect error when workers
    /// stopped unexpectedly).
    fn submit(&self, hash: ChunkHash, data: Vec<u8>) -> crate::Result<bool> {
        {
            let mut known = lock(&self.known);
            if known.contains(&hash) {
                return Ok(false);
            }
            known.insert(hash);
        }
        if self.raw_tx.send(RawChunk { hash, data }).is_err() {
            return Err(self.take_error().unwrap_or_else(|| {
                crate::Error::Repo("chunk pipeline stopped unexpectedly".into())
            }));
        }
        Ok(true)
    }

    /// Waits for every submitted chunk to land, then reports the first
    /// worker error, if any.
    ///
    /// # Errors
    ///
    /// Returns the first worker error recorded during the run.
    fn finish(self) -> crate::Result<()> {
        // Dropping the senders disconnects the queues; workers drain what
        // is left and exit, and join() waits for all of them.
        let Self {
            raw_tx,
            sealed_tx,
            known: _,
            first_error,
            workers,
        } = self;
        drop(raw_tx);
        drop(sealed_tx);
        for handle in workers {
            let _ = handle.join();
        }
        lock(&first_error).take().map_or(Ok(()), Err)
    }

    fn take_error(&self) -> Option<crate::Error> {
        lock(&self.first_error).take()
    }
}

/// Remembers the first error; later ones are dropped (the run reports the
/// root cause, not the cascade).
fn record_error(slot: &Mutex<Option<crate::Error>>, error: crate::Error) {
    let mut slot = lock(slot);
    if slot.is_none() {
        *slot = Some(error);
    }
}

/// Poisoning only happens on a panic, and a panicking worker means the run
/// is broken anyway: take the guard, report through normal errors.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Snapshot of every chunk the repository already holds, built once per
/// run. Malformed names (temp files, foreign files) are skipped: they are
/// not valid chunks and must not shadow a real hash.
fn load_chunk_index(repo: &Repository) -> crate::Result<HashSet<ChunkHash>> {
    let mut index = HashSet::new();
    for rel in repo.list_chunks()? {
        let Some((name, _)) = rel.rsplit_once('/') else {
            continue;
        };
        let Some(hex) = name.strip_suffix(".chunk") else {
            continue;
        };
        if let Ok(hash) = ChunkHash::from_hex(hex) {
            index.insert(hash);
        }
    }
    Ok(index)
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

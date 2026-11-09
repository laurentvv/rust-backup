//! The restore engine: bring files and directories from a snapshot back
//! to their original location, or to a destination of the user's choice.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::hash::ChunkHash;
use crate::manifest::{EntryKind, FileEntry, SnapshotManifest, SourceSnapshot};
use crate::pack::pack_rel;
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
    /// The work that follows: files to restore (after the skip policy) and
    /// their total size. Sent once, before the first file starts.
    Planned {
        /// Files that will be restored.
        files: u64,
        /// Their total size in bytes.
        bytes: u64,
    },
    /// Plaintext bytes were written to restored files since the last such
    /// event (a blob shared by several files counts once per file).
    BytesWritten {
        /// Bytes written.
        bytes: u64,
    },
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
/// The restore is plan driven. After the directories exist, every chunk of
/// every selected file is resolved to its location (a blob of a pack, or a
/// loose v1 chunk file) and the work is regrouped BY PACK: each pack is read
/// once, in offset order, through a few coalesced exact range reads
/// (`StorageBackend::read_ranges`), and each blob is authenticated,
/// decompressed, BLAKE3-checked and written at every file offset that needs
/// it (a blob shared by several files, or repeated inside one file, is read
/// once). Files are completed progressively: a file is written to a temp
/// name through positional writes, and renamed into place (mtime set) when
/// its last blob landed, so an interrupted restore never leaves half-written
/// files at their final names (under the overwrite policy, the previous file
/// survives) and removes its own temp files.
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
    // Shared lock: a concurrent prune or snapshot deletion must not remove
    // chunks this restore is about to read.
    let _lock = repo.acquire_shared_lock("restore")?;
    let manifest = repo.read_manifest(snapshot_id)?;
    let plan = build_plan(&manifest, selection, destination)?;

    let mut stats = RestoreStats::default();
    // Directories first, in plan order; symlinks are recorded as skipped;
    // files that already exist under the skip policy are skipped up front.
    let mut files: Vec<usize> = Vec::new();
    let mut parents: HashSet<PathBuf> = HashSet::new();
    for (idx, (_source, entry, dest)) in plan.iter().enumerate() {
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
                if options.overwrite == OverwritePolicy::Skip && dest.exists() {
                    stats.files_skipped += 1;
                    on_event(RestoreEvent::FileSkipped {
                        dest: dest.clone(),
                        reason: "already exists (skip policy)".into(),
                    })?;
                    continue;
                }
                if let Some(parent) = dest.parent()
                    && parents.insert(parent.to_path_buf())
                {
                    std::fs::create_dir_all(parent)?;
                }
                files.push(idx);
            }
        }
    }
    on_event(RestoreEvent::Planned {
        files: files.len() as u64,
        bytes: files.iter().map(|&i| plan[i].1.size).sum(),
    })?;

    // Files without chunks complete immediately; the others get a slot.
    let mut slots: Vec<Mutex<FileSlot>> = Vec::new();
    let mut slot_plan: Vec<usize> = Vec::new();
    for &idx in &files {
        let (_source, entry, dest) = &plan[idx];
        if entry.chunks.is_empty() {
            if entry.size != 0 {
                return Err(crate::Error::Repo(format!(
                    "{} has {} bytes but no chunks",
                    dest.display(),
                    entry.size
                )));
            }
            on_event(RestoreEvent::FileStarted {
                dest: dest.clone(),
                size: 0,
            })?;
            restore_empty_file(dest, entry.mtime_ns)?;
            stats.files_restored += 1;
            on_event(RestoreEvent::FileDone {
                dest: dest.clone(),
                bytes: 0,
            })?;
            continue;
        }
        slot_plan.push(idx);
        slots.push(Mutex::new(FileSlot {
            dest: dest.clone(),
            tmp: None,
            size: entry.size,
            mtime_ns: entry.mtime_ns,
            remaining: entry.chunks.len(),
            written: 0,
            finished: false,
        }));
    }
    if slots.is_empty() {
        return Ok(stats);
    }

    let jobs = build_jobs(repo, &plan, &slot_plan)?;
    let total_files = slots.len();
    let workers = restore_workers().min(jobs.order.len());
    let abort = AtomicBool::new(false);
    let ctx = Ctx {
        repo,
        slots,
        handles: Mutex::new(HandleCache::default()),
        abort: &abort,
        cancel: &options.cancel,
    };
    // Removes every incomplete temp file on EVERY exit path (error, cancel,
    // panic); the workers have all been joined by then.
    let _cleanup = Cleanup(&ctx);

    let (job_tx, job_rx) = flume::unbounded::<UnitRef>();
    let (msg_tx, msg_rx) = flume::unbounded::<Msg>();
    for unit in &jobs.order {
        // Receiver is alive: cannot fail.
        let _ = job_tx.send(*unit);
    }
    drop(job_tx);

    let mut first_error: Option<crate::Error> = None;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let worker = Worker {
                ctx: &ctx,
                tx: msg_tx.clone(),
            };
            let job_rx = job_rx.clone();
            let jobs = &jobs;
            scope.spawn(move || {
                while let Ok(unit) = job_rx.recv() {
                    if let Err(e) = worker.run_unit(jobs, unit) {
                        let _ = worker.tx.send(Msg::Failed(e));
                        return;
                    }
                }
            });
        }
        drop(msg_tx);

        let mut finished = 0usize;
        while finished < total_files {
            // All workers gone without finishing: an error was reported.
            let Ok(msg) = msg_rx.recv() else { break };
            let result = match msg {
                Msg::Started(slot) => {
                    let (_, entry, dest) = &plan[slot_plan[slot]];
                    on_event(RestoreEvent::FileStarted {
                        dest: dest.clone(),
                        size: entry.size,
                    })
                }
                Msg::Bytes(bytes) => on_event(RestoreEvent::BytesWritten { bytes }),
                Msg::Done(slot) => {
                    finished += 1;
                    let (_, entry, dest) = &plan[slot_plan[slot]];
                    stats.files_restored += 1;
                    stats.bytes_restored += entry.size;
                    on_event(RestoreEvent::FileDone {
                        dest: dest.clone(),
                        bytes: entry.size,
                    })
                }
                Msg::Failed(e) => Err(e),
            };
            if let Err(e) = result {
                // Keep the first error; a Cancelled echo from a worker must
                // not mask the real cause.
                first_error.get_or_insert(e);
                abort.store(true, Ordering::Relaxed);
                break;
            }
        }
        abort.store(true, Ordering::Relaxed);
    });
    match first_error {
        Some(e) => Err(e),
        None if is_cancelled(&options.cancel) => Err(crate::Error::Cancelled),
        None => Ok(stats),
    }
}

/// Worker-to-caller progress messages (events stay on the caller thread).
/// Slot numbers index the restore's file slots.
enum Msg {
    Started(usize),
    Bytes(u64),
    Done(usize),
    Failed(crate::Error),
}

/// Default number of restore worker threads (each one reads, decrypts and
/// writes whole packs).
const DEFAULT_RESTORE_WORKERS: usize = 4;
/// Environment override of the restore worker count (clamped to 1..=16).
pub const RESTORE_WORKERS_ENV: &str = "RBACKUP_RESTORE_WORKERS";
/// Two needed blobs of a pack at most this far apart share one range read
/// (the gap between them is read and thrown away).
const RUN_GAP: u64 = 1024 * 1024;
/// A coalesced run never grows beyond this (a larger single blob is its own
/// run).
const RUN_CAP: u64 = 16 * 1024 * 1024;
/// One `read_ranges` call carries at most this many bytes: with the default
/// 16 MiB packs that is one call per pack, and a worker never holds more than
/// about this much raw data.
const BATCH_CAP: u64 = 32 * 1024 * 1024;
/// Upper bound of simultaneously open temp-file handles.
const MAX_OPEN_HANDLES: usize = 256;

fn restore_workers() -> usize {
    std::env::var(RESTORE_WORKERS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(DEFAULT_RESTORE_WORKERS, |n| n.clamp(1, 16))
}

/// One needed blob and every `(file slot, file offset)` it must be written
/// at. A pack blob carries its `offset`/`length` in the pack file.
struct BlobJob {
    hash: ChunkHash,
    plain_len: u64,
    offset: u64,
    length: u64,
    targets: Vec<(usize, u64)>,
}

struct PackJob {
    id: String,
    blobs: Vec<BlobJob>,
}

#[derive(Clone, Copy)]
enum UnitRef {
    Pack(usize),
    Loose(usize),
}

/// The regrouped work: units in order of first appearance in the plan.
struct Jobs {
    packs: Vec<PackJob>,
    loose: Vec<BlobJob>,
    order: Vec<UnitRef>,
}

/// Resolves every chunk reference of every file slot to its location and
/// groups the needs by pack (distinct blobs, with all their targets).
fn build_jobs(
    repo: &Repository,
    plan: &[(&SourceSnapshot, &FileEntry, PathBuf)],
    slot_plan: &[usize],
) -> crate::Result<Jobs> {
    let index = repo.pack_index()?;
    let mut jobs = Jobs {
        packs: Vec::new(),
        loose: Vec::new(),
        order: Vec::new(),
    };
    let mut pack_of: HashMap<&str, (usize, HashMap<ChunkHash, usize>)> = HashMap::new();
    let mut loose_of: HashMap<ChunkHash, usize> = HashMap::new();
    for (slot, &plan_idx) in slot_plan.iter().enumerate() {
        let entry = plan[plan_idx].1;
        let mut file_offset = 0u64;
        for chunk in &entry.chunks {
            let target = (slot, file_offset);
            file_offset += chunk.plain_len;
            let blob = if let Some((pack, loc)) = index.blobs.get(&chunk.hash) {
                let (pack_idx, seen) = pack_of.entry(pack.as_str()).or_insert_with(|| {
                    jobs.packs.push(PackJob {
                        id: pack.clone(),
                        blobs: Vec::new(),
                    });
                    jobs.order.push(UnitRef::Pack(jobs.packs.len() - 1));
                    (jobs.packs.len() - 1, HashMap::new())
                });
                let blobs = &mut jobs.packs[*pack_idx].blobs;
                let blob_idx = *seen.entry(chunk.hash).or_insert_with(|| {
                    blobs.push(BlobJob {
                        hash: chunk.hash,
                        plain_len: chunk.plain_len,
                        offset: loc.offset,
                        length: loc.length,
                        targets: Vec::new(),
                    });
                    blobs.len() - 1
                });
                &mut blobs[blob_idx]
            } else {
                let loose_idx = *loose_of.entry(chunk.hash).or_insert_with(|| {
                    jobs.loose.push(BlobJob {
                        hash: chunk.hash,
                        plain_len: chunk.plain_len,
                        offset: 0,
                        length: 0,
                        targets: Vec::new(),
                    });
                    jobs.order.push(UnitRef::Loose(jobs.loose.len() - 1));
                    jobs.loose.len() - 1
                });
                &mut jobs.loose[loose_idx]
            };
            if blob.plain_len != chunk.plain_len {
                return Err(crate::Error::Repo(format!(
                    "chunk {} is referenced with two different lengths ({} and {})",
                    chunk.hash, blob.plain_len, chunk.plain_len
                )));
            }
            blob.targets.push(target);
        }
    }
    Ok(jobs)
}

/// Progress of one file being restored.
struct FileSlot {
    dest: PathBuf,
    /// Temp file, once created (lazily, by the first blob written).
    tmp: Option<PathBuf>,
    size: u64,
    mtime_ns: i64,
    /// Blob targets still to be written.
    remaining: usize,
    written: u64,
    /// Renamed into place.
    finished: bool,
}

/// Open temp-file handles, least recently used evicted past
/// [`MAX_OPEN_HANDLES`] (a 400k-file restore never holds 400k handles).
#[derive(Default)]
struct HandleCache {
    map: HashMap<usize, (Arc<std::fs::File>, u64)>,
    tick: u64,
}

/// State shared by the workers of one restore.
struct Ctx<'a> {
    repo: &'a Repository,
    slots: Vec<Mutex<FileSlot>>,
    handles: Mutex<HandleCache>,
    abort: &'a AtomicBool,
    cancel: &'a Option<Arc<AtomicBool>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Ctx<'_> {
    fn stopped(&self) -> bool {
        self.abort.load(Ordering::Relaxed) || is_cancelled(self.cancel)
    }

    fn cache_put(&self, slot: usize, file: &Arc<std::fs::File>) {
        let mut cache = lock(&self.handles);
        cache.tick += 1;
        let tick = cache.tick;
        cache.map.insert(slot, (Arc::clone(file), tick));
        if cache.map.len() > MAX_OPEN_HANDLES
            && let Some(oldest) = cache
                .map
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| *k)
        {
            cache.map.remove(&oldest);
        }
    }

    fn cache_get(&self, slot: usize) -> Option<Arc<std::fs::File>> {
        let mut cache = lock(&self.handles);
        cache.tick += 1;
        let tick = cache.tick;
        cache.map.get_mut(&slot).map(|(file, used)| {
            *used = tick;
            Arc::clone(file)
        })
    }
}

/// Removes the temp file of every incomplete slot when dropped.
struct Cleanup<'a>(&'a Ctx<'a>);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        lock(&self.0.handles).map.clear();
        for slot in &self.0.slots {
            let slot = lock(slot);
            if !slot.finished
                && let Some(tmp) = &slot.tmp
            {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }
}

struct Worker<'a> {
    ctx: &'a Ctx<'a>,
    tx: flume::Sender<Msg>,
}

impl Worker<'_> {
    fn run_unit(&self, jobs: &Jobs, unit: UnitRef) -> crate::Result<()> {
        match unit {
            UnitRef::Pack(i) => self.run_pack(&jobs.packs[i]),
            UnitRef::Loose(i) => {
                let blob = &jobs.loose[i];
                if self.ctx.stopped() {
                    return Err(crate::Error::Cancelled);
                }
                let stored = self
                    .ctx
                    .repo
                    .backend
                    .read(&Repository::chunk_rel(&blob.hash))?;
                let written = self.write_blob(blob, &stored)?;
                let _ = self.tx.send(Msg::Bytes(written));
                Ok(())
            }
        }
    }

    /// Reads one pack through coalesced exact ranges and restores every
    /// needed blob of it.
    fn run_pack(&self, pack: &PackJob) -> crate::Result<()> {
        let mut order: Vec<usize> = (0..pack.blobs.len()).collect();
        order.sort_by_key(|&i| pack.blobs[i].offset);

        // Runs: consecutive needed blobs, merged across small gaps.
        struct Run {
            start: u64,
            end: u64,
            first: usize,
            last: usize,
        }
        let mut runs: Vec<Run> = Vec::new();
        for (k, &i) in order.iter().enumerate() {
            let blob = &pack.blobs[i];
            let end = blob.offset + blob.length;
            match runs.last_mut() {
                Some(run)
                    if blob.offset <= run.end + RUN_GAP
                        && end.max(run.end) - run.start <= RUN_CAP =>
                {
                    run.end = run.end.max(end);
                    run.last = k + 1;
                }
                _ => runs.push(Run {
                    start: blob.offset,
                    end,
                    first: k,
                    last: k + 1,
                }),
            }
        }

        // One `read_ranges` call per batch of runs (one per pack by default).
        let rel = pack_rel(&pack.id);
        let mut from = 0usize;
        while from < runs.len() {
            let mut to = from;
            let mut bytes = 0u64;
            while to < runs.len()
                && (to == from || bytes + (runs[to].end - runs[to].start) <= BATCH_CAP)
            {
                bytes += runs[to].end - runs[to].start;
                to += 1;
            }
            if self.ctx.stopped() {
                return Err(crate::Error::Cancelled);
            }
            let ranges: Vec<(u64, u64)> = runs[from..to]
                .iter()
                .map(|r| (r.start, r.end - r.start))
                .collect();
            let buffers = self.ctx.repo.backend.read_ranges(&rel, &ranges)?;
            if buffers.len() != ranges.len() {
                return Err(crate::Error::Repo(format!(
                    "backend returned {} buffers for {} ranges of {rel}",
                    buffers.len(),
                    ranges.len()
                )));
            }
            for (run, buffer) in runs[from..to].iter().zip(&buffers) {
                let mut written = 0u64;
                for &i in &order[run.first..run.last] {
                    if self.ctx.stopped() {
                        return Err(crate::Error::Cancelled);
                    }
                    let blob = &pack.blobs[i];
                    let begin =
                        usize::try_from(blob.offset - run.start).map_err(std::io::Error::other)?;
                    let stored = usize::try_from(blob.length)
                        .ok()
                        .and_then(|len| buffer.get(begin..begin + len))
                        .ok_or_else(|| crate::Error::Repo(format!("short range read in {rel}")))?;
                    written += self.write_blob(blob, stored)?;
                }
                let _ = self.tx.send(Msg::Bytes(written));
            }
            from = to;
        }
        Ok(())
    }

    /// Opens a stored blob (MAC, length, BLAKE3 of the plaintext) and writes
    /// it at every target. Returns the bytes written.
    fn write_blob(&self, blob: &BlobJob, stored: &[u8]) -> crate::Result<u64> {
        let plaintext = self
            .ctx
            .repo
            .open_chunk_blob(stored, &blob.hash, blob.plain_len)?;
        if ChunkHash::from_data(&plaintext) != blob.hash {
            return Err(crate::Error::Repo(format!(
                "chunk {} content does not match its address",
                blob.hash
            )));
        }
        let mut written = 0u64;
        for &(slot, offset) in &blob.targets {
            if self.ctx.stopped() {
                return Err(crate::Error::Cancelled);
            }
            self.write_at(slot, offset, &plaintext)?;
            written += plaintext.len() as u64;
        }
        Ok(written)
    }

    /// Writes `data` at `offset` of the slot's temp file; completes the file
    /// when this was its last blob target.
    fn write_at(&self, slot: usize, offset: u64, data: &[u8]) -> crate::Result<()> {
        let file = self.handle(slot)?;
        write_all_at(&file, data, offset)?;
        drop(file);
        let complete = {
            let mut state = lock(&self.ctx.slots[slot]);
            state.written += data.len() as u64;
            state.remaining -= 1;
            state.remaining == 0
        };
        if complete {
            self.finish(slot)?;
        }
        Ok(())
    }

    /// The slot's temp file handle: created on first use (announced with a
    /// `Started` message), reopened when evicted from the handle cache.
    fn handle(&self, slot: usize) -> crate::Result<Arc<std::fs::File>> {
        if let Some(file) = self.ctx.cache_get(slot) {
            return Ok(file);
        }
        let mut state = lock(&self.ctx.slots[slot]);
        let file = if let Some(tmp) = &state.tmp {
            std::fs::OpenOptions::new().write(true).open(tmp)?
        } else {
            let tmp = temp_name(&state.dest);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            state.tmp = Some(tmp);
            let _ = self.tx.send(Msg::Started(slot));
            file
        };
        drop(state);
        let file = Arc::new(file);
        self.ctx.cache_put(slot, &file);
        Ok(file)
    }

    /// Last blob written: check the size, set mtime, rename into place.
    fn finish(&self, slot: usize) -> crate::Result<()> {
        lock(&self.ctx.handles).map.remove(&slot);
        let mut state = lock(&self.ctx.slots[slot]);
        if state.written != state.size {
            return Err(crate::Error::Repo(format!(
                "{} restored {} bytes, manifest says {}",
                state.dest.display(),
                state.written,
                state.size
            )));
        }
        let tmp = state
            .tmp
            .clone()
            .ok_or_else(|| crate::Error::Repo("file completed without a temp file".into()))?;
        set_mtime(&tmp, state.mtime_ns);
        // No `sync_all`: a restore is repeatable from the repository, so
        // per-file durability is not worth one flush per small file. The
        // temp file + atomic rename still guarantees that a crash never
        // leaves a half-written file under the final name. `rename`
        // replaces an existing file atomically (Windows and POSIX).
        std::fs::rename(&tmp, &state.dest)?;
        state.finished = true;
        drop(state);
        let _ = self.tx.send(Msg::Done(slot));
        Ok(())
    }
}

fn temp_name(dest: &Path) -> PathBuf {
    dest.with_extension(format!("rbrestore-{}", uuid::Uuid::new_v4().simple()))
}

/// Writes all of `buf` at `offset` without touching a shared file cursor.
fn write_all_at(file: &std::fs::File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        #[cfg(windows)]
        let n = {
            use std::os::windows::fs::FileExt as _;
            file.seek_write(buf, offset)?
        };
        #[cfg(unix)]
        let n = {
            use std::os::unix::fs::FileExt as _;
            file.write_at(buf, offset)?
        };
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Restores a zero-byte file: empty temp file, mtime, rename.
fn restore_empty_file(dest: &Path, mtime_ns: i64) -> crate::Result<()> {
    let tmp = TempFileGuard(temp_name(dest));
    std::fs::File::create(&tmp.0)?;
    set_mtime(&tmp.0, mtime_ns);
    std::fs::rename(&tmp.0, dest)?;
    tmp.disarm();
    Ok(())
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

/// Removes its temp file on drop unless disarmed, so that EVERY early
/// return of [`restore_empty_file`] leaves no `.rbrestore-*` file behind.
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
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
}

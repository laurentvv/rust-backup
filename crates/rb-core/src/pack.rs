//! Pack files and index files (repository format v2).
//!
//! Chunks are stored inside **pack files** (`packs/<2-hex>/<pack-id>.pack`)
//! instead of one file per chunk; `index/<index-id>.idx` files map every
//! chunk hash to its `(pack, offset, length)`. See `docs/REPO_FORMAT.md`.
//!
//! A pack is `"RBPK" | version u8 | blob_1 .. blob_n | trailer | trailer_len
//! u32 LE`, where every blob is exactly the v1 chunk blob and the trailer is
//! the sealed list of blobs (so an index can be rebuilt from packs alone).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::ChunkWriter;
use crate::hash::ChunkHash;
use crate::repo::{PruneReport, Repository, is_temp_file};

/// Pack file magic.
const PACK_MAGIC: &[u8; 4] = b"RBPK";
/// Index file magic.
const INDEX_MAGIC: &[u8; 4] = b"RBIX";
/// Version of the pack and index layouts.
const FORMAT_VERSION: u8 = 1;
/// Pack header size: magic + version.
const PACK_HEADER: usize = 5;
/// Chunk blob magic (v1 chunk blobs live unchanged inside packs).
const CHUNK_MAGIC: &[u8; 4] = b"RBCH";

/// Default pack size target in MiB.
const DEFAULT_PACK_MIB: u64 = 16;
/// Environment override of the pack size target (MiB, clamped to 4..=128).
pub const PACK_SIZE_ENV: &str = "RBACKUP_PACK_SIZE_MIB";

/// Pack size target in bytes (`RBACKUP_PACK_SIZE_MIB`, default 16, 4..=128).
#[must_use]
pub fn pack_target_bytes() -> usize {
    let mib = std::env::var(PACK_SIZE_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(DEFAULT_PACK_MIB, |n| n.clamp(4, 128));
    usize::try_from(mib * 1024 * 1024).unwrap_or(16 * 1024 * 1024)
}

/// Location of one chunk blob inside a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobEntry {
    /// Chunk hash, lowercase hex.
    pub hash: String,
    /// Offset of the blob from the start of the pack file.
    pub offset: u64,
    /// Stored blob length in bytes.
    pub length: u64,
    /// Uncompressed chunk length.
    pub plain_len: u64,
}

/// Decrypted pack trailer.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackTrailer {
    version: u32,
    blobs: Vec<BlobEntry>,
}

/// One pack as listed by an index file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexPack {
    /// Pack id (32 lowercase hex chars).
    pub pack: String,
    /// Blobs stored in the pack.
    pub blobs: Vec<BlobEntry>,
}

/// Decrypted index file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexFile {
    version: u32,
    packs: Vec<IndexPack>,
}

/// Repo-relative path of a pack file.
#[must_use]
pub fn pack_rel(pack_id: &str) -> String {
    let shard = pack_id.get(..2).unwrap_or("00");
    format!("packs/{shard}/{pack_id}.pack")
}

/// Pack id of a repo-relative pack path (`None` for foreign/temp names).
#[must_use]
pub fn pack_id_of(rel: &str) -> Option<String> {
    let stem = rel.rsplit('/').next()?.strip_suffix(".pack")?;
    Uuid::parse_str(stem).ok().map(|id| id.simple().to_string())
}

fn pack_ad(pack_id: &Uuid) -> Vec<u8> {
    let mut ad = Vec::with_capacity(20);
    ad.extend_from_slice(PACK_MAGIC);
    ad.extend_from_slice(pack_id.as_bytes());
    ad
}

fn index_ad(index_id: &Uuid) -> Vec<u8> {
    let mut ad = Vec::with_capacity(20);
    ad.extend_from_slice(INDEX_MAGIC);
    ad.extend_from_slice(index_id.as_bytes());
    ad
}

/// In-memory pack under construction.
pub(crate) struct PackBuilder {
    buf: Vec<u8>,
    blobs: Vec<BlobEntry>,
}

impl PackBuilder {
    pub(crate) fn new() -> Self {
        let mut buf = Vec::with_capacity(pack_target_bytes() + (1 << 20));
        buf.extend_from_slice(PACK_MAGIC);
        buf.push(FORMAT_VERSION);
        Self {
            buf,
            blobs: Vec::new(),
        }
    }

    /// Appends one v1 chunk blob.
    pub(crate) fn add(&mut self, hash: &ChunkHash, blob: &[u8], plain_len: u64) {
        self.blobs.push(BlobEntry {
            hash: hash.to_hex(),
            offset: self.buf.len() as u64,
            length: blob.len() as u64,
            plain_len,
        });
        self.buf.extend_from_slice(blob);
    }

    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

/// A finished pack ready to be written.
pub(crate) struct SealedPack {
    pub(crate) rel: String,
    pub(crate) data: Vec<u8>,
    pub(crate) entry: IndexPack,
}

/// Everything the index files say, merged (later files win; identical
/// content anyway).
pub(crate) struct PackIndex {
    /// Chunk hash to its location.
    pub(crate) blobs: HashMap<ChunkHash, (String, BlobEntry)>,
    /// Per pack: the union of its index entries, keyed by hash hex.
    pub(crate) packs: BTreeMap<String, BTreeMap<String, BlobEntry>>,
    /// Ids of the pack files present on the backend.
    pub(crate) existing_packs: HashSet<String>,
    /// Repo-relative paths of the index files that were read.
    pub(crate) index_files: Vec<String>,
}

/// Lazily loaded, shared pack state of a [`Repository`] (and its clones).
#[derive(Default)]
pub(crate) struct PackState {
    index: std::sync::Mutex<Option<Arc<PackIndex>>>,
    upgraded: std::sync::Mutex<bool>,
    /// Read-ahead windows of pack files (restore path).
    windows: std::sync::Mutex<Vec<Window>>,
}

/// Target size of a read-ahead window.
const WINDOW_TARGET: u64 = 4 * 1024 * 1024;
/// Maximum number of cached windows (bounds memory to ~8 x window).
const WINDOW_SLOTS: usize = 8;

/// A contiguous byte range of one pack, kept in memory.
struct Window {
    pack: String,
    start: u64,
    data: Arc<Vec<u8>>,
}

fn guard<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Repository {
    /// Seals a finished pack builder: trailer, length suffix, fresh pack id.
    pub(crate) fn seal_pack(&self, builder: PackBuilder) -> crate::Result<SealedPack> {
        let id = Uuid::new_v4();
        let trailer = serde_json::to_vec(&PackTrailer {
            version: 1,
            blobs: builder.blobs.clone(),
        })?;
        let sealed = self.cipher.seal_with_ad(&trailer, &pack_ad(&id))?;
        let trailer_len = u32::try_from(sealed.len())
            .map_err(|_| crate::Error::Repo("pack trailer too large".into()))?;
        let mut data = builder.buf;
        data.extend_from_slice(&sealed);
        data.extend_from_slice(&trailer_len.to_le_bytes());
        let pack = id.simple().to_string();
        Ok(SealedPack {
            rel: pack_rel(&pack),
            data,
            entry: IndexPack {
                pack,
                blobs: builder.blobs,
            },
        })
    }

    /// Switches a v1 repository to config version 2 (atomically) before its
    /// first pack lands; a no-op on version 2 and after the first call.
    pub(crate) fn ensure_pack_format(&self) -> crate::Result<()> {
        let mut done = guard(&self.pack_state.upgraded);
        if *done {
            return Ok(());
        }
        let mut config: crate::repo::RepoConfig =
            serde_json::from_slice(&self.backend.read("config.json")?)
                .map_err(|e| crate::Error::Repo(format!("invalid config.json: {e}")))?;
        if config.version < 2 {
            config.version = 2;
            self.backend.write_atomic(
                "config.json",
                serde_json::to_string_pretty(&config)?.as_bytes(),
            )?;
        }
        *done = true;
        Ok(())
    }

    /// Writes ONE index file listing `packs` (durable, atomic) and returns
    /// its repo-relative path. Callers write it after every pack is
    /// durable and before the snapshot manifest.
    ///
    /// # Errors
    ///
    /// Returns serialization, crypto or IO errors.
    pub fn write_index(&self, packs: &[IndexPack]) -> crate::Result<String> {
        let id = Uuid::new_v4();
        let json = serde_json::to_vec(&IndexFile {
            version: 1,
            packs: packs.to_vec(),
        })?;
        let sealed = self.cipher.seal_with_ad(&json, &index_ad(&id))?;
        let mut blob = Vec::with_capacity(PACK_HEADER + sealed.len());
        blob.extend_from_slice(INDEX_MAGIC);
        blob.push(FORMAT_VERSION);
        blob.extend_from_slice(&sealed);
        let rel = format!("index/{}.idx", id.simple());
        self.backend.write_atomic(&rel, &blob)?;
        self.invalidate_pack_index();
        Ok(rel)
    }

    /// Forgets the cached chunk map (after the index set changed).
    pub fn invalidate_pack_index(&self) {
        *guard(&self.pack_state.index) = None;
        guard(&self.pack_state.windows).clear();
    }

    /// The merged chunk map, loaded once per repository instance.
    pub(crate) fn pack_index(&self) -> crate::Result<Arc<PackIndex>> {
        let mut slot = guard(&self.pack_state.index);
        if let Some(index) = slot.as_ref() {
            return Ok(Arc::clone(index));
        }
        let index = Arc::new(self.load_pack_index()?);
        *slot = Some(Arc::clone(&index));
        Ok(index)
    }

    fn load_pack_index(&self) -> crate::Result<PackIndex> {
        let mut blobs = HashMap::new();
        let mut packs: BTreeMap<String, BTreeMap<String, BlobEntry>> = BTreeMap::new();
        let mut index_files = Vec::new();
        for rel in self.backend.list("index")? {
            if is_temp_file(&rel) || !rel.ends_with(".idx") {
                continue;
            }
            let stem = rel
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .trim_end_matches(".idx");
            let id = Uuid::parse_str(stem)
                .map_err(|_| crate::Error::Repo(format!("malformed index file name {rel}")))?;
            let data = self.backend.read(&rel)?;
            if data.len() < PACK_HEADER || &data[..4] != INDEX_MAGIC {
                return Err(crate::Error::Repo(format!("corrupt index file {rel}")));
            }
            if data[4] != FORMAT_VERSION {
                return Err(crate::Error::Repo(format!(
                    "unsupported index version {} in {rel}",
                    data[4]
                )));
            }
            let json = self
                .cipher
                .open_with_ad(&data[PACK_HEADER..], &index_ad(&id))?;
            let file: IndexFile = serde_json::from_slice(&json)
                .map_err(|e| crate::Error::Repo(format!("invalid index file {rel}: {e}")))?;
            for pack in file.packs {
                let entries = packs.entry(pack.pack.clone()).or_default();
                for blob in pack.blobs {
                    let hash = ChunkHash::from_hex(&blob.hash)?;
                    blobs.insert(hash, (pack.pack.clone(), blob.clone()));
                    entries.insert(blob.hash.clone(), blob);
                }
            }
            index_files.push(rel);
        }
        let existing_packs = self
            .backend
            .list("packs")?
            .iter()
            .filter(|rel| !is_temp_file(rel))
            .filter_map(|rel| pack_id_of(rel))
            .collect();
        Ok(PackIndex {
            blobs,
            packs,
            existing_packs,
            index_files,
        })
    }

    /// Number of blob entries stored across all indexed packs (a chunk
    /// stored twice counts twice). For diagnostics and tests.
    ///
    /// # Errors
    ///
    /// Returns errors from reading or authenticating the index files.
    pub fn packed_blob_count(&self) -> crate::Result<u64> {
        Ok(self
            .pack_index()?
            .packs
            .values()
            .map(|blobs| blobs.len() as u64)
            .sum())
    }

    /// Where a chunk lives inside a pack: `(pack repo-relative path, offset,
    /// length)`; `None` for loose (v1) or unknown chunks. For diagnostics
    /// and tests.
    ///
    /// # Errors
    ///
    /// Returns errors from reading or authenticating the index files.
    pub fn chunk_location(&self, hash: &ChunkHash) -> crate::Result<Option<(String, u64, u64)>> {
        Ok(self
            .pack_index()?
            .blobs
            .get(hash)
            .map(|(pack, e)| (pack_rel(pack), e.offset, e.length)))
    }

    /// Lists every pack file (repo-relative paths, temp files included).
    ///
    /// # Errors
    ///
    /// Returns IO errors from listing.
    pub fn list_packs(&self) -> crate::Result<Vec<String>> {
        self.backend.list("packs")
    }

    /// Reads and authenticates the trailer of a whole pack file.
    pub(crate) fn parse_pack_trailer(
        &self,
        pack_id: &str,
        data: &[u8],
    ) -> crate::Result<Vec<BlobEntry>> {
        let id = Uuid::parse_str(pack_id)
            .map_err(|_| crate::Error::Repo(format!("malformed pack id {pack_id}")))?;
        if data.len() < PACK_HEADER + 4 || &data[..4] != PACK_MAGIC {
            return Err(crate::Error::Repo(format!(
                "pack {pack_id}: corrupt header"
            )));
        }
        if data[4] != FORMAT_VERSION {
            return Err(crate::Error::Repo(format!(
                "pack {pack_id}: unsupported version {}",
                data[4]
            )));
        }
        let tail = data.len() - 4;
        let trailer_len = u32::from_le_bytes(
            data[tail..]
                .try_into()
                .map_err(|_| crate::Error::Repo("short pack tail".into()))?,
        ) as usize;
        let start = tail
            .checked_sub(trailer_len)
            .filter(|s| *s >= PACK_HEADER)
            .ok_or_else(|| crate::Error::Repo(format!("pack {pack_id}: bad trailer length")))?;
        let json = self
            .cipher
            .open_with_ad(&data[start..tail], &pack_ad(&id))?;
        let trailer: PackTrailer = serde_json::from_slice(&json)
            .map_err(|e| crate::Error::Repo(format!("pack {pack_id}: invalid trailer: {e}")))?;
        Ok(trailer.blobs)
    }

    /// Full-verify helper: reads pack `pack_id` and compares its trailer
    /// with the index entries. Returns human-readable problems.
    pub(crate) fn check_pack_against_index(
        &self,
        pack_id: &str,
        expected: &BTreeMap<String, BlobEntry>,
    ) -> Vec<String> {
        let data = match self.backend.read(&pack_rel(pack_id)) {
            Ok(d) => d,
            Err(e) => return vec![format!("pack {pack_id} unreadable: {e}")],
        };
        let trailer = match self.parse_pack_trailer(pack_id, &data) {
            Ok(t) => t,
            Err(e) => return vec![format!("pack {pack_id} trailer failed verification: {e}")],
        };
        let actual: BTreeMap<&str, &BlobEntry> =
            trailer.iter().map(|b| (b.hash.as_str(), b)).collect();
        let mut problems = Vec::new();
        for (hash, entry) in expected {
            match actual.get(hash.as_str()) {
                None => problems.push(format!(
                    "pack {pack_id}: index lists blob {hash} that the pack trailer does not"
                )),
                Some(t) if *t != entry => problems.push(format!(
                    "pack {pack_id}: index entry of blob {hash} differs from the pack trailer"
                )),
                Some(_) => {}
            }
        }
        for hash in actual.keys() {
            if !expected.contains_key(*hash) {
                problems.push(format!(
                    "pack {pack_id}: blob {hash} is in the pack trailer but not in the index"
                ));
            }
        }
        problems
    }

    /// Reads one chunk blob out of a pack by byte range.
    pub(crate) fn read_pack_blob(
        &self,
        pack_id: &str,
        entry: &BlobEntry,
    ) -> crate::Result<Vec<u8>> {
        self.backend
            .read_range(&pack_rel(pack_id), entry.offset, entry.length)
    }

    /// Like [`Self::read_pack_blob`], but served from a small shared
    /// read-ahead cache: a miss fetches a window (about 4 MiB, never less
    /// than the blob, clamped to the end of the pack's blob area) with ONE
    /// range read, so consecutive small blobs cost one round trip instead of
    /// one each. Blobs not fully inside a window use an exact range read.
    pub(crate) fn read_pack_blob_windowed(
        &self,
        pack_id: &str,
        entry: &BlobEntry,
        index: &PackIndex,
    ) -> crate::Result<Vec<u8>> {
        let end = entry.offset + entry.length;
        if entry.length >= WINDOW_TARGET {
            return self.read_pack_blob(pack_id, entry);
        }
        {
            let mut windows = guard(&self.pack_state.windows);
            if let Some(pos) = windows.iter().position(|w| {
                w.pack == pack_id && w.start <= entry.offset && end <= w.start + w.data.len() as u64
            }) {
                let win = windows.remove(pos);
                let from = (entry.offset - win.start) as usize;
                let blob = win.data[from..from + entry.length as usize].to_vec();
                windows.push(win); // most recently used goes last
                return Ok(blob);
            }
        }
        // End of the blob area: the largest blob end the index knows.
        let data_end = index
            .packs
            .get(pack_id)
            .and_then(|blobs| blobs.values().map(|b| b.offset + b.length).max())
            .unwrap_or(end);
        let want = WINDOW_TARGET.min(data_end.saturating_sub(entry.offset));
        if want <= entry.length {
            return self.read_pack_blob(pack_id, entry);
        }
        let data = Arc::new(
            self.backend
                .read_range(&pack_rel(pack_id), entry.offset, want)?,
        );
        let blob = data
            .get(..entry.length as usize)
            .ok_or_else(|| crate::Error::Repo("short window read".into()))?
            .to_vec();
        let mut windows = guard(&self.pack_state.windows);
        if windows.len() >= WINDOW_SLOTS {
            windows.remove(0);
        }
        windows.push(Window {
            pack: pack_id.to_owned(),
            start: entry.offset,
            data,
        });
        Ok(blob)
    }

    /// Pack part of [`Repository::prune_orphans`]: delete fully dead and
    /// unindexed packs, repack mostly dead ones, rewrite the index.
    /// The exclusive lock is held by the caller.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn prune_packs(
        &self,
        referenced: &HashSet<ChunkHash>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        report: &mut PruneReport,
    ) -> crate::Result<()> {
        self.invalidate_pack_index();
        let index = self.pack_index()?;

        // Temp files and unindexed packs (crash between pack and index).
        let mut unindexed: Vec<String> = Vec::new();
        for rel in self.backend.list("packs")? {
            if is_temp_file(&rel) {
                self.backend.remove(&rel)?;
                report.temp_files_removed += 1;
            } else if let Some(id) = pack_id_of(&rel)
                && !index.packs.contains_key(&id)
            {
                unindexed.push(rel);
            }
        }
        for rel in self.backend.list("index")? {
            if is_temp_file(&rel) {
                self.backend.remove(&rel)?;
                report.temp_files_removed += 1;
            }
        }

        let target = pack_target_bytes();
        let mut survivors: Vec<IndexPack> = Vec::new();
        let mut dropped: Vec<String> = Vec::new();
        let mut changed = false;
        let mut builder = PackBuilder::new();
        let mut writer: Option<Box<dyn ChunkWriter>> = None;
        let mut moved: HashSet<ChunkHash> = HashSet::new();

        for (id, blobs) in &index.packs {
            if let Some(c) = cancel
                && c.load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(crate::Error::Cancelled);
            }
            let whole = || IndexPack {
                pack: id.clone(),
                blobs: blobs.values().cloned().collect(),
            };
            if !index.existing_packs.contains(id) {
                // Never forget a pack the listing did not show: verify will
                // report its chunks missing instead of silently losing track.
                survivors.push(whole());
                continue;
            }
            let total: u64 = blobs.values().map(|b| b.length).sum();
            let live: Vec<&BlobEntry> = blobs
                .values()
                .filter(|b| {
                    ChunkHash::from_hex(&b.hash)
                        .map(|h| referenced.contains(&h))
                        .unwrap_or(false)
                })
                .collect();
            let live_bytes: u64 = live.iter().map(|b| b.length).sum();
            let dead_blobs = (blobs.len() - live.len()) as u64;
            if live.is_empty() {
                changed = true;
                dropped.push(pack_rel(id));
                report.chunks_deleted += dead_blobs;
                report.bytes_freed += total;
            } else if dead_blobs == 0 || (total - live_bytes) * 2 < total {
                report.chunks_kept += live.len() as u64;
                survivors.push(whole());
            } else {
                changed = true;
                for entry in &live {
                    let hash = ChunkHash::from_hex(&entry.hash)?;
                    if !moved.insert(hash) {
                        continue;
                    }
                    let blob = self.read_pack_blob(id, entry)?;
                    if blob.len() < 4 || &blob[..4] != CHUNK_MAGIC {
                        return Err(crate::Error::Repo(format!(
                            "pack {id}: blob {} is corrupt, refusing to repack",
                            entry.hash
                        )));
                    }
                    builder.add(&hash, &blob, entry.plain_len);
                    if builder.len() >= target {
                        self.flush_prune_pack(&mut builder, &mut writer, &mut survivors)?;
                    }
                }
                dropped.push(pack_rel(id));
                report.packs_repacked += 1;
                report.chunks_deleted += dead_blobs;
                report.chunks_kept += live.len() as u64;
                report.bytes_freed += total - live_bytes;
            }
        }
        if !builder.is_empty() {
            self.flush_prune_pack(&mut builder, &mut writer, &mut survivors)?;
        }
        if let Some(w) = writer.as_mut() {
            w.finish()?;
        }

        // Commit order: new packs are durable; the consolidated index
        // lands; only then the superseded index files and dead packs go.
        if changed || index.index_files.len() > 1 {
            if !survivors.is_empty() {
                self.write_index(&survivors)?;
                report.index_files_rewritten += 1;
            }
            for old in &index.index_files {
                self.backend.remove(old)?;
            }
        }
        report.packs_deleted += dropped.len() as u64;
        for rel in dropped {
            self.backend.remove(&rel)?;
        }
        for rel in unindexed {
            self.backend.remove(&rel)?;
            report.packs_deleted += 1;
        }
        self.invalidate_pack_index();
        Ok(())
    }

    fn flush_prune_pack(
        &self,
        builder: &mut PackBuilder,
        writer: &mut Option<Box<dyn ChunkWriter>>,
        survivors: &mut Vec<IndexPack>,
    ) -> crate::Result<()> {
        let sealed = self.seal_pack(std::mem::replace(builder, PackBuilder::new()))?;
        if writer.is_none() {
            *writer = Some(self.backend.open_writer()?);
        }
        if let Some(w) = writer.as_mut() {
            w.write_new(&sealed.rel, &sealed.data)?;
        }
        survivors.push(sealed.entry);
        Ok(())
    }
}

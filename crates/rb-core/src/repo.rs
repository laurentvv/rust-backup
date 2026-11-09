//! The backup repository: on-disk format v2 (v1 stays readable), self-contained
//! on any [`StorageBackend`].
//!
//! Layout (repo-relative paths):
//! - `config.json` — public repository parameters (chunking, compression);
//!   `version` 2 for new repositories, v1 ones are upgraded on their first
//!   pack write
//! - `keys/<id>.json` — master key sealed under the passphrase (Argon2id)
//! - `chunks/<2-hex>/<hash>.chunk` — v1 loose chunks (read forever, never
//!   written by v2 code)
//! - `packs/<2-hex>/<pack-id>.pack` — packs of chunk blobs (see [`crate::pack`])
//! - `index/<index-id>.idx` — sealed maps chunk hash to pack location
//! - `snapshots/<id>.snap` — sealed snapshot manifests, written LAST
//! - `locks/` — advisory lock files
//!
//! Write order of a backup: packs durable, then the index file, then the
//! snapshot manifest. A chunk blob inside a pack is exactly the v1 chunk
//! file; chunk blob layout: `RBCH` magic | version u8 | plain_len u64 LE |
//! AES-256-GCM(nonce || zstd(plaintext) || tag), with the chunk hash and
//! plain_len as associated data. Snapshot layout: `RBSN` magic | version
//! u8 | plain_len u64 LE | sealed manifest, AD = snapshot id + plain_len.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::StorageBackend;
use crate::chunker::ChunkerConfig;
use crate::crypto::{ChunkCipher, EncryptedKey, KdfParams, MasterKey};
use crate::hash::ChunkHash;
use crate::manifest::SnapshotManifest;
use crate::pack::pack_rel;

/// Chunk blob magic.
const CHUNK_MAGIC: &[u8; 4] = b"RBCH";
/// Manifest blob magic.
const MANIFEST_MAGIC: &[u8; 4] = b"RBSN";
/// On-disk format version for both blob kinds.
const BLOB_VERSION: u8 = 1;
/// Header size: magic(4) + version(1) + plain_len(8).
const HEADER_SIZE: usize = 13;

/// Public repository parameters, stored in `config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoConfig {
    /// On-disk format version (1 = loose chunks, 2 = packs + index).
    pub version: u32,
    /// Chunker tuning; must stay stable for meaningful dedup.
    pub chunker: ChunkerConfig,
    /// zstd level applied to chunk plaintext before encryption.
    pub compression_level: i32,
    /// When the repository was created (UTC).
    pub created_at: DateTime<Utc>,
}

/// Report returned by [`Repository::prune_orphans`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    /// Number of orphan chunks deleted.
    pub chunks_deleted: u64,
    /// Sum of bytes freed (stored compressed/sealed chunk blob sizes).
    pub bytes_freed: u64,
    /// Number of chunks preserved (referenced by at least one snapshot).
    pub chunks_kept: u64,
    /// Leftover temp files (`*.tmp-*`) from interrupted writes, removed.
    #[serde(default)]
    pub temp_files_removed: u64,
    /// Pack files deleted (fully unreferenced, repacked away, unindexed).
    #[serde(default)]
    pub packs_deleted: u64,
    /// Mostly-dead packs whose live blobs were copied into new packs.
    #[serde(default)]
    pub packs_repacked: u64,
    /// Consolidated index files written.
    #[serde(default)]
    pub index_files_rewritten: u64,
}

/// Lock information stored under `locks/<id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    /// Unique identifier for this lock acquisition.
    pub id: Uuid,
    /// Hostname where the process is running.
    pub hostname: String,
    /// Operating system process ID holding the lock.
    pub pid: u32,
    /// When the lock was acquired (UTC).
    pub created_at: DateTime<Utc>,
    /// High-level operation ("backup", "prune", "restore", "verify", ...).
    pub operation: String,
    /// Exclusive (writers) or shared (readers). Lock files written before
    /// this field existed are exclusive.
    #[serde(default = "default_exclusive")]
    pub exclusive: bool,
}

fn default_exclusive() -> bool {
    true
}

/// A lock is stale when older than 24 hours, or when it was written on this
/// machine by a process that no longer runs (or whose PID was reused).
fn lock_is_stale(info: &LockInfo) -> bool {
    if Utc::now().signed_duration_since(info.created_at) >= chrono::Duration::hours(24) {
        return true;
    }
    info.hostname
        .eq_ignore_ascii_case(&crate::process::current_hostname())
        && crate::process::process_state(info.pid, info.created_at)
            == crate::process::ProcessState::Dead
}

/// A repository lock handle (shared or exclusive). When dropped, the lock file on the
/// backend is removed automatically.
pub struct RepositoryLock {
    backend: std::sync::Arc<dyn StorageBackend>,
    lock_id: Uuid,
    released: bool,
}

impl RepositoryLock {
    /// Explicitly release the lock early.
    pub fn release(mut self) -> crate::Result<()> {
        if !self.released {
            self.released = true;
            let rel = format!("locks/{}.json", self.lock_id.simple());
            self.backend.remove(&rel)?;
        }
        Ok(())
    }

    /// Unique lock identifier.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.lock_id
    }
}

impl Drop for RepositoryLock {
    fn drop(&mut self) {
        if !self.released {
            let rel = format!("locks/{}.json", self.lock_id.simple());
            let _ = self.backend.remove(&rel);
        }
    }
}

/// An opened backup repository with the master key derived.
#[derive(Clone)]
pub struct Repository {
    pub(crate) backend: std::sync::Arc<dyn StorageBackend>,
    config: RepoConfig,
    pub(crate) cipher: ChunkCipher,
    key_id: String,
    /// Lazily loaded chunk map, shared by clones.
    pub(crate) pack_state: std::sync::Arc<crate::pack::PackState>,
}

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("key_id", &self.key_id)
            .field("config", &self.config)
            .field("backend", &self.backend.kind())
            .finish()
    }
}

impl Repository {
    /// Creates a new repository: writes the sealed key and `config.json`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) when the destination is
    /// already a repository, and crypto/IO errors on failure.
    pub fn init(
        backend: std::sync::Arc<dyn StorageBackend>,
        passphrase: &str,
        chunker: ChunkerConfig,
        compression_level: i32,
        kdf: KdfParams,
    ) -> crate::Result<Self> {
        chunker.validate()?;
        if backend.exists("config.json")? {
            return Err(crate::Error::Repo(
                "destination already contains a repository".into(),
            ));
        }
        let master = MasterKey::generate();
        let key_id = Uuid::new_v4().simple().to_string();
        let envelope = EncryptedKey::create(passphrase, kdf, &master)?;
        backend.write_atomic(
            &format!("keys/{key_id}.json"),
            envelope.to_json()?.as_bytes(),
        )?;

        let config = RepoConfig {
            version: 2,
            chunker,
            compression_level,
            created_at: Utc::now(),
        };
        backend.write_atomic(
            "config.json",
            serde_json::to_string_pretty(&config)?.as_bytes(),
        )?;
        backend.mkdirs("locks")?;

        let cipher = ChunkCipher::new(&master)?;
        Ok(Self {
            backend,
            config,
            cipher,
            key_id,
            pack_state: std::sync::Arc::default(),
        })
    }

    /// Opens an existing repository and unlocks the master key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) when the destination is
    /// not a repository (or an unsupported version) and
    /// [`Error::Crypto`](crate::Error::Crypto) on a wrong passphrase.
    pub fn open(
        backend: std::sync::Arc<dyn StorageBackend>,
        passphrase: &str,
    ) -> crate::Result<Self> {
        if !backend.exists("config.json")? {
            return Err(crate::Error::Repo(
                "no repository at destination (config.json missing)".into(),
            ));
        }
        let config: RepoConfig = serde_json::from_slice(&backend.read("config.json")?)
            .map_err(|e| crate::Error::Repo(format!("invalid config.json: {e}")))?;
        if !(1..=2).contains(&config.version) {
            return Err(crate::Error::Repo(format!(
                "unsupported repository version {}",
                config.version
            )));
        }
        let key_files = backend.list("keys")?;
        let key_path = key_files.first().ok_or_else(|| {
            crate::Error::Repo("repository has no key file (keys/ is empty)".into())
        })?;
        let envelope = EncryptedKey::from_json(&String::from_utf8_lossy(&backend.read(key_path)?))?;
        let master = envelope.unlock(passphrase)?;
        let key_id = key_path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".json")
            .to_owned();

        Ok(Self {
            backend,
            config,
            cipher: ChunkCipher::new(&master)?,
            key_id,
            pack_state: std::sync::Arc::default(),
        })
    }

    /// Public repository parameters.
    #[must_use]
    pub fn config(&self) -> &RepoConfig {
        &self.config
    }

    /// Id of the active key file (for `rbackup` diagnostics).
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Backend description (location of the repository).
    #[must_use]
    pub fn describe(&self) -> String {
        self.backend.describe()
    }

    /// Storage backend of this repository (used by the backup pipeline to
    /// open parallel chunk writers).
    #[must_use]
    pub fn backend(&self) -> &std::sync::Arc<dyn StorageBackend> {
        &self.backend
    }

    /// Repo-relative path of a chunk file (`chunks/<2-hex>/<hash>.chunk`).
    #[must_use]
    pub fn chunk_rel(hash: &ChunkHash) -> String {
        let hex = hash.to_hex();
        format!("chunks/{}/{}.chunk", &hex[..2], hex)
    }

    /// Compresses, seals and frames `plaintext` into its stored chunk blob.
    ///
    /// # Errors
    ///
    /// Returns compression and crypto errors as usual.
    pub fn seal_chunk_blob(&self, hash: &ChunkHash, plaintext: &[u8]) -> crate::Result<Vec<u8>> {
        let packed = crate::compress::compress(plaintext, self.config.compression_level)?;
        let sealed = self
            .cipher
            .seal(packed.as_slice(), hash, plaintext.len() as u64)?;
        let mut blob = Vec::with_capacity(HEADER_SIZE + sealed.len());
        blob.extend_from_slice(CHUNK_MAGIC);
        blob.push(BLOB_VERSION);
        blob.extend_from_slice(&(plaintext.len() as u64).to_le_bytes());
        blob.extend_from_slice(&sealed);
        Ok(blob)
    }

    /// Stores a chunk if not already present (dedup).
    ///
    /// Returns `true` when the chunk was newly written, `false` when an
    /// identical chunk (same hash) already existed.
    ///
    /// # Errors
    ///
    /// Returns compression/crypto/IO errors as usual; a storage failure
    /// never leaves a partially-written chunk behind (atomic writes).
    pub fn store_chunk(&self, hash: &ChunkHash, plaintext: &[u8]) -> crate::Result<bool> {
        let rel = Self::chunk_rel(hash);
        if self.backend.exists(&rel)? {
            return Ok(false);
        }
        let blob = self.seal_chunk_blob(hash, plaintext)?;
        self.backend.write_atomic(&rel, &blob)?;
        Ok(true)
    }

    /// Reads a chunk, verifying MAC, hash binding and length.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) on header/format
    /// violations and [`Error::Crypto`](crate::Error::Crypto) on
    /// authentication failure.
    pub fn read_chunk(&self, hash: &ChunkHash, plain_len: u64) -> crate::Result<Vec<u8>> {
        let located = self.pack_index()?.blobs.get(hash).cloned();
        let blob = if let Some((pack, entry)) = located {
            self.backend
                .read_range(&pack_rel(&pack), entry.offset, entry.length)?
        } else {
            self.backend.read(&Self::chunk_rel(hash))?
        };
        let (packed, actual_len) = Self::unpack(&blob, CHUNK_MAGIC)?;
        if actual_len != plain_len {
            return Err(crate::Error::Repo(format!(
                "chunk {hash} declares {actual_len} bytes, caller expected {plain_len}"
            )));
        }
        let opened = self.cipher.open(packed, hash, plain_len)?;
        crate::compress::decompress(&opened, plain_len as usize)
    }

    /// Whether a chunk with this hash exists in the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when the check fails.
    pub fn chunk_exists(&self, hash: &ChunkHash) -> crate::Result<bool> {
        let index = self.pack_index()?;
        if let Some((pack, _)) = index.blobs.get(hash)
            && index.existing_packs.contains(pack)
        {
            return Ok(true);
        }
        self.backend.exists(&Self::chunk_rel(hash))
    }

    /// Hashes of every chunk stored in packs (from the index files).
    ///
    /// # Errors
    ///
    /// Returns errors from reading or authenticating the index files.
    pub fn packed_chunk_hashes(&self) -> crate::Result<Vec<ChunkHash>> {
        Ok(self.pack_index()?.blobs.keys().copied().collect())
    }

    /// Lists every LOOSE (v1) chunk file in the repository (repo-relative
    /// paths); chunks stored in packs are not included.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when listing fails.
    pub fn list_chunks(&self) -> crate::Result<Vec<String>> {
        self.backend.list("chunks")
    }

    /// Finalizes a snapshot by writing its sealed manifest — this call is
    /// what makes the snapshot exist, and it happens LAST by construction.
    ///
    /// # Errors
    ///
    /// Returns serialization/crypto/IO errors; on failure the snapshot
    /// simply does not exist yet.
    pub fn write_manifest(&self, manifest: &SnapshotManifest) -> crate::Result<()> {
        let json = manifest.to_json()?;
        let ad = Self::manifest_ad(manifest.id, json.len() as u64);
        let sealed = self.cipher.seal_with_ad(&json, &ad)?;
        let mut blob = Vec::with_capacity(HEADER_SIZE + sealed.len());
        blob.extend_from_slice(MANIFEST_MAGIC);
        blob.push(BLOB_VERSION);
        blob.extend_from_slice(&(json.len() as u64).to_le_bytes());
        blob.extend_from_slice(&sealed);
        self.backend
            .write_atomic(&format!("snapshots/{}.snap", manifest.id.simple()), &blob)
    }

    /// Reads and parses a snapshot manifest by id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) when the snapshot does
    /// not exist or is malformed, and [`Error::Crypto`](crate::Error::Crypto)
    /// on authentication failure.
    pub fn read_manifest(&self, snapshot_id: &Uuid) -> crate::Result<SnapshotManifest> {
        let rel = format!("snapshots/{}.snap", snapshot_id.simple());
        let blob = self.backend.read(&rel)?;
        let (sealed, plain_len) = Self::unpack(&blob, MANIFEST_MAGIC)?;
        let json = self
            .cipher
            .open_with_ad(sealed, &Self::manifest_ad(*snapshot_id, plain_len))?;
        SnapshotManifest::from_json(&json)
    }

    /// Lists snapshot ids present in the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when listing fails.
    pub fn list_snapshot_ids(&self) -> crate::Result<Vec<Uuid>> {
        let mut ids = Vec::new();
        for rel in self.backend.list("snapshots")? {
            let stem = rel
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .trim_end_matches(".snap");
            if let Ok(id) = Uuid::parse_str(stem) {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Loads all manifests, newest first.
    ///
    /// # Errors
    ///
    /// Propagates read/parse errors of any manifest.
    pub fn all_manifests(&self) -> crate::Result<Vec<SnapshotManifest>> {
        let mut manifests: Vec<SnapshotManifest> = Vec::new();
        for id in self.list_snapshot_ids()? {
            manifests.push(self.read_manifest(&id)?);
        }
        manifests.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        Ok(manifests)
    }

    /// Newest manifest of a job, if the job has any snapshot.
    ///
    /// # Errors
    ///
    /// Propagates manifest read errors.
    pub fn latest_manifest_for_job(
        &self,
        job_id: &uuid::Uuid,
    ) -> crate::Result<Option<SnapshotManifest>> {
        Ok(self
            .all_manifests()?
            .into_iter()
            .find(|m| &m.job_id == job_id))
    }

    /// All manifests for a given job, newest first.
    ///
    /// # Errors
    ///
    /// Propagates manifest read errors.
    pub fn manifests_for_job(&self, job_id: &uuid::Uuid) -> crate::Result<Vec<SnapshotManifest>> {
        Ok(self
            .all_manifests()?
            .into_iter()
            .filter(|m| &m.job_id == job_id)
            .collect())
    }

    /// Counts how many consecutive incremental snapshots exist since the latest
    /// full snapshot for this job.
    ///
    /// # Errors
    ///
    /// Propagates manifest read errors.
    pub fn count_consecutive_incrementals_for_job(
        &self,
        job_id: &uuid::Uuid,
    ) -> crate::Result<u32> {
        let manifests = self.manifests_for_job(job_id)?;
        let mut count = 0;
        for m in manifests {
            if m.kind == crate::domain::BackupKind::Full {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// Acquires an exclusive write lock on the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RepositoryLocked`](crate::Error::RepositoryLocked) if
    /// any live lock (shared or exclusive) is held. Stale locks are broken
    /// automatically: older than 24 hours, or written on THIS machine by a
    /// process that no longer runs (crash, power loss, USB unplug).
    pub fn acquire_lock(&self, operation: &str) -> crate::Result<RepositoryLock> {
        self.acquire(operation, true)
    }

    /// Acquires a shared lock for a read-only operation (restore, verify):
    /// shared locks coexist, but block exclusive ones (prune, snapshot
    /// deletion, backup) so data cannot vanish mid-read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RepositoryLocked`](crate::Error::RepositoryLocked) if
    /// a live exclusive lock is held.
    pub fn acquire_shared_lock(&self, operation: &str) -> crate::Result<RepositoryLock> {
        self.acquire(operation, false)
    }

    /// Two-phase acquisition, safe on any backend without an atomic
    /// create-if-absent primitive: check, write our lock, then re-check. If
    /// the re-check sees a conflicting live lock (a concurrent writer), our
    /// lock is withdrawn and the call fails — two processes can both fail in
    /// a tight race, but never both succeed.
    fn acquire(&self, operation: &str, exclusive: bool) -> crate::Result<RepositoryLock> {
        let _ = self.backend.mkdirs("locks");
        self.check_lock_conflicts(exclusive, None)?;

        let lock_id = Uuid::new_v4();
        let info = LockInfo {
            id: lock_id,
            hostname: crate::process::current_hostname(),
            pid: std::process::id(),
            created_at: Utc::now(),
            operation: operation.to_string(),
            exclusive,
        };
        let rel = format!("locks/{}.json", lock_id.simple());
        let json = serde_json::to_string_pretty(&info)?;
        self.backend.write_atomic(&rel, json.as_bytes())?;
        let handle = RepositoryLock {
            backend: std::sync::Arc::clone(&self.backend),
            lock_id,
            released: false,
        };

        // Phase 2: dropping `handle` on error removes our lock file.
        self.check_lock_conflicts(exclusive, Some(lock_id))?;
        Ok(handle)
    }

    /// Fails on the first live lock that conflicts with the requested mode;
    /// removes stale ones on the way. `own` is skipped (phase 2).
    fn check_lock_conflicts(&self, exclusive: bool, own: Option<Uuid>) -> crate::Result<()> {
        for rel in self.backend.list("locks")? {
            if !rel.ends_with(".json") {
                continue;
            }
            let Ok(data) = self.backend.read(&rel) else {
                continue; // released between list and read
            };
            let Ok(info) = serde_json::from_slice::<LockInfo>(&data) else {
                continue; // not ours to judge; `break_locks` can clear it
            };
            if Some(info.id) == own || !(exclusive || info.exclusive) {
                continue;
            }
            if lock_is_stale(&info) {
                let _ = self.backend.remove(&rel);
                continue;
            }
            return Err(crate::Error::RepositoryLocked(format!(
                "operation '{}' by PID {} on host '{}' since {} — if no such operation is \
                 running anymore, unlock the repository",
                info.operation, info.pid, info.hostname, info.created_at
            )));
        }
        Ok(())
    }

    /// Lists the lock files currently present (unparseable ones are skipped).
    ///
    /// # Errors
    ///
    /// Returns I/O errors from the backend.
    pub fn list_locks(&self) -> crate::Result<Vec<LockInfo>> {
        let mut out = Vec::new();
        for rel in self.backend.list("locks")? {
            if rel.ends_with(".json")
                && let Ok(data) = self.backend.read(&rel)
                && let Ok(info) = serde_json::from_slice::<LockInfo>(&data)
            {
                out.push(info);
            }
        }
        Ok(out)
    }

    /// Forcibly removes EVERY lock file (including unparseable ones) and
    /// returns the parseable ones removed. Only call this after the user
    /// confirmed that no other operation runs on this repository.
    ///
    /// # Errors
    ///
    /// Returns I/O errors from the backend.
    pub fn break_locks(&self) -> crate::Result<Vec<LockInfo>> {
        let removed = self.list_locks()?;
        for rel in self.backend.list("locks")? {
            self.backend.remove(&rel)?;
        }
        Ok(removed)
    }

    /// Deletes a snapshot manifest from the repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RepositoryLocked`](crate::Error::RepositoryLocked) if
    /// locked, or IO errors.
    pub fn delete_snapshot(&self, snapshot_id: &Uuid) -> crate::Result<()> {
        let _lock = self.acquire_lock("delete_snapshot")?;
        let rel = format!("snapshots/{}.snap", snapshot_id.simple());
        self.backend.remove(&rel)?;
        Ok(())
    }

    /// Scans the repository for chunks that are no longer referenced by ANY
    /// snapshot manifest, and unlinks them to reclaim storage.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RepositoryLocked`](crate::Error::RepositoryLocked),
    /// [`Error::Cancelled`](crate::Error::Cancelled) if cooperative cancellation
    /// is triggered, or IO errors.
    pub fn prune_orphans(
        &self,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        mut on_progress: impl FnMut(u64, u64),
    ) -> crate::Result<PruneReport> {
        let _lock = self.acquire_lock("prune")?;

        // 1. Collect all referenced chunk hashes from all existing manifests.
        let manifests = self.all_manifests()?;
        let mut referenced: HashSet<ChunkHash> = HashSet::new();
        for m in &manifests {
            for s in &m.sources {
                for f in &s.entries {
                    for c in &f.chunks {
                        referenced.insert(c.hash);
                    }
                }
            }
        }

        // 2. Scan every stored chunk file on the backend.
        let chunk_files = self.list_chunks()?;
        let total = chunk_files.len() as u64;
        let mut report = PruneReport::default();

        for (ix, rel) in chunk_files.into_iter().enumerate() {
            if let Some(c) = cancel
                && c.load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(crate::Error::Cancelled);
            }
            on_progress(ix as u64, total);

            if is_temp_file(&rel) {
                // Leftover of an interrupted write. Safe to delete: we hold
                // the exclusive lock, so no writer is active right now.
                self.backend.remove(&rel)?;
                report.temp_files_removed += 1;
                continue;
            }
            if !rel.ends_with(".chunk") {
                continue;
            }
            let stem = rel
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .trim_end_matches(".chunk");
            if let Ok(hash) = ChunkHash::from_hex(stem) {
                if referenced.contains(&hash) {
                    report.chunks_kept += 1;
                } else {
                    let len = self.backend.read(&rel).map(|b| b.len() as u64).unwrap_or(0);
                    self.backend.remove(&rel)?;
                    report.chunks_deleted += 1;
                    report.bytes_freed += len;
                }
            }
        }
        // 2b. Packs: delete dead ones, repack mostly dead ones, rewrite the
        // index (see `Repository::prune_packs`).
        self.prune_packs(&referenced, cancel, &mut report)?;

        // 3. Interrupted snapshot writes leave temp files next to manifests.
        for rel in self.backend.list("snapshots")? {
            if is_temp_file(&rel) {
                self.backend.remove(&rel)?;
                report.temp_files_removed += 1;
            }
        }
        on_progress(total, total);
        Ok(report)
    }

    pub(crate) fn unpack<'a>(blob: &'a [u8], magic: &[u8; 4]) -> crate::Result<(&'a [u8], u64)> {
        if blob.len() < HEADER_SIZE || &blob[..4] != magic {
            return Err(crate::Error::Repo("corrupt blob header".into()));
        }
        if blob[4] != BLOB_VERSION {
            return Err(crate::Error::Repo(format!(
                "unsupported blob version {}",
                blob[4]
            )));
        }
        let plain_len = u64::from_le_bytes(
            blob[5..HEADER_SIZE]
                .try_into()
                .map_err(|_| crate::Error::Repo("short header".into()))?,
        );
        Ok((&blob[HEADER_SIZE..], plain_len))
    }

    fn manifest_ad(snapshot_id: Uuid, plain_len: u64) -> Vec<u8> {
        let mut ad = Vec::with_capacity(28);
        ad.extend_from_slice(b"RBSN");
        ad.extend_from_slice(snapshot_id.as_bytes());
        ad.extend_from_slice(&plain_len.to_be_bytes());
        ad
    }
}

/// True for temp names produced by `StorageBackend::write_atomic`
/// (`<stem>.tmp-<uuid>` locally, `<name>.tmp-<uuid>` over SFTP).
pub(crate) fn is_temp_file(rel: &str) -> bool {
    rel.rsplit('/')
        .next()
        .is_some_and(|name| name.contains(".tmp-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::LocalFs;

    fn temp_repo() -> (std::path::PathBuf, Repository) {
        let dir = std::env::temp_dir().join(format!("rb-repo-{}", Uuid::new_v4()));
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        let repo = Repository::init(
            backend,
            "pass phrase",
            ChunkerConfig::default(),
            5,
            fast_kdf(),
        )
        .expect("init");
        (dir, repo)
    }

    fn fast_kdf() -> KdfParams {
        KdfParams {
            m_cost: 1024,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn init_twice_on_same_destination_should_fail() {
        let (dir, _repo) = temp_repo();
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        assert!(
            Repository::init(backend, "other", ChunkerConfig::default(), 5, fast_kdf()).is_err()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_with_wrong_passphrase_should_fail_cleanly() {
        let (dir, _repo) = temp_repo();
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        let err = Repository::open(backend, "not the passphrase").unwrap_err();
        assert!(matches!(err, crate::Error::Crypto(_)), "got {err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chunk_store_read_roundtrip_and_dedup_flag() {
        let (_dir, repo) = temp_repo();
        let data = vec![5u8; 300_000];
        let hash = ChunkHash::from_data(&data);
        assert!(repo.store_chunk(&hash, &data).expect("store"));
        assert!(!repo.store_chunk(&hash, &data).expect("store again"));
        let back = repo.read_chunk(&hash, data.len() as u64).expect("read");
        assert_eq!(back, data);
    }

    #[test]
    fn manifest_write_read_roundtrip() {
        let (_dir, repo) = temp_repo();
        let mut manifest = SnapshotManifest::new(Uuid::new_v4(), "job");
        manifest.sources.push(crate::manifest::SourceSnapshot {
            root: std::path::PathBuf::from(r"C:\X"),
            entries: Vec::new(),
        });
        repo.write_manifest(&manifest).expect("write manifest");
        let ids = repo.list_snapshot_ids().expect("ids");
        assert_eq!(ids, vec![manifest.id]);
        let back = repo.read_manifest(&manifest.id).expect("read manifest");
        assert_eq!(back.id, manifest.id);
        assert_eq!(back.job_name, "job");
        assert!(
            repo.latest_manifest_for_job(&manifest.job_id)
                .expect("latest")
                .is_some()
        );
    }

    #[test]
    fn chunk_with_declared_length_mismatch_should_error() {
        let (_dir, repo) = temp_repo();
        let data = vec![1u8; 1000];
        let hash = ChunkHash::from_data(&data);
        repo.store_chunk(&hash, &data).expect("store");
        assert!(repo.read_chunk(&hash, 999).is_err());
    }

    #[test]
    fn new_repository_is_version_2_and_unknown_versions_are_refused() {
        let (dir, repo) = temp_repo();
        assert_eq!(repo.config().version, 2);
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        // A v1-only reader refuses a newer repository; the same check
        // refuses any version this code does not know (3).
        let mut config = repo.config().clone();
        config.version = 3;
        backend
            .write_atomic(
                "config.json",
                serde_json::to_string(&config).expect("json").as_bytes(),
            )
            .expect("rewrite config");
        let err = Repository::open(backend.clone(), "pass phrase").unwrap_err();
        assert!(
            err.to_string().contains("unsupported repository version 3"),
            "message must name the version: {err}"
        );
        config.version = 1;
        backend
            .write_atomic(
                "config.json",
                serde_json::to_string(&config).expect("json").as_bytes(),
            )
            .expect("rewrite config");
        Repository::open(backend, "pass phrase").expect("v1 repositories stay readable");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_on_empty_folder_should_report_missing_repo() {
        let dir = std::env::temp_dir().join(format!("rb-empty-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).ok();
        let backend = std::sync::Arc::new(LocalFs::new(&dir));
        let err = Repository::open(backend, "whatever").unwrap_err();
        assert!(matches!(err, crate::Error::Repo(_)), "got {err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exclusive_lock_prevents_concurrent_access_and_releases_on_drop() {
        let (dir, repo) = temp_repo();
        let lock1 = repo.acquire_lock("first_op").expect("acquire lock 1");
        assert!(
            repo.acquire_lock("second_op").is_err(),
            "concurrent lock must fail"
        );
        drop(lock1);
        let lock2 = repo
            .acquire_lock("third_op")
            .expect("acquire lock 2 after drop");
        drop(lock2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Plants a lock file as another process would have written it.
    fn plant_lock(repo: &Repository, hostname: &str, pid: u32, exclusive: bool) -> Uuid {
        let info = LockInfo {
            id: Uuid::new_v4(),
            hostname: hostname.to_string(),
            pid,
            created_at: Utc::now(),
            operation: "planted".to_string(),
            exclusive,
        };
        repo.backend.mkdirs("locks").expect("locks dir");
        repo.backend
            .write_atomic(
                &format!("locks/{}.json", info.id.simple()),
                serde_json::to_string(&info).expect("json").as_bytes(),
            )
            .expect("plant");
        info.id
    }

    #[cfg(windows)]
    #[test]
    fn dead_pid_lock_on_same_host_is_broken_automatically() {
        let (dir, repo) = temp_repo();
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("spawn");
        let dead_pid = child.id();
        child.wait().expect("wait");
        drop(child);
        plant_lock(&repo, &crate::process::current_hostname(), dead_pid, true);

        let lock = repo
            .acquire_lock("backup")
            .expect("crashed run's lock must not block");
        assert_eq!(repo.list_locks().expect("locks").len(), 1, "only ours");
        drop(lock);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_lock_from_other_host_is_reported_and_kept() {
        let (dir, repo) = temp_repo();
        plant_lock(&repo, "other-host", 4242, true);

        let err = repo.acquire_lock("backup").err().expect("must be refused");
        assert!(
            err.to_string().contains("other-host"),
            "names the owner: {err}"
        );
        assert_eq!(
            repo.list_locks().expect("locks").len(),
            1,
            "foreign lock kept"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_lock_without_mode_field_is_exclusive() {
        let (dir, repo) = temp_repo();
        repo.backend.mkdirs("locks").expect("locks dir");
        let legacy = format!(
            r#"{{"id":"{}","hostname":"other-host","pid":1,"created_at":"{}","operation":"backup"}}"#,
            Uuid::new_v4(),
            Utc::now().to_rfc3339()
        );
        repo.backend
            .write_atomic("locks/legacy.json", legacy.as_bytes())
            .expect("plant");
        assert!(
            repo.acquire_shared_lock("verify").is_err(),
            "old lock files block readers too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shared_locks_coexist_and_block_exclusive_ones() {
        let (dir, repo) = temp_repo();
        let verify = repo.acquire_shared_lock("verify").expect("shared 1");
        let restore = repo.acquire_shared_lock("restore").expect("shared 2");
        assert!(
            repo.acquire_lock("prune").is_err(),
            "prune must wait for readers"
        );
        drop(verify);
        drop(restore);
        let prune = repo.acquire_lock("prune").expect("exclusive after readers");
        assert!(
            repo.acquire_shared_lock("verify").is_err(),
            "readers wait for the writer"
        );
        drop(prune);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_acquisitions_never_grant_two_exclusive_locks() {
        let (dir, repo) = temp_repo();
        let repo = std::sync::Arc::new(repo);
        let holders = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (repo, holders, max_seen, barrier) = (
                    std::sync::Arc::clone(&repo),
                    std::sync::Arc::clone(&holders),
                    std::sync::Arc::clone(&max_seen),
                    std::sync::Arc::clone(&barrier),
                );
                std::thread::spawn(move || {
                    use std::sync::atomic::Ordering::SeqCst;
                    barrier.wait();
                    for _ in 0..20 {
                        if let Ok(lock) = repo.acquire_lock("race") {
                            let now = holders.fetch_add(1, SeqCst) + 1;
                            max_seen.fetch_max(now, SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            holders.fetch_sub(1, SeqCst);
                            drop(lock);
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("thread");
        }
        assert!(
            max_seen.load(std::sync::atomic::Ordering::SeqCst) <= 1,
            "two exclusive holders at once"
        );
        assert!(repo.list_locks().expect("locks").is_empty(), "all released");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn break_locks_removes_every_lock_file() {
        let (dir, repo) = temp_repo();
        plant_lock(&repo, "other-host", 1, true);
        plant_lock(&repo, "other-host", 2, false);
        repo.backend
            .write_atomic("locks/garbage.json", b"not json")
            .expect("plant garbage");
        let removed = repo.break_locks().expect("break");
        assert_eq!(removed.len(), 2);
        assert!(repo.backend.list("locks").expect("list").is_empty());
        repo.acquire_lock("backup").expect("free again");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_snapshot_removes_manifest_and_leaves_others_intact() {
        let (dir, repo) = temp_repo();
        let m1 = SnapshotManifest::new(Uuid::new_v4(), "job1");
        let m2 = SnapshotManifest::new(Uuid::new_v4(), "job2");
        repo.write_manifest(&m1).expect("write m1");
        repo.write_manifest(&m2).expect("write m2");
        assert_eq!(repo.list_snapshot_ids().expect("ids").len(), 2);

        repo.delete_snapshot(&m1.id).expect("delete m1");
        let ids = repo.list_snapshot_ids().expect("ids after delete");
        assert_eq!(ids, vec![m2.id]);
        assert!(repo.read_manifest(&m1.id).is_err());
        assert!(repo.read_manifest(&m2.id).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_orphans_cleans_unreferenced_chunks_and_keeps_live_ones() {
        let (dir, repo) = temp_repo();
        // 1. Store two chunks
        let data1 = vec![10u8; 1000];
        let hash1 = ChunkHash::from_data(&data1);
        repo.store_chunk(&hash1, &data1).expect("store 1");

        let data2 = vec![20u8; 2000];
        let hash2 = ChunkHash::from_data(&data2);
        repo.store_chunk(&hash2, &data2).expect("store 2");

        // 2. Snapshot references ONLY chunk1
        let mut m = SnapshotManifest::new(Uuid::new_v4(), "job");
        m.sources.push(crate::manifest::SourceSnapshot {
            root: std::path::PathBuf::from(r"C:\test"),
            entries: vec![crate::manifest::FileEntry {
                path: std::path::PathBuf::from("file1.bin"),
                size: 1000,
                mtime_ns: 0,
                chunks: vec![crate::manifest::ChunkRef {
                    hash: hash1,
                    plain_len: 1000,
                }],
                kind: crate::manifest::EntryKind::File,
            }],
        });
        repo.write_manifest(&m).expect("write m");

        // 3. Prune: chunk2 should be deleted, chunk1 kept
        let report = repo.prune_orphans(None, |_, _| {}).expect("prune");
        assert_eq!(report.chunks_deleted, 1);
        assert_eq!(report.chunks_kept, 1);
        assert!(repo.chunk_exists(&hash1).expect("chunk1 exists"));
        assert!(!repo.chunk_exists(&hash2).expect("chunk2 pruned"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_removes_stale_temp_files() {
        let (dir, repo) = temp_repo();
        let data = vec![7u8; 500];
        let hash = ChunkHash::from_data(&data);
        repo.store_chunk(&hash, &data).expect("store");
        let mut m = SnapshotManifest::new(Uuid::new_v4(), "job");
        m.sources.push(crate::manifest::SourceSnapshot {
            root: std::path::PathBuf::from(r"C:\test"),
            entries: vec![crate::manifest::FileEntry {
                path: std::path::PathBuf::from("f.bin"),
                size: 500,
                mtime_ns: 0,
                chunks: vec![crate::manifest::ChunkRef {
                    hash,
                    plain_len: 500,
                }],
                kind: crate::manifest::EntryKind::File,
            }],
        });
        repo.write_manifest(&m).expect("write m");

        // Leftovers of interrupted writes, in both naming styles
        // (LocalFs replaces the extension, SFTP appends it).
        let backend = &repo.backend;
        backend
            .write_atomic("chunks/ab/abcd.tmp-0123", b"partial")
            .expect("tmp chunk");
        backend
            .write_atomic("snapshots/dead.snap.tmp-4567", b"partial")
            .expect("tmp snap");

        let report = repo.prune_orphans(None, |_, _| {}).expect("prune");
        assert_eq!(report.temp_files_removed, 2);
        assert_eq!(report.chunks_kept, 1, "live chunk untouched");
        assert!(!backend.exists("chunks/ab/abcd.tmp-0123").expect("exists"));
        assert!(
            !backend
                .exists("snapshots/dead.snap.tmp-4567")
                .expect("exists")
        );
        assert_eq!(repo.list_snapshot_ids().expect("ids"), vec![m.id]);
        std::fs::remove_dir_all(&dir).ok();
    }
}

//! Storage backends: where a backup repository physically lives.
//!
//! All backends address repository files through repo-relative paths with
//! `/` separators (e.g. `chunks/ab/<hash>.chunk`), so the engine code is
//! identical for a USB disk and a remote SFTP server.

pub mod local;
pub mod sftp;

pub use local::LocalFs;
pub use sftp::{HostKeyPolicy, SftpBackend, SftpConnect};

/// Read/write access to the physical repository files.
///
/// Implementations must make writes atomic: `write_atomic` either lands the
/// complete file at its final name or leaves no trace of it — a reader must
/// never observe a half-written chunk or manifest.
pub trait StorageBackend: Send + Sync {
    /// Opens a dedicated writer handle for parallel chunk uploads.
    ///
    /// Each handle carries its own resources (SFTP channel and directory
    /// cache, or local directory cache) so several upload workers can write
    /// chunks concurrently without contending on one session.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot open another writer
    /// (connection failure, unsupported backend).
    fn open_writer(&self) -> crate::Result<Box<dyn ChunkWriter>>;

    /// Backend identifier for logs and the UI ("local", "sftp").
    fn kind(&self) -> &'static str;

    /// Human-readable location of the repository.
    fn describe(&self) -> String;

    /// Reads a repository file fully into memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when the file is missing or
    /// unreadable.
    fn read(&self, rel: &str) -> crate::Result<Vec<u8>>;

    /// Atomically and durably writes `data` at `rel`: write to a temp name in
    /// the same directory, flush it to stable storage, then rename it into
    /// place, creating parent directories as needed. On failure the temp file
    /// is removed (best effort); temp names contain `.tmp-` so that
    /// [`Repository::prune_orphans`](crate::repo::Repository::prune_orphans)
    /// can sweep leftovers from crashed runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) on any filesystem failure.
    fn write_atomic(&self, rel: &str, data: &[u8]) -> crate::Result<()>;

    /// Whether a repository file exists.
    ///
    /// `Ok(false)` ONLY when the backend definitively reports the file as
    /// absent; callers rely on this to never mistake an outage for an empty
    /// destination (no implicit repository creation).
    ///
    /// # Errors
    ///
    /// Returns an error when the check itself fails (I/O error, permission
    /// denied, dropped SFTP connection, timeout).
    fn exists(&self, rel: &str) -> crate::Result<bool>;

    /// Creates a directory (and parents) relative to the repository root.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) on failure.
    fn mkdirs(&self, rel: &str) -> crate::Result<()>;

    /// Lists repository files under `prefix` (prefix `""` = everything), as
    /// repo-relative `/`-separated paths, sorted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when listing fails.
    fn list(&self, prefix: &str) -> crate::Result<Vec<String>>;

    /// Removes a repository file if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) on failure.
    fn remove(&self, rel: &str) -> crate::Result<()>;
}

/// Dedicated write handle for NEW chunk files, used by the backup
/// pipeline's upload workers.
///
/// Unlike [`StorageBackend::write_atomic`], `write_new` skips the existence
/// check and the per-segment directory probing: the pipeline already knows
/// (in-memory chunk index + exclusive repository lock) that the target is
/// absent, and the writer caches directories it has created.
///
/// Durability contract (shared by every implementation):
/// - a chunk is NEVER visible under its final name before its bytes are on
///   stable storage (temp name, flush, then rename);
/// - once [`ChunkWriter::finish`] returns `Ok`, every chunk passed to
///   `write_new` is durable under its final name.
///
/// Implementations may defer the flush and the rename of several chunks to
/// one batched barrier (SFTP with remote `sync`), which is why the final
/// name may only appear after a later `write_new` or `finish` call.
pub trait ChunkWriter: Send {
    /// Writes `data` to `rel`, which must not exist yet, under the contract
    /// above. On failure the temp file is removed (best effort).
    ///
    /// # Errors
    ///
    /// Returns IO errors as usual; nothing partial is left under `rel`.
    fn write_new(&mut self, rel: &str, data: &[u8]) -> crate::Result<()>;

    /// Makes every chunk written so far durable under its final name.
    /// Must be called once after the last `write_new` and before anything
    /// (a snapshot manifest) refers to those chunks.
    ///
    /// # Errors
    ///
    /// Returns IO errors from the deferred flush or renames.
    fn finish(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

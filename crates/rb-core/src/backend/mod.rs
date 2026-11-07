//! Storage backends: where a backup repository physically lives.
//!
//! All backends address repository files through repo-relative paths with
//! `/` separators (e.g. `chunks/ab/<hash>.chunk`), so the engine code is
//! identical for a USB disk and a remote SFTP server.

pub mod local;

pub use local::LocalFs;

/// Read/write access to the physical repository files.
///
/// Implementations must make writes atomic: `write_atomic` either lands the
/// complete file at its final name or leaves no trace of it — a reader must
/// never observe a half-written chunk or manifest.
pub trait StorageBackend: Send + Sync {
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

    /// Atomically writes `data` at `rel` (write to a temp name, then
    /// rename into place), creating parent directories as needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) on any filesystem failure.
    fn write_atomic(&self, rel: &str, data: &[u8]) -> crate::Result<()>;

    /// Whether a repository file exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) when the check itself fails.
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
}

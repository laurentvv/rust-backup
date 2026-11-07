//! Error types for the RustBackup core library.

/// Errors returned by the RustBackup core engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A catalog (DuckDB) operation failed.
    #[error("catalog database error: {0}")]
    Catalog(#[from] duckdb::Error),

    /// A job configuration is invalid (empty sources, missing name, ...).
    #[error("invalid job configuration: {0}")]
    InvalidJob(String),

    /// An I/O operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// (De)serialization of stored JSON documents failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The platform provided no local application-data directory.
    #[error("no platform app-data directory available")]
    NoAppDataDir,

    /// A chunker configuration is invalid.
    #[error("invalid chunker configuration: {0}")]
    InvalidChunker(String),

    /// A hash string could not be parsed.
    #[error("invalid hash: {0}")]
    InvalidHash(String),

    /// A compression operation failed.
    #[error("compression error: {0}")]
    Compression(String),

    /// An encryption or authentication operation failed.
    #[error("crypto error: {0}")]
    Crypto(String),

    /// The operation was cancelled through the cooperative cancel flag.
    #[error("cancelled by user")]
    Cancelled,

    /// The repository is locked by another operation.
    #[error("repository is locked: {0}")]
    RepositoryLocked(String),

    /// The repository format is violated (bad magic, unsupported version,
    /// inconsistent metadata).
    #[error("repository format error: {0}")]
    Repo(String),

    /// A directory walk failed.
    #[error("directory walk error: {0}")]
    Walk(#[from] walkdir::Error),

    /// An SSH transport error.
    #[error("ssh error: {0}")]
    Ssh(#[from] russh::Error),

    /// An SFTP protocol error.
    #[error("sftp error: {0}")]
    Sftp(#[from] russh_sftp::client::error::Error),
}

/// Convenience alias used across the library.
pub type Result<T> = std::result::Result<T, Error>;

//! Core domain types: backup jobs, destinations and run records.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier of a backup job.
pub type JobId = Uuid;

/// Unique identifier of a snapshot inside a repository.
pub type SnapshotId = Uuid;

/// Where a backup job writes its repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    /// A locally mounted folder (USB disk, internal drive, ...).
    Local {
        /// Repository root folder on the local filesystem.
        root: PathBuf,
    },
    /// A remote repository reached over SFTP with public-key auth.
    Sftp {
        /// Remote host name or address.
        host: String,
        /// SSH port (usually 22).
        port: u16,
        /// SSH user name.
        username: String,
        /// Path to the private key file (a path only, never key material).
        key_path: PathBuf,
        /// Repository root on the remote host.
        remote_root: String,
    },
}

impl Destination {
    /// Short human-readable description used in listings.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Local { root } => format!("local: {}", root.display()),
            Self::Sftp {
                host,
                username,
                remote_root,
                ..
            } => format!("sftp: {username}@{host}:{remote_root}"),
        }
    }
}

/// A user-configured backup job (sources, destination, tuning).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobConfig {
    /// Unique id of the job.
    pub id: JobId,
    /// User-chosen display name.
    pub name: String,
    /// Source directories to back up (absolute paths).
    pub sources: Vec<PathBuf>,
    /// Repository destination (local folder or SFTP server).
    pub destination: Destination,
    /// zstd compression level for stored chunks (1..=22, 0 = store raw).
    pub compression_level: i32,
    /// Creation timestamp (UTC).
    pub created_at: DateTime<Utc>,
}

impl JobConfig {
    /// Validates a job configuration before it is persisted or run.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidJob`](crate::Error::InvalidJob) when the name
    /// is empty, no source is configured, a source is not absolute, or the
    /// compression level is out of range.
    pub fn validate(&self) -> crate::Result<()> {
        if self.name.trim().is_empty() {
            return Err(crate::Error::InvalidJob("name must not be empty".into()));
        }
        if self.sources.is_empty() {
            return Err(crate::Error::InvalidJob(
                "at least one source directory is required".into(),
            ));
        }
        if let Some(rel) = self.sources.iter().find(|s| !s.is_absolute()) {
            return Err(crate::Error::InvalidJob(format!(
                "source must be an absolute path: {}",
                rel.display()
            )));
        }
        if !(0..=22).contains(&self.compression_level) {
            return Err(crate::Error::InvalidJob(format!(
                "compression level must be in 0..=22, got {}",
                self.compression_level
            )));
        }
        Ok(())
    }
}

/// Terminal status of a backup run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The run finished and its snapshot was finalized atomically.
    Success,
    /// The run failed or was cancelled; no complete snapshot was left.
    Failed,
}

impl RunStatus {
    /// Stable lowercase keyword stored in the catalog.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }

    /// Parses the keyword stored by [`RunStatus::as_str`].
    ///
    /// # Errors
    ///
    /// Returns the offending keyword in an [`Error::Catalog`] style error
    /// string when it does not match a known status.
    pub fn parse(value: &str) -> crate::Result<Self> {
        match value {
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            other => Err(crate::Error::InvalidJob(format!(
                "unknown run status: {other}"
            ))),
        }
    }
}

/// Outcome summary of one backup run, as recorded in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Unique id of the run.
    pub id: Uuid,
    /// Job that produced this run.
    pub job_id: JobId,
    /// Run start timestamp (UTC).
    pub started_at: DateTime<Utc>,
    /// Run end timestamp (UTC).
    pub finished_at: DateTime<Utc>,
    /// Terminal status.
    pub status: RunStatus,
    /// Number of files considered during the run.
    pub files_total: u64,
    /// Total size in bytes of the scanned files.
    pub bytes_total: u64,
    /// Number of chunks newly written to the repository.
    pub chunks_new: u64,
    /// Number of (uncompressed) bytes in newly written chunks.
    pub bytes_new: u64,
    /// Error message when the run failed.
    pub error: Option<String>,
}

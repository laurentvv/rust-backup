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
    /// Glob or name patterns to exclude from backups (e.g. `target`, `node_modules`, `*.tmp`).
    #[serde(default)]
    pub excludes: Vec<String>,
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

/// Checks whether a relative path matches an exclusion pattern.
///
/// Supported patterns:
/// - Directory or file name exact match (e.g. `target`, `node_modules`, `.git`, `Thumbs.db`)
/// - Wildcard extension match (e.g. `*.tmp`, `*.log`, `*.bak`)
/// - Prefix wildcard match (e.g. `cache*`, `temp*`)
/// - Relative path match (e.g. `build/output`, `src/generated`)
#[must_use]
pub fn matches_exclude(rel_path: &std::path::Path, pattern: &str) -> bool {
    let p = pattern.trim();
    if p.is_empty() {
        return false;
    }
    let p_clean = p.trim_matches('/').trim_matches('\\');
    let rel_str = rel_path.to_string_lossy();
    let file_name = rel_path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();

    // 1. Wildcard extension, e.g. *.tmp
    if let Some(ext) = p_clean.strip_prefix("*.")
        && let Some(file_ext) = rel_path.extension().and_then(|e| e.to_str())
        && file_ext.eq_ignore_ascii_case(ext)
    {
        return true;
    }

    // 2. Prefix wildcard, e.g. cache*
    if let Some(prefix) = p_clean.strip_suffix('*')
        && file_name.starts_with(prefix)
    {
        return true;
    }

    // 3. Exact component or directory name, e.g. node_modules, target, .git
    if file_name.eq_ignore_ascii_case(p_clean) {
        return true;
    }

    // 4. Any path component matches, e.g. rel_path has "target" as a directory
    for comp in rel_path.components() {
        if comp
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(p_clean)
        {
            return true;
        }
    }

    // 5. Full relative path matches (normalized slashes)
    let norm_rel = rel_str.replace('\\', "/");
    let norm_p = p_clean.replace('\\', "/");
    norm_rel == norm_p || norm_rel.ends_with(&format!("/{norm_p}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_matches_exclude() {
        // Name matching
        assert!(matches_exclude(Path::new("target"), "target"));
        assert!(matches_exclude(Path::new("project/target"), "target"));
        assert!(matches_exclude(
            Path::new("project/target/debug/app.exe"),
            "target"
        ));
        assert!(matches_exclude(
            Path::new("node_modules/package/index.js"),
            "node_modules"
        ));
        assert!(matches_exclude(Path::new(".git/HEAD"), ".git"));

        // Extension wildcard
        assert!(matches_exclude(Path::new("temp.tmp"), "*.tmp"));
        assert!(matches_exclude(Path::new("sub/dir/test.log"), "*.log"));
        assert!(!matches_exclude(Path::new("test.txt"), "*.tmp"));

        // Prefix wildcard
        assert!(matches_exclude(Path::new("cache_001.dat"), "cache*"));
        assert!(matches_exclude(Path::new("sub/temp_dir"), "temp*"));

        // Negative cases
        assert!(!matches_exclude(Path::new("src/main.rs"), "target"));
        assert!(!matches_exclude(Path::new("target_builder.rs"), "target"));
    }
}

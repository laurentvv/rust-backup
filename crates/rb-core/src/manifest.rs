//! Snapshot manifests: the encrypted file index stored inside the
//! repository. A manifest is what turns a pile of deduplicated chunks into
//! a browsable, restorable snapshot.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{BackupKind, JobId, SnapshotId};
use crate::hash::ChunkHash;

/// Manifest format version.
pub const MANIFEST_VERSION: u32 = 1;

/// Kind of a tracked filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A regular file (with chunk list).
    File,
    /// A directory.
    Dir,
    /// A symbolic link (recorded, not followed; restored as a link).
    Symlink,
}

/// Reference to one content-addressed chunk of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    /// Content hash of the chunk plaintext.
    pub hash: ChunkHash,
    /// Plaintext length in bytes (stored in the chunk header too).
    pub plain_len: u64,
}

/// One tracked entry (file/dir/symlink) relative to its source root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the source root, `/`-separated.
    pub path: PathBuf,
    /// File size in bytes (0 for dirs/symlinks).
    pub size: u64,
    /// Modification time as nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Entry kind.
    pub kind: EntryKind,
    /// Chunks of the file, in order (empty for dirs/symlinks).
    pub chunks: Vec<ChunkRef>,
}

/// All entries captured under one configured source root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    /// Absolute source root as configured in the job.
    pub root: PathBuf,
    /// Entries, sorted by path.
    pub entries: Vec<FileEntry>,
}

/// A file that could not be read during the run (e.g. exclusively locked
/// on Windows) — reported to the user instead of failing the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedItem {
    /// Path relative to the source root.
    pub path: PathBuf,
    /// Why it was skipped.
    pub reason: String,
}

/// The complete, self-describing snapshot index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// Manifest format version.
    pub version: u32,
    /// Snapshot id (also the repository file name).
    pub id: SnapshotId,
    /// Job that produced this snapshot.
    pub job_id: JobId,
    /// Job display name at backup time.
    pub job_name: String,
    /// Creation timestamp (UTC).
    pub created_at: DateTime<Utc>,
    /// Machine name that produced the snapshot.
    pub host: String,
    /// Sources captured in this snapshot.
    pub sources: Vec<SourceSnapshot>,
    /// Files skipped during the run, with reasons.
    pub skipped: Vec<SkippedItem>,
    /// Kind of backup that produced this snapshot (full or incremental).
    #[serde(default)]
    pub kind: BackupKind,
}

impl SnapshotManifest {
    /// Builds an empty manifest skeleton for a job.
    #[must_use]
    pub fn new(job_id: JobId, job_name: &str) -> Self {
        Self {
            version: MANIFEST_VERSION,
            id: Uuid::new_v4(),
            job_id,
            job_name: job_name.to_owned(),
            created_at: Utc::now(),
            host: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into()),
            sources: Vec::new(),
            skipped: Vec::new(),
            kind: BackupKind::Incremental,
        }
    }

    /// Serializes the manifest to its canonical JSON form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`](crate::Error::Json) on serialization
    /// failure.
    pub fn to_json(&self) -> crate::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Parses a manifest written by [`SnapshotManifest::to_json`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Repo`](crate::Error::Repo) for an unsupported
    /// version or [`Error::Json`](crate::Error::Json) on parse failure.
    pub fn from_json(data: &[u8]) -> crate::Result<Self> {
        let manifest: Self = serde_json::from_slice(data)?;
        if manifest.version != MANIFEST_VERSION {
            return Err(crate::Error::Repo(format!(
                "unsupported manifest version {} (expected {MANIFEST_VERSION})",
                manifest.version
            )));
        }
        Ok(manifest)
    }

    /// Flattens `(source root, entry)` pairs of every file in the snapshot.
    pub fn iter_files(&self) -> impl Iterator<Item = (&SourceSnapshot, &FileEntry)> {
        self.sources.iter().flat_map(|s| {
            s.entries
                .iter()
                .filter(|e| e.kind == EntryKind::File)
                .map(move |e| (s, e))
        })
    }

    /// Total file count across sources.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.iter_files().count()
    }

    /// Total bytes across sources.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.iter_files().map(|(_, e)| e.size).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_json_roundtrip_should_preserve_everything() {
        let mut manifest = SnapshotManifest::new(Uuid::new_v4(), "job");
        manifest.sources.push(SourceSnapshot {
            root: PathBuf::from(r"C:\Data"),
            entries: vec![
                FileEntry {
                    path: PathBuf::from("docs/report.pdf"),
                    size: 1024,
                    mtime_ns: 1_700_000_000_000_000_000,
                    kind: EntryKind::File,
                    chunks: vec![ChunkRef {
                        hash: ChunkHash::from_data(b"x"),
                        plain_len: 1024,
                    }],
                },
                FileEntry {
                    path: PathBuf::from("docs"),
                    size: 0,
                    mtime_ns: 0,
                    kind: EntryKind::Dir,
                    chunks: Vec::new(),
                },
            ],
        });
        manifest.skipped.push(SkippedItem {
            path: PathBuf::from("locked.pst"),
            reason: "sharing violation".into(),
        });

        let json = manifest.to_json().expect("to_json");
        let back = SnapshotManifest::from_json(&json).expect("from_json");
        assert_eq!(back.job_name, "job");
        assert_eq!(back.file_count(), 1);
        assert_eq!(back.total_bytes(), 1024);
        assert_eq!(back.sources, manifest.sources);
        assert_eq!(back.skipped, manifest.skipped);
    }

    #[test]
    fn from_json_should_reject_future_versions() {
        let json = r#"{"version":99,"id":"00000000-0000-0000-0000-000000000000","job_id":"00000000-0000-0000-0000-000000000000","job_name":"x","created_at":"2026-01-01T00:00:00Z","host":"h","sources":[],"skipped":[]}"#;
        assert!(SnapshotManifest::from_json(json.as_bytes()).is_err());
    }
}

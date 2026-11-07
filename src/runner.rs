//! Bridge between the GUI and the rb-core engine: runs a backup on a
//! background thread and streams progress events over a flume channel.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use flume::{Receiver, Sender};
use uuid::Uuid;

use rb_core::backup::{BackupEvent, BackupOptions, BackupStats, run_backup};
use rb_core::catalog::Catalog;
use rb_core::domain::{Destination, JobConfig};

/// Events flowing from the engine thread to the UI.
pub enum EngineEvent {
    /// Live progress from the backup engine.
    Progress(BackupEvent),
    /// The run finished (`Err` = user-facing error message).
    Finished(Result<BackupSummary, String>),
}

/// What a successful run produced.
#[derive(Debug, Clone, Copy)]
pub struct BackupSummary {
    /// Id of the finalized snapshot.
    pub snapshot_id: Uuid,
    /// Aggregate counters.
    pub stats: BackupStats,
}

/// Spawns a backup run on its own thread. Returns the event stream and the
/// cooperative cancel flag.
pub fn spawn_backup(
    job: JobConfig,
    passphrase: String,
) -> (Receiver<EngineEvent>, Arc<AtomicBool>) {
    let (tx, rx) = flume::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let runner = BackupRunner {
        tx,
        cancel: Arc::clone(&cancel),
    };
    std::thread::spawn(move || runner.execute(job, passphrase));
    (rx, cancel)
}

struct BackupRunner {
    tx: Sender<EngineEvent>,
    cancel: Arc<AtomicBool>,
}

impl BackupRunner {
    fn execute(self, job: JobConfig, passphrase: String) {
        let started = chrono::Utc::now();
        let result = self.run(&job, &passphrase);
        let finished = chrono::Utc::now();

        // Record the outcome in the local catalog (best effort — the
        // repository remains the source of truth).
        if let Ok(catalog) = Catalog::default_path()
            .map_err(|e| e.to_string())
            .and_then(|p| Catalog::open(&p).map_err(|e| e.to_string()))
        {
            let record = match &result {
                Ok(summary) => rb_core::domain::RunRecord {
                    id: Uuid::new_v4(),
                    job_id: job.id,
                    started_at: started,
                    finished_at: finished,
                    status: rb_core::domain::RunStatus::Success,
                    files_total: summary.1.files_total,
                    bytes_total: summary.1.bytes_total,
                    chunks_new: summary.1.chunks_new,
                    bytes_new: summary.1.bytes_new,
                    error: None,
                },
                Err(message) => rb_core::domain::RunRecord {
                    id: Uuid::new_v4(),
                    job_id: job.id,
                    started_at: started,
                    finished_at: finished,
                    status: rb_core::domain::RunStatus::Failed,
                    files_total: 0,
                    bytes_total: 0,
                    chunks_new: 0,
                    bytes_new: 0,
                    error: Some(message.clone()),
                },
            };
            if catalog.insert_run(&record).is_ok()
                && let Ok(summary) = &result
            {
                let _ = catalog.insert_snapshot(
                    summary.0,
                    job.id,
                    finished,
                    summary.1.files_total,
                    summary.1.bytes_total,
                );
            }
        }

        let _ = self
            .tx
            .send(EngineEvent::Finished(result.map(|s| BackupSummary {
                snapshot_id: s.0,
                stats: s.1,
            })));
    }

    fn run(&self, job: &JobConfig, passphrase: &str) -> Result<(Uuid, BackupStats), String> {
        let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
        let repo = rb_core::repo::Repository::open(backend, passphrase)
            .map_err(|e| format!("ouverture du dépôt impossible : {e}"))?;
        let tx = self.tx.clone();
        let (manifest, stats) = run_backup(
            &repo,
            job,
            &BackupOptions {
                cancel: Some(Arc::clone(&self.cancel)),
            },
            move |event| {
                // The engine is cancelled by dropping interest too.
                if tx.send(EngineEvent::Progress(event)).is_err() {
                    return Err(rb_core::Error::Cancelled);
                }
                Ok(())
            },
        )
        .map_err(|e| e.to_string())?;
        Ok((manifest.id, stats))
    }
}

/// Builds a storage backend for a destination (TOFU known-hosts for SFTP).
fn open_backend(
    destination: &Destination,
) -> Result<Arc<dyn rb_core::backend::StorageBackend>, rb_core::Error> {
    match destination {
        Destination::Local { root } => Ok(Arc::new(rb_core::backend::LocalFs::new(root))),
        Destination::Sftp {
            host,
            port,
            username,
            key_path,
            remote_root,
        } => {
            let known_hosts = Catalog::default_path()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("known_hosts")))
                .unwrap_or_else(|| "known_hosts".into());
            let backend = rb_core::backend::SftpBackend::connect(&rb_core::backend::SftpConnect {
                host: host.clone(),
                port: *port,
                username: username.clone(),
                key_path: key_path.clone(),
                key_passphrase: None,
                remote_root: remote_root.clone(),
                host_key_policy: rb_core::backend::HostKeyPolicy::TrustOnFirstUse { known_hosts },
            })?;
            Ok(Arc::new(backend))
        }
    }
}

// ---------------------------------------------------------------------------
// Restore + verify + snapshot browsing bridges
// ---------------------------------------------------------------------------

/// A snapshot row for the restore browser.
#[derive(Debug, Clone)]
pub struct SnapshotRow {
    /// Snapshot id.
    pub id: Uuid,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Files captured.
    pub files: usize,
    /// Entries: (source root, relative path, size in bytes).
    pub entries: Vec<(String, String, u64)>,
}

/// Loads the snapshots of a job's repository on a background thread.
pub fn spawn_snapshots(
    job: JobConfig,
    passphrase: String,
) -> Receiver<Result<Vec<SnapshotRow>, String>> {
    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
        let result = (|| {
            let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
            let repo = rb_core::repo::Repository::open(backend, &passphrase)
                .map_err(|e| format!("ouverture du dépôt impossible : {e}"))?;
            let mut rows = Vec::new();
            for manifest in repo
                .all_manifests()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|m| m.job_id == job.id)
            {
                let mut entries = Vec::new();
                for (source, entry) in manifest.iter_files() {
                    entries.push((
                        source.root.to_string_lossy().to_string(),
                        entry.path.to_string_lossy().replace('\\', "/"),
                        entry.size,
                    ));
                }
                rows.push(SnapshotRow {
                    id: manifest.id,
                    created_at: manifest.created_at.to_rfc3339(),
                    files: manifest.file_count(),
                    entries,
                });
            }
            Ok(rows)
        })();
        let _ = tx.send(result);
    });
    rx
}

/// Restore progress + outcome stream.
pub enum RestoreEvent {
    /// Live event from the restore engine.
    Progress(rb_core::restore::RestoreEvent),
    /// The restore finished (`Err` = user-facing message).
    Finished(Result<rb_core::restore::RestoreStats, String>),
}

/// Runs a restore on a background thread.
#[allow(clippy::too_many_arguments)]
pub fn spawn_restore(
    job: JobConfig,
    passphrase: String,
    snapshot_id: Uuid,
    selection: Vec<rb_core::restore::RestoreSelection>,
    destination: rb_core::restore::RestoreDestination,
    overwrite: rb_core::restore::OverwritePolicy,
) -> Receiver<RestoreEvent> {
    let (tx, rx) = flume::unbounded();
    std::thread::spawn(move || {
        let result = (|| {
            let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
            let repo = rb_core::repo::Repository::open(backend, &passphrase)
                .map_err(|e| format!("ouverture du dépôt impossible : {e}"))?;
            rb_core::restore::restore(
                &repo,
                &snapshot_id,
                &selection,
                &destination,
                &rb_core::restore::RestoreOptions {
                    overwrite,
                    cancel: None,
                },
                |event| {
                    if tx.send(RestoreEvent::Progress(event)).is_err() {
                        return Err(rb_core::Error::Cancelled);
                    }
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())
        })();
        let _ = tx.send(RestoreEvent::Finished(result));
    });
    rx
}

/// Runs a verification pass on a background thread; reports a summary line
/// or an error message.
pub fn spawn_verify(
    job: JobConfig,
    passphrase: String,
    full: bool,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
        let result = (|| {
            let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
            let repo = rb_core::repo::Repository::open(backend, &passphrase)
                .map_err(|e| format!("ouverture du dépôt impossible : {e}"))?;
            let mode = if full {
                rb_core::verify::VerifyMode::Full
            } else {
                rb_core::verify::VerifyMode::Quick
            };
            let report = rb_core::verify::verify(&repo, mode, None, |_, _| Ok(()))
                .map_err(|e| e.to_string())?;
            if let Ok(catalog) = Catalog::default_path()
                .map_err(|e| e.to_string())
                .and_then(|p| Catalog::open(&p).map_err(|e| e.to_string()))
            {
                let _ = catalog.insert_verify_result(&repo.describe(), &report);
            }
            let head = format!(
                "{} [{}] — {} instantané(s), {} bloc(s), {} orphelin(s)",
                if report.ok() { "OK" } else { "FAILED" },
                report.mode.as_str(),
                report.manifests_checked,
                report.chunks_checked,
                report.orphan_chunks
            );
            let issues: Vec<String> = report.issues.iter().map(|i| i.message.clone()).collect();
            if issues.is_empty() {
                Ok(head)
            } else {
                Ok(format!("{head}\n{}", issues.join("\n")))
            }
        })();
        let _ = tx.send(result);
    });
    rx
}

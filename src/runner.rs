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

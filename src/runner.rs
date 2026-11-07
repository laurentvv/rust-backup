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
    force_kind: Option<rb_core::domain::BackupKind>,
) -> (Receiver<EngineEvent>, Arc<AtomicBool>) {
    let (tx, rx) = flume::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let runner = BackupRunner {
        tx,
        cancel: Arc::clone(&cancel),
        force_kind,
    };
    std::thread::spawn(move || runner.execute(job, passphrase));
    (rx, cancel)
}

struct BackupRunner {
    tx: Sender<EngineEvent>,
    cancel: Arc<AtomicBool>,
    force_kind: Option<rb_core::domain::BackupKind>,
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
                    kind: summary.1.kind,
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
                    kind: self
                        .force_kind
                        .unwrap_or(rb_core::domain::BackupKind::Incremental),
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
                    summary.1.kind,
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
        let repo = match rb_core::repo::Repository::open(Arc::clone(&backend), passphrase) {
            Ok(r) => r,
            Err(e) => {
                if !backend.exists("config.json").unwrap_or(false) {
                    rb_core::repo::Repository::init(
                        backend,
                        passphrase,
                        rb_core::chunker::ChunkerConfig::default(),
                        job.compression_level,
                        rb_core::crypto::KdfParams::default(),
                    )
                    .map_err(|init_err| format!("Auto-init failed: {init_err}"))?
                } else {
                    return Err(e.to_string());
                }
            }
        };
        let tx = self.tx.clone();
        let (manifest, stats) = run_backup(
            &repo,
            job,
            &BackupOptions {
                cancel: Some(Arc::clone(&self.cancel)),
                force_kind: self.force_kind,
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

/// Initializes a repository explicitly for a job destination.
pub fn spawn_init_repo(
    job: JobConfig,
    passphrase: String,
) -> flume::Receiver<Result<String, String>> {
    let (tx, rx) = flume::unbounded();
    std::thread::spawn(move || {
        let result = (|| {
            let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
            if backend.exists("config.json").unwrap_or(false) {
                return Err("Repository already exists at destination".into());
            }
            let repo = rb_core::repo::Repository::init(
                backend,
                &passphrase,
                rb_core::chunker::ChunkerConfig::default(),
                job.compression_level,
                rb_core::crypto::KdfParams::default(),
            )
            .map_err(|e| e.to_string())?;
            Ok(format!("Repository initialized (key: {})", repo.key_id()))
        })();
        let _ = tx.send(result);
    });
    rx
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
    /// Kind of backup (full or incremental).
    pub kind: rb_core::domain::BackupKind,
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
            let repo =
                rb_core::repo::Repository::open(backend, &passphrase).map_err(|e| e.to_string())?;
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
                    kind: manifest.kind,
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
) -> (Receiver<RestoreEvent>, Arc<AtomicBool>) {
    let (tx, rx) = flume::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = Arc::clone(&cancel);
    std::thread::spawn(move || {
        let result = (|| {
            let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
            let repo =
                rb_core::repo::Repository::open(backend, &passphrase).map_err(|e| e.to_string())?;
            rb_core::restore::restore(
                &repo,
                &snapshot_id,
                &selection,
                &destination,
                &rb_core::restore::RestoreOptions {
                    overwrite,
                    cancel: Some(cancel_worker),
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
    (rx, cancel)
}

/// Verification event stream.
pub enum VerifyEvent {
    /// Progress event: checked chunks so far and total chunks.
    Progress {
        /// Chunks verified so far.
        current: usize,
        /// Total chunks to verify.
        total: usize,
    },
    /// The verification finished (`Err` = user-facing error message).
    Finished(Result<rb_core::verify::VerifyReport, String>),
}

/// Runs a verification pass on a background thread; reports progress and the structured report
/// or an error message.
pub fn spawn_verify(
    job: JobConfig,
    passphrase: String,
    full: bool,
) -> (Receiver<VerifyEvent>, Arc<AtomicBool>) {
    let (tx, rx) = flume::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = Arc::clone(&cancel);
    let tx_worker = tx.clone();
    std::thread::spawn(move || {
        let result = (|| {
            let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
            let repo =
                rb_core::repo::Repository::open(backend, &passphrase).map_err(|e| e.to_string())?;
            let mode = if full {
                rb_core::verify::VerifyMode::Full
            } else {
                rb_core::verify::VerifyMode::Quick
            };
            let report =
                rb_core::verify::verify(&repo, mode, Some(cancel_worker), |current, total| {
                    if tx_worker
                        .send(VerifyEvent::Progress { current, total })
                        .is_err()
                    {
                        return Err(rb_core::Error::Cancelled);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            if let Ok(catalog) = Catalog::default_path()
                .map_err(|e| e.to_string())
                .and_then(|p| Catalog::open(&p).map_err(|e| e.to_string()))
            {
                let _ = catalog.insert_verify_result(&repo.describe(), &report);
            }
            Ok(report)
        })();
        let _ = tx.send(VerifyEvent::Finished(result));
    });
    (rx, cancel)
}

/// Prune event stream.
#[allow(dead_code)]
pub enum PruneEvent {
    /// Progress event: checked chunks so far and total chunks.
    Progress {
        /// Chunks checked so far.
        current: u64,
        /// Total chunks to check.
        total: u64,
    },
    /// The prune finished (`Err` = user-facing error message).
    Finished(Result<rb_core::repo::PruneReport, String>),
}

/// Runs orphan chunk pruning on a background thread.
pub fn spawn_prune(job: JobConfig, passphrase: String) -> (Receiver<PruneEvent>, Arc<AtomicBool>) {
    let (tx, rx) = flume::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = Arc::clone(&cancel);
    let tx_worker = tx.clone();
    std::thread::spawn(move || {
        let result = (|| {
            let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
            let repo =
                rb_core::repo::Repository::open(backend, &passphrase).map_err(|e| e.to_string())?;
            let report = repo
                .prune_orphans(Some(&cancel_worker), |current, total| {
                    let _ = tx_worker.send(PruneEvent::Progress { current, total });
                })
                .map_err(|e| e.to_string())?;
            Ok(report)
        })();
        let _ = tx.send(PruneEvent::Finished(result));
    });
    (rx, cancel)
}

/// Deletes a snapshot in the repository and local catalog on a background thread.
pub fn spawn_delete_snapshot(
    job: JobConfig,
    passphrase: String,
    snapshot_id: Uuid,
) -> Receiver<Result<(), String>> {
    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
        let result = (|| {
            delete_snapshot_in_repo(&job, &passphrase, &snapshot_id)?;
            if let Ok(catalog) = Catalog::default_path()
                .map_err(|e| e.to_string())
                .and_then(|p| Catalog::open(&p).map_err(|e| e.to_string()))
            {
                let _ = catalog.delete_snapshot(snapshot_id);
            }
            Ok(())
        })();
        let _ = tx.send(result);
    });
    rx
}

/// Repository side of a snapshot deletion: opens the job's repository with the
/// given passphrase and removes the snapshot manifest.
fn delete_snapshot_in_repo(
    job: &JobConfig,
    passphrase: &str,
    snapshot_id: &Uuid,
) -> Result<(), String> {
    let backend = open_backend(&job.destination).map_err(|e| e.to_string())?;
    let repo = rb_core::repo::Repository::open(backend, passphrase).map_err(|e| e.to_string())?;
    repo.delete_snapshot(snapshot_id).map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rb_core::backend::{LocalFs, StorageBackend};
    use rb_core::chunker::ChunkerConfig;
    use rb_core::crypto::KdfParams;
    use rb_core::domain::JobId;
    use rb_core::repo::Repository;
    use std::path::PathBuf;

    const PASSPHRASE: &str = "runner test passphrase";

    struct Fixture {
        base: PathBuf,
        job: JobConfig,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// A local repository holding two snapshots of a small source tree.
    fn fixture(tag: &str) -> Fixture {
        let base = std::env::temp_dir().join(format!("rb-runner-{tag}-{}", Uuid::new_v4()));
        let source = base.join("src");
        let dest = base.join("repo");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::write(source.join("a.txt"), b"first version\n".repeat(64)).expect("a.txt");

        let job = JobConfig {
            id: JobId::new_v4(),
            name: format!("runner-{tag}"),
            sources: vec![source.clone()],
            destination: Destination::Local { root: dest.clone() },
            compression_level: 3,
            excludes: vec![],
            max_incrementals: 10,
            created_at: chrono::Utc::now(),
        };
        let backend: Arc<dyn StorageBackend> = Arc::new(LocalFs::new(&dest));
        let fast_kdf = KdfParams {
            m_cost: 1024,
            t_cost: 1,
            p_cost: 1,
        };
        let repo = Repository::init(backend, PASSPHRASE, ChunkerConfig::default(), 3, fast_kdf)
            .expect("repo init");
        run_backup(&repo, &job, &BackupOptions::default(), |_| Ok(())).expect("backup 1");
        std::fs::write(source.join("a.txt"), b"second version\n".repeat(64)).expect("a.txt v2");
        run_backup(&repo, &job, &BackupOptions::default(), |_| Ok(())).expect("backup 2");
        Fixture { base, job }
    }

    fn open(job: &JobConfig) -> Repository {
        Repository::open(open_backend(&job.destination).expect("backend"), PASSPHRASE)
            .expect("repo open")
    }

    #[test]
    fn delete_snapshot_with_settings_passphrase_removes_only_that_snapshot() {
        let fx = fixture("delete");
        let ids = open(&fx.job).list_snapshot_ids().expect("ids");
        assert_eq!(ids.len(), 2);

        delete_snapshot_in_repo(&fx.job, PASSPHRASE, &ids[0]).expect("delete succeeds");

        let left = open(&fx.job).list_snapshot_ids().expect("ids after");
        assert_eq!(left, vec![ids[1]]);
    }

    #[test]
    fn delete_snapshot_with_empty_passphrase_is_refused() {
        let fx = fixture("delete-empty");
        let ids = open(&fx.job).list_snapshot_ids().expect("ids");

        assert!(delete_snapshot_in_repo(&fx.job, "", &ids[0]).is_err());
        assert_eq!(open(&fx.job).list_snapshot_ids().expect("ids").len(), 2);
    }

    #[test]
    fn spawn_prune_with_settings_passphrase_reclaims_orphans() {
        let fx = fixture("prune");
        let ids = open(&fx.job).list_snapshot_ids().expect("ids");
        delete_snapshot_in_repo(&fx.job, PASSPHRASE, &ids[0]).expect("delete");

        let (rx, _cancel) = spawn_prune(fx.job.clone(), PASSPHRASE.to_string());
        let report = loop {
            match rx.recv().expect("prune event") {
                PruneEvent::Progress { .. } => {}
                PruneEvent::Finished(result) => break result.expect("prune succeeds"),
            }
        };
        assert!(
            report.chunks_deleted > 0,
            "first version's chunk is orphaned"
        );
        // The remaining snapshot is still fully readable.
        let repo = open(&fx.job);
        for manifest in repo.all_manifests().expect("manifests") {
            for (_, entry) in manifest.iter_files() {
                for chunk in &entry.chunks {
                    assert!(repo.read_chunk(&chunk.hash, chunk.plain_len).is_ok());
                }
            }
        }
    }
}

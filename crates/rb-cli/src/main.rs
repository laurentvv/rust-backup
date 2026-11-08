//! `rbackup` — command-line interface for RustBackup.
//!
//! Headless driver for the core engine; also used by the integration test
//! suite to exercise the real end-to-end path.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use rb_core::catalog::Catalog;
use rb_core::domain::{Destination, JobConfig};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "rbackup",
    version,
    about = "RustBackup — encrypted, deduplicated backups to USB or SFTP"
)]
struct Cli {
    /// Override the catalog database path (default: local app data).
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Accept ANY SSH host key (TESTS ONLY; default is trust-on-first-use).
    #[arg(long, global = true)]
    insecure_accept_any_host_key: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage backup jobs.
    Jobs {
        #[command(subcommand)]
        action: JobsAction,
    },
    /// Inspect the local catalog database.
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Initialize a backup repository at a destination.
    Repo {
        #[command(subcommand)]
        action: RepoAction,
    },
    /// Run a backup job now.
    Backup {
        /// Job name or id.
        #[arg(long)]
        job: String,
    },
    /// List snapshots of a job.
    Snapshots {
        /// Job name or id.
        #[arg(long)]
        job: String,
    },
    /// Restore files or directories from a snapshot.
    Restore {
        /// Job name or id.
        #[arg(long)]
        job: String,
        /// Snapshot id to restore from.
        #[arg(long)]
        snapshot: Uuid,
        /// Path to restore, relative to a source root (repeatable; default:
        /// the whole snapshot).
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        /// Restore into this directory instead of the original location
        /// (structure preserved under it).
        #[arg(long)]
        to: Option<PathBuf>,
        /// Overwrite policy for existing files: skip (default) or overwrite.
        #[arg(long, default_value = "skip")]
        overwrite: String,
    },
    /// Verify repository integrity (quick or full).
    Verify {
        /// Job name or id (selects the repository).
        #[arg(long)]
        job: String,
        /// Full check: read + authenticate + re-hash every chunk.
        #[arg(long)]
        full: bool,
    },
    /// Delete a snapshot manifest and catalog entry.
    DeleteSnapshot {
        /// Job name or id.
        #[arg(long)]
        job: String,
        /// Snapshot id to delete.
        #[arg(long)]
        snapshot: Uuid,
    },
    /// Prune unreferenced orphan chunks from the repository.
    Prune {
        /// Job name or id.
        #[arg(long)]
        job: String,
    },
}

#[derive(Subcommand)]
enum RepoAction {
    /// Create a new encrypted repository (local or SFTP destination).
    Init {
        /// Local repository root folder (USB disk, ...).
        #[arg(long, required_unless_present = "sftp_host")]
        dest: Option<PathBuf>,
        /// zstd compression level 0..=22 (default 5).
        #[arg(long, default_value_t = 5)]
        level: i32,
        /// SFTP server host (switches the destination to SFTP).
        #[arg(long)]
        sftp_host: Option<String>,
        /// SFTP port (default 22).
        #[arg(long, default_value_t = 22)]
        sftp_port: u16,
        /// SFTP user name.
        #[arg(long)]
        sftp_user: Option<String>,
        /// Private key file for SFTP (passphrase via RBACKUP_KEY_PASSPHRASE).
        #[arg(long)]
        sftp_key: Option<PathBuf>,
        /// Repository root on the SFTP server.
        #[arg(long)]
        sftp_root: Option<String>,
    },
}

#[derive(Subcommand)]
enum JobsAction {
    /// Create a backup job writing to a local destination (USB disk, ...).
    Add {
        /// Display name of the job.
        #[arg(long)]
        name: String,
        /// Source directory (repeat the flag for several sources).
        #[arg(long = "source", required = true)]
        sources: Vec<PathBuf>,
        /// Repository root folder on the local filesystem.
        #[arg(long, required_unless_present = "sftp_host")]
        local_root: Option<PathBuf>,
        /// zstd compression level 0..=22 (default 5).
        #[arg(long, default_value_t = 5)]
        compression: i32,
        /// SFTP server host (switches the destination to SFTP).
        #[arg(long)]
        sftp_host: Option<String>,
        /// SFTP port (default 22).
        #[arg(long, default_value_t = 22)]
        sftp_port: u16,
        /// SFTP user name.
        #[arg(long)]
        sftp_user: Option<String>,
        /// Private key file for SFTP.
        #[arg(long)]
        sftp_key: Option<PathBuf>,
        /// Repository root on the SFTP server.
        #[arg(long)]
        sftp_root: Option<String>,
        /// File/directory name or wildcard pattern to exclude (repeatable).
        #[arg(long = "exclude")]
        excludes: Vec<String>,
    },
    /// List configured jobs.
    List,
    /// Remove a job by id (run history is kept).
    Remove {
        /// Id of the job to remove.
        id: Uuid,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Print the resolved catalog path.
    Path,
    /// Create/migrate the catalog file now.
    Migrate,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let db_path = match cli.db.clone() {
        Some(path) => path,
        None => Catalog::default_path()?,
    };
    match cli.command {
        Command::Db { action } => match action {
            DbAction::Path => {
                println!("{}", db_path.display());
            }
            DbAction::Migrate => {
                let _catalog = Catalog::open(&db_path)?;
                println!("catalog ready: {}", db_path.display());
            }
        },
        Command::Jobs { action } => match action {
            JobsAction::Add {
                name,
                sources,
                local_root,
                compression,
                sftp_host,
                sftp_port,
                sftp_user,
                sftp_key,
                sftp_root,
                excludes,
            } => {
                let catalog = Catalog::open(&db_path)?;
                let destination = build_destination(
                    local_root, sftp_host, sftp_port, sftp_user, sftp_key, sftp_root,
                )?;
                let job = JobConfig {
                    id: Uuid::new_v4(),
                    name,
                    sources,
                    destination,
                    compression_level: compression,
                    excludes,
                    created_at: chrono_now(),
                };
                job.validate().map_err(|e| anyhow::anyhow!(e))?;
                catalog.insert_job(&job)?;
                println!("job created: {} ({})", job.name, job.id);
            }
            JobsAction::List => {
                let catalog = Catalog::open(&db_path)?;
                let jobs = catalog.list_jobs()?;
                if jobs.is_empty() {
                    println!("no jobs configured — create one with `rbackup jobs add`");
                    return Ok(());
                }
                for job in jobs {
                    println!(
                        "{}\t{}\t{}\t{} source(s)\tzstd-{}",
                        job.id,
                        job.name,
                        job.destination.describe(),
                        job.sources.len(),
                        job.compression_level
                    );
                }
            }
            JobsAction::Remove { id } => {
                let catalog = Catalog::open(&db_path)?;
                if catalog.delete_job(id)? {
                    println!("job removed: {id}");
                } else {
                    println!("no job with id {id}");
                }
            }
        },
        Command::Repo { action } => match action {
            RepoAction::Init {
                dest,
                level,
                sftp_host,
                sftp_port,
                sftp_user,
                sftp_key,
                sftp_root,
            } => {
                let passphrase = read_passphrase()?;
                let destination =
                    build_destination(dest, sftp_host, sftp_port, sftp_user, sftp_key, sftp_root)?;
                let backend = open_backend(&destination, cli.insecure_accept_any_host_key)?;
                let repo = rb_core::repo::Repository::init(
                    backend,
                    &passphrase,
                    rb_core::chunker::ChunkerConfig::default(),
                    level,
                    rb_core::crypto::KdfParams::default(),
                )
                .map_err(|e| anyhow::anyhow!(e))?;
                println!(
                    "repository initialized at {} (key {})",
                    repo.describe(),
                    repo.key_id()
                );
            }
        },
        Command::Backup { job } => {
            let catalog = Catalog::open(&db_path)?;
            let config = find_job(&catalog, &job)?;
            let passphrase = read_passphrase()?;
            let run = run_job(&catalog, &config, &passphrase)?;
            println!(
                "snapshot {} finalized: {} files, {} bytes, {} new chunks ({} bytes), {} skipped",
                run.0,
                run.1.files_total,
                run.1.bytes_total,
                run.1.chunks_new,
                run.1.bytes_new,
                run.1.files_skipped
            );
        }
        Command::Snapshots { job } => {
            let catalog = Catalog::open(&db_path)?;
            let config = find_job(&catalog, &job)?;
            let passphrase = read_passphrase()?;
            let repo = open_repo(&config, &passphrase, cli.insecure_accept_any_host_key)?;
            for manifest in repo.all_manifests().map_err(|e| anyhow::anyhow!(e))? {
                if manifest.job_id != config.id {
                    continue;
                }
                println!(
                    "{}\t{}\t{} files\t{} bytes\t{} skipped",
                    manifest.id,
                    manifest.created_at.to_rfc3339(),
                    manifest.file_count(),
                    manifest.total_bytes(),
                    manifest.skipped.len()
                );
            }
        }
        Command::Restore {
            job,
            snapshot,
            paths,
            to,
            overwrite,
        } => {
            let catalog = Catalog::open(&db_path)?;
            let config = find_job(&catalog, &job)?;
            let passphrase = read_passphrase()?;
            let repo = open_repo(&config, &passphrase, cli.insecure_accept_any_host_key)?;

            let manifest = repo
                .read_manifest(&snapshot)
                .map_err(|e| anyhow::anyhow!(e))?;
            let selection: Vec<rb_core::restore::RestoreSelection> = manifest
                .sources
                .iter()
                .flat_map(|s| {
                    let root = s.root.clone();
                    let paths = if paths.is_empty() {
                        vec![PathBuf::new()]
                    } else {
                        paths.clone()
                    };
                    paths
                        .into_iter()
                        .map(move |p| rb_core::restore::RestoreSelection {
                            source_root: root.clone(),
                            rel_path: p,
                        })
                })
                .collect();

            let destination = match &to {
                Some(base) => rb_core::restore::RestoreDestination::Target(base.clone()),
                None => rb_core::restore::RestoreDestination::Original,
            };
            let overwrite = match overwrite.as_str() {
                "overwrite" => rb_core::restore::OverwritePolicy::Overwrite,
                _ => rb_core::restore::OverwritePolicy::Skip,
            };
            let options = rb_core::restore::RestoreOptions {
                overwrite,
                cancel: None,
            };

            let stats = rb_core::restore::restore(
                &repo,
                &snapshot,
                &selection,
                &destination,
                &options,
                |event| {
                    if let rb_core::restore::RestoreEvent::FileSkipped { dest, reason } = event {
                        eprintln!("skipped: {} ({reason})", dest.display());
                    }
                    Ok(())
                },
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "restored: {} files ({} bytes), {} skipped, {} directories created",
                stats.files_restored, stats.bytes_restored, stats.files_skipped, stats.dirs_created
            );
        }
        Command::Verify { job, full } => {
            let catalog = Catalog::open(&db_path)?;
            let config = find_job(&catalog, &job)?;
            let passphrase = read_passphrase()?;
            let repo = open_repo(&config, &passphrase, cli.insecure_accept_any_host_key)?;
            let mode = if full {
                rb_core::verify::VerifyMode::Full
            } else {
                rb_core::verify::VerifyMode::Quick
            };
            let report = rb_core::verify::verify(&repo, mode, None, |_, _| Ok(()))
                .map_err(|e| anyhow::anyhow!(e))?;
            catalog
                .insert_verify_result(&repo.describe(), &report)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "verify [{}] on {}: {} ({} snapshots, {} chunks, {} orphans)",
                report.mode.as_str(),
                repo.describe(),
                if report.ok() { "OK" } else { "FAILED" },
                report.manifests_checked,
                report.chunks_checked,
                report.orphan_chunks
            );
            for issue in &report.issues {
                println!("  ! {}", issue.message);
            }
            if !report.ok() {
                std::process::exit(2);
            }
        }
        Command::DeleteSnapshot { job, snapshot } => {
            let catalog = Catalog::open(&db_path)?;
            let config = find_job(&catalog, &job)?;
            let passphrase = read_passphrase()?;
            let repo = open_repo(&config, &passphrase, cli.insecure_accept_any_host_key)?;
            repo.delete_snapshot(&snapshot)
                .map_err(|e| anyhow::anyhow!(e))?;
            let _ = catalog.delete_snapshot(snapshot);
            println!("snapshot {snapshot} deleted");
        }
        Command::Prune { job } => {
            let catalog = Catalog::open(&db_path)?;
            let config = find_job(&catalog, &job)?;
            let passphrase = read_passphrase()?;
            let repo = open_repo(&config, &passphrase, cli.insecure_accept_any_host_key)?;
            let report = repo
                .prune_orphans(None, |cur, tot| {
                    if tot > 0 && cur % 100 == 0 {
                        eprintln!("scanning chunks: {cur}/{tot}...");
                    }
                })
                .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "prune complete: {} orphan chunks deleted ({} bytes freed), {} preserved",
                report.chunks_deleted, report.bytes_freed, report.chunks_kept
            );
        }
    }
    Ok(())
}

/// Resolves a job by name or exact id.
fn find_job(catalog: &Catalog, name_or_id: &str) -> anyhow::Result<JobConfig> {
    let jobs = catalog.list_jobs()?;
    if let Ok(id) = Uuid::parse_str(name_or_id)
        && let Some(job) = jobs.iter().find(|j| j.id == id)
    {
        return Ok(job.clone());
    }
    jobs.into_iter()
        .find(|j| j.name == name_or_id)
        .ok_or_else(|| anyhow::anyhow!("no job named or id'd '{name_or_id}'"))
}

/// Reads the repository passphrase from the environment (never an
/// argument, never printed).
fn read_passphrase() -> anyhow::Result<String> {
    std::env::var("RBACKUP_PASSPHRASE")
        .map_err(|_| anyhow::anyhow!("set RBACKUP_PASSPHRASE to the repository passphrase"))
}

/// Builds a destination from CLI flags (exactly one of local/SFTP).
#[allow(clippy::too_many_arguments)]
fn build_destination(
    local_root: Option<PathBuf>,
    sftp_host: Option<String>,
    sftp_port: u16,
    sftp_user: Option<String>,
    sftp_key: Option<PathBuf>,
    sftp_root: Option<String>,
) -> anyhow::Result<Destination> {
    if let Some(host) = sftp_host {
        let username = sftp_user.ok_or_else(|| anyhow::anyhow!("--sftp-user is required"))?;
        let key_path = sftp_key.ok_or_else(|| anyhow::anyhow!("--sftp-key is required"))?;
        let remote_root = sftp_root.ok_or_else(|| anyhow::anyhow!("--sftp-root is required"))?;
        Ok(Destination::Sftp {
            host,
            port: sftp_port,
            username,
            key_path,
            remote_root,
        })
    } else {
        Ok(Destination::Local {
            root: local_root.ok_or_else(|| anyhow::anyhow!("--dest/--local-root is required"))?,
        })
    }
}

/// Opens a storage backend for a destination.
fn open_backend(
    destination: &Destination,
    insecure_accept_any: bool,
) -> anyhow::Result<std::sync::Arc<dyn rb_core::backend::StorageBackend>> {
    match destination {
        Destination::Local { root } => {
            Ok(std::sync::Arc::new(rb_core::backend::LocalFs::new(root)))
        }
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
            let policy = if insecure_accept_any {
                rb_core::backend::HostKeyPolicy::AcceptAll
            } else {
                rb_core::backend::HostKeyPolicy::TrustOnFirstUse { known_hosts }
            };
            let backend = rb_core::backend::SftpBackend::connect(&rb_core::backend::SftpConnect {
                host: host.clone(),
                port: *port,
                username: username.clone(),
                key_path: key_path.clone(),
                key_passphrase: std::env::var("RBACKUP_KEY_PASSPHRASE").ok(),
                remote_root: remote_root.clone(),
                host_key_policy: policy,
            })
            .map_err(|e| anyhow::anyhow!(e))?;
            Ok(std::sync::Arc::new(backend))
        }
    }
}

/// Opens the job's repository (local or SFTP destination).
fn open_repo(
    config: &JobConfig,
    passphrase: &str,
    insecure_accept_any: bool,
) -> anyhow::Result<rb_core::repo::Repository> {
    let backend = open_backend(&config.destination, insecure_accept_any)?;
    rb_core::repo::Repository::open(backend, passphrase).map_err(|e| anyhow::anyhow!(e))
}

/// Runs one backup, records the outcome in the catalog, prints progress.
fn run_job(
    catalog: &Catalog,
    config: &JobConfig,
    passphrase: &str,
) -> anyhow::Result<(Uuid, rb_core::backup::BackupStats)> {
    let repo = open_repo(config, passphrase, false)?;
    let started = chrono_now();
    let mut files_done = 0u64;

    let result = rb_core::backup::run_backup(
        &repo,
        config,
        &rb_core::backup::BackupOptions::default(),
        |event| {
            match event {
                rb_core::backup::BackupEvent::FileDone { .. } => files_done += 1,
                rb_core::backup::BackupEvent::FileSkipped { path, reason } => {
                    eprintln!("skipped: {} ({reason})", path.display());
                }
                _ => {}
            }
            Ok(())
        },
    );

    let (snapshot_id, stats) = match result {
        Ok((manifest, stats)) => (manifest.id, stats),
        Err(e) => {
            let record = rb_core::domain::RunRecord {
                id: Uuid::new_v4(),
                job_id: config.id,
                started_at: started,
                finished_at: chrono_now(),
                status: rb_core::domain::RunStatus::Failed,
                files_total: 0,
                bytes_total: 0,
                chunks_new: 0,
                bytes_new: 0,
                error: Some(e.to_string()),
            };
            catalog.insert_run(&record)?;
            return Err(anyhow::anyhow!(e));
        }
    };

    let record = rb_core::domain::RunRecord {
        id: Uuid::new_v4(),
        job_id: config.id,
        started_at: started,
        finished_at: chrono_now(),
        status: rb_core::domain::RunStatus::Success,
        files_total: stats.files_total,
        bytes_total: stats.bytes_total,
        chunks_new: stats.chunks_new,
        bytes_new: stats.bytes_new,
        error: None,
    };
    catalog.insert_run(&record)?;
    catalog.insert_snapshot(
        snapshot_id,
        config.id,
        record.finished_at,
        stats.files_total,
        stats.bytes_total,
    )?;
    let _ = files_done;
    Ok((snapshot_id, stats))
}

/// Current UTC timestamp (kept in one place for testability later).
fn chrono_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

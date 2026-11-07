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
        #[arg(long)]
        local_root: PathBuf,
        /// zstd compression level 0..=22 (default 5).
        #[arg(long, default_value_t = 5)]
        compression: i32,
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
            } => {
                let catalog = Catalog::open(&db_path)?;
                let job = JobConfig {
                    id: Uuid::new_v4(),
                    name,
                    sources,
                    destination: Destination::Local { root: local_root },
                    compression_level: compression,
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
    }
    Ok(())
}

/// Current UTC timestamp (kept in one place for testability later).
fn chrono_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

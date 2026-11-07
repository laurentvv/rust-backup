//! Local DuckDB catalog: backup jobs, run history, and (in later
//! milestones) snapshot and verification caches.
//!
//! The catalog is a local tracking database — it never lives inside a backup
//! repository and losing it never prevents a restore.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::{JobConfig, JobId};

/// Ordered schema migrations; the highest applied version is stored in
/// `schema_migrations`.
const MIGRATIONS: &[(&str, &str)] = &[(
    "0001_jobs_and_runs",
    r#"
    CREATE TABLE IF NOT EXISTS jobs (
        id                 VARCHAR PRIMARY KEY,
        name               VARCHAR NOT NULL,
        sources_json       VARCHAR NOT NULL,
        destination_json   VARCHAR NOT NULL,
        compression_level  INTEGER NOT NULL,
        created_at         VARCHAR NOT NULL
    );
    CREATE TABLE IF NOT EXISTS runs (
        id            VARCHAR PRIMARY KEY,
        job_id        VARCHAR NOT NULL,
        started_at    VARCHAR NOT NULL,
        finished_at   VARCHAR NOT NULL,
        status        VARCHAR NOT NULL,
        files_total   UBIGINT NOT NULL,
        bytes_total   UBIGINT NOT NULL,
        chunks_new    UBIGINT NOT NULL,
        bytes_new     UBIGINT NOT NULL,
        error         VARCHAR
    );
    "#,
)];

/// Handle to the local catalog database. Opening migrates the schema to the
/// latest version.
#[derive(Debug)]
pub struct Catalog {
    conn: duckdb::Connection,
}

impl Catalog {
    /// Opens (creating if needed) the catalog at `path` and migrates it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`](crate::Error::Catalog) when the file
    /// cannot be opened or a migration fails, and [`Error::Io`] when a
    /// parent directory must be created but cannot.
    pub fn open(path: &Path) -> crate::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = duckdb::Connection::open(path)?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Opens an in-memory catalog (used by tests).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`](crate::Error::Catalog) on migration
    /// failure.
    pub fn open_in_memory() -> crate::Result<Self> {
        let conn = duckdb::Connection::open_in_memory()?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Default on-disk location of the catalog on this machine
    /// (`%LOCALAPPDATA%/RustBackup/catalog.duckdb` on Windows).
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoAppDataDir`] when the platform exposes no local
    /// app-data directory.
    pub fn default_path() -> crate::Result<PathBuf> {
        let base = dirs::data_local_dir().ok_or(crate::Error::NoAppDataDir)?;
        Ok(base.join("RustBackup").join("catalog.duckdb"))
    }

    fn migrate(conn: &duckdb::Connection) -> crate::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                name       VARCHAR PRIMARY KEY,
                applied_at VARCHAR NOT NULL
            );",
        )?;
        for (name, sql) in MIGRATIONS {
            let applied: i64 = conn.query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE name = ?",
                [name],
                |row| row.get(0),
            )?;
            if applied > 0 {
                continue;
            }
            conn.execute_batch(sql)?;
            conn.execute(
                "INSERT INTO schema_migrations (name, applied_at) VALUES (?, ?)",
                duckdb::params![name, Utc::now().to_rfc3339()],
            )?;
        }
        Ok(())
    }

    /// Persists a job after validating it. Inserting an existing id fails
    /// with a catalog constraint error.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidJob`] on validation failure, [`Error::Catalog`] on
    /// database errors.
    pub fn insert_job(&self, job: &JobConfig) -> crate::Result<()> {
        job.validate()?;
        let sources = serde_json::to_string(&job.sources)?;
        let destination = serde_json::to_string(&job.destination)?;
        self.conn.execute(
            "INSERT INTO jobs (id, name, sources_json, destination_json, compression_level, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            duckdb::params![
                job.id.to_string(),
                job.name,
                sources,
                destination,
                job.compression_level,
                job.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Lists all jobs ordered by name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors and
    /// [`Error::Json`] if a stored document cannot be decoded.
    pub fn list_jobs(&self) -> crate::Result<Vec<JobConfig>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, sources_json, destination_json, compression_level, created_at FROM jobs ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i32>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut jobs = Vec::new();
        for row in rows {
            let (id, name, sources_json, dest_json, level, created) = row?;
            jobs.push(JobConfig {
                id: Uuid::parse_str(&id).map_err(|e| {
                    crate::Error::InvalidJob(format!("stored job id not a uuid: {e}"))
                })?,
                name,
                sources: serde_json::from_str(&sources_json)?,
                destination: serde_json::from_str(&dest_json)?,
                compression_level: level,
                created_at: DateTime::parse_from_rfc3339(&created)
                    .map_err(|e| {
                        crate::Error::InvalidJob(format!("stored created_at invalid: {e}"))
                    })?
                    .with_timezone(&Utc),
            });
        }
        Ok(jobs)
    }

    /// Deletes a job configuration by id. Run history is kept.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors.
    pub fn delete_job(&self, id: JobId) -> crate::Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM jobs WHERE id = ?", [id.to_string()])?;
        Ok(deleted > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Destination;

    fn sample_job(name: &str) -> JobConfig {
        JobConfig {
            id: Uuid::new_v4(),
            name: name.into(),
            sources: vec![PathBuf::from(r"C:\Users\sample\Documents")],
            destination: Destination::Local {
                root: PathBuf::from(r"E:\Backups\rust-backup"),
            },
            compression_level: 5,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn insert_and_list_job_should_roundtrip_all_fields() {
        let catalog = Catalog::open_in_memory().expect("in-memory catalog");
        let job = sample_job("documents");
        catalog.insert_job(&job).expect("insert job");

        let listed = catalog.list_jobs().expect("list jobs");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, job.id);
        assert_eq!(listed[0].name, "documents");
        assert_eq!(listed[0].sources, job.sources);
        assert_eq!(listed[0].destination, job.destination);
        assert_eq!(listed[0].compression_level, 5);
    }

    #[test]
    fn insert_job_should_reject_empty_name_and_relative_source() {
        let catalog = Catalog::open_in_memory().expect("in-memory catalog");
        let mut job = sample_job("x");
        job.name = "  ".into();
        assert!(catalog.insert_job(&job).is_err());

        let mut job = sample_job("y");
        job.sources = vec![PathBuf::from("relative/path")];
        assert!(catalog.insert_job(&job).is_err());
    }

    #[test]
    fn delete_job_should_remove_it_and_report_missing() {
        let catalog = Catalog::open_in_memory().expect("in-memory catalog");
        let job = sample_job("temp");
        catalog.insert_job(&job).expect("insert job");
        assert!(catalog.delete_job(job.id).expect("delete job"));
        assert!(!catalog.delete_job(job.id).expect("delete again"));
        assert!(catalog.list_jobs().expect("list").is_empty());
    }

    #[test]
    fn open_file_catalog_should_migrate_idempotently() {
        let dir = std::env::temp_dir().join(format!("rb-catalog-test-{}", Uuid::new_v4()));
        let path = dir.join("nested/catalog.duckdb");
        {
            let catalog = Catalog::open(&path).expect("open with dirs");
            catalog.insert_job(&sample_job("first")).expect("insert");
        }
        // Reopening applies no duplicate migrations and keeps the data.
        let catalog = Catalog::open(&path).expect("reopen");
        assert_eq!(catalog.list_jobs().expect("list").len(), 1);
        let applied: i64 = catalog
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .expect("count migrations");
        assert_eq!(applied, MIGRATIONS.len() as i64);
        std::fs::remove_dir_all(&dir).ok();
    }
}

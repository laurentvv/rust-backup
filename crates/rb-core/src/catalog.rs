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
const MIGRATIONS: &[(&str, &str)] = &[
    (
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
    ),
    (
        "0002_snapshots_and_verify",
        r#"
    CREATE TABLE IF NOT EXISTS snapshots (
        id           VARCHAR PRIMARY KEY,
        job_id       VARCHAR NOT NULL,
        created_at   VARCHAR NOT NULL,
        files_total  UBIGINT NOT NULL,
        bytes_total  UBIGINT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS verify_results (
        id              VARCHAR PRIMARY KEY,
        repo            VARCHAR NOT NULL,
        mode            VARCHAR NOT NULL,
        ok              BOOLEAN NOT NULL,
        checked_chunks  UBIGINT NOT NULL,
        errors_json     VARCHAR NOT NULL,
        finished_at     VARCHAR NOT NULL
    );
    "#,
    ),
    (
        "0003_job_excludes",
        r#"
    ALTER TABLE jobs ADD COLUMN IF NOT EXISTS excludes_json VARCHAR DEFAULT '[]';
    "#,
    ),
];

/// Cached snapshot row for fast browsing (local catalog only; the
/// repository remains the source of truth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSummary {
    /// Snapshot id.
    pub id: crate::domain::SnapshotId,
    /// When the snapshot was created (UTC).
    pub created_at: DateTime<Utc>,
    /// Files captured.
    pub files_total: u64,
    /// Bytes captured.
    pub bytes_total: u64,
}

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
        let excludes = serde_json::to_string(&job.excludes)?;
        self.conn.execute(
            "INSERT INTO jobs (id, name, sources_json, destination_json, compression_level, created_at, excludes_json)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            duckdb::params![
                job.id.to_string(),
                job.name,
                sources,
                destination,
                job.compression_level,
                job.created_at.to_rfc3339(),
                excludes,
            ],
        )?;
        Ok(())
    }

    /// Updates an existing job configuration.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidJob`] on validation failure, [`Error::Catalog`] on
    /// database errors.
    pub fn update_job(&self, job: &JobConfig) -> crate::Result<bool> {
        job.validate()?;
        let sources = serde_json::to_string(&job.sources)?;
        let destination = serde_json::to_string(&job.destination)?;
        let excludes = serde_json::to_string(&job.excludes)?;
        let updated = self.conn.execute(
            "UPDATE jobs SET name = ?, sources_json = ?, destination_json = ?, compression_level = ?, excludes_json = ?
             WHERE id = ?",
            duckdb::params![
                job.name,
                sources,
                destination,
                job.compression_level,
                excludes,
                job.id.to_string(),
            ],
        )?;
        Ok(updated > 0)
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
            .prepare("SELECT id, name, sources_json, destination_json, compression_level, created_at, COALESCE(excludes_json, '[]') FROM jobs ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i32>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut jobs = Vec::new();
        for row in rows {
            let (id, name, sources_json, dest_json, level, created, excludes_json) = row?;
            jobs.push(JobConfig {
                id: Uuid::parse_str(&id).map_err(|e| {
                    crate::Error::InvalidJob(format!("stored job id not a uuid: {e}"))
                })?,
                name,
                sources: serde_json::from_str(&sources_json)?,
                destination: serde_json::from_str(&dest_json)?,
                compression_level: level,
                excludes: serde_json::from_str(&excludes_json).unwrap_or_default(),
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

    /// Records the outcome of a backup run.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors.
    pub fn insert_run(&self, run: &crate::domain::RunRecord) -> crate::Result<()> {
        self.conn.execute(
            "INSERT INTO runs (id, job_id, started_at, finished_at, status, files_total, bytes_total, chunks_new, bytes_new, error)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            duckdb::params![
                run.id.to_string(),
                run.job_id.to_string(),
                run.started_at.to_rfc3339(),
                run.finished_at.to_rfc3339(),
                run.status.as_str(),
                run.files_total as i64,
                run.bytes_total as i64,
                run.chunks_new as i64,
                run.bytes_new as i64,
                run.error,
            ],
        )?;
        Ok(())
    }

    /// Lists runs of a job, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors and
    /// [`Error::InvalidJob`] when a stored row cannot be decoded.
    pub fn list_runs(&self, job_id: JobId) -> crate::Result<Vec<crate::domain::RunRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job_id, started_at, finished_at, status, files_total, bytes_total, chunks_new, bytes_new, error
             FROM runs WHERE job_id = ? ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([job_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (id, job, started, finished, status, files, bytes, chunks, new_bytes, error) = row?;
            let parse_ts = |v: &str| -> crate::Result<DateTime<Utc>> {
                DateTime::parse_from_rfc3339(v)
                    .map(|t| t.with_timezone(&Utc))
                    .map_err(|e| crate::Error::InvalidJob(format!("stored timestamp invalid: {e}")))
            };
            runs.push(crate::domain::RunRecord {
                id: Uuid::parse_str(&id)
                    .map_err(|e| crate::Error::InvalidJob(format!("stored run id invalid: {e}")))?,
                job_id: Uuid::parse_str(&job)
                    .map_err(|e| crate::Error::InvalidJob(format!("stored job id invalid: {e}")))?,
                started_at: parse_ts(&started)?,
                finished_at: parse_ts(&finished)?,
                status: crate::domain::RunStatus::parse(&status)?,
                files_total: files.unsigned_abs(),
                bytes_total: bytes.unsigned_abs(),
                chunks_new: chunks.unsigned_abs(),
                bytes_new: new_bytes.unsigned_abs(),
                error,
            });
        }
        Ok(runs)
    }

    /// Records a finalized snapshot (cache for fast browsing).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors.
    pub fn insert_snapshot(
        &self,
        snapshot_id: crate::domain::SnapshotId,
        job_id: JobId,
        created_at: DateTime<Utc>,
        files_total: u64,
        bytes_total: u64,
    ) -> crate::Result<()> {
        self.conn.execute(
            "INSERT INTO snapshots (id, job_id, created_at, files_total, bytes_total) VALUES (?, ?, ?, ?, ?)",
            duckdb::params![
                snapshot_id.to_string(),
                job_id.to_string(),
                created_at.to_rfc3339(),
                files_total as i64,
                bytes_total as i64,
            ],
        )?;
        Ok(())
    }

    /// Lists cached snapshots of a job, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors.
    pub fn list_snapshots(&self, job_id: JobId) -> crate::Result<Vec<SnapshotSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, files_total, bytes_total FROM snapshots
             WHERE job_id = ? ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([job_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, created, files, bytes) = row?;
            let created_at = DateTime::parse_from_rfc3339(&created)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|e| crate::Error::InvalidJob(format!("stored timestamp invalid: {e}")))?;
            let id = Uuid::parse_str(&id).map_err(|e| {
                crate::Error::InvalidJob(format!("stored snapshot id invalid: {e}"))
            })?;
            out.push(SnapshotSummary {
                id,
                created_at,
                files_total: files.unsigned_abs(),
                bytes_total: bytes.unsigned_abs(),
            });
        }
        Ok(out)
    }

    /// Records a verification result.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors and
    /// [`Error::Json`] when the issues list cannot be serialized.
    pub fn insert_verify_result(
        &self,
        repo: &str,
        report: &crate::verify::VerifyReport,
    ) -> crate::Result<()> {
        let issues: Vec<String> = report.issues.iter().map(|i| i.message.clone()).collect();
        self.conn.execute(
            "INSERT INTO verify_results (id, repo, mode, ok, checked_chunks, errors_json, finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            duckdb::params![
                Uuid::new_v4().to_string(),
                repo,
                report.mode.as_str(),
                report.ok(),
                report.chunks_checked as i64,
                serde_json::to_string(&issues)?,
                report.finished_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Latest verification results (newest first), with their issue lists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] on database errors.
    pub fn list_verify_results(
        &self,
        limit: u64,
    ) -> crate::Result<Vec<crate::verify::VerifyOutcome>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo, mode, ok, checked_chunks, errors_json, finished_at
             FROM verify_results ORDER BY finished_at DESC LIMIT ?",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (repo, mode, ok, chunks, errors_json, finished) = row?;
            let issues: Vec<String> =
                serde_json::from_str(&errors_json).map_err(crate::Error::Json)?;
            let mode = match mode.as_str() {
                "quick" => crate::verify::VerifyMode::Quick,
                _ => crate::verify::VerifyMode::Full,
            };
            let finished_at = DateTime::parse_from_rfc3339(&finished)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|e| crate::Error::InvalidJob(format!("stored timestamp invalid: {e}")))?;
            out.push(crate::verify::VerifyOutcome {
                repo,
                mode,
                ok,
                chunks_checked: chunks.unsigned_abs(),
                issues,
                finished_at,
            });
        }
        Ok(out)
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
            excludes: vec!["target".into(), "*.tmp".into()],
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
        assert_eq!(listed[0].excludes, vec!["target", "*.tmp"]);
    }

    #[test]
    fn update_job_should_modify_fields_and_persist() {
        let catalog = Catalog::open_in_memory().expect("in-memory catalog");
        let mut job = sample_job("photos");
        catalog.insert_job(&job).expect("insert job");

        job.name = "photos-edited".into();
        job.compression_level = 9;
        job.excludes = vec!["*.raw".into()];
        assert!(catalog.update_job(&job).expect("update job"));

        let listed = catalog.list_jobs().expect("list jobs");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "photos-edited");
        assert_eq!(listed[0].compression_level, 9);
        assert_eq!(listed[0].excludes, vec!["*.raw".to_string()]);
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

    #[test]
    fn insert_and_list_runs_should_roundtrip_newest_first() {
        use crate::domain::{RunRecord, RunStatus};
        let catalog = Catalog::open_in_memory().expect("catalog");
        let job = sample_job("runner");
        catalog.insert_job(&job).expect("insert job");

        let base = Utc::now();
        for (i, status) in [RunStatus::Success, RunStatus::Failed, RunStatus::Success]
            .into_iter()
            .enumerate()
        {
            let record = RunRecord {
                id: Uuid::new_v4(),
                job_id: job.id,
                started_at: base - chrono::Duration::minutes((10 - i as i64) * 10),
                finished_at: base - chrono::Duration::minutes((9 - i as i64) * 10),
                status,
                files_total: 10 + i as u64,
                bytes_total: 1000 * (i as u64 + 1),
                chunks_new: 3 + i as u64,
                bytes_new: 300 * (i as u64 + 1),
                error: (status == RunStatus::Failed).then(|| "boom".into()),
            };
            catalog.insert_run(&record).expect("insert run");
        }

        let runs = catalog.list_runs(job.id).expect("list runs");
        assert_eq!(runs.len(), 3);
        assert!(runs[0].started_at > runs[1].started_at, "newest first");
        assert_eq!(runs[0].files_total, 12);
        assert_eq!(
            runs.iter()
                .filter(|r| r.status == RunStatus::Failed)
                .count(),
            1
        );
        assert_eq!(runs[1].error.as_deref(), Some("boom"));
    }

    #[test]
    fn snapshots_and_verify_results_should_roundtrip() {
        let catalog = Catalog::open_in_memory().expect("catalog");
        let job = sample_job("tracked");
        catalog.insert_job(&job).expect("insert job");

        catalog
            .insert_snapshot(Uuid::new_v4(), job.id, Utc::now(), 12, 34_000)
            .expect("snapshot 1");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = Uuid::new_v4();
        catalog
            .insert_snapshot(second, job.id, Utc::now(), 15, 40_000)
            .expect("snapshot 2");

        let snapshots = catalog.list_snapshots(job.id).expect("list snapshots");
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].id, second, "newest first");
        assert_eq!(snapshots[0].files_total, 15);
        assert_eq!(snapshots[1].bytes_total, 34_000);

        let report = {
            let mut r = crate::verify::VerifyReport {
                mode: crate::verify::VerifyMode::Full,
                started_at: Utc::now(),
                finished_at: Utc::now(),
                manifests_checked: 2,
                chunks_checked: 9,
                orphan_chunks: 0,
                issues: vec![crate::verify::VerifyIssue {
                    message: "chunk abc is MISSING".into(),
                }],
            };
            r.finished_at = Utc::now();
            r
        };
        catalog
            .insert_verify_result("local: E:\\bk", &report)
            .expect("insert verify");

        let outcomes = catalog.list_verify_results(10).expect("list verify");
        assert_eq!(outcomes.len(), 1);
        assert!(!outcomes[0].ok);
        assert_eq!(outcomes[0].mode, crate::verify::VerifyMode::Full);
        assert_eq!(outcomes[0].chunks_checked, 9);
        assert_eq!(outcomes[0].issues, vec!["chunk abc is MISSING".to_string()]);
    }
}

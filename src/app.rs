//! Main application view: sidebar navigation, dashboard, jobs, live run,
//! history and settings.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gpui::prelude::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, TitleBar, h_flex, v_flex};
use gpui_kit::*;
use uuid::Uuid;

use rb_core::backup::BackupEvent;
use rb_core::catalog::Catalog;
use rb_core::domain::{Destination, JobConfig, JobId, RunRecord, RunStatus};

use crate::i18n::{self, Lang, human_bytes_lang};
use crate::runner::{self, EngineEvent};
use crate::theme;

/// Active navigation section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Dashboard.
    Dashboard,
    /// Jobs list + creation form.
    Jobs,
    /// Live run view.
    Run,
    /// Restore browser.
    Restore,
    /// Run history.
    History,
    /// Settings.
    Settings,
}

/// Phase of the current (or last) run.
#[derive(Debug, Clone)]
pub enum RunPhase {
    /// Nothing running, nothing recent.
    Idle,
    /// A run is in flight.
    Running {
        /// Job name.
        job: String,
        /// When the run started.
        started: Instant,
    },
    /// Last run succeeded.
    Done {
        /// Snapshot id (short form).
        snapshot: String,
        /// Seconds elapsed.
        seconds: u64,
    },
    /// Last run failed.
    Failed {
        /// User-facing error.
        error: String,
    },
    /// Run was cancelled by the user.
    Cancelled,
}

/// The root application view.
pub struct RustBackupApp {
    section: Section,
    lang: Lang,
    jobs: Vec<JobConfig>,
    recent_runs: Vec<(String, RunRecord)>,
    passphrase: String,

    form_name: gpui::Entity<InputState>,
    form_source: gpui::Entity<InputState>,
    form_dest: gpui::Entity<InputState>,
    form_is_sftp: bool,
    form_sftp_host: gpui::Entity<InputState>,
    form_sftp_port: gpui::Entity<InputState>,
    form_sftp_user: gpui::Entity<InputState>,
    form_sftp_key: gpui::Entity<InputState>,
    form_sftp_root: gpui::Entity<InputState>,
    form_excludes: gpui::Entity<InputState>,
    form_compression: i32,
    editing_job_id: Option<JobId>,
    form_passphrase: gpui::Entity<InputState>,

    phase: RunPhase,
    run_files_done: u64,
    run_bytes_done: u64,
    run_chunks_new: u64,
    run_bytes_new: u64,
    run_skipped: u64,
    run_current_file: String,
    cancel: Option<Arc<AtomicBool>>,

    toast: Option<(bool, String)>, // (is_error, message)

    // --- restore browser state ---
    snapshots: Vec<runner::SnapshotRow>,
    selected_snapshot: Option<Uuid>,
    selected_restore_job: Option<JobId>,
    restore_search: gpui::Entity<InputState>,
    restore_target: gpui::Entity<InputState>,
    selected_paths: std::collections::BTreeSet<String>,
    restore_to_original: bool,
    restore_overwrite: bool,
    restore_busy: bool,
    restore_result: Option<(bool, String)>,
    dark: bool,
    run_est_files: u64,

    // --- cached statistics for fast render ---
    cached_snapshot_count: usize,
    cached_protected_volume: u64,
}

impl RustBackupApp {
    /// Builds the view, loading the local catalog.
    pub fn new(window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut app = Self {
            section: Section::Dashboard,
            lang: Lang::default(),
            jobs: Vec::new(),
            recent_runs: Vec::new(),
            passphrase: String::new(),
            form_name: cx.new(|cx| InputState::new(window, cx).placeholder("documents")),
            form_source: cx
                .new(|cx| InputState::new(window, cx).placeholder(r"C:\Users\moi\Documents")),
            form_dest: cx
                .new(|cx| InputState::new(window, cx).placeholder(r"E:\Sauvegardes\depot")),
            form_is_sftp: false,
            form_sftp_host: cx
                .new(|cx| InputState::new(window, cx).placeholder("backup.example.com")),
            form_sftp_port: cx.new(|cx| InputState::new(window, cx).placeholder("22")),
            form_sftp_user: cx.new(|cx| InputState::new(window, cx).placeholder("backup")),
            form_sftp_key: cx
                .new(|cx| InputState::new(window, cx).placeholder(r"C:\Users\moi\.ssh\id_ed25519")),
            form_sftp_root: cx
                .new(|cx| InputState::new(window, cx).placeholder("/var/backups/rust-backup")),
            form_excludes: cx.new(|cx| {
                InputState::new(window, cx).placeholder("target, node_modules, .git, *.tmp")
            }),
            form_compression: 5,
            editing_job_id: None,
            form_passphrase: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("••••••••••")
                    .masked(true)
            }),
            phase: RunPhase::Idle,
            run_files_done: 0,
            run_bytes_done: 0,
            run_chunks_new: 0,
            run_bytes_new: 0,
            run_skipped: 0,
            run_current_file: String::new(),
            cancel: None,
            toast: None,
            snapshots: Vec::new(),
            selected_snapshot: None,
            selected_restore_job: None,
            restore_search: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::i18n::Lang::default().texts().search_files)
            }),
            restore_target: cx
                .new(|cx| InputState::new(window, cx).placeholder(r"D:\Restauration")),
            selected_paths: std::collections::BTreeSet::new(),
            restore_to_original: false,
            restore_overwrite: false,
            restore_busy: false,
            restore_result: None,
            dark: false,
            run_est_files: 0,
            cached_snapshot_count: 0,
            cached_protected_volume: 0,
        };
        // Palette applied once at startup; runtime switching happens in
        // Settings via theme::apply_brand_theme.
        app.refresh_catalog();
        app
    }

    fn texts(&self) -> &'static i18n::Texts {
        self.lang.texts()
    }

    fn catalog_path(&self) -> Option<std::path::PathBuf> {
        Catalog::default_path().ok()
    }

    /// Reloads jobs, recent runs and aggregate stats from the local catalog.
    fn refresh_catalog(&mut self) {
        let Some(path) = self.catalog_path() else {
            return;
        };
        let Ok(catalog) = Catalog::open(&path) else {
            return;
        };
        let Ok(jobs) = catalog.list_jobs() else {
            return;
        };
        self.jobs = jobs;

        if self.selected_restore_job.is_none()
            || !self
                .jobs
                .iter()
                .any(|j| Some(j.id) == self.selected_restore_job)
        {
            self.selected_restore_job = self.jobs.first().map(|j| j.id);
        }

        let mut runs: Vec<(String, RunRecord)> = Vec::new();
        let mut total_snapshots = 0;
        let mut protected_vol = 0;
        for job in &self.jobs {
            if let Ok(job_runs) = catalog.list_runs(job.id) {
                for record in job_runs.into_iter().take(10) {
                    runs.push((job.name.clone(), record));
                }
            }
            if let Ok(snapshots) = catalog.list_snapshots(job.id) {
                total_snapshots += snapshots.len();
                if let Some(first) = snapshots.first() {
                    protected_vol += first.bytes_total;
                }
            }
        }
        runs.sort_by_key(|(_, run)| std::cmp::Reverse(run.started_at));
        self.recent_runs = runs.into_iter().take(30).collect();
        self.cached_snapshot_count = total_snapshots;
        self.cached_protected_volume = protected_vol;
    }

    fn start_backup(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
        job_id: JobId,
    ) {
        let t = self.texts();
        if let RunPhase::Running { .. } = self.phase {
            self.toast = Some((true, format!("{} ({})", t.running, t.run_title)));
            cx.notify();
            return;
        }
        let Some(job) = self.jobs.iter().find(|job| job.id == job_id).cloned() else {
            return;
        };
        let passphrase = if self.passphrase.is_empty() {
            self.form_passphrase.read(cx).value().to_string()
        } else {
            self.passphrase.clone()
        };
        if passphrase.is_empty() {
            self.toast = Some((true, format!("{} — {}", t.passphrase, t.settings)));
            self.section = Section::Settings;
            cx.notify();
            return;
        }

        let (rx, cancel) = runner::spawn_backup(job, passphrase);
        self.phase = RunPhase::Running {
            job: self
                .jobs
                .iter()
                .find(|j| j.id == job_id)
                .map(|j| j.name.clone())
                .unwrap_or_default(),
            started: Instant::now(),
        };
        self.run_files_done = 0;
        self.run_bytes_done = 0;
        self.run_chunks_new = 0;
        self.run_bytes_new = 0;
        self.run_skipped = 0;
        self.run_current_file.clear();
        // Estimated total from the latest cached snapshot (0 = unknown).
        self.run_est_files = self
            .catalog_path()
            .and_then(|path| Catalog::open(&path).ok())
            .and_then(|catalog| catalog.list_snapshots(job_id).ok())
            .and_then(|snapshots| snapshots.first().map(|s| s.files_total))
            .unwrap_or(0);
        self.cancel = Some(cancel);
        self.section = Section::Run;
        self.toast = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv_async().await {
                let Ok(()) = this.update(cx, |app, cx| app.on_engine_event(event, cx)) else {
                    break;
                };
            }
        })
        .detach();
    }

    fn on_engine_event(&mut self, event: EngineEvent, cx: &mut gpui::Context<Self>) {
        match event {
            EngineEvent::Progress(BackupEvent::FileStarted { path, .. }) => {
                self.run_current_file = path.to_string_lossy().to_string();
            }
            EngineEvent::Progress(BackupEvent::FileDone {
                size,
                new_chunks,
                new_bytes,
                ..
            }) => {
                self.run_files_done += 1;
                self.run_bytes_done += size;
                self.run_chunks_new += new_chunks;
                self.run_bytes_new += new_bytes;
            }
            EngineEvent::Progress(BackupEvent::FileSkipped { .. }) => {
                self.run_skipped += 1;
            }
            EngineEvent::Progress(BackupEvent::SnapshotFinalized { snapshot_id }) => {
                self.phase = RunPhase::Done {
                    snapshot: snapshot_id.simple().to_string()[..12].to_string(),
                    seconds: self.elapsed_seconds(),
                };
            }
            EngineEvent::Finished(Ok(summary)) => {
                self.cancel = None;
                let _snapshot = summary.snapshot_id.simple().to_string();
                self.run_files_done = summary.stats.files_total;
                self.run_bytes_done = summary.stats.bytes_total;
                self.run_chunks_new = summary.stats.chunks_new;
                self.run_bytes_new = summary.stats.bytes_new;
                self.refresh_catalog();
            }
            EngineEvent::Finished(Err(message)) => {
                self.cancel = None;
                if message.to_lowercase().contains("cancel") {
                    self.phase = RunPhase::Cancelled;
                    self.toast = Some((false, self.texts().cancelled.to_string()));
                } else {
                    self.phase = RunPhase::Failed {
                        error: message.clone(),
                    };
                    self.toast = Some((true, message));
                }
            }
        }
        cx.notify();
    }

    fn passphrase_or_form(&self, cx: &gpui::Context<Self>) -> String {
        if self.passphrase.is_empty() {
            self.form_passphrase.read(cx).value().to_string()
        } else {
            self.passphrase.clone()
        }
    }

    fn load_snapshots(&mut self, cx: &mut gpui::Context<Self>) {
        let t = self.texts();
        let job = self
            .selected_restore_job
            .and_then(|id| self.jobs.iter().find(|j| j.id == id).cloned())
            .or_else(|| self.jobs.first().cloned());
        let Some(job) = job else {
            return;
        };
        let passphrase = self.passphrase_or_form(cx);
        if passphrase.is_empty() {
            self.restore_result = Some((true, t.passphrase_needed.to_string()));
            cx.notify();
            return;
        }
        self.restore_busy = true;
        cx.notify();
        let rx = runner::spawn_snapshots(job, passphrase);
        cx.spawn(async move |this, cx| {
            if let Ok(rows) = rx.recv_async().await {
                let _ = this.update(cx, |app, cx| {
                    app.restore_busy = false;
                    match rows {
                        Ok(mut rows) => {
                            rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                            app.selected_snapshot = rows.first().map(|r| r.id);
                            app.selected_paths.clear();
                            app.snapshots = rows;
                        }
                        Err(message) => app.restore_result = Some((true, message)),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn run_restore(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let t = self.texts();
        if self.restore_busy {
            return;
        }
        let job = self
            .selected_restore_job
            .and_then(|id| self.jobs.iter().find(|j| j.id == id).cloned())
            .or_else(|| self.jobs.first().cloned());
        let Some(job) = job else {
            return;
        };
        let passphrase = self.passphrase_or_form(cx);
        if passphrase.is_empty() {
            self.restore_result = Some((true, t.passphrase_needed.to_string()));
            cx.notify();
            return;
        }
        let Some(snapshot) = self
            .snapshots
            .iter()
            .find(|s| Some(s.id) == self.selected_snapshot)
        else {
            return;
        };
        if self.selected_paths.is_empty() {
            return;
        }
        let roots: std::collections::BTreeSet<String> = snapshot
            .entries
            .iter()
            .map(|(root, _, _)| root.clone())
            .collect();
        let selection: Vec<rb_core::restore::RestoreSelection> = self
            .selected_paths
            .iter()
            .flat_map(|rel| {
                roots
                    .iter()
                    .map(move |root| rb_core::restore::RestoreSelection {
                        source_root: root.clone().into(),
                        rel_path: rel.clone().into(),
                    })
            })
            .collect();
        let destination = if self.restore_to_original {
            rb_core::restore::RestoreDestination::Original
        } else {
            let target = self.restore_target.read(cx).value().to_string();
            rb_core::restore::RestoreDestination::Target(target.into())
        };
        let overwrite = if self.restore_overwrite {
            rb_core::restore::OverwritePolicy::Overwrite
        } else {
            rb_core::restore::OverwritePolicy::Skip
        };

        self.restore_busy = true;
        self.restore_result = None;
        cx.notify();
        let rx = runner::spawn_restore(
            job,
            passphrase,
            snapshot.id,
            selection,
            destination,
            overwrite,
        );
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv_async().await {
                let Ok(()) = this.update(cx, |app, _cx| match event {
                    runner::RestoreEvent::Progress(rb_core::restore::RestoreEvent::FileDone {
                        ..
                    }) => {}
                    runner::RestoreEvent::Finished(Ok(stats)) => {
                        app.restore_busy = false;
                        app.restore_result = Some((
                            false,
                            format!(
                                "{} ✓ {} {}, {} {}, {} {}",
                                app.texts().restored,
                                stats.files_restored,
                                app.texts().files,
                                stats.files_skipped,
                                app.texts().skipped,
                                stats.dirs_created,
                                app.texts().directories
                            ),
                        ));
                    }
                    runner::RestoreEvent::Finished(Err(message)) => {
                        app.restore_busy = false;
                        app.restore_result =
                            Some((true, format!("{} — {message}", app.texts().restore_failed)));
                    }
                    _ => {}
                }) else {
                    break;
                };
                let _ = this.update(cx, |_, cx| cx.notify());
            }
        })
        .detach();
    }

    fn run_verify(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
        job_id: JobId,
        full: bool,
    ) {
        let t = self.texts();
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id).cloned() else {
            return;
        };
        let passphrase = self.passphrase_or_form(cx);
        if passphrase.is_empty() {
            self.toast = Some((true, t.passphrase_needed.to_string()));
            cx.notify();
            return;
        }
        self.toast = Some((false, format!("{} …", t.verify)));
        cx.notify();
        let rx = runner::spawn_verify(job, passphrase, full);
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv_async().await {
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok(report) => {
                            if report.ok() {
                                let msg = match app.lang {
                                    Lang::Fr => format!(
                                        "Intégrité OK : {} instantanés, {} fragments vérifiés",
                                        report.manifests_checked, report.chunks_checked
                                    ),
                                    Lang::En => format!(
                                        "Integrity OK: {} snapshots, {} chunks verified",
                                        report.manifests_checked, report.chunks_checked
                                    ),
                                };
                                app.toast = Some((false, msg));
                            } else {
                                let issues_msg = report
                                    .issues
                                    .iter()
                                    .map(|i| i.message.as_str())
                                    .collect::<Vec<_>>()
                                    .join("; ");
                                let msg = match app.lang {
                                    Lang::Fr => format!(
                                        "{} problème(s) détecté(s) : {}",
                                        report.issues.len(),
                                        issues_msg
                                    ),
                                    Lang::En => format!(
                                        "{} issue(s) detected: {}",
                                        report.issues.len(),
                                        issues_msg
                                    ),
                                };
                                app.toast = Some((true, msg));
                            }
                        }
                        Err(message) => app.toast = Some((true, message)),
                    }
                    app.auto_dismiss_toast(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn elapsed_seconds(&self) -> u64 {
        match &self.phase {
            RunPhase::Running { started, .. } => started.elapsed().as_secs(),
            RunPhase::Done { seconds, .. } => *seconds,
            _ => 0,
        }
    }

    fn browse_path(
        &self,
        target_state: gpui::Entity<InputState>,
        directories: bool,
        files: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files,
            directories,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.first()
            {
                let path_str = path.display().to_string();
                let _ = this.update_in(cx, |_, window, cx| {
                    target_state.update(cx, |input, cx| {
                        input.set_value(path_str, window, cx);
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn create_or_update_job(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let name = self.form_name.read(cx).value().to_string();
        let source_str = self.form_source.read(cx).value().to_string();
        let sources: Vec<std::path::PathBuf> = source_str
            .split(&[';', ','][..])
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .collect();

        let raw_excludes = self.form_excludes.read(cx).value().to_string();
        let excludes: Vec<String> = raw_excludes
            .split(&[',', ';'][..])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let destination = if self.form_is_sftp {
            let host = self.form_sftp_host.read(cx).value().to_string();
            let port = self
                .form_sftp_port
                .read(cx)
                .value()
                .parse::<u16>()
                .unwrap_or(22);
            let username = self.form_sftp_user.read(cx).value().to_string();
            let key_path =
                std::path::PathBuf::from(self.form_sftp_key.read(cx).value().to_string());
            let remote_root = self.form_sftp_root.read(cx).value().to_string();
            Destination::Sftp {
                host,
                port,
                username,
                key_path,
                remote_root,
            }
        } else {
            let dest = self.form_dest.read(cx).value().to_string();
            Destination::Local {
                root: std::path::PathBuf::from(dest),
            }
        };

        let is_editing = self.editing_job_id.is_some();
        let job_id = self.editing_job_id.unwrap_or_else(Uuid::new_v4);
        let job = JobConfig {
            id: job_id,
            name: name.clone(),
            sources,
            destination,
            compression_level: self.form_compression,
            excludes,
            created_at: chrono::Utc::now(),
        };

        if let Err(e) = job.validate() {
            self.toast = Some((true, e.to_string()));
            cx.notify();
            return;
        }

        let Some(path) = self.catalog_path() else {
            return;
        };

        let Ok(catalog) = Catalog::open(&path) else {
            self.toast = Some((true, "Failed to open catalog".into()));
            cx.notify();
            return;
        };

        if is_editing {
            match catalog.update_job(&job) {
                Ok(_) => {
                    if let Some(existing) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                        *existing = job.clone();
                    }
                    self.editing_job_id = None;
                    self.toast = Some((
                        false,
                        format!("{} ✓ {}", self.texts().job_updated, job.name),
                    ));
                    self.reset_job_form(window, cx);
                }
                Err(e) => self.toast = Some((true, e.to_string())),
            }
        } else {
            match catalog.insert_job(&job) {
                Ok(_) => {
                    self.jobs.push(job.clone());
                    self.jobs.sort_by(|a, b| a.name.cmp(&b.name));
                    self.toast = Some((false, format!("{} ✓ {}", self.texts().create, job.name)));
                    self.reset_job_form(window, cx);
                }
                Err(e) => self.toast = Some((true, e.to_string())),
            }
        }
        self.auto_dismiss_toast(cx);
        cx.notify();
    }

    fn reset_job_form(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        self.editing_job_id = None;
        self.form_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_source
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_dest
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_sftp_host
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_sftp_port
            .update(cx, |input, cx| input.set_value("22", window, cx));
        self.form_sftp_user
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_sftp_key
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_sftp_root
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_excludes
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.form_compression = 5;
    }

    fn start_edit_job(
        &mut self,
        job_id: JobId,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id).cloned() else {
            return;
        };
        self.editing_job_id = Some(job_id);
        self.form_name
            .update(cx, |input, cx| input.set_value(&job.name, window, cx));
        let sources_str = job
            .sources
            .iter()
            .map(|s| s.display().to_string())
            .collect::<Vec<_>>()
            .join("; ");
        self.form_source
            .update(cx, |input, cx| input.set_value(sources_str, window, cx));
        self.form_compression = job.compression_level;
        self.form_excludes.update(cx, |input, cx| {
            input.set_value(job.excludes.join(", "), window, cx);
        });

        match &job.destination {
            Destination::Local { root } => {
                self.form_is_sftp = false;
                self.form_dest.update(cx, |input, cx| {
                    input.set_value(root.display().to_string(), window, cx);
                });
            }
            Destination::Sftp {
                host,
                port,
                username,
                key_path,
                remote_root,
            } => {
                self.form_is_sftp = true;
                self.form_sftp_host
                    .update(cx, |input, cx| input.set_value(host, window, cx));
                self.form_sftp_port.update(cx, |input, cx| {
                    input.set_value(port.to_string(), window, cx)
                });
                self.form_sftp_user
                    .update(cx, |input, cx| input.set_value(username, window, cx));
                self.form_sftp_key.update(cx, |input, cx| {
                    input.set_value(key_path.display().to_string(), window, cx);
                });
                self.form_sftp_root
                    .update(cx, |input, cx| input.set_value(remote_root, window, cx));
            }
        }
        cx.notify();
    }

    fn init_job_repo(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
        job_id: JobId,
    ) {
        let t = self.texts();
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id).cloned() else {
            return;
        };
        let passphrase = self.passphrase_or_form(cx);
        if passphrase.is_empty() {
            self.toast = Some((true, t.passphrase_needed.to_string()));
            cx.notify();
            return;
        }
        self.toast = Some((false, format!("{} …", t.init_repo)));
        cx.notify();
        let rx = runner::spawn_init_repo(job, passphrase);
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv_async().await {
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok(msg) => {
                            app.toast =
                                Some((false, format!("{} ✓ {msg}", app.texts().repo_initialized)));
                        }
                        Err(err) => app.toast = Some((true, err)),
                    }
                    app.auto_dismiss_toast(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Clears the current toast after a few seconds.
    fn auto_dismiss_toast(&self, cx: &mut gpui::Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(4))
                .await;
            let _ = this.update(cx, |app, cx| {
                app.toast = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_job(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
        job_id: JobId,
    ) {
        if let Some(path) = self.catalog_path()
            && let Ok(catalog) = Catalog::open(&path)
            && catalog.delete_job(job_id).unwrap_or(false)
        {
            self.jobs.retain(|job| job.id != job_id);
            self.recent_runs.retain(|(_, run)| run.job_id != job_id);
        }
        cx.notify();
    }

    fn cancel_run(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if let Some(flag) = &self.cancel {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn stat_card(
        &self,
        title: &str,
        value: &str,
        icon: IconName,
        accent: gpui::Hsla,
        cx: &gpui::Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .min_w(px(180.0))
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .id(gpui::SharedString::new(format!("chip-{title}")))
                            .p_2()
                            .rounded_md()
                            .bg(accent.opacity(0.16))
                            .border_1()
                            .border_color(accent.opacity(0.35))
                            .text_color(accent)
                            .child(Icon::new(icon).size_5()),
                    )
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(title.to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(22.0))
                                    .text_color(cx.theme().foreground)
                                    .child(value.to_string()),
                            ),
                    ),
            )
    }

    /// Horizontal bar chart of the most recent runs (protected volume per
    /// run, green = success, red = failed).
    fn recent_runs_chart(&self, cx: &gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let runs: Vec<&(String, RunRecord)> = self.recent_runs.iter().take(8).collect();
        let max_bytes = runs
            .iter()
            .map(|(_, r)| r.bytes_total)
            .max()
            .unwrap_or(1)
            .max(1);
        let mut list = v_flex().gap_1();
        for (job, run) in runs {
            let frac = (run.bytes_total as f64 / max_bytes as f64).clamp(0.02, 1.0);
            let color = if run.status == RunStatus::Success {
                cx.theme().success
            } else {
                cx.theme().danger
            };
            list = list.child(
                h_flex()
                    .id(gpui::SharedString::new(run.id.to_string()))
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .w(px(150.0))
                            .text_size(px(11.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(job.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(14.0))
                            .rounded_full()
                            .bg(cx.theme().selection.opacity(0.5))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(frac as f32))
                                    .rounded_full()
                                    .bg(color.opacity(0.85)),
                            ),
                    )
                    .child(
                        div()
                            .w(px(90.0))
                            .justify_end()
                            .text_size(px(11.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(human_bytes_lang(run.bytes_total, self.lang)),
                    ),
            );
        }
        v_flex()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_color(cx.theme().primary)
                            .child(Icon::new(IconName::ChartColumn).size_4()),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(cx.theme().foreground)
                            .child(t.history),
                    ),
            )
            .child(list)
    }

    /// Small icon + title header for a section.
    fn section_header(
        &self,
        icon: IconName,
        title: &str,
        cx: &gpui::Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .gap_3()
            .child(
                div()
                    .p_2()
                    .rounded_lg()
                    .bg(cx.theme().primary.opacity(0.15))
                    .border_1()
                    .border_color(cx.theme().primary.opacity(0.35))
                    .text_color(cx.theme().primary)
                    .child(Icon::new(icon).size_5()),
            )
            .child(
                div()
                    .text_size(px(24.0))
                    .text_color(cx.theme().foreground)
                    .child(title.to_string()),
            )
    }

    fn render_dashboard(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let last_run_label = self
            .recent_runs
            .first()
            .map(|(_, run)| match run.status {
                RunStatus::Success => t.success.to_string(),
                RunStatus::Failed => t.failed.to_string(),
            })
            .unwrap_or_else(|| t.idle.to_string());
        let protected: u64 = if self.cached_protected_volume > 0 {
            self.cached_protected_volume
        } else {
            self.recent_runs
                .first()
                .filter(|(_, run)| run.status == RunStatus::Success)
                .map(|(_, run)| run.bytes_total)
                .unwrap_or(0)
        };

        v_flex()
            .gap_4()
            .child(
                div()
                    .text_size(px(24.0))
                    .text_color(cx.theme().foreground)
                    .child(t.dashboard),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(t.tagline),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(self.stat_card(
                        t.jobs_count,
                        &self.jobs.len().to_string(),
                        IconName::Database,
                        cx.theme().primary,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.snapshots_count,
                        &self.cached_snapshot_count.to_string(),
                        IconName::Layers,
                        cx.theme().success,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.last_run,
                        &last_run_label,
                        IconName::Clock,
                        cx.theme().warning,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.protected_volume,
                        &human_bytes_lang(protected, self.lang),
                        IconName::ShieldCheck,
                        cx.theme().danger,
                        cx,
                    )),
            )
            .when(!self.recent_runs.is_empty(), |this| {
                this.child(self.recent_runs_chart(cx))
            })
    }

    fn render_jobs(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let rows: Vec<AnyElement> =
            self.jobs
                .iter()
                .map(|job| {
                    let (dest_icon, dest_type_name) = match &job.destination {
                        Destination::Local { .. } => (IconName::HardDrive, t.local),
                        Destination::Sftp { .. } => (IconName::Server, t.sftp),
                    };
                    let id = job.id;
                    let sources_display = job
                        .sources
                        .iter()
                        .map(|s| s.display().to_string())
                        .collect::<Vec<_>>()
                        .join("; ");
                    let excludes_display = if job.excludes.is_empty() {
                        String::new()
                    } else {
                        format!("{} : {}", t.excludes, job.excludes.join(", "))
                    };

                    h_flex()
                        .id(gpui::SharedString::new(job.id.to_string()))
                        .justify_between()
                        .gap_3()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_size(px(14.0))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(cx.theme().foreground)
                                                .child(job.name.clone()),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .items_center()
                                                .px_2()
                                                .py_0p5()
                                                .rounded_md()
                                                .bg(cx.theme().primary.opacity(0.12))
                                                .text_size(px(10.0))
                                                .text_color(cx.theme().primary)
                                                .child(Icon::new(dest_icon).size_3())
                                                .child(dest_type_name),
                                        )
                                        .child(
                                            div()
                                                .px_2()
                                                .py_0p5()
                                                .rounded_md()
                                                .bg(cx.theme().selection.opacity(0.5))
                                                .text_size(px(10.0))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("zstd-{}", job.compression_level)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(job.destination.describe()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} : {}", t.source, sources_display)),
                                )
                                .when(!excludes_display.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(cx.theme().muted_foreground.opacity(0.8))
                                            .child(excludes_display),
                                    )
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Button::new(gpui::SharedString::new(format!(
                                        "run-{}",
                                        job.id.simple()
                                    )))
                                    .primary()
                                    .label(t.run_now)
                                    .icon(IconName::Play)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.start_backup(window, cx, id);
                                    })),
                                )
                                .child(
                                    Button::new(gpui::SharedString::new(format!(
                                        "verify-{}",
                                        job.id.simple()
                                    )))
                                    .ghost()
                                    .icon(IconName::ShieldCheck)
                                    .label(t.verify)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.run_verify(window, cx, id, false);
                                    })),
                                )
                                .child(
                                    Button::new(gpui::SharedString::new(format!(
                                        "edit-{}",
                                        job.id.simple()
                                    )))
                                    .ghost()
                                    .icon(IconName::SlidersHorizontal)
                                    .label(t.edit)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.start_edit_job(id, window, cx);
                                    })),
                                )
                                .child(
                                    Button::new(gpui::SharedString::new(format!(
                                        "init-{}",
                                        job.id.simple()
                                    )))
                                    .ghost()
                                    .icon(IconName::Sparkles)
                                    .label(t.init_repo)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.init_job_repo(window, cx, id);
                                    })),
                                )
                                .child(
                                    Button::new(gpui::SharedString::new(format!(
                                        "del-{}",
                                        job.id.simple()
                                    )))
                                    .ghost()
                                    .icon(IconName::Trash)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.delete_job(window, cx, id);
                                    })),
                                ),
                        )
                        .into_any_element()
                })
                .collect();

        let is_editing = self.editing_job_id.is_some();
        let form_title = if is_editing {
            format!("{} ({})", t.edit, t.jobs)
        } else {
            t.new_job.to_string()
        };

        let form =
            v_flex()
                .gap_3()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(form_title),
                )
                .child(self.labeled_input(t.name, &self.form_name))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_size(px(12.0)).child(t.source.to_string()))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(&self.form_source)))
                                .child(
                                    Button::new("browse-source")
                                        .ghost()
                                        .icon(IconName::FolderOpen)
                                        .label(t.browse)
                                        .on_click(cx.listener({
                                            let state = self.form_source.clone();
                                            move |this, _, window, cx| {
                                                this.browse_path(
                                                    state.clone(),
                                                    true,
                                                    false,
                                                    window,
                                                    cx,
                                                );
                                            }
                                        })),
                                ),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .child(t.destination_type.to_string()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("dest-type-local")
                                        .label(t.local)
                                        .when(!self.form_is_sftp, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.form_is_sftp = false;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("dest-type-sftp")
                                        .label(t.sftp)
                                        .when(self.form_is_sftp, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.form_is_sftp = true;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .when(!self.form_is_sftp, |this| {
                    this.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_size(px(12.0)).child(t.local_root.to_string()))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(&self.form_dest)))
                                    .child(
                                        Button::new("browse-dest")
                                            .ghost()
                                            .icon(IconName::FolderOpen)
                                            .label(t.browse)
                                            .on_click(cx.listener({
                                                let state = self.form_dest.clone();
                                                move |this, _, window, cx| {
                                                    this.browse_path(
                                                        state.clone(),
                                                        true,
                                                        false,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            })),
                                    ),
                            ),
                    )
                })
                .when(self.form_is_sftp, |this| {
                    this.child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(
                                        self.labeled_input(t.sftp_host, &self.form_sftp_host),
                                    ))
                                    .child(div().w(px(120.0)).child(
                                        self.labeled_input(t.sftp_port, &self.form_sftp_port),
                                    )),
                            )
                            .child(self.labeled_input(t.sftp_user, &self.form_sftp_user))
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_size(px(12.0)).child(t.sftp_key.to_string()))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .child(Input::new(&self.form_sftp_key)),
                                            )
                                            .child(
                                                Button::new("browse-key")
                                                    .ghost()
                                                    .icon(IconName::FolderOpen)
                                                    .label(t.browse)
                                                    .on_click(cx.listener({
                                                        let state = self.form_sftp_key.clone();
                                                        move |this, _, window, cx| {
                                                            this.browse_path(
                                                                state.clone(),
                                                                false,
                                                                true,
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                    })),
                                            ),
                                    ),
                            )
                            .child(self.labeled_input(t.sftp_root, &self.form_sftp_root)),
                    )
                })
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .child(format!("{} ({})", t.excludes, t.excludes_help)),
                                )
                                .child(
                                    Button::new("preset-dev")
                                        .ghost()
                                        .label("Preset: Dev")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.form_excludes.update(cx, |input, cx| {
                                                input.set_value(
                                                    "target, node_modules, .git, *.tmp",
                                                    window,
                                                    cx,
                                                );
                                            });
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(Input::new(&self.form_excludes)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_size(px(12.0)).child(t.compression.to_string()))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("comp-0")
                                        .label("0 (aucun)")
                                        .when(self.form_compression == 0, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.form_compression = 0;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("comp-1")
                                        .label("1 (rapide)")
                                        .when(self.form_compression == 1, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.form_compression = 1;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("comp-5")
                                        .label("5 (équilibré)")
                                        .when(self.form_compression == 5, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.form_compression = 5;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("comp-15")
                                        .label("15 (max)")
                                        .when(self.form_compression == 15, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.form_compression = 15;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("submit-job")
                                .primary()
                                .icon(if is_editing {
                                    IconName::Check
                                } else {
                                    IconName::Plus
                                })
                                .label(if is_editing { t.save } else { t.create })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.create_or_update_job(window, cx);
                                })),
                        )
                        .when(is_editing, |this| {
                            this.child(
                                Button::new("cancel-edit")
                                    .ghost()
                                    .icon(IconName::X)
                                    .label(t.cancel)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.reset_job_form(window, cx);
                                        cx.notify();
                                    })),
                            )
                        }),
                );

        let mut jobs_list = v_flex().gap_2();
        for row in rows {
            jobs_list = jobs_list.child(row);
        }
        let list = if self.jobs.is_empty() {
            v_flex()
                .p_6()
                .rounded_lg()
                .border_1()
                .border_dashed()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(Icon::new(IconName::Folder).size_4()),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(t.no_jobs),
                        ),
                )
                .into_any_element()
        } else {
            jobs_list.into_any_element()
        };

        v_flex()
            .gap_4()
            .child(self.section_header(IconName::Database, t.jobs, cx))
            .child(list)
            .child(form)
    }

    fn labeled_input(&self, label: &str, state: &gpui::Entity<InputState>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(div().text_size(px(12.0)).child(label.to_string()))
            .child(Input::new(state))
    }

    fn render_run(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let (title, banner) = match &self.phase {
            RunPhase::Idle => (t.run.to_string(), None),
            RunPhase::Running { job, .. } => (
                format!("{} — {}", t.run_title, job),
                Some((cx.theme().primary, format!("{} …", t.running))),
            ),
            RunPhase::Done { snapshot, seconds } => (
                t.run.to_string(),
                Some((
                    cx.theme().success,
                    format!(
                        "{} ✓ {} ({snapshot} · {seconds}s)",
                        t.backup_done, t.success
                    ),
                )),
            ),
            RunPhase::Failed { error } => (
                t.run.to_string(),
                Some((cx.theme().danger, format!("{} — {error}", t.backup_failed))),
            ),
            RunPhase::Cancelled => (
                t.run.to_string(),
                Some((cx.theme().warning, t.cancelled_by_user.to_string())),
            ),
        };

        let running = matches!(self.phase, RunPhase::Running { .. });

        let (frac, pct_label) = if self.run_est_files > 0 {
            let frac = (self.run_files_done as f64 / self.run_est_files as f64).clamp(0.0, 0.99);
            (
                frac as f32,
                format!("{} / ≈{}", self.run_files_done, self.run_est_files),
            )
        } else {
            (0.05f32, format!("{} / ?", self.run_files_done))
        };
        let progress = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(pct_label),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{:.0} %", frac * 100.0)),
                    ),
            )
            .child(
                div()
                    .id("run-progress")
                    .w_full()
                    .h(px(10.0))
                    .rounded_full()
                    .bg(cx.theme().selection.opacity(0.6))
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .w(relative(frac))
                            .rounded_full()
                            .bg(if running {
                                cx.theme().primary
                            } else {
                                cx.theme().success
                            }),
                    ),
            );

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .p_2()
                            .rounded_lg()
                            .bg(cx.theme().primary.opacity(0.15))
                            .border_1()
                            .border_color(cx.theme().primary.opacity(0.35))
                            .text_color(cx.theme().primary)
                            .child(Icon::new(IconName::Play).size_5()),
                    )
                    .child(
                        div()
                            .text_size(px(24.0))
                            .text_color(cx.theme().foreground)
                            .child(title),
                    )
                    .when(running, |this| this.child(Spinner::new())),
            )
            .when_some(banner, |this, (color, message)| {
                this.child(
                    div()
                        .p_3()
                        .rounded_lg()
                        .bg(color.opacity(0.12))
                        .border_1()
                        .border_color(color.opacity(0.4))
                        .text_size(px(13.0))
                        .text_color(cx.theme().foreground)
                        .child(message),
                )
            })
            .child(progress)
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(self.stat_card(
                        t.files_done,
                        &self.run_files_done.to_string(),
                        IconName::Files,
                        cx.theme().primary,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.new_chunks,
                        &self.run_chunks_new.to_string(),
                        IconName::Package,
                        cx.theme().success,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.bytes_processed,
                        &human_bytes_lang(self.run_bytes_done, self.lang),
                        IconName::HardDrive,
                        cx.theme().warning,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.skipped,
                        &self.run_skipped.to_string(),
                        IconName::TriangleAlert,
                        cx.theme().danger,
                        cx,
                    )),
            )
            .when(running, |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(cx.theme().muted_foreground)
                                .child(self.run_current_file.clone()),
                        )
                        .child(
                            Button::new("cancel-run")
                                .danger()
                                .icon(IconName::X)
                                .label(t.cancel)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_run(window, cx);
                                })),
                        ),
                )
            })
    }

    fn render_restore(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let selected = self.selected_snapshot;

        let snapshot_rows: Vec<AnyElement> = self
            .snapshots
            .iter()
            .map(|row| {
                let is_selected = Some(row.id) == selected;
                let row_id = row.id;
                h_flex()
                    .id(gpui::SharedString::new(row.id.to_string()))
                    .justify_between()
                    .p_2()
                    .rounded_md()
                    .when(is_selected, |this| {
                        this.border_1().border_color(cx.theme().primary)
                    })
                    .when(!is_selected, |this| {
                        this.border_1().border_color(cx.theme().border)
                    })
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(row.created_at.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .child(format!("{} {}", row.files, t.files)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_snapshot = Some(row_id);
                            this.selected_paths.clear();
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            })
            .collect();

        let search = self
            .restore_search
            .read(cx)
            .value()
            .to_string()
            .to_lowercase();
        let files: Vec<&(String, String, u64)> = self
            .snapshots
            .iter()
            .find(|s| Some(s.id) == selected)
            .map(|s| {
                s.entries
                    .iter()
                    .filter(|(_, rel, _)| search.is_empty() || rel.to_lowercase().contains(&search))
                    .collect()
            })
            .unwrap_or_default();

        let file_rows: Vec<AnyElement> = files
            .iter()
            .map(|(_, rel, size)| {
                let rel = rel.clone();
                let size = *size;
                let picked = self.selected_paths.contains(rel.as_str());
                h_flex()
                    .id(gpui::SharedString::new(rel.clone()))
                    .gap_2()
                    .p_1()
                    .rounded_md()
                    .when(picked, |this| this.bg(cx.theme().selection))
                    .child(
                        div()
                            .w(px(16.0))
                            .text_color(if picked {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(if picked {
                                Icon::new(IconName::Check).into_any_element()
                            } else {
                                Icon::new(IconName::Square).into_any_element()
                            }),
                    )
                    .child(div().flex_1().text_size(px(12.0)).child(rel.clone()))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(human_bytes_lang(size, self.lang)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if this.selected_paths.contains(rel.as_str()) {
                                this.selected_paths.remove(rel.as_str());
                            } else {
                                this.selected_paths.insert(rel.to_string());
                            }
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            })
            .collect();

        let banner = self.restore_result.clone().map(|(is_error, message)| {
            let color = if is_error {
                cx.theme().danger
            } else {
                cx.theme().success
            };
            div()
                .p_3()
                .rounded_lg()
                .bg(color.opacity(0.12))
                .border_1()
                .border_color(color.opacity(0.4))
                .text_size(px(13.0))
                .child(message)
        });

        let all_selected = !files.is_empty()
            && files
                .iter()
                .all(|(_, rel, _)| self.selected_paths.contains(rel.as_str()));

        let job_selector = if self.jobs.len() > 1 {
            let mut tabs = h_flex().gap_2().items_center().flex_wrap();
            tabs = tabs.child(
                div()
                    .text_size(px(12.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{}:", t.select_job)),
            );
            for job in &self.jobs {
                let is_current = Some(job.id) == self.selected_restore_job;
                let jid = job.id;
                let jname = job.name.clone();
                tabs = tabs.child(
                    Button::new(gpui::SharedString::new(format!(
                        "restore-job-{}",
                        job.id.simple()
                    )))
                    .label(jname)
                    .when(is_current, |b| b.primary())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_restore_job = Some(jid);
                        this.snapshots.clear();
                        this.selected_snapshot = None;
                        this.selected_paths.clear();
                        this.load_snapshots(cx);
                        cx.notify();
                    })),
                );
            }
            Some(tabs)
        } else {
            None
        };

        v_flex()
            .gap_4()
            .child(self.section_header(IconName::Archive, t.restore, cx))
            .when_some(job_selector, |this, js| this.child(js))
            .when_some(banner, |this, b| this.child(b))
            .child(
                Button::new("reload-snapshots")
                    .label(format!("{} ({})", t.snapshots, t.refresh))
                    .icon(IconName::RefreshCcw)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_snapshots(cx);
                    })),
            )
            .child(if self.snapshots.is_empty() {
                div()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_dashed()
                    .border_color(cx.theme().border)
                    .text_size(px(13.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(t.no_snapshots)
                    .into_any_element()
            } else {
                let mut snap_list = v_flex().gap_1();
                for row in snapshot_rows {
                    snap_list = snap_list.child(row);
                }
                snap_list.into_any_element()
            })
            .when(!self.snapshots.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Input::new(&self.restore_search))
                                .child(
                                    Button::new("select-all")
                                        .ghost()
                                        .label(if all_selected {
                                            t.deselect_all
                                        } else {
                                            t.select_all
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let files: Vec<String> = this
                                                .snapshots
                                                .iter()
                                                .find(|s| Some(s.id) == this.selected_snapshot)
                                                .map(|s| {
                                                    s.entries
                                                        .iter()
                                                        .map(|(_, rel, _)| rel.clone())
                                                        .collect()
                                                })
                                                .unwrap_or_default();
                                            if files
                                                .iter()
                                                .all(|rel| this.selected_paths.contains(rel))
                                            {
                                                for rel in files {
                                                    this.selected_paths.remove(&rel);
                                                }
                                            } else {
                                                for rel in files {
                                                    this.selected_paths.insert(rel);
                                                }
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} {}",
                                    self.selected_paths.len(),
                                    t.selected_files
                                )),
                        )
                        .child({
                            let mut file_list = v_flex().gap_1();
                            for row in file_rows {
                                file_list = file_list.child(row);
                            }
                            file_list
                                .id("restore-file-list")
                                .max_h(px(340.0))
                                .overflow_y_scroll()
                        }),
                )
            })
            .when(!self.snapshots.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .p_4()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("dest-original")
                                        .label(t.to_original)
                                        .when(self.restore_to_original, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.restore_to_original = true;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("dest-target")
                                        .label(t.to_target)
                                        .when(!self.restore_to_original, |b| b.primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.restore_to_original = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(!self.restore_to_original, |this| {
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(&self.restore_target)))
                                    .child(
                                        Button::new("browse-restore-target")
                                            .ghost()
                                            .icon(IconName::FolderOpen)
                                            .label(t.browse)
                                            .on_click(cx.listener({
                                                let state = self.restore_target.clone();
                                                move |this, _, window, cx| {
                                                    this.browse_path(
                                                        state.clone(),
                                                        true,
                                                        false,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            })),
                                    ),
                            )
                        })
                        .child(
                            Button::new("toggle-overwrite")
                                .ghost()
                                .when(self.restore_overwrite, |b| b.primary())
                                .icon(if self.restore_overwrite {
                                    IconName::Check
                                } else {
                                    IconName::Square
                                })
                                .label(t.overwrite_files)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.restore_overwrite = !this.restore_overwrite;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("restore-run")
                                .primary()
                                .icon(IconName::Download)
                                .label(format!("{} ({})", t.restore_now, self.selected_paths.len()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.run_restore(window, cx);
                                })),
                        ),
                )
            })
    }

    fn render_history(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let header = h_flex()
            .gap_4()
            .p_2()
            .text_size(px(12.0))
            .text_color(cx.theme().muted_foreground)
            .child(div().w(px(180.0)).child(t.jobs.to_string()))
            .child(div().w(px(90.0)).child(t.status))
            .child(div().flex_1().child("UTC"))
            .child(div().w(px(110.0)).child(t.files_done))
            .child(div().w(px(110.0)).child(t.new_chunks));

        let rows: Vec<AnyElement> = self
            .recent_runs
            .iter()
            .map(|(job, run)| {
                let (color, label) = match run.status {
                    RunStatus::Success => (cx.theme().success, t.success),
                    RunStatus::Failed => (cx.theme().danger, t.failed),
                };
                h_flex()
                    .id(gpui::SharedString::new(run.id.to_string()))
                    .gap_4()
                    .p_2()
                    .rounded_md()
                    .child(div().w(px(180.0)).child(job.clone()))
                    .child(div().w(px(90.0)).text_color(color).child(label.to_string()))
                    .child(
                        div()
                            .flex_1()
                            .text_color(cx.theme().muted_foreground)
                            .child(run.started_at.to_rfc3339()),
                    )
                    .child(div().w(px(110.0)).child(run.files_total.to_string()))
                    .child(div().w(px(110.0)).child(run.chunks_new.to_string()))
                    .into_any_element()
            })
            .collect();

        let mut history_list = v_flex().gap_1();
        for row in rows {
            history_list = history_list.child(row);
        }
        let body = if self.recent_runs.is_empty() {
            v_flex()
                .p_6()
                .rounded_lg()
                .border_1()
                .border_dashed()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(t.no_history),
                )
                .into_any_element()
        } else {
            history_list.into_any_element()
        };

        v_flex()
            .gap_4()
            .child(self.section_header(IconName::Clock, t.history, cx))
            .child(
                v_flex()
                    .p_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(header)
                    .child(body),
            )
    }

    fn render_settings(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        v_flex()
            .gap_4()
            .child(self.section_header(IconName::Settings, t.settings, cx))
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(cx.theme().foreground)
                            .child(t.language),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("lang-fr")
                                    .label("Français")
                                    .when(self.lang == Lang::Fr, |b| b.primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.lang = Lang::Fr;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("lang-en")
                                    .label("English")
                                    .when(self.lang == Lang::En, |b| b.primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.lang = Lang::En;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(Icon::new(IconName::Sparkles).size_4()),
                            )
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .text_color(cx.theme().foreground)
                                    .child(t.theme),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("theme-dark")
                                    .label(t.theme_dark)
                                    .when(self.dark, |b| b.primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dark = true;
                                        theme::apply_brand_theme(true, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("theme-light")
                                    .label(t.theme_light)
                                    .when(!self.dark, |b| b.primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dark = false;
                                        theme::apply_brand_theme(false, cx);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(cx.theme().foreground)
                            .child(t.passphrase),
                    )
                    .child(Input::new(&self.form_passphrase).mask_toggle())
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(t.passphrase_help),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(cx.theme().foreground)
                            .child(t.catalog_location),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                self.catalog_path()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default(),
                            ),
                    ),
            )
    }
}

impl gpui::Render for RustBackupApp {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let t = self.texts();
        let active = self.section;

        let menu_item = |label: &str, icon: IconName, section: Section, is_active: bool| {
            let label = label.to_string();
            SidebarMenuItem::new(label.as_str())
                .icon(icon)
                .active(is_active)
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.section = section;
                    cx.notify();
                }))
        };

        let (lock_icon, lock_label, lock_color) =
            if self.passphrase.is_empty() && self.form_passphrase.read(cx).value().is_empty() {
                (IconName::LockOpen, t.passphrase_unset, cx.theme().warning)
            } else {
                (IconName::Lock, t.passphrase_set, cx.theme().success)
            };

        let sidebar = Sidebar::new("main-sidebar")
            .header(
                SidebarHeader::new().child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(img("icons/brand-mark.png").size(px(24.0)))
                        .child(
                            div()
                                .text_size(px(16.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child("RustBackup"),
                        ),
                ),
            )
            .child(
                SidebarGroup::new(t.navigation).child(
                    SidebarMenu::new()
                        .child(menu_item(
                            t.dashboard,
                            IconName::LayoutDashboard,
                            Section::Dashboard,
                            active == Section::Dashboard,
                        ))
                        .child(menu_item(
                            t.jobs,
                            IconName::Database,
                            Section::Jobs,
                            active == Section::Jobs,
                        ))
                        .child(menu_item(
                            t.run,
                            IconName::Play,
                            Section::Run,
                            active == Section::Run,
                        ))
                        .child(menu_item(
                            t.restore,
                            IconName::Archive,
                            Section::Restore,
                            active == Section::Restore,
                        ))
                        .child(menu_item(
                            t.history,
                            IconName::Clock,
                            Section::History,
                            active == Section::History,
                        ))
                        .child(menu_item(
                            t.settings,
                            IconName::Settings,
                            Section::Settings,
                            active == Section::Settings,
                        )),
                ),
            )
            .footer(
                SidebarFooter::new().child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_size(px(11.0))
                                .text_color(lock_color)
                                .child(Icon::new(lock_icon).size_3())
                                .child(lock_label),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(cx.theme().muted_foreground)
                                .child("AES-256-GCM · Argon2id · zstd"),
                        ),
                ),
            );

        let content = match self.section {
            Section::Dashboard => self.render_dashboard(cx).into_any_element(),
            Section::Jobs => self.render_jobs(cx).into_any_element(),
            Section::Run => self.render_run(cx).into_any_element(),
            Section::Restore => self.render_restore(cx).into_any_element(),
            Section::History => self.render_history(cx).into_any_element(),
            Section::Settings => self.render_settings(cx).into_any_element(),
        };

        let toast = self.toast.clone().map(|(is_error, message)| {
            let color = if is_error {
                cx.theme().danger
            } else {
                cx.theme().success
            };
            div()
                .id("toast")
                .px_4()
                .py_2()
                .rounded_md()
                .bg(color.opacity(0.15))
                .border_1()
                .border_color(color.opacity(0.5))
                .text_size(px(12.0))
                .text_color(cx.theme().foreground)
                .child(message)
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(
                TitleBar::new().child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(img("icons/brand-mark.png").size(px(16.0)))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child("RustBackup"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("— {}", t.tagline)),
                        ),
                ),
            )
            .child(
                div().flex().flex_1().min_h_0().child(sidebar).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .p_6()
                        .id("main-content")
                        .overflow_y_scroll()
                        .child(
                            v_flex()
                                .gap_3()
                                .child(content)
                                .when_some(toast, |this, t| this.child(t)),
                        ),
                ),
            )
    }
}

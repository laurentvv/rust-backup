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

use crate::i18n::{self, Lang, human_bytes};
use crate::runner::{self, EngineEvent};

/// Active navigation section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Dashboard.
    Dashboard,
    /// Jobs list + creation form.
    Jobs,
    /// Live run view.
    Run,
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
            form_passphrase: cx.new(|cx| InputState::new(window, cx).placeholder("••••••••••")),
            phase: RunPhase::Idle,
            run_files_done: 0,
            run_bytes_done: 0,
            run_chunks_new: 0,
            run_bytes_new: 0,
            run_skipped: 0,
            run_current_file: String::new(),
            cancel: None,
            toast: None,
        };
        app.refresh_catalog();
        app
    }

    fn texts(&self) -> &'static i18n::Texts {
        self.lang.texts()
    }

    fn catalog_path(&self) -> Option<std::path::PathBuf> {
        Catalog::default_path().ok()
    }

    /// Reloads jobs and recent runs from the local catalog.
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

        let mut runs: Vec<(String, RunRecord)> = Vec::new();
        for job in &self.jobs {
            if let Ok(job_runs) = catalog.list_runs(job.id) {
                for record in job_runs.into_iter().take(10) {
                    runs.push((job.name.clone(), record));
                }
            }
        }
        runs.sort_by_key(|(_, run)| std::cmp::Reverse(run.started_at));
        self.recent_runs = runs.into_iter().take(30).collect();
    }

    fn snapshot_count(&self) -> usize {
        let Some(path) = self.catalog_path() else {
            return 0;
        };
        let Ok(catalog) = Catalog::open(&path) else {
            return 0;
        };
        self.jobs
            .iter()
            .filter_map(|job| catalog.list_snapshots(job.id).ok())
            .map(|snapshots| snapshots.len())
            .sum()
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
                let _snapshot = summary.snapshot_id.simple().to_string();
                self.run_files_done = summary.stats.files_total;
                self.run_bytes_done = summary.stats.bytes_total;
                self.run_chunks_new = summary.stats.chunks_new;
                self.run_bytes_new = summary.stats.bytes_new;
                self.refresh_catalog();
            }
            EngineEvent::Finished(Err(message)) => {
                self.phase = RunPhase::Failed {
                    error: message.clone(),
                };
                self.toast = Some((true, message));
            }
        }
        self.cancel = None;
        cx.notify();
    }

    fn elapsed_seconds(&self) -> u64 {
        match &self.phase {
            RunPhase::Running { started, .. } => started.elapsed().as_secs(),
            RunPhase::Done { seconds, .. } => *seconds,
            _ => 0,
        }
    }

    fn create_job(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let name = self.form_name.read(cx).value().to_string();
        let source = self.form_source.read(cx).value().to_string();
        let dest = self.form_dest.read(cx).value().to_string();
        let job = JobConfig {
            id: Uuid::new_v4(),
            name,
            sources: vec![source.into()],
            destination: Destination::Local { root: dest.into() },
            compression_level: 5,
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
        match Catalog::open(&path).and_then(|catalog| catalog.insert_job(&job).map(|_| catalog)) {
            Ok(_) => {
                self.toast = Some((false, format!("{} ✓ {}", self.texts().create, job.name)));
                self.refresh_catalog();
            }
            Err(e) => self.toast = Some((true, e.to_string())),
        }
        cx.notify();
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
            self.refresh_catalog();
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

    fn stat_card(
        &self,
        title: &str,
        value: &str,
        icon: IconName,
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
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().selection)
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

    fn render_dashboard(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let last_run_label = self
            .recent_runs
            .first()
            .map(|(job, run)| {
                format!(
                    "{} · {}",
                    job,
                    match run.status {
                        RunStatus::Success => t.success,
                        RunStatus::Failed => t.failed,
                    }
                )
            })
            .unwrap_or_else(|| t.idle.to_string());
        let protected: u64 = self
            .recent_runs
            .first()
            .filter(|(_, run)| run.status == RunStatus::Success)
            .map(|(_, run)| run.bytes_total)
            .unwrap_or(0);

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
                        cx,
                    ))
                    .child(self.stat_card(
                        t.snapshots_count,
                        &self.snapshot_count().to_string(),
                        IconName::Layers,
                        cx,
                    ))
                    .child(self.stat_card(t.last_run, &last_run_label, IconName::Clock, cx))
                    .child(self.stat_card(
                        t.protected_volume,
                        &human_bytes(protected),
                        IconName::ShieldCheck,
                        cx,
                    )),
            )
    }

    fn render_jobs(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.texts();
        let rows: Vec<AnyElement> = self
            .jobs
            .iter()
            .map(|job| {
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
                                div()
                                    .text_size(px(14.0))
                                    .text_color(cx.theme().foreground)
                                    .child(job.name.clone()),
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
                                    .child(format!(
                                        "{} : {}",
                                        t.source,
                                        job.sources
                                            .first()
                                            .map(|s| s.display().to_string())
                                            .unwrap_or_default()
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(gpui::SharedString::new(format!(
                                    "run-{}",
                                    job.id.simple()
                                )))
                                .primary()
                                .label(t.run_now)
                                .icon(IconName::Play)
                                .on_click(cx.listener({
                                    let id = job.id;
                                    move |this, _, window, cx| {
                                        this.start_backup(window, cx, id);
                                    }
                                })),
                            )
                            .child(
                                Button::new(gpui::SharedString::new(format!(
                                    "del-{}",
                                    job.id.simple()
                                )))
                                .ghost()
                                .icon(IconName::Trash)
                                .on_click(cx.listener({
                                    let id = job.id;
                                    move |this, _, window, cx| {
                                        this.delete_job(window, cx, id);
                                    }
                                })),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        let form = v_flex()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                div()
                    .text_size(px(15.0))
                    .text_color(cx.theme().foreground)
                    .child(t.new_job),
            )
            .child(self.labeled_input(t.name, &self.form_name))
            .child(self.labeled_input(t.source, &self.form_source))
            .child(self.labeled_input(t.local_root, &self.form_dest))
            .child(
                h_flex().gap_2().child(
                    Button::new("create-job")
                        .primary()
                        .icon(IconName::Plus)
                        .label(t.create)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.create_job(window, cx);
                        })),
                ),
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
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(t.no_jobs),
                )
                .into_any_element()
        } else {
            jobs_list.into_any_element()
        };

        v_flex()
            .gap_4()
            .child(
                div()
                    .text_size(px(24.0))
                    .text_color(cx.theme().foreground)
                    .child(t.jobs),
            )
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
        };

        let running = matches!(self.phase, RunPhase::Running { .. });

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
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
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(self.stat_card(
                        t.files_done,
                        &self.run_files_done.to_string(),
                        IconName::Files,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.new_chunks,
                        &self.run_chunks_new.to_string(),
                        IconName::Package,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.bytes_processed,
                        &human_bytes(self.run_bytes_done),
                        IconName::HardDrive,
                        cx,
                    ))
                    .child(self.stat_card(
                        t.skipped,
                        &self.run_skipped.to_string(),
                        IconName::TriangleAlert,
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
            .child(
                div()
                    .text_size(px(24.0))
                    .text_color(cx.theme().foreground)
                    .child(t.history),
            )
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
            .child(
                div()
                    .text_size(px(24.0))
                    .text_color(cx.theme().foreground)
                    .child(t.settings),
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
                        div()
                            .text_size(px(14.0))
                            .text_color(cx.theme().foreground)
                            .child(t.passphrase),
                    )
                    .child(Input::new(&self.form_passphrase))
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

        let sidebar = Sidebar::new("main-sidebar")
            .header(
                SidebarHeader::new().child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::ShieldCheck).size_5())
                        .child(
                            div()
                                .text_size(px(15.0))
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
                    div()
                        .text_size(px(11.0))
                        .text_color(cx.theme().muted_foreground)
                        .child("AES-256-GCM · Argon2id · zstd"),
                ),
            );

        let content = match self.section {
            Section::Dashboard => self.render_dashboard(cx).into_any_element(),
            Section::Jobs => self.render_jobs(cx).into_any_element(),
            Section::Run => self.render_run(cx).into_any_element(),
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
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(cx.theme().muted_foreground)
                                .child("RustBackup"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(cx.theme().muted_foreground)
                                .child(t.tagline),
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

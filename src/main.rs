//! RustBackup — GUI entry point (GPUI Kit).

mod app;
mod i18n;
mod runner;
mod theme;

use std::borrow::Cow;

use gpui_kit::assets::{Assets as ComponentAssets, icon_assets};
use gpui_kit::component::TitleBar;
use gpui_kit::gpui::{AppContext, AssetSource, KeyBinding, Result, SharedString};
use gpui_kit::{application, init, open_window};

// Extra Lucide icons beyond the default component set (embedded on purpose:
// only what the UI uses ships in the binary).
icon_assets!(
    ExtraIcons,
    [
        Archive,
        ChartColumn,
        CheckCheck,
        ChevronDown,
        ChevronRight,
        Clock,
        Database,
        Download,
        EyeOff,
        FileText,
        Files,
        Folder,
        FolderOpen,
        House,
        Key,
        Layers,
        List,
        Lock,
        LockOpen,
        Package,
        RefreshCcw,
        Server,
        ShieldAlert,
        ShieldCheck,
        SlidersHorizontal,
        Sparkles,
        Trash,
        Usb,
        X
    ]
);

/// Composite asset source: extra icons first, brand mark, then component defaults.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "icons/brand-mark.png" {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/brand-mark.png"
            ))));
        }
        if path == "icons/brand-mark.svg" {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/brand-mark.svg"
            ))));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = ExtraIcons.list(path)?;
        if path.is_empty() || path.starts_with("icons") {
            out.push("icons/brand-mark.png".into());
            out.push("icons/brand-mark.svg".into());
        }
        out.extend(ComponentAssets.list(path)?);
        Ok(out)
    }
}

fn main() {
    application().with_assets(AppAssets).run(|cx| {
        init(cx);

        // Global keyboard shortcuts
        cx.bind_keys([
            KeyBinding::new("ctrl-1", app::GoDashboardAction, None),
            KeyBinding::new("ctrl-2", app::GoJobsAction, None),
            KeyBinding::new("ctrl-3", app::GoRestoreAction, None),
            KeyBinding::new("ctrl-4", app::GoHistoryAction, None),
            KeyBinding::new("ctrl-b", app::StartBackupAction, None),
            KeyBinding::new("ctrl-,", app::OpenSettingsAction, None),
        ]);

        // Signature palette: steel-blue tinted light theme by default;
        // switching to dark mode retains the deep slate palette.
        theme::apply_brand_theme(false, cx);

        let options = TitleBar::window_options();
        if let Err(e) = open_window(options, cx, |window, cx| {
            cx.new(|cx| app::RustBackupApp::new(window, cx))
        }) {
            eprintln!("failed to open window: {e}");
        }
    });
}

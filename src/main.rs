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

/// Brand marks embedded in the binary (Arche direction, `assets/brand/`).
/// `brand-mark.*` is the previous mark, kept until every view uses the new one.
const BRAND_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/brand-mark.png",
        include_bytes!("../assets/brand-mark.png"),
    ),
    (
        "icons/brand-mark.svg",
        include_bytes!("../assets/brand-mark.svg"),
    ),
    (
        "brand/mark-light.svg",
        include_bytes!("../assets/brand/mark-light.svg"),
    ),
    (
        "brand/mark-dark.svg",
        include_bytes!("../assets/brand/mark-dark.svg"),
    ),
    (
        "brand/mark-fine-light.svg",
        include_bytes!("../assets/brand/mark-fine-light.svg"),
    ),
    (
        "brand/mark-fine-dark.svg",
        include_bytes!("../assets/brand/mark-fine-dark.svg"),
    ),
    (
        "brand/app-tile.svg",
        include_bytes!("../assets/brand/app-tile.svg"),
    ),
];

/// Display serif (OFL, `assets/fonts/OFL.txt`), registered at startup.
const DISPLAY_FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Newsreader16pt-Regular.ttf"),
    include_bytes!("../assets/fonts/Newsreader16pt-Medium.ttf"),
];

/// Composite asset source: brand marks, extra icons, then component defaults.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND_ASSETS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = ExtraIcons.list(path)?;
        if path.is_empty() || path.starts_with("icons") {
            out.extend(BRAND_ASSETS.iter().map(|(p, _)| SharedString::from(*p)));
        }
        out.extend(ComponentAssets.list(path)?);
        Ok(out)
    }
}

fn main() {
    application().with_assets(AppAssets).run(|cx| {
        init(cx);

        // Display serif for titles and the wordmark; the UI falls back to the
        // system font if registration fails.
        if let Err(e) = cx
            .text_system()
            .add_fonts(DISPLAY_FONTS.iter().map(|f| Cow::Borrowed(*f)).collect())
        {
            eprintln!("failed to register display fonts: {e}");
        }

        // Global keyboard shortcuts
        cx.bind_keys([
            KeyBinding::new("ctrl-1", app::GoDashboardAction, None),
            KeyBinding::new("ctrl-2", app::GoJobsAction, None),
            KeyBinding::new("ctrl-3", app::GoRestoreAction, None),
            KeyBinding::new("ctrl-4", app::GoHistoryAction, None),
            KeyBinding::new("ctrl-b", app::StartBackupAction, None),
            KeyBinding::new("ctrl-,", app::OpenSettingsAction, None),
        ]);

        // Arche brand theme (light by default; Settings switches to dark).
        theme::apply_brand_theme(false, cx);

        let options = TitleBar::window_options();
        if let Err(e) = open_window(options, cx, |window, cx| {
            cx.new(|cx| app::RustBackupApp::new(window, cx))
        }) {
            eprintln!("failed to open window: {e}");
        }
    });
}

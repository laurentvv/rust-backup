//! RustBackup — GUI entry point (GPUI Kit).

mod app;
mod i18n;
mod runner;

use std::borrow::Cow;

use gpui_kit::assets::{Assets as ComponentAssets, icon_assets};
use gpui_kit::component::TitleBar;
use gpui_kit::gpui::{AppContext, AssetSource, Result, SharedString};
use gpui_kit::{application, init, open_window};

// Extra Lucide icons beyond the default component set (embedded on purpose:
// only what the UI uses ships in the binary).
icon_assets!(
    ExtraIcons,
    [
        Archive,
        ChartColumn,
        Clock,
        Database,
        Files,
        House,
        Key,
        Layers,
        List,
        Package,
        Server,
        ShieldCheck,
        Usb,
        X
    ]
);

/// Composite asset source: extra icons first, then the component defaults.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = ExtraIcons.list(path)?;
        out.extend(ComponentAssets.list(path)?);
        Ok(out)
    }
}

fn main() {
    application().with_assets(AppAssets).run(|cx| {
        init(cx);

        let options = TitleBar::window_options();
        if let Err(e) = open_window(options, cx, |window, cx| {
            cx.new(|cx| app::RustBackupApp::new(window, cx))
        }) {
            eprintln!("failed to open window: {e}");
        }
    });
}

//! RustBackup — GUI entry point (GPUI Kit).

mod app;
mod i18n;
mod runner;

use std::borrow::Cow;

use gpui_kit::assets::{Assets as ComponentAssets, icon_assets};
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
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
        out.extend(ComponentAssets.list(path)?);
        Ok(out)
    }
}

/// Applies the RustBackup signature tint to the active light theme:
/// a calm steel-blue workspace ("coffre-fort" feel) instead of pure
/// white, with the teal backup accent.
fn tint_theme(cx: &mut gpui_kit::gpui::App) {
    use gpui_kit::component::Theme;
    let tint = |hex: u32| -> gpui_kit::gpui::Hsla { gpui_kit::gpui::rgb(hex).into() };
    let theme = Theme::global_mut(cx);
    let c = &mut theme.colors;
    c.background = tint(0xE7EDF4); // steel blue-gray workspace
    c.popover = tint(0xF4F7FA); // cards / elevated surfaces
    c.secondary = tint(0xD9E3EE);
    c.selection = tint(0xCFDEEB);
    c.primary = tint(0x0B8F80); // backup teal (brand)
    c.primary_foreground = tint(0xFFFFFF);
    c.title_bar = tint(0xE7EDF4);
}

fn main() {
    application().with_assets(AppAssets).run(|cx| {
        init(cx);

        // Signature palette: steel-blue tinted LIGHT theme (not pure white)
        // with the teal backup accent; dark mode stays one click away in
        // Settings.
        Theme::change(ThemeMode::Light, None, cx);
        tint_theme(cx);

        let options = TitleBar::window_options();
        if let Err(e) = open_window(options, cx, |window, cx| {
            cx.new(|cx| app::RustBackupApp::new(window, cx))
        }) {
            eprintln!("failed to open window: {e}");
        }
    });
}

//! RustBackup brand theme — "Arche" direction (see `docs/design/directions.md`).
//!
//! Quiet, editorial palette: green-black ink on cool limestone (light) or
//! chalk on night (dark), brass reserved for short emphasis, semantic colors
//! kept apart from the accent. Opaque bases only (no Mica). Fluent geometry:
//! 6 px controls, 8 px windows and dialogs.
//!
//! [`Palette`] exposes the raw brand tokens for custom-drawn elements; the
//! gpui-kit component theme is derived from it in [`apply_brand_theme`].

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::gpui::{Hsla, px, rgb};

/// Display serif family (Newsreader, 16 pt optical size, weights 400/500),
/// embedded and registered in `main.rs`. Titles, hero sentence and wordmark
/// only — never below 20 px; all other UI text uses the system font.
#[allow(dead_code)] // used by the J4 page redesign
pub const DISPLAY_FONT: &str = "Newsreader 16pt";

/// Raw Arche brand tokens for one mode. Contrast ratios (WCAG 2.2) are noted
/// against `content` unless stated otherwise.
#[allow(dead_code)] // tokens are consumed progressively by the J4 page redesign
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window and navigation base (limestone / night).
    pub base: u32,
    /// Content layer (paper).
    pub content: u32,
    /// Hairline rules and quiet borders.
    pub hairline: u32,
    /// Control borders that must reach 3:1 (inputs, checkboxes).
    pub control_border: u32,
    /// Quiet filled surface (secondary buttons, hover rows).
    pub surface: u32,
    /// Body text and primary button fill (ink / chalk).
    pub text: u32,
    /// Secondary text (5.5:1 on light content).
    pub muted: u32,
    /// Brass: eyebrows, emphasis, active underline. Short text only (4.75:1 on light base).
    pub brass: u32,
    /// Selected row tint (brass wash).
    pub brass_wash: u32,
    pub ok: u32,
    pub warn: u32,
    pub bad: u32,
}

pub const LIGHT: Palette = Palette {
    base: 0xEFEEE8,
    content: 0xFAFAF7,
    hairline: 0xDEDBD2,
    control_border: 0x84877F,
    surface: 0xE6E4DC,
    text: 0x1B2420,
    muted: 0x5F6862,
    brass: 0x82642B,
    brass_wash: 0xEFE7D3,
    ok: 0x37704C,
    warn: 0x8C5D00,
    bad: 0xA8322D,
};

pub const DARK: Palette = Palette {
    base: 0x121614,
    content: 0x1A201D,
    hairline: 0x2E3632,
    control_border: 0x66706A,
    surface: 0x252C28,
    text: 0xECEAE2,
    muted: 0x9DA59F,
    brass: 0xD2B577,
    brass_wash: 0x3A3322,
    ok: 0x86C29C,
    warn: 0xE2B85A,
    bad: 0xEE8A80,
};

/// Brand tokens for the given mode.
#[allow(dead_code)] // used by the J4 page redesign
pub fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Same hue, lightness shifted by `delta` (positive = lighter).
fn shade(hex: u32, delta: f32) -> Hsla {
    let mut h = c(hex);
    h.l = (h.l + delta).clamp(0.0, 1.0);
    h
}

/// Applies the RustBackup Arche theme for `dark` mode to the gpui-kit theme.
pub fn apply_brand_theme(dark: bool, cx: &mut gpui_kit::gpui::App) {
    let mode = if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);
    let p = palette(dark);
    // Hover/active steps move toward more contrast with the base.
    let step = if dark { 0.05 } else { -0.04 };
    // Text placed on a filled ink/chalk or semantic button.
    let on_fill = if dark { p.base } else { p.content };

    let theme = Theme::global_mut(cx);
    theme.radius = px(6.0);
    theme.radius_lg = px(8.0);

    let t = &mut theme.colors;
    // Layers.
    t.background = c(p.base);
    t.popover = c(p.content);
    t.popover_foreground = c(p.text);
    t.foreground = c(p.text);
    t.title_bar = c(p.base);
    t.title_bar_border = c(p.hairline);
    t.status_bar = c(p.base);
    t.status_bar_border = c(p.hairline);
    t.window_border = c(p.hairline);
    t.sidebar = c(p.base);
    t.sidebar_foreground = c(p.text);
    t.sidebar_border = c(p.hairline);
    t.sidebar_accent = c(p.surface);
    t.sidebar_accent_foreground = c(p.text);
    t.sidebar_primary = c(p.text);
    t.sidebar_primary_foreground = c(on_fill);

    // Lines and quiet fills.
    t.border = c(p.hairline);
    t.input = c(p.control_border);
    t.muted = c(p.surface);
    t.muted_foreground = c(p.muted);
    t.accent = c(p.surface);
    t.accent_foreground = c(p.text);
    t.secondary = c(p.surface);
    t.secondary_hover = shade(p.surface, step);
    t.secondary_active = shade(p.surface, step * 2.0);
    t.secondary_foreground = c(p.text);
    t.skeleton = c(p.surface);
    t.scrollbar_thumb = c(p.hairline);
    t.scrollbar_thumb_hover = c(p.control_border);

    // Primary = ink fill (light) / chalk fill (dark); brass never fills a button.
    t.primary = c(p.text);
    t.primary_hover = shade(p.text, -step * 2.0);
    t.primary_active = shade(p.text, -step * 3.0);
    t.primary_foreground = c(on_fill);
    t.button_primary = t.primary;
    t.button_primary_hover = t.primary_hover;
    t.button_primary_active = t.primary_active;
    t.button_primary_foreground = t.primary_foreground;
    t.progress_bar = c(p.text);
    t.switch = c(p.text);
    t.slider_bar = c(p.text);
    t.slider_thumb = c(p.content);
    t.caret = c(p.text);

    // Brass: focus ring, links, selection and active markers.
    t.ring = c(p.brass);
    t.link = c(p.brass);
    t.link_hover = shade(p.brass, -step * 2.0);
    t.link_active = shade(p.brass, -step * 3.0);
    t.selection = c(p.brass_wash);
    t.list_active = c(p.brass_wash);
    t.list_active_border = c(p.brass);
    t.table_active = c(p.brass_wash);
    t.table_active_border = c(p.brass);
    t.tab_active = c(p.content);
    t.tab_active_foreground = c(p.text);
    t.tab_bar = c(p.base);
    t.tab_foreground = c(p.muted);

    // Semantics (never the accent).
    t.success = c(p.ok);
    t.success_hover = shade(p.ok, -step * 2.0);
    t.success_active = shade(p.ok, -step * 3.0);
    t.success_foreground = c(on_fill);
    t.warning = c(p.warn);
    t.warning_hover = shade(p.warn, -step * 2.0);
    t.warning_active = shade(p.warn, -step * 3.0);
    t.warning_foreground = c(on_fill);
    t.danger = c(p.bad);
    t.danger_hover = shade(p.bad, -step * 2.0);
    t.danger_active = shade(p.bad, -step * 3.0);
    t.danger_foreground = c(on_fill);

    // Charts: ink, brass, then quiet supporting tones.
    t.chart_1 = c(p.text);
    t.chart_2 = c(p.brass);
    t.chart_3 = c(p.muted);
    t.chart_4 = c(p.ok);
    t.chart_5 = c(p.warn);
    t.chart_grid = c(p.hairline);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.2 relative luminance of an sRGB hex color.
    fn luminance(hex: u32) -> f64 {
        let ch = |v: u32| {
            let s = f64::from(v & 0xFF) / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(hex >> 16) + 0.7152 * ch(hex >> 8) + 0.0722 * ch(hex)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn body_text_meets_aa_on_every_layer() {
        for p in [&LIGHT, &DARK] {
            for layer in [p.base, p.content, p.surface] {
                assert!(contrast(p.text, layer) >= 4.5, "text on {layer:06X}");
                assert!(contrast(p.muted, layer) >= 4.5, "muted on {layer:06X}");
            }
        }
    }

    #[test]
    fn brass_and_semantics_meet_aa_on_base_and_content() {
        for p in [&LIGHT, &DARK] {
            for fg in [p.brass, p.ok, p.warn, p.bad] {
                for layer in [p.base, p.content] {
                    assert!(contrast(fg, layer) >= 4.5, "{fg:06X} on {layer:06X}");
                }
            }
        }
    }

    #[test]
    fn control_borders_meet_non_text_contrast() {
        for p in [&LIGHT, &DARK] {
            assert!(contrast(p.control_border, p.content) >= 3.0);
            assert!(contrast(p.control_border, p.base) >= 3.0);
        }
    }

    #[test]
    fn filled_buttons_keep_text_readable() {
        // Light: paper text on ink fill / semantic fills; dark: night text on chalk / semantic fills.
        assert!(contrast(LIGHT.content, LIGHT.text) >= 4.5);
        assert!(contrast(DARK.base, DARK.text) >= 4.5);
        for p in [(&LIGHT, LIGHT.content), (&DARK, DARK.base)] {
            for fill in [p.0.ok, p.0.warn, p.0.bad] {
                assert!(contrast(p.1, fill) >= 4.5, "{:06X} on {fill:06X}", p.1);
            }
        }
    }
}

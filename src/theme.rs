//! Signature themes for RustBackup.
//!
//! Provides two curated themes:
//! - **Vault Light**: calm steel-blue workspace (`#E7EDF4`), elevated card surfaces (`#F4F7FA`),
//!   and signature backup teal accent (`#0B8F80`).
//! - **Vault Dark**: deep slate workspace (`#0F172A`), elevated card surfaces (`#1E293B`),
//!   and luminous cyan/teal accent (`#14E0C8`).

use gpui_kit::component::{Theme, ThemeMode};

fn hex_hsla(hex: u32) -> gpui_kit::gpui::Hsla {
    gpui_kit::gpui::rgb(hex).into()
}

/// Applies the RustBackup signature theme according to `dark` mode.
pub fn apply_brand_theme(dark: bool, cx: &mut gpui_kit::gpui::App) {
    if dark {
        Theme::change(ThemeMode::Dark, None, cx);
        let theme = Theme::global_mut(cx);
        let c = &mut theme.colors;
        c.background = hex_hsla(0x0F172A); // deep slate background
        c.popover = hex_hsla(0x1E293B); // elevated cards
        c.secondary = hex_hsla(0x334155);
        c.selection = hex_hsla(0x1E3A5F);
        c.border = hex_hsla(0x334155);
        c.primary = hex_hsla(0x14E0C8); // luminous cyan-teal
        c.primary_foreground = hex_hsla(0x042F2E);
        c.title_bar = hex_hsla(0x0F172A);
    } else {
        Theme::change(ThemeMode::Light, None, cx);
        let theme = Theme::global_mut(cx);
        let c = &mut theme.colors;
        c.background = hex_hsla(0xE7EDF4); // calm steel-blue workspace
        c.popover = hex_hsla(0xF4F7FA); // elevated cards
        c.secondary = hex_hsla(0xD9E3EE);
        c.selection = hex_hsla(0xCFDEEB);
        c.border = hex_hsla(0xCBD5E1);
        c.primary = hex_hsla(0x0B8F80); // signature backup teal
        c.primary_foreground = hex_hsla(0xFFFFFF);
        c.title_bar = hex_hsla(0xE7EDF4);
    }
}

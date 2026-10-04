//! FaderFrame's GTK stylesheet, generated from the active [`Theme`]: the
//! theme's colours become `@define-color` names the rules below use, and
//! GTK's own light or dark variant follows the theme.

use faderframe_ui_canvas::{Color, Theme};
use gtk::{gdk, glib, prelude::*};
use std::cell::RefCell;

thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

const CSS: &str = r#"
window { background-color: @ff_bg; color: @ff_fg; }
headerbar {
    background-image: linear-gradient(to bottom, @ff_header_top, @ff_header_bottom);
    border-bottom: 1px solid @ff_border;
    color: @ff_fg;
    min-height: 46px;
    box-shadow: none;
}
headerbar popovermenubar { background: transparent; }
.transport { margin: 0 6px; }
.transport button { min-width: 28px; min-height: 28px; border-radius: 7px; padding: 0 3px; }
.transport button.play-active { color: @ff_ok; }
.transport button.rec-active { color: @ff_error; }
.transport button.loop-active { color: @ff_accent; }
.transport button.click-active { color: @ff_selection; }
.lcd { border-radius: 6px; margin: 4px 6px; }
notebook.dock-tabs > header {
    background-color: @ff_surface;
    border-bottom: 1px solid @ff_border;
    box-shadow: none;
}
notebook.dock-tabs > header > tabs > tab {
    padding: 2px 10px;
    min-height: 24px;
    color: @ff_dim;
    border: none;
}
notebook.dock-tabs > header > tabs > tab:checked {
    color: @ff_fg;
    background-color: @ff_surface_alt;
    box-shadow: inset 0 -2px @ff_accent;
}
.tab-button { min-width: 16px; min-height: 16px; padding: 0; margin: 0; }
paned > separator { background-color: @ff_border; min-width: 5px; min-height: 5px; }
.statusbar {
    background-color: @ff_status;
    color: @ff_dim;
    border-top: 1px solid @ff_border;
    padding: 2px 10px;
    font-size: 9.5pt;
}
.statusbar .notice-error { color: @ff_error; }
.toast { background-color: alpha(@ff_surface, 0.97); color: @ff_fg; border-radius: 8px;
  padding: 8px 8px 8px 16px; border: 1px solid @ff_surface_alt; box-shadow: 0 4px 14px rgba(0, 0, 0, 0.45); }
.toast.toast-warning { border-color: @ff_warning; }
.toast.toast-error { border-color: @ff_error; }
.statusbar .notice-warning { color: @ff_warning; }
.statusbar .engine { font-family: monospace; }
.statusbar .engine-button { padding: 0 6px; min-height: 0; }
.midi-led { color: @ff_faint; font-size: 9pt; }
.midi-led.active { color: @ff_ok; }
.statusbar .midi-button { padding: 0 6px; min-height: 0; }
.statusbar .midi-button label { font-family: monospace; }
.statusbar .midi-button.learning { background-color: alpha(@ff_accent, 0.25); }
.midi-port { padding: 6px 10px; }
popover.ff-menu > contents { padding: 4px; }
popover.ff-menu button { padding: 3px 12px; min-height: 24px; }
scrollbar { background-color: @ff_bg; }
.audio-settings { padding: 16px; }

/* Plugin browser */
.plugin-browser searchentry { min-height: 30px; }
.plugin-browser .linked button { padding: 2px 12px; }
.plugin-browser .linked button:checked { background-image: none; background-color: alpha(@ff_accent, 0.22); color: @ff_fg; box-shadow: inset 0 -2px @ff_accent; }
.plugin-sidebar { background-color: @ff_surface; padding: 6px 0; }

/* Generic plugin editor */
.plugin-editor .param-search { margin: 8px 12px; min-height: 30px; }
.plugin-editor .param-list { background-color: transparent; }
.plugin-editor .param-list row { padding: 0; }
.plugin-editor .param-list row:hover { background-color: alpha(@ff_fg, 0.04); }
.plugin-editor .param-row { padding: 3px 16px; }
.plugin-editor .param-name { color: @ff_fg; }
.plugin-editor .param-value { font-family: monospace; font-size: 9.5pt; color: @ff_accent; }
.plugin-editor .param-module { font-size: 8pt; font-weight: 800; letter-spacing: 1px; color: @ff_faint; margin: 14px 16px 4px 16px; }
.plugin-editor .bypass-toggle:checked { background-image: none; background-color: alpha(@ff_warning, 0.25); color: @ff_fg; }
.plugin-sidebar row { padding: 0; border-radius: 6px; margin: 0 6px; }
.plugin-sidebar row:selected { background-color: @ff_surface_alt; }
.sidebar-heading { font-size: 8pt; font-weight: 800; letter-spacing: 1px; color: @ff_faint; margin: 12px 10px 4px 10px; }
.sidebar-item { padding: 5px 10px; }
.sidebar-count { color: @ff_dim; font-size: 9pt; font-feature-settings: "tnum"; }
.browser-top { padding: 10px 16px 6px 16px; }
.browser-subtitle { font-weight: 700; color: @ff_fg; }
.plugin-list { background-color: @ff_bg; }
.plugin-list row { padding: 0; border-bottom: 1px solid @ff_surface; }
.plugin-list row:selected { background-color: alpha(@ff_accent, 0.14); box-shadow: inset 3px 0 @ff_accent; }
.plugin-list row:hover:not(:selected) { background-color: @ff_surface; }
.plugin-row { padding: 9px 16px; }
.plugin-name { font-weight: 700; font-size: 10.5pt; color: @ff_fg; }
.plugin-sub { color: @ff_dim; font-size: 9pt; }
.avatar { min-width: 34px; min-height: 34px; border-radius: 9px; font-weight: 800; font-size: 10pt; color: #121212; }
.avatar-large { min-width: 64px; min-height: 64px; border-radius: 16px; font-size: 18pt; margin-bottom: 4px; }
.av0 { background-image: linear-gradient(135deg, #ff9b6a, #ff6a3d); }
.av1 { background-image: linear-gradient(135deg, #8fd6ff, #4f9de8); }
.av2 { background-image: linear-gradient(135deg, #b9f28a, #5fc27a); }
.av3 { background-image: linear-gradient(135deg, #f7d774, #e5a93b); }
.av4 { background-image: linear-gradient(135deg, #d7a6ff, #9a6be8); }
.av5 { background-image: linear-gradient(135deg, #ffa6c9, #e8638f); }
.badge { font-size: 8pt; font-weight: 700; padding: 1px 7px; border-radius: 10px; margin-left: 4px; }
.badge-effect { background-color: alpha(@ff_selection, 0.2); color: @ff_fg; }
.badge-instrument { background-color: alpha(#c77dff, 0.22); color: @ff_fg; }
.badge-builtin { background-color: @ff_surface_alt; color: @ff_dim; }
.badge-clap { background-color: alpha(@ff_accent, 0.22); color: @ff_fg; }
.badge-vst3 { background-color: alpha(@ff_selection, 0.3); color: @ff_fg; }
.edit-toggle { font-weight: 700; padding-left: 10px; padding-right: 10px; }
.edit-toggle.edit-active { background-color: alpha(@ff_accent, 0.35); color: @ff_fg; }
.badge-other { background-color: @ff_surface_alt; color: @ff_dim; }
.plugin-detail { padding: 22px 20px 18px 20px; background-color: @ff_surface; }
.detail-title { font-size: 16pt; font-weight: 800; color: @ff_fg; }
.detail-grid { margin-top: 10px; font-size: 9.5pt; }
.chip { font-size: 8pt; padding: 2px 8px; border-radius: 10px; background-color: @ff_surface_alt; color: @ff_dim; }
.place-button { min-height: 34px; font-weight: 700; border-radius: 8px; }
"#;

fn css_color(c: Color) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.a >= 1.0 {
        format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
    } else {
        format!("rgba({}, {}, {}, {:.3})", b(c.r), b(c.g), b(c.b), c.a)
    }
}

/// The stylesheet for `theme`.
pub fn css(theme: &Theme) -> String {
    let ui = &theme.ui;
    let led = &theme.console.led;
    let header_top = if theme.dark {
        ui.surface.lighten(0.06)
    } else {
        ui.surface.lighten(0.3)
    };
    let defines = [
        ("ff_bg", ui.background),
        ("ff_fg", ui.text),
        ("ff_dim", ui.text_dim),
        ("ff_faint", ui.text_faint),
        ("ff_surface", ui.surface),
        ("ff_surface_alt", ui.surface_alt),
        ("ff_border", ui.border),
        ("ff_accent", ui.accent),
        ("ff_selection", ui.selection),
        ("ff_header_top", header_top),
        ("ff_header_bottom", ui.surface),
        ("ff_status", ui.background.mix(ui.border, 0.35)),
        ("ff_ok", led.solo),
        ("ff_warning", led.mute),
        ("ff_error", led.record),
    ];
    let mut out = String::new();
    for (name, color) in defines {
        out.push_str(&format!("@define-color {name} {};\n", css_color(color)));
    }
    out.push_str(CSS);
    out
}

/// Apply `theme` to GTK: its light/dark variant and FaderFrame's CSS
/// (replacing the previous theme's).
pub fn install(theme: &Theme) {
    if let Some(settings) = gtk::Settings::default() {
        // GTK's own widgets (buttons, entries, dialogs) follow the skin, not
        // the desktop's GTK theme: a light skin on a dark desktop otherwise
        // gets dark buttons.
        settings.set_gtk_theme_name(Some("Adwaita"));
        settings.set_gtk_application_prefer_dark_theme(theme.dark);
        // GTK ≥ 4.20 chooses the variant from its colour scheme.
        let nick = if theme.dark { "dark" } else { "light" };
        if let Some(pspec) = settings.find_property("gtk-interface-color-scheme")
            && let Some(class) = glib::EnumClass::with_type(pspec.value_type())
            && let Some(value) = class.to_value_by_nick(nick)
        {
            settings.set_property_from_value("gtk-interface-color-scheme", &value);
        }
    }
    let Some(display) = gdk::Display::default() else {
        glib::g_warning!("faderframe", "no display available");
        return;
    };
    PROVIDER.with(|p| {
        let mut p = p.borrow_mut();
        let provider = p.get_or_insert_with(|| {
            let provider = gtk::CssProvider::new();
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            provider
        });
        provider.load_from_string(&css(theme));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_skin_defines_every_colour_it_uses() {
        for theme in Theme::all() {
            let css = css(&theme);
            for used in css.match_indices('@').map(|(i, _)| &css[i + 1..]) {
                let name: String = used
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if name == "define" {
                    continue;
                }
                assert!(
                    css.contains(&format!("@define-color {name} ")),
                    "{}: @{name} is not defined",
                    theme.id
                );
            }
        }
        assert_eq!(css_color(Color::hex(0x0a0b0c)), "#0a0b0c");
    }
}

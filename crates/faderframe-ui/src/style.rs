//! FaderFrame's GTK stylesheet, generated from the active [`Theme`]: the
//! theme's colours become `@define-color` names the rules below use, and
//! GTK's own light or dark variant follows the theme.

use faderframe_ui_canvas::{Chrome, Color, Theme};
use gtk::{gdk, glib, prelude::*};
use std::cell::RefCell;

thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    /// Displays besides the default one that windows open on (the X11
    /// display of framed plugin editors): styled like it.
    static OTHER_DISPLAYS: RefCell<Vec<gdk::Display>> = const { RefCell::new(Vec::new()) };
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
.transport button.quantize-toggle { font-weight: 800; }
.transport button.q-active { color: @ff_accent; }
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
.master-panel { border-left: 1px solid @ff_border; }

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
.badge-midi { background-color: alpha(#c6e05c, 0.22); color: @ff_fg; }
.badge-builtin { background-color: @ff_surface_alt; color: @ff_dim; }
.badge-clap { background-color: alpha(@ff_accent, 0.22); color: @ff_fg; }
.badge-vst3 { background-color: alpha(@ff_selection, 0.3); color: @ff_fg; }
.badge-lv2 { background-color: alpha(#c6e05c, 0.18); color: @ff_fg; }
.edit-toggle { font-weight: 700; padding-left: 10px; padding-right: 10px; }
.edit-toggle.edit-active { background-color: alpha(@ff_accent, 0.35); color: @ff_fg; }
.inspector-toggle { font-family: serif; font-style: italic; font-weight: 800; padding-left: 11px; padding-right: 11px; }
.dock-collapsed { background-color: @ff_surface; border-top: 1px solid @ff_border; padding: 0 4px; }
.dock-collapsed button { min-height: 22px; padding: 0 10px; color: @ff_dim; }
.badge-other { background-color: @ff_surface_alt; color: @ff_dim; }
.plugin-detail { padding: 22px 20px 18px 20px; background-color: @ff_surface; }
.detail-title { font-size: 16pt; font-weight: 800; color: @ff_fg; }
.detail-grid { margin-top: 10px; font-size: 9.5pt; }
.chip { font-size: 8pt; padding: 2px 8px; border-radius: 10px; background-color: @ff_surface_alt; color: @ff_dim; }
.place-button { min-height: 34px; font-weight: 700; border-radius: 8px; }
.badge-rt-ok { background-color: alpha(@ff_ok, 0.25); color: @ff_fg; }
.badge-rt-warn { background-color: alpha(@ff_warning, 0.3); color: @ff_fg; }
.badge-rt-bad { background-color: alpha(@ff_error, 0.35); color: @ff_fg; }
.badge-rt-failed { background-color: @ff_surface_alt; color: @ff_dim; }
.check-section { margin-top: 14px; font-size: 9.5pt; }
.check-heading { font-weight: 800; font-size: 10pt; color: @ff_fg; }
.check-verdict { font-weight: 700; padding: 2px 8px; border-radius: 8px; }
.check-numbers { font-size: 8.5pt; color: @ff_dim; }
.check-status { color: @ff_dim; font-size: 9pt; }
.favorite-star { min-width: 26px; min-height: 26px; padding: 0; font-size: 13pt; color: @ff_faint; }
.favorite-star.starred { color: @ff_warning; }
.favorite-toggle.starred { color: @ff_warning; }
.plugin-missing .plugin-name, .plugin-missing .plugin-sub { color: @ff_faint; }
.badge-missing { background-color: alpha(@ff_error, 0.25); color: @ff_fg; }
.favorites-section { margin-top: 10px; font-size: 9.5pt; }
.sidebar-new { margin: 2px 6px 2px 6px; font-size: 9pt; }
"#;

/// The planes chrome ([`Chrome::Planes`]) over the rules above: flat header
/// and tabs set apart by hairlines, pill buttons, rounded floating menus
/// and popovers with soft elevation, the accent for check marks, switches,
/// selections and focus rings.
const PLANES_CSS: &str = r#"
window, dialog { background-color: @ff_bg; }
headerbar {
    background-image: none;
    background-color: @ff_bg;
    border-bottom: 1px solid @ff_hairline;
}
headerbar popovermenubar > item { border-radius: 8px; padding: 4px 10px; }
headerbar popovermenubar > item:hover,
headerbar popovermenubar > item:selected { background-color: @ff_hover; }
headerbar button, .transport button {
    border-radius: 999px;
    background-image: none;
    background-color: transparent;
    border: none;
    box-shadow: none;
}
headerbar button:hover, .transport button:hover { background-image: image(@ff_hover); }
headerbar button:checked, .transport button:checked { background-image: image(alpha(@ff_accent, 0.16)); color: @ff_accent; }
.transport button.play-active { color: @ff_accent; }
.transport button.loop-active { color: @ff_accent; }
.transport button.click-active { color: @ff_accent_2; }
.transport button.q-active { color: @ff_accent; }
.edit-toggle.edit-active { background-image: image(alpha(@ff_accent, 0.18)); color: @ff_accent; }
.lcd { border-radius: 12px; }
button {
    border-radius: 8px;
    background-image: image(@ff_surface_alt);
    border-color: @ff_hairline;
    box-shadow: none;
}
button:hover { background-image: image(shade(@ff_surface_alt, 1.18)); }
button:active, button:checked { background-image: image(alpha(@ff_accent, 0.2)); color: @ff_fg; }
button.flat, .tab-button, popover.ff-menu button { background-image: none; border-color: transparent; }
button.flat:hover, .tab-button:hover { background-image: image(@ff_hover); }
button.suggested-action { background-image: image(@ff_accent); border-color: @ff_accent; color: @ff_on_accent; }
button.destructive-action { background-image: image(alpha(@ff_error, 0.85)); border-color: @ff_error; color: @ff_fg; }
popover > contents {
    background-color: @ff_surface;
    border-radius: 12px;
    border: 1px solid @ff_hairline;
    box-shadow: 0 14px 36px rgba(0, 0, 0, 0.6), 0 2px 6px rgba(0, 0, 0, 0.4);
}
popover > arrow { background-color: @ff_surface; border: 1px solid @ff_hairline; }
popover.menu > contents { padding: 6px; }
popover.menu modelbutton, popover.ff-menu button { border-radius: 7px; }
popover.menu modelbutton:hover, popover.menu modelbutton:selected { background-color: alpha(@ff_accent, 0.13); }
popover.ff-menu button:hover { background-image: image(alpha(@ff_accent, 0.13)); }
popover.menu modelbutton check, popover.menu modelbutton radio,
popover.menu modelbutton arrow { color: @ff_accent; }
popover.menu modelbutton accelerator { color: @ff_faint; }
popover.menu separator { background-color: @ff_hairline; margin: 4px 6px; }
tooltip, tooltip > * { border-radius: 8px; }
tooltip { background-color: @ff_surface_alt; color: @ff_fg; border: 1px solid @ff_hairline; }
notebook.dock-tabs > header { background-color: @ff_bg; border-bottom: 1px solid @ff_hairline; }
notebook.dock-tabs > header > tabs > tab { border-radius: 8px 8px 0 0; }
notebook.dock-tabs > header > tabs > tab:hover { background-color: @ff_hover; }
notebook.dock-tabs > header > tabs > tab:checked {
    background-color: transparent;
    color: @ff_fg;
    box-shadow: inset 0 -2px @ff_accent;
}
paned > separator { background-color: @ff_bg; }
.statusbar { background-color: @ff_bg; border-top: 1px solid @ff_hairline; }
.dock-collapsed { background-color: @ff_bg; border-top: 1px solid @ff_hairline; }
entry, spinbutton, searchentry, dropdown > button, combobox > box > button {
    border-radius: 8px;
    background-image: none;
    background-color: @ff_well;
    box-shadow: inset 0 0 0 1px @ff_hairline;
}
*:focus-visible, entry:focus-within, spinbutton:focus-within { outline-color: alpha(@ff_accent, 0.8); }
textview > text > selection:focus-within, entry > text > selection:focus-within,
label > selection:focus-within { background-color: alpha(@ff_accent, 0.35); }
switch, switch slider { border-radius: 999px; }
switch:checked { background-color: @ff_accent; border-color: @ff_accent; color: @ff_on_accent; }
check, radio { background-image: image(@ff_well); border-color: alpha(@ff_fg, 0.22); box-shadow: none; }
check:checked, radio:checked, check:indeterminate, radio:indeterminate {
    background-image: image(@ff_accent);
    border-color: @ff_accent;
    color: @ff_on_accent;
}
check:checked:hover, radio:checked:hover { background-image: image(shade(@ff_accent, 1.1)); }
popover.menu check, popover.menu radio {
    background-image: image(transparent);
    border-color: transparent;
    color: @ff_accent;
}
popover.menu radio { border-color: alpha(@ff_fg, 0.22); }
progressbar > trough > progress, scale > trough > highlight { background-color: @ff_accent; border-color: @ff_accent; }
scale > trough > slider { background-image: image(@ff_fg); border-color: transparent; border-radius: 999px; }
list, listview { background-color: @ff_surface; }
row:selected { background-color: alpha(@ff_accent, 0.14); }
.plugin-list row:selected { background-color: alpha(@ff_accent, 0.12); box-shadow: inset 2px 0 @ff_accent; }
.toast { border-radius: 12px; border-color: @ff_hairline; }
scrollbar { background-color: transparent; }
scrollbar slider { border-radius: 999px; background-color: alpha(@ff_fg, 0.24); }
scrollbar slider:hover { background-color: alpha(@ff_fg, 0.4); }
scrollbar slider:active { background-color: @ff_accent; }
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
    if theme.chrome == Chrome::Planes {
        let planes = [
            ("ff_accent_2", ui.accent_2),
            ("ff_hairline", ui.text.with_alpha(0.07)),
            ("ff_hover", ui.text.with_alpha(0.07)),
            ("ff_well", ui.background.mix(ui.border, 0.4)),
            ("ff_on_accent", ui.background),
        ];
        for (name, color) in planes {
            out.push_str(&format!("@define-color {name} {};\n", css_color(color)));
        }
        out.push_str(PLANES_CSS);
    }
    out
}

/// GTK's own widgets (buttons, entries, dialogs) follow the skin, not the
/// desktop's GTK theme: a light skin on a dark desktop otherwise gets dark
/// buttons.
fn apply_settings(settings: &gtk::Settings, theme: &Theme) {
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

/// Apply `theme` to GTK: its light/dark variant and FaderFrame's CSS
/// (replacing the previous theme's).
pub fn install(theme: &Theme) {
    if let Some(settings) = gtk::Settings::default() {
        apply_settings(&settings, theme);
    }
    OTHER_DISPLAYS.with(|d| {
        for display in d.borrow().iter() {
            apply_settings(&gtk::Settings::for_display(display), theme);
        }
    });
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
            OTHER_DISPLAYS.with(|d| {
                for other in d.borrow().iter() {
                    gtk::style_context_add_provider_for_display(
                        other,
                        &provider,
                        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                    );
                }
            });
            provider
        });
        provider.load_from_string(&css(theme));
    });
}

/// Style windows on `display` (not the default one) like the others, now
/// and after every theme change (plugin editor frames on X11 open on
/// GDK's X11 display).
pub fn add_display(display: &gdk::Display, theme: &Theme) {
    if OTHER_DISPLAYS.with(|d| d.borrow().contains(display)) {
        return;
    }
    OTHER_DISPLAYS.with(|d| d.borrow_mut().push(display.clone()));
    let settings = gtk::Settings::for_display(display);
    // The desktop's choices reach the default display only (portal or
    // XSETTINGS): the same title bar buttons and fonts here.
    if let Some(default) = gtk::Settings::default() {
        for name in [
            "gtk-decoration-layout",
            "gtk-font-name",
            "gtk-icon-theme-name",
        ] {
            settings.set_property_from_value(name, &default.property_value(name));
        }
    }
    apply_settings(&settings, theme);
    PROVIDER.with(|p| {
        if let Some(provider) = p.borrow().as_ref() {
            gtk::style_context_add_provider_for_display(
                display,
                provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
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

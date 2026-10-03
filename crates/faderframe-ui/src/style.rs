use gtk::{gdk, glib};

const CSS: &str = r#"
window { background-color: #17181b; color: #e6e3dc; }
headerbar {
    background-image: linear-gradient(to bottom, #2b2d32, #222428);
    border-bottom: 1px solid #0b0c0e;
    color: #e6e3dc;
    min-height: 46px;
    box-shadow: none;
}
headerbar popovermenubar { background: transparent; }
.transport { margin: 0 6px; }
.transport button { min-width: 28px; min-height: 28px; border-radius: 7px; padding: 0 3px; }
.transport button.play-active { color: #5ad66b; }
.transport button.rec-active { color: #ff4b4b; }
.transport button.loop-active { color: #ff8a5c; }
.lcd { border-radius: 6px; margin: 4px 6px; }
notebook.dock-tabs > header {
    background-color: #1f2125;
    border-bottom: 1px solid #0b0c0e;
    box-shadow: none;
}
notebook.dock-tabs > header > tabs > tab {
    padding: 2px 10px;
    min-height: 24px;
    color: #9a9c9f;
    border: none;
}
notebook.dock-tabs > header > tabs > tab:checked {
    color: #f0ede6;
    background-color: #2a2d32;
    box-shadow: inset 0 -2px #ff6a3d;
}
.tab-button { min-width: 16px; min-height: 16px; padding: 0; margin: 0; }
paned > separator { background-color: #0b0c0e; min-width: 5px; min-height: 5px; }
.statusbar {
    background-color: #131416;
    color: #8e9094;
    border-top: 1px solid #0b0c0e;
    padding: 2px 10px;
    font-size: 9.5pt;
}
.statusbar .notice-error { color: #ff7a66; }
.statusbar .notice-warning { color: #e8c35a; }
.statusbar .engine { font-family: monospace; }
popover.ff-menu > contents { padding: 4px; }
popover.ff-menu button { padding: 3px 12px; min-height: 24px; }
scrollbar { background-color: #17181b; }
.audio-settings { padding: 16px; }

/* Plugin browser */
.plugin-browser searchentry { min-height: 30px; }
.plugin-browser .linked button { padding: 2px 12px; }
.plugin-browser .linked button:checked { background-image: none; background-color: rgba(255,106,61,0.22); color: #ffb38f; box-shadow: inset 0 -2px #ff6a3d; }
.plugin-sidebar { background-color: #1b1c20; padding: 6px 0; }

/* Generic plugin editor */
.plugin-editor .param-search { margin: 8px 12px; min-height: 30px; }
.plugin-editor .param-list { background-color: transparent; }
.plugin-editor .param-list row { padding: 0; }
.plugin-editor .param-list row:hover { background-color: rgba(255,255,255,0.03); }
.plugin-editor .param-row { padding: 3px 16px; }
.plugin-editor .param-name { color: #c9ccd1; }
.plugin-editor .param-value { font-family: monospace; font-size: 9.5pt; color: #ffb38f; }
.plugin-editor .param-module { font-size: 8pt; font-weight: 800; letter-spacing: 1px; color: #6f7278; margin: 14px 16px 4px 16px; }
.plugin-editor .bypass-toggle:checked { background-image: none; background-color: rgba(232,195,90,0.25); color: #ffe08a; }
.plugin-sidebar row { padding: 0; border-radius: 6px; margin: 0 6px; }
.plugin-sidebar row:selected { background-color: #2c3038; }
.sidebar-heading { font-size: 8pt; font-weight: 800; letter-spacing: 1px; color: #6f7278; margin: 12px 10px 4px 10px; }
.sidebar-item { padding: 5px 10px; }
.sidebar-count { color: #7c7f85; font-size: 9pt; font-feature-settings: "tnum"; }
.browser-top { padding: 10px 16px 6px 16px; }
.browser-subtitle { font-weight: 700; color: #cfccc5; }
.plugin-list { background-color: #17181b; }
.plugin-list row { padding: 0; border-bottom: 1px solid #1f2125; }
.plugin-list row:selected { background-color: rgba(255,106,61,0.14); box-shadow: inset 3px 0 #ff6a3d; }
.plugin-list row:hover:not(:selected) { background-color: #1e2024; }
.plugin-row { padding: 9px 16px; }
.plugin-name { font-weight: 700; font-size: 10.5pt; color: #ece9e2; }
.plugin-sub { color: #8b8e93; font-size: 9pt; }
.avatar { min-width: 34px; min-height: 34px; border-radius: 9px; font-weight: 800; font-size: 10pt; color: #121212; }
.avatar-large { min-width: 64px; min-height: 64px; border-radius: 16px; font-size: 18pt; margin-bottom: 4px; }
.av0 { background-image: linear-gradient(135deg, #ff9b6a, #ff6a3d); }
.av1 { background-image: linear-gradient(135deg, #8fd6ff, #4f9de8); }
.av2 { background-image: linear-gradient(135deg, #b9f28a, #5fc27a); }
.av3 { background-image: linear-gradient(135deg, #f7d774, #e5a93b); }
.av4 { background-image: linear-gradient(135deg, #d7a6ff, #9a6be8); }
.av5 { background-image: linear-gradient(135deg, #ffa6c9, #e8638f); }
.badge { font-size: 8pt; font-weight: 700; padding: 1px 7px; border-radius: 10px; margin-left: 4px; }
.badge-effect { background-color: #263040; color: #9cc3f2; }
.badge-instrument { background-color: #3a2b40; color: #e0aef5; }
.badge-builtin { background-color: #2a2c30; color: #b5b2ab; }
.badge-clap { background-color: #3d2a20; color: #ffb38f; }
.badge-other { background-color: #2a2c30; color: #b5b2ab; }
.plugin-detail { padding: 22px 20px 18px 20px; background-color: #1b1c20; }
.detail-title { font-size: 16pt; font-weight: 800; color: #f2efe8; }
.detail-grid { margin-top: 10px; font-size: 9.5pt; }
.chip { font-size: 8pt; padding: 2px 8px; border-radius: 10px; background-color: #26282d; color: #b9b6af; }
.place-button { min-height: 34px; font-weight: 700; border-radius: 8px; }
"#;

/// Dark theme preference and FaderFrame's CSS.
pub fn install() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }
    let Some(display) = gdk::Display::default() else {
        glib::g_warning!("faderframe", "no display available");
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

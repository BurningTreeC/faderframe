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

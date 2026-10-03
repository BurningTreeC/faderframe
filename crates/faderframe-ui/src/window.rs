//! Main window chrome: header bar (menu, transport, LCD display, workspace
//! switcher), the dock slot and the status bar.

use crate::canvas::CanvasWidget;
use crate::state::AppState;
use crate::transport_display::TransportDisplay;
use faderframe_audio::format_sample_rate;
use faderframe_session::{Action, NoticeLevel, Session, WorkspaceAction};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;

/// Widgets the frame tick updates.
pub struct Chrome {
    pub display: CanvasWidget,
    pub play: gtk::Button,
    pub record: gtk::Button,
    pub looping: gtk::Button,
    pub notice: gtk::Label,
    pub engine: gtk::Label,
    pub workspaces: gtk::DropDown,
    pub workspace_guard: Rc<Cell<bool>>,
}

fn set_class(w: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        w.add_css_class(class);
    } else {
        w.remove_css_class(class);
    }
}

impl Chrome {
    pub fn update(&self, s: &Session, full: bool) {
        let t = s.transport();
        if t.playing {
            self.display.queue_draw();
        }
        self.play.set_icon_name(if t.playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        set_class(&self.play, "play-active", t.playing);
        set_class(&self.record, "rec-active", t.recording);
        set_class(&self.looping, "loop-active", s.project().loop_enabled);
        if !full {
            return;
        }
        self.display.queue_draw();
        match s.latest_notice().filter(|n| n.at.elapsed().as_secs() < 10) {
            Some(n) => {
                self.notice.set_text(&n.text);
                set_class(&self.notice, "notice-error", n.level == NoticeLevel::Error);
                set_class(
                    &self.notice,
                    "notice-warning",
                    n.level == NoticeLevel::Warning,
                );
            }
            None => self.notice.set_text(""),
        }
        let m = s.metrics();
        let engine = match (s.stream_info(), s.stream_status()) {
            (Some(info), Some(status)) => format!(
                "{} · {} · {} fr ({:.1} ms) · DSP {:>3.0}% · p99 {:>3.0}% · xruns {}",
                info.backend.to_uppercase(),
                format_sample_rate(status.sample_rate),
                status.buffer_size,
                status.buffer_size as f64 * 1000.0 / status.sample_rate.max(1) as f64,
                m.last_load() * 100.0,
                m.p99_load() * 100.0,
                status.xruns
            ),
            _ => "audio stopped".to_string(),
        };
        if self.engine.text() != engine {
            self.engine.set_text(&engine);
        }
        let active = s.workspace().active as u32;
        if self.workspaces.selected() != active {
            self.workspace_guard.set(true);
            self.workspaces.set_selected(active);
            self.workspace_guard.set(false);
        }
    }
}

fn icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.set_action_name(Some(action));
    b
}

pub fn menu_model() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = |items: &[(&str, &str)]| {
        let s = gio::Menu::new();
        for (label, action) in items {
            s.append(Some(label), Some(action));
        }
        s
    };
    let file = gio::Menu::new();
    file.append_section(
        None,
        &section(&[
            ("New Project", "app.new"),
            ("New Demo Session", "app.new-demo"),
            ("Open…", "app.open"),
        ]),
    );
    file.append_section(
        None,
        &section(&[("Save", "app.save"), ("Save As…", "app.save-as")]),
    );
    file.append_section(None, &section(&[("Render / Export…", "app.render")]));
    file.append_section(None, &section(&[("Preferences…", "app.preferences")]));
    file.append_section(None, &section(&[("Quit", "app.quit")]));
    menu.append_submenu(Some("_File"), &file);

    let edit = gio::Menu::new();
    edit.append_section(
        None,
        &section(&[("Undo", "app.undo"), ("Redo", "app.redo")]),
    );
    edit.append_section(
        None,
        &section(&[
            ("Delete Selection", "app.delete"),
            ("Split Clips at Playhead", "app.split"),
        ]),
    );
    edit.append_section(
        None,
        &section(&[
            ("Snap to Grid", "app.toggle-snap"),
            ("Follow Playhead", "app.toggle-follow"),
        ]),
    );
    menu.append_submenu(Some("_Edit"), &edit);

    let track = gio::Menu::new();
    track.append_section(
        None,
        &section(&[
            ("Add Audio Track", "app.add-audio"),
            ("Add Instrument Track", "app.add-instrument"),
            ("Add MIDI Track", "app.add-midi"),
            ("Add Bus", "app.add-bus"),
            ("Add Aux (FX Return)", "app.add-aux"),
        ]),
    );
    track.append_section(
        None,
        &section(&[("Remove Selected Tracks", "app.remove-tracks")]),
    );
    menu.append_submenu(Some("_Track"), &track);

    let transport = gio::Menu::new();
    transport.append_section(
        None,
        &section(&[
            ("Play / Pause", "app.play"),
            ("Stop", "app.stop"),
            ("Return to Start", "app.to-start"),
        ]),
    );
    transport.append_section(
        None,
        &section(&[("Loop", "app.loop"), ("Record Mode", "app.record")]),
    );
    transport.append_section(None, &section(&[("Panic (All Notes Off)", "app.panic")]));
    menu.append_submenu(Some("T_ransport"), &transport);

    let view = gio::Menu::new();
    view.append_section(
        None,
        &section(&[
            ("Mixer", "app.show-mixer"),
            ("Piano Roll", "app.show-piano-roll"),
            ("Automation", "app.show-automation"),
            ("Show / Hide Bottom Dock", "app.toggle-dock"),
        ]),
    );
    view.append_section(
        None,
        &section(&[
            ("Detach Mixer", "app.detach-mixer"),
            ("Detach Piano Roll", "app.detach-piano-roll"),
            ("Dock All Windows", "app.dock-all"),
        ]),
    );
    let ws = gio::Menu::new();
    for (i, name) in ["Recording", "Editing", "Mixing", "MIDI", "Mastering"]
        .iter()
        .enumerate()
    {
        ws.append(Some(name), Some(&format!("app.workspace-{}", i + 1)));
    }
    ws.append_section(
        None,
        &section(&[("Reset Current Workspace", "app.workspace-reset")]),
    );
    view.append_submenu(Some("Workspace"), &ws);
    menu.append_submenu(Some("_View"), &view);

    let audio = gio::Menu::new();
    audio.append_section(
        None,
        &section(&[
            ("Audio Settings…", "app.audio-settings"),
            ("Restart Audio", "app.restart-audio"),
        ]),
    );
    menu.append_submenu(Some("_Audio"), &audio);

    let help = gio::Menu::new();
    help.append(Some("About FaderFrame"), Some("app.about"));
    menu.append_submenu(Some("_Help"), &help);
    menu
}

/// Build the main window and register its chrome with the app state.
pub fn build(app: &Rc<AppState>) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(&app.app)
        .title("FaderFrame")
        .default_width(1560)
        .default_height(960)
        .show_menubar(false)
        .build();

    let header = gtk::HeaderBar::new();
    let menubar = gtk::PopoverMenuBar::from_model(Some(&menu_model()));
    header.pack_start(&menubar);

    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    transport.add_css_class("transport");
    transport.append(&icon_button(
        "media-skip-backward-symbolic",
        "Return to start (Home)",
        "app.to-start",
    ));
    transport.append(&icon_button(
        "media-playback-stop-symbolic",
        "Stop (twice: to start)",
        "app.stop",
    ));
    let play = icon_button(
        "media-playback-start-symbolic",
        "Play / pause (Space)",
        "app.play",
    );
    transport.append(&play);
    let record = icon_button(
        "media-record-symbolic",
        "Record mode (Shift+R)",
        "app.record",
    );
    transport.append(&record);
    let looping = icon_button("media-playlist-repeat-symbolic", "Loop (L)", "app.loop");
    transport.append(&looping);

    let display = CanvasWidget::new(app, Box::new(TransportDisplay));
    display.set_size_request(290, 38);
    display.set_hexpand(false);
    display.set_vexpand(false);
    display.add_css_class("lcd");
    app.register_canvas(&display);
    transport.append(&display);
    header.set_title_widget(Some(&transport));

    let names: Vec<String> = app
        .session
        .borrow()
        .workspace()
        .workspaces
        .iter()
        .map(|w| w.name.clone())
        .collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let workspaces = gtk::DropDown::from_strings(&name_refs);
    workspaces.set_tooltip_text(Some("Workspace (Ctrl+1 … Ctrl+5)"));
    let guard = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(app);
    workspaces.connect_selected_notify(glib::clone!(
        #[strong]
        guard,
        move |d| {
            if guard.get() {
                return;
            }
            if let Some(app) = weak.upgrade() {
                app.dispatch(Action::Workspace(WorkspaceAction::Switch(
                    d.selected() as usize
                )));
            }
        }
    ));
    header.pack_end(&workspaces);
    let render = gtk::Button::from_icon_name("document-save-as-symbolic");
    render.set_action_name(Some("app.render"));
    render.set_tooltip_text(Some("Render / export audio (Ctrl+Shift+R)"));
    header.pack_end(&render);
    window.set_titlebar(Some(&header));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    slot.set_hexpand(true);
    slot.set_vexpand(true);
    content.append(&slot);

    let status = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status.add_css_class("statusbar");
    let notice = gtk::Label::new(None);
    notice.set_xalign(0.0);
    notice.set_hexpand(true);
    notice.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let engine = gtk::Label::new(None);
    engine.add_css_class("engine");
    let platform = gtk::Label::new(Some(&platform_label(&window)));
    status.append(&notice);
    status.append(&engine);
    status.append(&platform);
    content.append(&status);
    window.set_child(Some(&content));

    app.dock.borrow_mut().main_slot = Some(slot);
    *app.chrome.borrow_mut() = Some(Chrome {
        display,
        play,
        record,
        looping,
        notice,
        engine,
        workspaces,
        workspace_guard: guard,
    });
    *app.window.borrow_mut() = Some(window.clone());
    window
}

/// "Wayland · GTK 4.22" — makes the windowing backend visible at a glance.
fn platform_label(window: &gtk::ApplicationWindow) -> String {
    let display = gtk::prelude::WidgetExt::display(window);
    let backend = match display.type_().name() {
        "GdkWaylandDisplay" => "Wayland",
        "GdkX11Display" => "X11",
        "GdkWin32Display" => "Win32",
        "GdkMacosDisplay" => "macOS",
        other => other,
    };
    format!(
        "{backend} · GTK {}.{}.{}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version()
    )
}

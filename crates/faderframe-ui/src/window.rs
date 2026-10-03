//! Main window chrome: header bar (menu, transport, LCD display, workspace
//! switcher), the dock slot and the status bar.

use crate::canvas::CanvasWidget;
use crate::state::AppState;
use crate::transport_display::TransportDisplay;
use faderframe_audio::format_sample_rate;
use faderframe_engine::MetronomeMode;
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
    pub metronome: gtk::Button,
    pub edit_button: gtk::Button,
    pub edit_bar: CanvasWidget,
    /// Measures how tall the edit toolbar must be at the window's width.
    pub edit_bar_layout: faderframe_view_arranger::edit_bar::EditToolbarView,
    pub notice: gtk::Label,
    /// Warnings and errors pop up here for a few seconds.
    pub toast: Toast,
    pub engine: gtk::Label,
    pub midi_button: gtk::Button,
    pub midi_led: gtk::Label,
    pub midi_text: gtk::Label,
    pub import_box: gtk::Box,
    pub import_bar: gtk::ProgressBar,
    pub workspaces: gtk::DropDown,
    pub workspace_guard: Rc<Cell<bool>>,
}

/// A message that slides in at the top of the window.
pub struct Toast {
    pub revealer: gtk::Revealer,
    label: gtk::Label,
    frame: gtk::Box,
    /// Time stamp of the notice shown last, and until when it stays.
    shown: Cell<Option<std::time::Instant>>,
    until: Cell<Option<std::time::Instant>>,
}

impl Toast {
    fn new() -> Self {
        let label = gtk::Label::new(None);
        label.set_wrap(true);
        label.set_max_width_chars(80);
        label.set_xalign(0.0);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_valign(gtk::Align::Center);
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        frame.add_css_class("toast");
        frame.append(&label);
        frame.append(&close);
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_halign(gtk::Align::Center);
        revealer.set_valign(gtk::Align::Start);
        revealer.set_margin_top(10);
        revealer.set_child(Some(&frame));
        revealer.set_can_target(true);
        let r = revealer.clone();
        close.connect_clicked(move |_| r.set_reveal_child(false));
        Self {
            revealer,
            label,
            frame,
            shown: Cell::new(None),
            until: Cell::new(None),
        }
    }

    /// Show new warnings and errors; hide after five seconds.
    fn update(&self, s: &Session) {
        let now = std::time::Instant::now();
        if let Some(n) = s.latest_notice()
            && n.level != NoticeLevel::Info
            && self.shown.get() != Some(n.at)
            && n.at.elapsed().as_secs() < 5
        {
            self.shown.set(Some(n.at));
            self.label.set_text(&n.text);
            set_class(&self.frame, "toast-error", n.level == NoticeLevel::Error);
            set_class(
                &self.frame,
                "toast-warning",
                n.level == NoticeLevel::Warning,
            );
            self.revealer.set_reveal_child(true);
            self.until
                .set(Some(now + std::time::Duration::from_secs(5)));
        }
        if self.until.get().is_some_and(|u| now >= u) {
            self.until.set(None);
            self.revealer.set_reveal_child(false);
        }
    }
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
        let click = s.record.metronome;
        if self.metronome.has_css_class("click-active") != (click != MetronomeMode::Off) {
            set_class(&self.metronome, "click-active", click != MetronomeMode::Off);
            self.metronome
                .set_tooltip_text(Some(&format!("Metronome: {} (K)", click.label())));
        }
        let show = s.editor.show_edit_toolbar;
        if self.edit_bar.is_visible() != show {
            self.edit_bar.set_visible(show);
        }
        set_class(&self.edit_button, "edit-active", show);
        if show {
            let w = self.edit_bar.width();
            if w > 0 {
                let h = self.edit_bar_layout.preferred_height(w as f32, s) as i32;
                if self.edit_bar.height_request() != h {
                    self.edit_bar.set_size_request(-1, h);
                }
            }
            self.edit_bar.queue_draw();
        }
        self.toast.update(s);
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
        let imports = s.imports();
        self.import_box.set_visible(!imports.is_empty());
        if let Some(job) = imports.first() {
            self.import_bar.set_fraction(job.fraction());
            let more = imports.len() - 1;
            let text = if more > 0 {
                format!("Importing {} (+{more} queued)", job.current_name())
            } else {
                format!("Importing {}", job.current_name())
            };
            self.import_bar.set_text(Some(&text));
        }
        let active = s.midi_active();
        if active != self.midi_led.has_css_class("active") {
            if active {
                self.midi_led.add_css_class("active");
            } else {
                self.midi_led.remove_css_class("active");
            }
        }
        let learning = s.midi_learning().is_some();
        if learning != self.midi_button.has_css_class("learning") {
            if learning {
                self.midi_button.add_css_class("learning");
            } else {
                self.midi_button.remove_css_class("learning");
            }
        }
        // MIDI learn, or the external timing source.
        let sync = s.sync_status();
        let text = if learning {
            "MIDI LEARN".to_string()
        } else {
            match sync.source {
                faderframe_session::SyncSource::Internal => "MIDI".into(),
                faderframe_session::SyncSource::MidiClock => match sync.tempo {
                    Some(t) if sync.receiving => format!("MIDI · CLK {t:.1}"),
                    _ => "MIDI · CLK –".into(),
                },
                faderframe_session::SyncSource::Mtc => match sync.timecode {
                    Some((tc, _)) if sync.receiving => format!("MIDI · MTC {tc}"),
                    _ => "MIDI · MTC –".into(),
                },
            }
        };
        if self.midi_text.text() != text {
            self.midi_text.set_text(&text);
        }
        let load = s.dsp_load();
        let engine = match (s.stream_info(), s.stream_status()) {
            (Some(info), Some(status)) => format!(
                "{} · {} · {} fr ({:.1} ms) · DSP {:>3.0}% · peak {:>3.0}% · xruns {}",
                info.backend.to_uppercase(),
                format_sample_rate(status.sample_rate),
                status.buffer_size,
                status.buffer_size as f64 * 1000.0 / status.sample_rate.max(1) as f64,
                load.average * 100.0,
                load.peak * 100.0,
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

pub fn menu_model(recent: &gio::Menu) -> gio::Menu {
    let menu = gio::Menu::new();
    let section = |items: &[(&str, &str)]| {
        let s = gio::Menu::new();
        for (label, action) in items {
            s.append(Some(label), Some(action));
        }
        s
    };
    let file = gio::Menu::new();
    file.append_section(None, &{
        let s = section(&[
            ("New Project", "app.new"),
            ("New Demo Session", "app.new-demo"),
            ("Open…", "app.open"),
        ]);
        s.append_submenu(Some("Open Recent"), recent);
        s
    });
    file.append_section(
        None,
        &section(&[("Save", "app.save"), ("Save As…", "app.save-as")]),
    );
    file.append_section(None, &section(&[("Import Audio…", "app.import-audio")]));
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
            ("Show / Hide Take Lanes", "app.toggle-take-lanes"),
            ("Show / Hide Automation", "app.toggle-automation"),
        ]),
    );
    let target = |label: &str, action: &str, arg: &str| {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&arg.to_variant()));
        item
    };
    let range = gio::Menu::new();
    for (label, arg) in [
        ("Separate Clips (B)", "separate"),
        ("Trim Clips to Selection (Ctrl+Alt+T)", "trim"),
        ("Clear Selection Range", "clear"),
        ("Insert Silence", "silence"),
        ("Copy Range (Ctrl+C)", "copy"),
        ("Cut Range (Ctrl+X)", "cut"),
        ("Paste at Playhead (Ctrl+V)", "paste"),
        ("Duplicate Range (Ctrl+D)", "duplicate"),
    ] {
        range.append_item(&target(label, "app.edit", arg));
    }
    edit.append_section(None, &range);
    let warp = gio::Menu::new();
    for (label, arg) in [
        ("Quantize Transients to Grid", "quantize"),
        ("Separate at Transients", "separate-transients"),
        ("Remove Warp", "unwarp"),
    ] {
        warp.append_item(&target(label, "app.edit", arg));
    }
    edit.append_section(None, &warp);
    let modes = gio::Menu::new();
    for (label, arg) in [
        ("Shuffle (Alt+1)", "shuffle"),
        ("Slip (Alt+2)", "slip"),
        ("Spot (Alt+3)", "spot"),
        ("Grid (Alt+4)", "grid"),
    ] {
        modes.append_item(&target(label, "app.edit-mode", arg));
    }
    let tools = gio::Menu::new();
    for (label, arg) in [
        ("Smart (Alt+S)", "smart"),
        ("Zoom (F5)", "zoom"),
        ("Trim (F6)", "trim"),
        ("Time-Stretch Trim", "stretch"),
        ("Selector (F7)", "select"),
        ("Grabber (Alt+8)", "grab"),
        ("Separation Grabber", "separate"),
        ("Scrubber (F9)", "scrub"),
        ("Pencil (F10)", "pencil"),
    ] {
        tools.append_item(&target(label, "app.edit-tool", arg));
    }
    let edit_opts = gio::Menu::new();
    edit_opts.append_submenu(Some("Edit Mode"), &modes);
    edit_opts.append_submenu(Some("Edit Tool"), &tools);
    edit_opts.append(Some("Edit Toolbar"), Some("app.toggle-edit-toolbar"));
    edit.append_section(None, &edit_opts);
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
            ("Add Audio Track (Mono)", "app.add-audio"),
            ("Add Audio Track (Stereo)", "app.add-audio-stereo"),
            ("Add Instrument Track", "app.add-instrument"),
            ("Add MIDI Track", "app.add-midi"),
            ("Add Bus", "app.add-bus"),
            ("Add Aux (FX Return)", "app.add-aux"),
        ]),
    );
    track.append_section(
        None,
        &section(&[
            ("Plugin Browser…", "app.plugin-browser"),
            ("Record-Arm Selected Tracks", "app.arm-selected"),
            ("Remove Selected Tracks", "app.remove-tracks"),
        ]),
    );
    track.append_section(
        None,
        &section(&[
            ("Save Selected Track as Preset", "app.save-track-preset"),
            ("New Track from Preset…", "app.track-from-preset"),
            ("Apply Preset to Selected Track…", "app.apply-track-preset"),
            (
                "Export Selected Track as Preset…",
                "app.export-track-preset",
            ),
        ]),
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
        &section(&[("Loop", "app.loop"), ("Record", "app.record")]),
    );
    transport.append_section(None, &crate::recording::menu());
    transport.append_section(None, &section(&[("Panic (All Notes Off)", "app.panic")]));
    menu.append_submenu(Some("T_ransport"), &transport);

    let view = gio::Menu::new();
    view.append_section(
        None,
        &section(&[
            ("Mixer", "app.show-mixer"),
            ("Piano Roll", "app.show-piano-roll"),
            ("Automation", "app.show-automation"),
            ("Performance Meter", "app.show-performance"),
            ("Show / Hide Bottom Dock", "app.toggle-dock"),
        ]),
    );
    view.append_section(
        None,
        &section(&[
            ("Detach Mixer", "app.detach-mixer"),
            ("Detach Piano Roll", "app.detach-piano-roll"),
            ("Detach Performance Meter", "app.detach-performance"),
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
    let heights = gio::Menu::new();
    for (i, (name, _)) in faderframe_view_arranger::TRACK_HEIGHTS.iter().enumerate() {
        heights.append(Some(name), Some(&format!("app.track-height-{i}")));
    }
    view.append_submenu(Some("Track Height (all tracks · Alt+wheel)"), &heights);
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
    let menubar = gtk::PopoverMenuBar::from_model(Some(&menu_model(&app.recent_menu)));
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
    let metronome = gtk::Button::new();
    metronome.set_child(Some(&crate::icons::image(
        "faderframe-metronome-symbolic",
        "♩",
    )));
    metronome.set_action_name(Some("app.toggle-metronome"));
    metronome.set_tooltip_text(Some("Metronome (K)"));
    transport.append(&metronome);
    let edit_button = gtk::Button::with_label("Edit");
    edit_button.set_tooltip_text(Some(
        "Edit toolbar: edit modes, tools, grid and nudge, options, selection (Ctrl+E)",
    ));
    edit_button.set_action_name(Some("app.toggle-edit-toolbar"));
    edit_button.add_css_class("edit-toggle");
    transport.append(&edit_button);

    let display = CanvasWidget::new(app, Box::new(TransportDisplay::new()));
    display.set_size_request(370, 38);
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
    // The edit toolbar: full width under the header bar.
    let edit_bar = CanvasWidget::new(
        app,
        Box::new(faderframe_view_arranger::edit_bar::EditToolbarView::new(
            app.theme.clone(),
        )),
    );
    edit_bar.set_size_request(-1, 36);
    edit_bar.set_hexpand(true);
    edit_bar.set_vexpand(false);
    edit_bar.set_visible(app.session.borrow().editor.show_edit_toolbar);
    app.register_canvas(&edit_bar);
    content.append(&edit_bar);
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
    // The DSP readout opens the performance meter.
    let engine_button = gtk::Button::new();
    engine_button.set_child(Some(&engine));
    engine_button.add_css_class("flat");
    engine_button.add_css_class("engine-button");
    engine_button.set_action_name(Some("app.show-performance"));
    engine_button.set_tooltip_text(Some("Performance meter (F8): load per track and plugin"));
    let import_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let import_bar = gtk::ProgressBar::new();
    import_bar.set_show_text(true);
    import_bar.set_valign(gtk::Align::Center);
    import_bar.set_width_request(260);
    let cancel = gtk::Button::from_icon_name("process-stop-symbolic");
    cancel.set_action_name(Some("app.cancel-import"));
    cancel.set_tooltip_text(Some("Cancel import"));
    cancel.add_css_class("flat");
    import_box.append(&import_bar);
    import_box.append(&cancel);
    import_box.set_visible(false);
    let platform = gtk::Label::new(Some(&platform_label(&window)));
    status.append(&notice);
    status.append(&import_box);
    // MIDI activity (and MIDI learn) indicator → Preferences, MIDI.
    let midi_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let midi_led = gtk::Label::new(Some("●"));
    midi_led.add_css_class("midi-led");
    let midi_text = gtk::Label::new(Some("MIDI"));
    midi_box.append(&midi_led);
    midi_box.append(&midi_text);
    let midi_button = gtk::Button::new();
    midi_button.set_child(Some(&midi_box));
    midi_button.add_css_class("flat");
    midi_button.add_css_class("midi-button");
    midi_button.set_action_name(Some("app.midi-settings"));
    midi_button.set_tooltip_text(Some("MIDI inputs and controller mappings"));
    status.append(&midi_button);
    status.append(&engine_button);
    status.append(&platform);
    content.append(&status);
    let toast = Toast::new();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&content));
    overlay.add_overlay(&toast.revealer);
    window.set_child(Some(&overlay));

    app.dock.borrow_mut().main_slot = Some(slot);
    *app.chrome.borrow_mut() = Some(Chrome {
        display,
        play,
        record,
        looping,
        metronome,
        edit_button,
        edit_bar,
        edit_bar_layout: faderframe_view_arranger::edit_bar::EditToolbarView::new(
            app.theme.clone(),
        ),
        notice,
        toast,
        engine,
        midi_button,
        midi_led,
        midi_text,
        import_box,
        import_bar,
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

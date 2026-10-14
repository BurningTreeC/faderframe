//! The Metronome page of the Preferences window: when it clicks, its sound
//! (built in, or the user's audio files) and the level of each kind of
//! click. Changes apply at once and are remembered.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_engine::MetronomeMode;
use faderframe_session::metronome::MIN_LEVEL_DB;
use faderframe_session::{MetronomeSettings, MetronomeSound};
use gtk::gio;
use gtk::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;

/// Change the metronome's sound everywhere: the session, the saved
/// preferences.
pub fn apply(app: &Rc<AppState>, settings: MetronomeSettings) {
    app.with_session(|s| {
        // A file that cannot be read is reported by the session (the
        // built-in click plays meanwhile).
        let _ = s.set_metronome(settings.clone());
        Ok(())
    });
    let mut p = Preferences::load();
    p.metronome_sound = settings;
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
}

/// A level slider in dB, "Off" at the bottom.
fn level(value: f32) -> gtk::Scale {
    let s = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        f64::from(MIN_LEVEL_DB),
        6.0,
        0.5,
    );
    s.set_value(f64::from(value));
    s.set_draw_value(true);
    s.set_value_pos(gtk::PositionType::Right);
    s.set_format_value_func(|_, v| {
        if v <= f64::from(MIN_LEVEL_DB) {
            "Off".into()
        } else {
            format!("{v:+.1} dB")
        }
    });
    s.add_mark(0.0, gtk::PositionType::Bottom, None);
    s.set_hexpand(true);
    s
}

/// A file row: its name, a chooser, and a way back to none.
#[derive(Clone)]
struct FileRow {
    widget: gtk::Box,
    label: gtk::Label,
    choose: gtk::Button,
    clear: gtk::Button,
}

impl FileRow {
    fn new(path: Option<&PathBuf>, none: &str) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let choose = gtk::Button::with_label("Choose…");
        let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
        clear.set_tooltip_text(Some("No file"));
        widget.append(&label);
        widget.append(&choose);
        widget.append(&clear);
        let row = Self {
            widget,
            label,
            choose,
            clear,
        };
        row.show(path, none);
        row
    }

    fn show(&self, path: Option<&PathBuf>, none: &str) {
        match path {
            Some(p) => {
                let name = p
                    .file_name()
                    .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into());
                self.label.set_text(&name);
                self.label.set_tooltip_text(Some(&p.display().to_string()));
                self.label.remove_css_class("dim-label");
            }
            None => {
                self.label.set_text(none);
                self.label.set_tooltip_text(None);
                self.label.add_css_class("dim-label");
            }
        }
        self.clear.set_sensitive(path.is_some());
    }
}

/// Ask for an audio file, then `done` with it.
fn choose_audio(parent: &gtk::Widget, title: &str, done: impl Fn(PathBuf) + 'static) {
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .modal(true)
        .filters(&crate::dialogs::audio_filters())
        .build();
    let window = parent.root().and_downcast::<gtk::Window>();
    dialog.open(window.as_ref(), gio::Cancellable::NONE, move |res| {
        if let Some(path) = res.ok().and_then(|f| f.path()) {
            done(path);
        }
    });
}

/// The Metronome page.
pub fn page(app: &Rc<AppState>, row: impl Fn(&gtk::Grid, i32, &str, &gtk::Widget)) -> gtk::Widget {
    let g = gtk::Grid::new();
    g.set_row_spacing(10);
    g.set_column_spacing(14);
    g.add_css_class("audio-settings");
    let m = app.session.borrow().metronome_settings().clone();
    let mode_now = app.session.borrow().record.metronome;

    let modes: Vec<String> = MetronomeMode::ALL
        .iter()
        .map(|m| m.label().into())
        .collect();
    let modes: Vec<&str> = modes.iter().map(String::as_str).collect();
    let mode = gtk::DropDown::from_strings(&modes);
    mode.set_selected(
        MetronomeMode::ALL
            .iter()
            .position(|x| *x == mode_now)
            .unwrap_or(0) as u32,
    );
    row(&g, 0, "Clicks", mode.upcast_ref());

    let sounds: Vec<&str> = MetronomeSound::ALL.iter().map(|s| s.label()).collect();
    let sound = gtk::DropDown::from_strings(&sounds);
    sound.set_selected(
        MetronomeSound::ALL
            .iter()
            .position(|s| *s == m.sound)
            .unwrap_or(0) as u32,
    );
    row(&g, 1, "Sound", sound.upcast_ref());
    let file = FileRow::new(m.file.as_ref(), "No file chosen");
    row(&g, 2, "Audio file", file.widget.upcast_ref());
    let accent_file = FileRow::new(m.accent_file.as_ref(), "The audio file above");
    row(&g, 3, "First beat's file", accent_file.widget.upcast_ref());

    let volume = level(m.volume_db);
    row(&g, 4, "Volume", volume.upcast_ref());
    let accent = level(m.accent_db);
    row(&g, 5, "First beat of a bar", accent.upcast_ref());
    let beat = level(m.beat_db);
    row(&g, 6, "Other beats", beat.upcast_ref());
    let off_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let off_on = gtk::Switch::new();
    off_on.set_active(m.off_beats);
    off_on.set_valign(gtk::Align::Center);
    off_on.set_tooltip_text(Some("Click the eighths between the beats too"));
    let off = level(m.off_beat_db);
    off.set_sensitive(m.off_beats);
    off_box.append(&off_on);
    off_box.append(&off);
    row(&g, 7, "Off-beats", off_box.upcast_ref());
    let main_on = gtk::Switch::new();
    main_on.set_active(m.main_outputs);
    main_on.set_halign(gtk::Align::Start);
    main_on.set_valign(gtk::Align::Center);
    main_on.set_tooltip_text(Some(
        "Click on the main outputs (the speakers). Off: only in the cue mixes whose click is on \
         (Audio → Cue Mixes)",
    ));
    row(&g, 8, "Main outputs", main_on.upcast_ref());

    let help = gtk::Label::new(Some(
        "Off-beats are the eighths between the beats. With an audio file the metronome plays it \
         on every beat (up to two seconds of it, at the levels above); a second file can mark the \
         first beat of each bar. Each cue mix has its own click switch and level (Audio → Cue \
         Mixes). The metronome is heard live only — renders never contain it.",
    ));
    help.set_wrap(true);
    help.set_max_width_chars(80);
    help.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
    help.set_xalign(0.0);
    help.add_css_class("dim-label");
    g.attach(&help, 0, 9, 2, 1);

    let custom = |s: &gtk::DropDown| {
        MetronomeSound::ALL[s.selected() as usize % MetronomeSound::ALL.len()]
            == MetronomeSound::Custom
    };
    let set_sensitive = {
        let (file, accent_file) = (file.widget.clone(), accent_file.widget.clone());
        move |on: bool| {
            file.set_sensitive(on);
            accent_file.set_sensitive(on);
        }
    };
    set_sensitive(custom(&sound));

    // The settings as the page shows them (files kept beside the widgets).
    let files = Rc::new(std::cell::RefCell::new((
        m.file.clone(),
        m.accent_file.clone(),
    )));
    let update = {
        let weak = Rc::downgrade(app);
        let (sound, volume, accent, beat, off_on, off, main_on, files) = (
            sound.clone(),
            volume.clone(),
            accent.clone(),
            beat.clone(),
            off_on.clone(),
            off.clone(),
            main_on.clone(),
            Rc::clone(&files),
        );
        Rc::new(move || {
            let Some(app) = weak.upgrade() else { return };
            let (file, accent_file) = files.borrow().clone();
            let settings = MetronomeSettings {
                sound: MetronomeSound::ALL[sound.selected() as usize % MetronomeSound::ALL.len()],
                file,
                accent_file,
                volume_db: volume.value() as f32,
                accent_db: accent.value() as f32,
                beat_db: beat.value() as f32,
                off_beats: off_on.is_active(),
                off_beat_db: off.value() as f32,
                main_outputs: main_on.is_active(),
            };
            off.set_sensitive(settings.off_beats);
            if &settings != app.session.borrow().metronome_settings() {
                apply(&app, settings);
            }
        })
    };
    {
        let weak = Rc::downgrade(app);
        mode.connect_selected_notify(move |d| {
            let Some(app) = weak.upgrade() else { return };
            let mut r = app.session.borrow().record;
            r.metronome = MetronomeMode::ALL[d.selected() as usize % MetronomeMode::ALL.len()];
            if r != app.session.borrow().record {
                crate::recording::apply(&app, r);
            }
        });
    }
    {
        let u = Rc::clone(&update);
        sound.connect_selected_notify(move |d| {
            set_sensitive(custom(d));
            u();
        });
    }
    for s in [&volume, &accent, &beat, &off] {
        let u = Rc::clone(&update);
        s.connect_value_changed(move |_| u());
    }
    {
        let u = Rc::clone(&update);
        off_on.connect_active_notify(move |_| u());
    }
    {
        let u = Rc::clone(&update);
        main_on.connect_active_notify(move |_| u());
    }
    // The files: chosen, or cleared.
    for (which, row, none) in [
        (0usize, &file, "No file chosen"),
        (1, &accent_file, "The audio file above"),
    ] {
        let (u, files, label_row) = (Rc::clone(&update), Rc::clone(&files), Rc::new(row.clone()));
        {
            let (u, files, label_row) = (Rc::clone(&u), Rc::clone(&files), Rc::clone(&label_row));
            row.choose.connect_clicked(move |b| {
                let (u, files, label_row) =
                    (Rc::clone(&u), Rc::clone(&files), Rc::clone(&label_row));
                let title = if which == 0 {
                    "Metronome Click"
                } else {
                    "Metronome Click for the First Beat"
                };
                choose_audio(b.upcast_ref(), title, move |path| {
                    {
                        let mut f = files.borrow_mut();
                        if which == 0 {
                            f.0 = Some(path.clone());
                        } else {
                            f.1 = Some(path.clone());
                        }
                    }
                    label_row.show(Some(&path), none);
                    u();
                });
            });
        }
        row.clear.connect_clicked(move |_| {
            {
                let mut f = files.borrow_mut();
                if which == 0 {
                    f.0 = None;
                } else {
                    f.1 = None;
                }
            }
            label_row.show(None, none);
            u();
        });
    }
    g.upcast()
}

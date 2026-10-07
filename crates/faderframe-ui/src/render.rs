//! The Render / Export window.

use crate::state::AppState;
use faderframe_audio::{STANDARD_SAMPLE_RATES, format_sample_rate};
use faderframe_audio_files::{Dither, WavFormat};
use faderframe_session::NoticeLevel;
use faderframe_session::delivery::{DELIVERY_PRESETS, Finish, PeakHandling, describe};
use faderframe_session::render::{
    RenderChannels, RenderJob, RenderRange, RenderSettings, RenderSource, resolve_range,
};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

const FORMATS: [WavFormat; 3] = [WavFormat::Pcm24, WavFormat::Pcm16, WavFormat::Float32];

/// Level choices (the `level` drop-down's order; 0 leaves it unchanged).
const LEVEL_PEAK: u32 = 1;
const LEVEL_CEILING: u32 = 2;
const LEVEL_LOUDNESS: u32 = 3;

struct Form {
    preset: gtk::DropDown,
    /// Set while a preset fills the form (its changes keep the preset).
    applying: Cell<bool>,
    range: gtk::DropDown,
    bar_from: gtk::SpinButton,
    bar_to: gtk::SpinButton,
    source: gtk::DropDown,
    format: gtk::DropDown,
    rate: gtk::DropDown,
    channels: gtk::DropDown,
    /// What each entry of `channels` renders.
    channel_choices: Vec<RenderChannels>,
    tail: gtk::SpinButton,
    level: gtk::DropDown,
    peak_db: gtk::SpinButton,
    lufs: gtk::SpinButton,
    ceiling: gtk::SpinButton,
    peaks: gtk::DropDown,
    level_parts: Vec<(gtk::Widget, &'static [u32])>,
    dither: gtk::DropDown,
    output: gtk::Entry,
    info: gtk::Label,
}

fn default_output(app: &Rc<AppState>) -> PathBuf {
    let s = app.session.borrow();
    let base = s
        .path()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .or_else(|| {
            glib::user_special_dir(glib::UserDirectory::Music).map(|m| m.join("FaderFrame"))
        })
        .unwrap_or_else(glib::home_dir);
    base.join(format!("{}.wav", s.project().name))
}

impl Form {
    fn settings(&self, project_rate: u32) -> RenderSettings {
        let range = match self.range.selected() {
            1 => RenderRange::Loop,
            2 => RenderRange::Bars {
                start: self.bar_from.value() as i32 - 1,
                end: self.bar_to.value() as i32,
            },
            _ => RenderRange::Project,
        };
        let sample_rate = match self.rate.selected() {
            0 => project_rate,
            i => STANDARD_SAMPLE_RATES[(i as usize - 1).min(STANDARD_SAMPLE_RATES.len() - 1)],
        };
        RenderSettings {
            range,
            source: if self.source.selected() == 1 {
                RenderSource::Stems
            } else {
                RenderSource::Master
            },
            format: FORMATS[self.format.selected() as usize % FORMATS.len()],
            sample_rate,
            channels: self
                .channel_choices
                .get(self.channels.selected() as usize)
                .copied()
                .unwrap_or(RenderChannels::Stereo),
            tail_seconds: self.tail.value() as f32,
            normalize_db: (self.level.selected() == LEVEL_PEAK)
                .then(|| self.peak_db.value() as f32),
            finish: self.finish(),
            dither: Dither::ALL[self.dither.selected() as usize % Dither::ALL.len()],
            report: true,
            output: PathBuf::from(self.output.text().as_str()),
        }
    }

    fn finish(&self) -> Finish {
        let ceiling = Some(self.ceiling.value() as f32);
        match self.level.selected() {
            LEVEL_CEILING => Finish {
                ceiling,
                ..Finish::default()
            },
            LEVEL_LOUDNESS => Finish {
                loudness: Some(self.lufs.value() as f32),
                ceiling,
                peaks: if self.peaks.selected() == 1 {
                    PeakHandling::LowerGain
                } else {
                    PeakHandling::Limit
                },
            },
            _ => Finish::default(),
        }
    }

    /// Fill the form from delivery preset `i` (0 = custom: nothing).
    fn apply_preset(&self, i: u32, project_rate: u32) {
        let Some(p) = (i as usize)
            .checked_sub(1)
            .and_then(|i| DELIVERY_PRESETS.get(i))
        else {
            return;
        };
        self.applying.set(true);
        if let Some(f) = FORMATS.iter().position(|f| *f == p.format) {
            self.format.set_selected(f as u32);
        }
        let rate = p.sample_rate.filter(|r| *r != project_rate);
        let rate_index = rate
            .and_then(|r| STANDARD_SAMPLE_RATES.iter().position(|s| *s == r))
            .map_or(0, |i| i as u32 + 1);
        self.rate.set_selected(rate_index);
        if let Some(d) = Dither::ALL.iter().position(|d| *d == p.dither) {
            self.dither.set_selected(d as u32);
        }
        if let Some(c) = p.finish.ceiling {
            self.ceiling.set_value(c as f64);
        }
        match p.finish.loudness {
            Some(l) => {
                self.lufs.set_value(l as f64);
                self.level.set_selected(LEVEL_LOUDNESS);
            }
            None => self.level.set_selected(LEVEL_CEILING),
        }
        self.peaks.set_selected(0);
        self.applying.set(false);
    }

    /// A choice changed by hand: the preset no longer describes the form.
    fn edited(&self) {
        if !self.applying.get() {
            self.preset.set_selected(0);
        }
    }

    fn update_info(&self, app: &Rc<AppState>) {
        let s = app.session.borrow();
        let p = s.project();
        let settings = self.settings(p.sample_rate);
        self.bar_from.set_sensitive(self.range.selected() == 2);
        self.bar_to.set_sensitive(self.range.selected() == 2);
        self.dither.set_sensitive(settings.format.is_integer());
        let level = self.level.selected();
        for (w, modes) in &self.level_parts {
            w.set_visible(modes.contains(&level));
        }
        let text = match resolve_range(p, settings.range) {
            Ok((a, b)) => {
                let secs = p.timeline.tempo.musical_to_seconds(b)
                    - p.timeline.tempo.musical_to_seconds(a)
                    + settings.tail_seconds as f64;
                let adm = matches!(settings.channels, RenderChannels::Adm(_));
                let ch = match settings.channels {
                    RenderChannels::Mono | RenderChannels::First => 1,
                    RenderChannels::Master => faderframe_session::render::master_bed(p)
                        .map_or(2, faderframe_core::SurroundFormat::channels),
                    RenderChannels::Stereo | RenderChannels::Binaural(_) => 2,
                    RenderChannels::Adm(_) => {
                        faderframe_session::adm::plan(p).map_or(0, |plan| plan.channels())
                    }
                };
                // An ADM master is 24-bit at 48 kHz (96 kHz if chosen).
                let (bits, rate) = if adm {
                    (
                        24,
                        if settings.sample_rate == 96_000 {
                            96_000
                        } else {
                            48_000
                        },
                    )
                } else {
                    (settings.format.bits(), settings.sample_rate)
                };
                let files = match settings.source {
                    RenderSource::Master => 1,
                    RenderSource::Stems => p
                        .tracks
                        .iter()
                        .filter(|t| {
                            t.kind != faderframe_project::TrackKind::Master && t.kind.has_audio()
                        })
                        .count(),
                };
                let bytes = secs * rate as f64 * ch as f64 * (bits / 8) as f64 * files as f64;
                format!(
                    "{} → {}  ·  {:.1} s  ·  {} file{}  ·  ≈ {:.1} MB  ·  {} · {}",
                    p.timeline.format_bbt(a),
                    p.timeline.format_bbt(b),
                    secs,
                    files,
                    if files == 1 { "" } else { "s" },
                    bytes / 1_000_000.0,
                    if adm {
                        "PCM 24-bit"
                    } else {
                        settings.format.label()
                    },
                    format_sample_rate(rate)
                )
            }
            Err(e) => e.to_string(),
        };
        self.info.set_text(&text);
    }
}

fn labelled(grid: &gtk::Grid, y: i32, label: &str, w: &impl IsA<gtk::Widget>) {
    let l = gtk::Label::new(Some(label));
    l.set_xalign(1.0);
    l.add_css_class("dim-label");
    grid.attach(&l, 0, y, 1, 1);
    w.set_hexpand(true);
    grid.attach(w, 1, y, 1, 1);
}

thread_local! {
    static FORM: RefCell<Option<std::rc::Weak<Form>>> = const { RefCell::new(None) };
}

/// Open the window with delivery preset `preset` (1-based; 0 = custom).
pub fn open_with_preset(app: &Rc<AppState>, preset: u32) {
    open(app);
    if let Some(form) = FORM.with(|f| f.borrow().as_ref().and_then(std::rc::Weak::upgrade)) {
        form.preset.set_selected(preset);
    }
}

pub fn open(app: &Rc<AppState>) {
    if let Some(win) = OPEN.with(|o| o.borrow().as_ref().and_then(|w| w.upgrade())) {
        win.present();
        return;
    }
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Render / Export — FaderFrame")
        .default_width(640)
        .resizable(false)
        .build();
    if let Some(main) = app.window.borrow().as_ref() {
        win.set_transient_for(Some(main));
    }
    let grid = gtk::Grid::new();
    grid.set_row_spacing(10);
    grid.set_column_spacing(14);
    grid.add_css_class("audio-settings");

    let mut preset_names = vec!["Custom"];
    preset_names.extend(DELIVERY_PRESETS.iter().map(|p| p.name));
    let preset = gtk::DropDown::from_strings(&preset_names);
    preset.set_tooltip_text(Some(
        "Fills in format, sample rate, dither and level for a destination",
    ));
    let range = gtk::DropDown::from_strings(&["Entire project", "Loop range", "Bars"]);
    let bars = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let bar_from = gtk::SpinButton::with_range(1.0, 9999.0, 1.0);
    let bar_to = gtk::SpinButton::with_range(1.0, 9999.0, 1.0);
    {
        let s = app.session.borrow();
        let p = s.project();
        let end_bar = p.timeline.meter.bar_at(p.content_end()).max(1);
        bar_from.set_value(1.0);
        bar_to.set_value(end_bar as f64);
        if p.loop_range.is_some() && p.loop_enabled {
            range.set_selected(1);
        }
    }
    bars.append(&gtk::Label::new(Some("from bar")));
    bars.append(&bar_from);
    bars.append(&gtk::Label::new(Some("to end of bar")));
    bars.append(&bar_to);
    let range_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    range_box.append(&range);
    range_box.append(&bars);
    labelled(&grid, 0, "Delivery", &preset);
    labelled(&grid, 1, "Range", &range_box);

    let source = gtk::DropDown::from_strings(&["Master mix", "Stems (one file per track)"]);
    labelled(&grid, 2, "Source", &source);
    let format_names: Vec<&str> = FORMATS.iter().map(|f| f.label()).collect();
    let format = gtk::DropDown::from_strings(&format_names);
    labelled(&grid, 3, "Format", &format);
    let project_rate = app.session.borrow().project().sample_rate;
    let mut rates = vec![format!(
        "Project rate ({})",
        format_sample_rate(project_rate)
    )];
    rates.extend(STANDARD_SAMPLE_RATES.iter().map(|r| format_sample_rate(*r)));
    let rate_refs: Vec<&str> = rates.iter().map(String::as_str).collect();
    let rate = gtk::DropDown::from_strings(&rate_refs);
    labelled(&grid, 4, "Sample rate", &rate);
    // A surround master: as it is, or folded down.
    let bed = faderframe_session::render::master_bed(app.session.borrow().project());
    let (channel_names, channel_choices): (Vec<String>, Vec<RenderChannels>) = match bed {
        Some(f) => {
            let mut names = vec![
                format!("As the master ({}, every channel)", f.name()),
                "Stereo (folded down)".into(),
                "Mono (summed)".into(),
            ];
            let mut choices = vec![
                RenderChannels::Master,
                RenderChannels::Stereo,
                RenderChannels::Mono,
            ];
            // For headphones, in the room listened with (Mid otherwise).
            let room = app.session.borrow().headphones().unwrap_or_default();
            names.push(format!("Headphones (binaural · {})", room.name()));
            choices.push(RenderChannels::Binaural(room));
            // An object-based master: the bed and the object tracks.
            if let Ok(plan) = faderframe_session::adm::plan(app.session.borrow().project()) {
                let what = plan.describe();
                names.push(format!("ADM BWF · Dolby Atmos master ({what})"));
                choices.push(RenderChannels::Adm(faderframe_adm::Profile::DolbyAtmos));
                names.push(format!("ADM BWF · ITU-R BS.2076 ({what})"));
                choices.push(RenderChannels::Adm(faderframe_adm::Profile::Itu));
            }
            (names, choices)
        }
        None => (
            vec!["Stereo".into(), "Mono (summed)".into()],
            vec![RenderChannels::Stereo, RenderChannels::Mono],
        ),
    };
    let names: Vec<&str> = channel_names.iter().map(String::as_str).collect();
    let channels = gtk::DropDown::from_strings(&names);
    labelled(&grid, 5, "Channels", &channels);
    let tail = gtk::SpinButton::with_range(0.0, 60.0, 0.5);
    tail.set_digits(1);
    tail.set_value(2.0);
    tail.set_tooltip_text(Some(
        "Extra seconds rendered after the range so echo/reverb tails ring out",
    ));
    labelled(&grid, 6, "Tail (seconds)", &tail);
    let level_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let level = gtk::DropDown::from_strings(&[
        "Unchanged",
        "Normalise peak",
        "Limit true peak",
        "Loudness target",
    ]);
    let spin = |lo: f64, hi: f64, step: f64, value: f64| {
        let b = gtk::SpinButton::with_range(lo, hi, step);
        b.set_digits(1);
        b.set_value(value);
        b
    };
    let peak_db = spin(-24.0, 0.0, 0.1, -1.0);
    let lufs = spin(-36.0, -5.0, 0.5, -14.0);
    let ceiling = spin(-6.0, 0.0, 0.1, -1.0);
    let peaks = gtk::DropDown::from_strings(&["limit peaks", "use less gain"]);
    peaks.set_tooltip_text(Some(
        "When the gain would push the true peak over the ceiling: limit the peaks (the loudness reaches the target) or use less gain (it stays below)",
    ));
    let unit = |t: &str| gtk::Label::new(Some(t)).upcast::<gtk::Widget>();
    level_box.append(&level);
    let level_parts: Vec<(gtk::Widget, &'static [u32])> = vec![
        (peak_db.clone().upcast(), &[LEVEL_PEAK]),
        (unit("dBFS"), &[LEVEL_PEAK]),
        (lufs.clone().upcast(), &[LEVEL_LOUDNESS]),
        (unit("LUFS, true peak ≤"), &[LEVEL_LOUDNESS]),
        (unit("true peak ≤"), &[LEVEL_CEILING]),
        (ceiling.clone().upcast(), &[LEVEL_CEILING, LEVEL_LOUDNESS]),
        (unit("dBTP"), &[LEVEL_CEILING, LEVEL_LOUDNESS]),
        (peaks.clone().upcast(), &[LEVEL_LOUDNESS]),
    ];
    for (w, _) in &level_parts {
        level_box.append(w);
    }
    labelled(&grid, 7, "Level", &level_box);
    let dither_names: Vec<&str> = Dither::ALL.iter().map(|d| d.label()).collect();
    let dither = gtk::DropDown::from_strings(&dither_names);
    dither.set_selected(1);
    dither.set_tooltip_text(Some(
        "Integer formats only. Noise shaping (at 44.1 and 48 kHz) moves the dither noise out of the ear's most sensitive range — for 16-bit delivery",
    ));
    labelled(&grid, 8, "Dither", &dither);
    let out_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let output = gtk::Entry::new();
    output.set_text(&default_output(app).to_string_lossy());
    output.set_hexpand(true);
    let choose = gtk::Button::with_label("Choose…");
    out_box.append(&output);
    out_box.append(&choose);
    labelled(&grid, 9, "Output", &out_box);
    let info = gtk::Label::new(None);
    info.set_xalign(0.0);
    info.set_wrap(true);
    info.set_selectable(true);
    info.add_css_class("dim-label");
    grid.attach(&info, 0, 10, 2, 1);
    let progress = gtk::ProgressBar::new();
    progress.set_show_text(true);
    grid.attach(&progress, 0, 11, 2, 1);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    // Closes the window, or stops a running render.
    let cancel = gtk::Button::with_label("Close");
    let render = gtk::Button::with_label("Render");
    render.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&render);
    grid.attach(&actions, 0, 12, 2, 1);
    win.set_child(Some(&grid));

    let form = Rc::new(Form {
        preset,
        applying: Cell::new(false),
        range,
        bar_from,
        bar_to,
        source,
        format,
        rate,
        channels,
        channel_choices,
        tail,
        level,
        peak_db,
        lufs,
        ceiling,
        peaks,
        level_parts,
        dither,
        output,
        info,
    });
    form.update_info(app);

    // Keep the summary and the output path in sync with the choices.
    let refresh = {
        let weak = Rc::downgrade(app);
        let form = Rc::clone(&form);
        move || {
            if let Some(app) = weak.upgrade() {
                form.update_info(&app);
            }
        }
    };
    for dd in [&form.range, &form.channels] {
        let r = refresh.clone();
        dd.connect_selected_notify(move |_| r());
    }
    // What a preset sets: changed by hand, the form is custom again.
    for dd in [
        &form.format,
        &form.rate,
        &form.level,
        &form.peaks,
        &form.dither,
    ] {
        let r = refresh.clone();
        let f = Rc::clone(&form);
        dd.connect_selected_notify(move |_| {
            f.edited();
            r();
        });
    }
    for sb in [&form.lufs, &form.ceiling] {
        let f = Rc::clone(&form);
        sb.connect_value_changed(move |_| f.edited());
    }
    {
        let r = refresh.clone();
        let f = Rc::clone(&form);
        form.preset.connect_selected_notify(move |d| {
            f.apply_preset(d.selected(), project_rate);
            r();
        });
    }
    {
        let r = refresh.clone();
        let weak = Rc::downgrade(app);
        let f = Rc::clone(&form);
        form.source.connect_selected_notify(move |d| {
            if let Some(app) = weak.upgrade() {
                let path = default_output(&app);
                let out = if d.selected() == 1 {
                    path.with_extension("").to_string_lossy().to_string() + " stems"
                } else {
                    path.to_string_lossy().to_string()
                };
                f.output.set_text(&out);
            }
            r();
        });
    }
    for sb in [&form.bar_from, &form.bar_to, &form.tail] {
        let r = refresh.clone();
        sb.connect_value_changed(move |_| r());
    }

    {
        let form = Rc::clone(&form);
        let win_weak = win.downgrade();
        choose.connect_clicked(move |_| {
            let Some(win) = win_weak.upgrade() else {
                return;
            };
            let stems = form.source.selected() == 1;
            let dialog = gtk::FileDialog::builder()
                .title("Render to…")
                .modal(true)
                .build();
            let current = PathBuf::from(form.output.text().as_str());
            if let Some(name) = current.file_name() {
                dialog.set_initial_name(Some(&name.to_string_lossy()));
            }
            let form = Rc::clone(&form);
            let done = move |res: Result<gio::File, glib::Error>| {
                if let Ok(f) = res
                    && let Some(p) = f.path()
                {
                    form.output.set_text(&p.to_string_lossy());
                }
            };
            if stems {
                dialog.select_folder(Some(&win), gio::Cancellable::NONE, done);
            } else {
                dialog.save(Some(&win), gio::Cancellable::NONE, done);
            }
        });
    }

    let job: Rc<RefCell<Option<RenderJob>>> = Rc::new(RefCell::new(None));
    {
        let job = Rc::clone(&job);
        let win_weak = win.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(j) = job.borrow().as_ref() {
                j.cancel();
            } else if let Some(win) = win_weak.upgrade() {
                win.close();
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        let form = Rc::clone(&form);
        let job = Rc::clone(&job);
        let cancel = cancel.clone();
        render.connect_clicked(move |button| {
            let Some(app) = weak.upgrade() else { return };
            if job.borrow().is_some() {
                return;
            }
            let mut settings = form.settings(project_rate);
            if settings.source == RenderSource::Master && settings.output.extension().is_none() {
                settings.output.set_extension("wav");
            }
            if let Some(dir) = settings.output.parent()
                && !dir.as_os_str().is_empty()
                && let Err(e) = std::fs::create_dir_all(dir)
            {
                form.info
                    .set_text(&format!("Cannot create {}: {e}", dir.display()));
                return;
            }
            let started = app.session.borrow_mut().render(settings);
            match started {
                Ok(j) => {
                    *job.borrow_mut() = Some(j);
                    button.set_sensitive(false);
                    cancel.set_label("Stop");
                    progress.set_fraction(0.0);
                    progress.set_text(Some("Rendering…"));
                    let job = Rc::clone(&job);
                    let weak = Rc::downgrade(&app);
                    let button = button.clone();
                    let cancel = cancel.clone();
                    let progress = progress.clone();
                    let info = form.info.clone();
                    glib::timeout_add_local(Duration::from_millis(80), move || {
                        let finished = {
                            let j = job.borrow();
                            let Some(j) = j.as_ref() else {
                                return glib::ControlFlow::Break;
                            };
                            progress.set_fraction(j.progress.fraction());
                            j.is_finished()
                        };
                        if !finished {
                            return glib::ControlFlow::Continue;
                        }
                        let Some(j) = job.borrow_mut().take() else {
                            return glib::ControlFlow::Break;
                        };
                        button.set_sensitive(true);
                        cancel.set_label("Close");
                        let (level, text) = match j.join() {
                            Ok(files) => {
                                progress.set_fraction(1.0);
                                progress.set_text(Some("Done"));
                                let list: Vec<String> = files
                                    .iter()
                                    .map(|f| {
                                        let name = f.path.file_name().map_or_else(
                                            || f.path.display().to_string(),
                                            |n| n.to_string_lossy().to_string(),
                                        );
                                        match &f.finished {
                                            Some(done) => format!("{name}: {}", describe(done)),
                                            None => name,
                                        }
                                    })
                                    .collect();
                                (
                                    NoticeLevel::Info,
                                    format!(
                                        "Rendered {} file(s)\n{}",
                                        files.len(),
                                        list.join("\n")
                                    ),
                                )
                            }
                            Err(e) => {
                                progress.set_text(Some("Stopped"));
                                (NoticeLevel::Warning, format!("Render: {e}"))
                            }
                        };
                        info.set_text(&text);
                        if let Some(app) = weak.upgrade() {
                            app.session.borrow_mut().notify(level, text);
                        }
                        glib::ControlFlow::Break
                    });
                }
                Err(e) => form.info.set_text(&e.to_string()),
            }
        });
    }
    {
        let job = Rc::clone(&job);
        win.connect_close_request(move |_| {
            if let Some(j) = job.borrow().as_ref() {
                j.cancel();
            }
            glib::Propagation::Proceed
        });
    }
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    FORM.with(|f| *f.borrow_mut() = Some(Rc::downgrade(&form)));
    win.present();
}

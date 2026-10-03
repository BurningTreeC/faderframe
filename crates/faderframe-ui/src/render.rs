//! The Render / Export window.

use crate::state::AppState;
use faderframe_audio::{STANDARD_SAMPLE_RATES, format_sample_rate};
use faderframe_audio_files::WavFormat;
use faderframe_session::NoticeLevel;
use faderframe_session::render::{
    RenderChannels, RenderJob, RenderRange, RenderSettings, RenderSource, resolve_range,
};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

const FORMATS: [WavFormat; 3] = [WavFormat::Pcm24, WavFormat::Pcm16, WavFormat::Float32];

struct Form {
    range: gtk::DropDown,
    bar_from: gtk::SpinButton,
    bar_to: gtk::SpinButton,
    source: gtk::DropDown,
    format: gtk::DropDown,
    rate: gtk::DropDown,
    channels: gtk::DropDown,
    tail: gtk::SpinButton,
    normalize: gtk::CheckButton,
    normalize_db: gtk::SpinButton,
    dither: gtk::CheckButton,
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
            channels: if self.channels.selected() == 1 {
                RenderChannels::Mono
            } else {
                RenderChannels::Stereo
            },
            tail_seconds: self.tail.value() as f32,
            normalize_db: self
                .normalize
                .is_active()
                .then(|| self.normalize_db.value() as f32),
            dither: self.dither.is_active(),
            output: PathBuf::from(self.output.text().as_str()),
        }
    }

    fn update_info(&self, app: &Rc<AppState>) {
        let s = app.session.borrow();
        let p = s.project();
        let settings = self.settings(p.sample_rate);
        self.bar_from.set_sensitive(self.range.selected() == 2);
        self.bar_to.set_sensitive(self.range.selected() == 2);
        self.dither.set_sensitive(settings.format.is_integer());
        self.normalize_db.set_sensitive(self.normalize.is_active());
        let text = match resolve_range(p, settings.range) {
            Ok((a, b)) => {
                let secs = p.timeline.tempo.musical_to_seconds(b)
                    - p.timeline.tempo.musical_to_seconds(a)
                    + settings.tail_seconds as f64;
                let ch = if settings.channels == RenderChannels::Mono {
                    1
                } else {
                    2
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
                let bytes = secs
                    * settings.sample_rate as f64
                    * ch as f64
                    * (settings.format.bits() / 8) as f64
                    * files as f64;
                format!(
                    "{} → {}  ·  {:.1} s  ·  {} file{}  ·  ≈ {:.1} MB  ·  {} · {}",
                    p.timeline.format_bbt(a),
                    p.timeline.format_bbt(b),
                    secs,
                    files,
                    if files == 1 { "" } else { "s" },
                    bytes / 1_000_000.0,
                    settings.format.label(),
                    format_sample_rate(settings.sample_rate)
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
    labelled(&grid, 0, "Range", &range_box);

    let source = gtk::DropDown::from_strings(&["Master mix", "Stems (one file per track)"]);
    labelled(&grid, 1, "Source", &source);
    let format_names: Vec<&str> = FORMATS.iter().map(|f| f.label()).collect();
    let format = gtk::DropDown::from_strings(&format_names);
    labelled(&grid, 2, "Format", &format);
    let project_rate = app.session.borrow().project().sample_rate;
    let mut rates = vec![format!(
        "Project rate ({})",
        format_sample_rate(project_rate)
    )];
    rates.extend(STANDARD_SAMPLE_RATES.iter().map(|r| format_sample_rate(*r)));
    let rate_refs: Vec<&str> = rates.iter().map(String::as_str).collect();
    let rate = gtk::DropDown::from_strings(&rate_refs);
    labelled(&grid, 3, "Sample rate", &rate);
    let channels = gtk::DropDown::from_strings(&["Stereo", "Mono (summed)"]);
    labelled(&grid, 4, "Channels", &channels);
    let tail = gtk::SpinButton::with_range(0.0, 60.0, 0.5);
    tail.set_digits(1);
    tail.set_value(2.0);
    tail.set_tooltip_text(Some(
        "Extra seconds rendered after the range so echo/reverb tails ring out",
    ));
    labelled(&grid, 5, "Tail (seconds)", &tail);
    let norm_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let normalize = gtk::CheckButton::with_label("Normalise peak to");
    let normalize_db = gtk::SpinButton::with_range(-24.0, 0.0, 0.1);
    normalize_db.set_digits(1);
    normalize_db.set_value(-1.0);
    norm_box.append(&normalize);
    norm_box.append(&normalize_db);
    norm_box.append(&gtk::Label::new(Some("dBFS")));
    labelled(&grid, 6, "Level", &norm_box);
    let dither = gtk::CheckButton::with_label("TPDF dither (integer formats)");
    dither.set_active(true);
    labelled(&grid, 7, "Dither", &dither);
    let out_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let output = gtk::Entry::new();
    output.set_text(&default_output(app).to_string_lossy());
    output.set_hexpand(true);
    let choose = gtk::Button::with_label("Choose…");
    out_box.append(&output);
    out_box.append(&choose);
    labelled(&grid, 8, "Output", &out_box);
    let info = gtk::Label::new(None);
    info.set_xalign(0.0);
    info.set_wrap(true);
    info.add_css_class("dim-label");
    grid.attach(&info, 0, 9, 2, 1);
    let progress = gtk::ProgressBar::new();
    progress.set_show_text(true);
    grid.attach(&progress, 0, 10, 2, 1);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_sensitive(false);
    let render = gtk::Button::with_label("Render");
    render.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&render);
    grid.attach(&actions, 0, 11, 2, 1);
    win.set_child(Some(&grid));

    let form = Rc::new(Form {
        range,
        bar_from,
        bar_to,
        source,
        format,
        rate,
        channels,
        tail,
        normalize,
        normalize_db,
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
    for dd in [&form.range, &form.format, &form.rate, &form.channels] {
        let r = refresh.clone();
        dd.connect_selected_notify(move |_| r());
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
        let r = refresh.clone();
        form.normalize.connect_toggled(move |_| r());
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
        cancel.connect_clicked(move |_| {
            if let Some(j) = job.borrow().as_ref() {
                j.cancel();
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
            let started = app.session.borrow().render(settings);
            match started {
                Ok(j) => {
                    *job.borrow_mut() = Some(j);
                    button.set_sensitive(false);
                    cancel.set_sensitive(true);
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
                        cancel.set_sensitive(false);
                        let (level, text) = match j.join() {
                            Ok(files) => {
                                progress.set_fraction(1.0);
                                progress.set_text(Some("Done"));
                                let list: Vec<String> =
                                    files.iter().map(|f| f.display().to_string()).collect();
                                (
                                    NoticeLevel::Info,
                                    format!(
                                        "Rendered {} file(s): {}",
                                        files.len(),
                                        list.join(", ")
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
    win.present();
}

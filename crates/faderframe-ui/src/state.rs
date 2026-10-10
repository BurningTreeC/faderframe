//! Shared GUI state: the session plus the GTK objects that present it.

use crate::canvas::CanvasWidget;
use crate::dock::DockState;
use crate::window::Chrome;
use faderframe_audio::AudioBackend;
use faderframe_session::{Action, AudioPreferences, NoticeLevel, Session, SessionError};
use faderframe_ui_canvas::Theme;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    /// Linux: native PipeWire if its server runs, else JACK, else ALSA;
    /// elsewhere the system API. The silent dummy device last.
    #[default]
    Auto,
    /// Linux only.
    PipeWire,
    /// Linux only.
    Jack,
    /// The operating system's API: WASAPI, CoreAudio or ALSA.
    System,
    /// Steinberg ASIO drivers (Windows builds with the `asio` feature).
    Asio,
    Dummy,
}

impl BackendChoice {
    /// The choices this platform offers.
    pub fn available() -> Vec<BackendChoice> {
        if cfg!(target_os = "linux") {
            vec![
                BackendChoice::Auto,
                BackendChoice::PipeWire,
                BackendChoice::Jack,
                BackendChoice::System,
                BackendChoice::Dummy,
            ]
        } else if faderframe_audio_cpal::ASIO {
            vec![
                BackendChoice::Auto,
                BackendChoice::Asio,
                BackendChoice::System,
                BackendChoice::Dummy,
            ]
        } else {
            vec![
                BackendChoice::Auto,
                BackendChoice::System,
                BackendChoice::Dummy,
            ]
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BackendChoice::Auto if cfg!(target_os = "linux") => {
                "Automatic (PipeWire, else JACK, else ALSA, else silent)"
            }
            BackendChoice::Auto if faderframe_audio_cpal::ASIO => {
                "Automatic (ASIO, else WASAPI, else silent)"
            }
            BackendChoice::Auto if cfg!(windows) => "Automatic (WASAPI, else silent)",
            BackendChoice::Auto => "Automatic (CoreAudio, else silent)",
            BackendChoice::PipeWire => "PipeWire (native)",
            BackendChoice::Jack => "JACK / PipeWire-JACK",
            BackendChoice::System if cfg!(windows) => "WASAPI",
            BackendChoice::System if cfg!(target_os = "macos") => "CoreAudio",
            BackendChoice::System => "ALSA (direct)",
            BackendChoice::Asio => "ASIO",
            BackendChoice::Dummy => "No audio device (silent)",
        }
    }

    pub fn backends(self) -> Vec<Box<dyn AudioBackend>> {
        let system =
            || Box::new(faderframe_audio_cpal::CpalBackend::default()) as Box<dyn AudioBackend>;
        // ASIO devices first where the build has them (none otherwise).
        #[cfg(not(target_os = "linux"))]
        let asio = || -> Vec<Box<dyn AudioBackend>> {
            #[cfg(all(windows, feature = "asio"))]
            {
                vec![Box::new(faderframe_audio_cpal::CpalBackend::new(
                    faderframe_audio_cpal::Api::Asio,
                ))]
            }
            #[cfg(not(all(windows, feature = "asio")))]
            Vec::new()
        };
        // FADERFRAME_DUMMY_TONE=<Hz> feeds a test tone to the dummy
        // device's inputs (for trying out recording without hardware).
        let tone = std::env::var("FADERFRAME_DUMMY_TONE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok());
        // FADERFRAME_DUMMY_LOOPBACK=<frames>: its outputs come back on its
        // inputs (trying hardware inserts without hardware).
        let loopback = std::env::var("FADERFRAME_DUMMY_LOOPBACK")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let dummy = move || {
            Box::new(match (tone, loopback) {
                (Some(hz), _) => faderframe_audio::dummy::DummyBackend::with_input_pluck(hz),
                (None, Some(d)) => faderframe_audio::dummy::DummyBackend::with_loopback(d),
                (None, None) => faderframe_audio::dummy::DummyBackend::default(),
            }) as Box<dyn AudioBackend>
        };
        #[cfg(target_os = "linux")]
        {
            let jack = || Box::new(faderframe_audio_jack::JackBackend) as Box<dyn AudioBackend>;
            let pipewire =
                || Box::new(faderframe_audio_pipewire::PipeWireBackend) as Box<dyn AudioBackend>;
            match self {
                BackendChoice::Auto => vec![pipewire(), jack(), system(), dummy()],
                BackendChoice::PipeWire => vec![pipewire()],
                BackendChoice::Jack => vec![jack()],
                BackendChoice::System | BackendChoice::Asio => vec![system()],
                BackendChoice::Dummy => vec![dummy()],
            }
        }
        #[cfg(not(target_os = "linux"))]
        match self {
            BackendChoice::Dummy => vec![dummy()],
            BackendChoice::Asio if faderframe_audio_cpal::ASIO => asio(),
            BackendChoice::System
            | BackendChoice::Asio
            | BackendChoice::PipeWire
            | BackendChoice::Jack => vec![system()],
            BackendChoice::Auto => {
                let mut all = asio();
                all.extend([system(), dummy()]);
                all
            }
        }
    }
}

/// Command-line / startup options.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub backend: BackendChoice,
    pub project: Option<PathBuf>,
    /// Start with a new, empty project (`--empty`).
    pub empty: bool,
    /// Start with the demo session (`--demo`).
    pub demo: bool,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    /// Processing threads (audio thread included; `None`: one per core).
    pub threads: Option<u16>,
    /// Audio files to import after start-up (onto new tracks at bar 1).
    pub import: Vec<PathBuf>,
}

pub struct AppState {
    pub app: gtk::Application,
    pub session: RefCell<Session>,
    /// The active skin (Preferences → General → Theme, View → Theme).
    pub theme: RefCell<Theme>,
    pub options: RefCell<RunOptions>,
    pub dock: RefCell<DockState>,
    pub window: RefCell<Option<gtk::ApplicationWindow>>,
    pub chrome: RefCell<Option<Chrome>>,
    canvases: RefCell<Vec<glib::WeakRef<CanvasWidget>>>,
    /// Draw dense views on the GPU (Preferences → General).
    pub gpu_painter: Cell<bool>,
    layout_rev: Cell<u64>,
    /// Session revision the widgets show (changes made by the frame tick —
    /// finished recordings, imports, analyses — redraw too).
    shown_rev: Cell<u64>,
    last_tick: Cell<Option<Instant>>,
    frame: Cell<u64>,
    /// File → Open Recent (rebuilt when the list changes).
    pub recent_menu: gio::Menu,
    /// The project file the recent list last recorded.
    pub shown_path: RefCell<Option<PathBuf>>,
    /// Background CLAP scan in progress.
    pub plugin_scan: RefCell<Option<std::sync::mpsc::Receiver<crate::plugins::ScanReport>>>,
}

impl AppState {
    pub fn new(app: &gtk::Application, session: Session, options: RunOptions) -> Rc<Self> {
        Rc::new(Self {
            app: app.clone(),
            layout_rev: Cell::new(session.layout_revision()),
            shown_rev: Cell::new(session.revision()),
            session: RefCell::new(session),
            theme: RefCell::new(Theme::by_id(&crate::prefs::Preferences::load().theme)),
            options: RefCell::new(options),
            dock: RefCell::new(DockState::default()),
            window: RefCell::new(None),
            chrome: RefCell::new(None),
            canvases: RefCell::new(Vec::new()),
            gpu_painter: Cell::new(crate::gpu::wanted(
                crate::prefs::Preferences::load().gpu_painter,
            )),
            last_tick: Cell::new(None),
            frame: Cell::new(0),
            plugin_scan: RefCell::new(None),
            recent_menu: {
                let m = gio::Menu::new();
                crate::recent::rebuild_menu(&m, &crate::prefs::Preferences::load().recent_projects);
                m
            },
            shown_path: RefCell::new(None),
        })
    }

    pub fn register_canvas(&self, c: &CanvasWidget) {
        self.canvases.borrow_mut().push(c.downgrade());
    }

    /// Switch the skin: GTK's CSS and every canvas follow at once, and the
    /// choice is remembered.
    pub fn set_theme(&self, id: &str) {
        let theme = Theme::by_id(id);
        crate::style::install(&theme);
        self.canvases.borrow_mut().retain(|w| match w.upgrade() {
            Some(c) => {
                c.set_theme(&theme);
                true
            }
            None => false,
        });
        *self.theme.borrow_mut() = theme;
        if let Some(a) = self
            .app
            .lookup_action("theme")
            .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
        {
            a.set_state(&id.to_variant());
        }
        let mut prefs = crate::prefs::Preferences::load();
        prefs.theme = id.into();
        if let Err(e) = prefs.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    }

    pub fn redraw_all(&self) {
        self.canvases.borrow_mut().retain(|w| match w.upgrade() {
            Some(c) => {
                c.queue_draw();
                true
            }
            None => false,
        });
    }

    /// Apply an action from any view or menu.
    pub fn dispatch(self: &Rc<Self>, action: Action) {
        let lanes_before = self.session.borrow().editor.lanes;
        let result = self.session.borrow_mut().dispatch(action);
        if let Err(e) = result {
            self.report(e, false);
        }
        let lanes = self.session.borrow().editor.lanes;
        if lanes != lanes_before {
            let mut prefs = crate::prefs::Preferences::load();
            prefs.global_lanes = lanes;
            if let Err(e) = prefs.save() {
                tracing::warn!("cannot save lane preferences: {e}");
            }
        }
        self.after_change();
    }

    /// Run `f` on the session and report errors.
    pub fn with_session<R>(
        self: &Rc<Self>,
        f: impl FnOnce(&mut Session) -> Result<R, SessionError>,
    ) -> Option<R> {
        let result = f(&mut self.session.borrow_mut());
        let out = match result {
            Ok(v) => Some(v),
            Err(e) => {
                self.report(e, true);
                None
            }
        };
        self.after_change();
        out
    }

    /// Bring widgets in line with the session after a change.
    pub fn after_change(self: &Rc<Self>) {
        let (rev, model_rev) = {
            let s = self.session.borrow();
            (s.layout_revision(), s.revision())
        };
        self.shown_rev.set(model_rev);
        if rev != self.layout_rev.get() {
            self.layout_rev.set(rev);
            crate::dock::realize(self);
        }
        self.redraw_all();
        self.update_chrome(true);
        crate::recent::note_session_path(self);
        crate::listen::follow(self);
        crate::console::follow(self);
    }

    /// Show an error: always in the status bar, optionally as a dialog.
    pub fn report(self: &Rc<Self>, err: SessionError, dialog: bool) {
        let text = err.to_string();
        self.session
            .borrow_mut()
            .notify(NoticeLevel::Error, text.clone());
        if dialog && let Some(win) = self.window.borrow().as_ref() {
            gtk::AlertDialog::builder()
                .message("FaderFrame")
                .detail(text)
                .modal(true)
                .build()
                .show(Some(win));
        }
    }

    /// Start (or restart) audio with the current options.
    pub fn start_audio(self: &Rc<Self>) {
        let (choice, prefs) = {
            let o = self.options.borrow();
            (
                o.backend,
                AudioPreferences {
                    sample_rate: o.sample_rate,
                    buffer_size: o.buffer_size,
                    threads: o.threads,
                },
            )
        };
        let result = self
            .session
            .borrow_mut()
            .start_audio(choice.backends(), &prefs);
        if let Err(e) = result {
            self.report(e, false);
        }
        self.after_change();
    }

    /// Frame tick (~60 Hz): poll the engine, animate chrome.
    pub fn tick(self: &Rc<Self>) {
        let now = Instant::now();
        let dt = self
            .last_tick
            .replace(Some(now))
            .map_or(1.0 / 60.0, |t| (now - t).as_secs_f32().min(0.25));
        let changed = match self.session.try_borrow_mut() {
            Ok(mut s) => {
                s.tick(dt);
                s.revision() != self.shown_rev.get()
            }
            Err(_) => false,
        };
        if changed {
            // e.g. a take placed after the recording stopped.
            self.after_change();
        }
        crate::plugin_window::tick(self);
        // Windows views asked for.
        let requests = self
            .session
            .try_borrow_mut()
            .map(|mut s| s.take_ui_requests())
            .unwrap_or_default();
        for r in requests {
            match r {
                faderframe_session::UiRequest::ReopenAudio => self.start_audio(),
                faderframe_session::UiRequest::TempoFromHits => {
                    crate::dialogs::tempo_from_hits(self);
                }
                faderframe_session::UiRequest::PluginBrowser { track, target } => {
                    crate::plugin_browser::open(self, track, target);
                }
                faderframe_session::UiRequest::PluginEditor {
                    plugin, generic, ..
                } => crate::plugin_window::open(self, plugin, generic),
                faderframe_session::UiRequest::ImportSysex { clip, at } => {
                    crate::dialogs::import_sysex(self, clip, at);
                }
                faderframe_session::UiRequest::AlbumDetails(song) => {
                    crate::dialogs::album_details(self, song);
                }
                faderframe_session::UiRequest::SavePluginPreset { plugin } => {
                    crate::dialogs::save_preset(self, plugin);
                }
                faderframe_session::UiRequest::SaveTrackPreset { tracks } => {
                    crate::dialogs::save_track_presets(self, tracks);
                }
                faderframe_session::UiRequest::DeletePreset { path, name } => {
                    crate::dialogs::delete_preset(self, path, &name);
                }
                faderframe_session::UiRequest::RenameGroup(group) => {
                    crate::dialogs::rename_group(self, group);
                }
                faderframe_session::UiRequest::PickColor(target) => {
                    crate::dialogs::pick_color(self, target);
                }
                faderframe_session::UiRequest::SaveVersion => crate::dialogs::save_version(self),
                faderframe_session::UiRequest::Versions => crate::dialogs::versions(self),
                faderframe_session::UiRequest::SaveTemplate => crate::templates::save_prompt(self),
                faderframe_session::UiRequest::Templates => crate::templates::window(self),
                faderframe_session::UiRequest::ImportVideo => crate::video::import(self),
                faderframe_session::UiRequest::ExportMovie => crate::video::export(self),
                faderframe_session::UiRequest::FullScreen(view) => {
                    crate::video::full_screen(self, view);
                }
                faderframe_session::UiRequest::SaveSample {
                    track,
                    start,
                    end,
                    name,
                } => crate::dialogs::save_sample(self, track, start, end, &name),
            }
        }
        let report = self
            .plugin_scan
            .borrow()
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(r) = report {
            self.plugin_scan.borrow_mut().take();
            if let Ok(mut s) = self.session.try_borrow_mut() {
                for e in &r.new_errors {
                    s.notify(
                        faderframe_session::NoticeLevel::Warning,
                        format!("plugin scan: {e}"),
                    );
                }
                let text = if r.lv2 > 0 {
                    format!(
                        "{} CLAP, {} VST3 and {} LV2 plugin{} available",
                        r.clap,
                        r.vst3,
                        r.lv2,
                        if r.lv2 == 1 { "" } else { "s" }
                    )
                } else {
                    format!(
                        "{} CLAP and {} VST3 plugin{} available",
                        r.clap,
                        r.vst3,
                        if r.vst3 == 1 { "" } else { "s" }
                    )
                };
                s.notify(faderframe_session::NoticeLevel::Info, text);
            }
        }
        let frame = self.frame.get() + 1;
        self.frame.set(frame);
        // Text-heavy chrome updates at ~10 Hz; the time display every frame.
        self.update_chrome(frame.is_multiple_of(6));
    }

    fn update_chrome(self: &Rc<Self>, full: bool) {
        let Ok(session) = self.session.try_borrow() else {
            return;
        };
        if let Some(chrome) = self.chrome.borrow().as_ref() {
            chrome.update(&session, full);
        }
        if full && let Some(win) = self.window.borrow().as_ref() {
            let title = session.title();
            if win.title().as_deref() != Some(title.as_str()) {
                win.set_title(Some(&title));
            }
        }
    }
}

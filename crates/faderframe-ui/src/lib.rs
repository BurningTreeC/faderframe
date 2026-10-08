//! FaderFrame's GTK 4 application shell.
//!
//! GTK owns the event loop, windows, menus, dialogs, text input,
//! accessibility and platform integration. The DAW work surfaces (arranger,
//! mixer, piano roll, transport display) are toolkit-independent
//! `CanvasView`s hosted by [`canvas::CanvasWidget`] and painted through GSK
//! render nodes. Nothing in the engine depends on this crate.
//!
//! Wayland: the shell uses only GTK/GDK APIs, so under a Wayland session it
//! runs as a native Wayland client (no XWayland); the status bar shows the
//! active GDK backend. HiDPI and fractional scaling are handled by GTK —
//! views work in logical pixels.

mod actions;
pub mod canvas;
mod console;
mod dialogs;
pub mod dock;
pub mod gpu;
#[cfg(all(feature = "gpu-painter", windows))]
mod gpu_win32;
mod icons;
mod learn;
mod listen;
mod midi_prefs;
pub mod painter;
mod palette;
mod paths;
mod plugin_browser;
mod plugin_window;
pub mod plugins;
mod preferences;
pub mod prefs;
mod recent;
mod recording;
mod render;
mod screenshot;
pub mod state;
mod style;
mod surface_prefs;
mod templates;
mod transport_display;
mod transport_keys;
mod video;
mod window;

pub use state::{BackendChoice, RunOptions};

use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::Session;
use gtk::prelude::*;
use gtk::{gio, glib};
use state::AppState;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

pub const APP_ID: &str = "io.github.BurningTreeC.FaderFrame";

/// What to start with: a project file, a new empty project or the demo.
enum Start {
    Open(std::path::PathBuf),
    New,
    /// A new project from the default template (its file).
    Template(std::path::PathBuf),
    Demo,
}

/// A new project: from the default template when one is set.
fn new_start() -> Start {
    templates::default_template().map_or(Start::New, |t| Start::Template(t.path))
}

/// The command line first (`PROJECT`, `--empty`, `--demo`), then the
/// start-up preference; the last project falls back to the demo session on
/// a first start and to a new project when its file is gone.
fn start_choice(
    options: &RunOptions,
    prefs: &prefs::Preferences,
) -> (Start, Option<std::path::PathBuf>) {
    use recent::StartupProject as S;
    if let Some(p) = &options.project {
        return (Start::Open(p.clone()), None);
    }
    if options.empty {
        return (Start::New, None);
    }
    if options.demo {
        return (Start::Demo, None);
    }
    match S::from_id(&prefs.startup_project).unwrap_or_default() {
        S::New => (new_start(), None),
        S::Demo => (Start::Demo, None),
        S::Last => match recent::startup_path(prefs) {
            Some(p) if p.exists() => (Start::Open(p), None),
            // The last project is gone.
            Some(p) => (new_start(), Some(p)),
            None => (Start::Demo, None),
        },
    }
}

fn build_session(options: &RunOptions, prefs: &prefs::Preferences) -> (Session, Option<String>) {
    let config = EngineConfig {
        sample_rate: options.sample_rate.unwrap_or(48_000),
        ..EngineConfig::default()
    };
    let (start, gone) = start_choice(options, prefs);
    let mut error = None;
    match &start {
        Start::Open(p) => tracing::info!("start-up: opening {}", p.display()),
        Start::New => tracing::info!("start-up: a new project"),
        Start::Template(p) => tracing::info!("start-up: a new project from {}", p.display()),
        Start::Demo => tracing::info!("start-up: the demo session"),
    }
    let make = || -> Result<Session, faderframe_session::SessionError> {
        match start {
            Start::Demo => Session::demo(config),
            Start::New | Start::Open(_) | Start::Template(_) => {
                let mut s =
                    Session::new(Project::new("Untitled", config.sample_rate), None, config)?;
                match &start {
                    Start::Template(path) => s.new_from_template(path)?,
                    _ => s.new_project(false)?,
                }
                Ok(s)
            }
        }
    };
    let mut session = match make() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("cannot create the session: {e}");
            #[allow(clippy::expect_used)]
            let s = Session::new(Project::new("Untitled", config.sample_rate), None, config)
                .expect("an empty session can always be created");
            s
        }
    };
    if let Err(e) = session.set_plugin_double_precision(prefs.plugin_double_precision) {
        tracing::warn!("plugin precision: {e}");
    }
    let ahead = (prefs.render_ahead_ms > 0)
        .then(|| std::time::Duration::from_millis(u64::from(prefs.render_ahead_ms)));
    if let Err(e) = session
        .set_render_ahead_buses(prefs.render_ahead_buses)
        .and_then(|()| session.set_render_ahead(ahead))
    {
        tracing::warn!("render ahead: {e}");
    }
    listen::apply_preferences(&mut session, prefs);
    let room = prefs
        .headphones
        .as_deref()
        .and_then(faderframe_binaural::Room::from_id);
    if let Err(e) = session.set_headphones(room) {
        tracing::warn!("headphones: {e}");
    }
    if let Some(p) = gone {
        session.notify(
            faderframe_session::NoticeLevel::Warning,
            format!(
                "The last project ({}) is gone; started a new one",
                p.display()
            ),
        );
        recent::forget_path(&p);
    }
    if let Start::Open(path) = &start
        && let Err(e) = session.open(path)
    {
        error = Some(format!("Cannot open {}: {e}", path.display()));
    }
    if !options.import.is_empty() {
        session.import_audio(
            options.import.clone(),
            faderframe_session::ImportTarget {
                track: None,
                at: faderframe_timeline::MusicalTime::ZERO,
            },
        );
    }
    (session, error)
}

fn activate(app: &gtk::Application, options: &RunOptions) -> Rc<AppState> {
    let mut options = options.clone();
    let prefs = prefs::Preferences::load();
    if options.backend == BackendChoice::Auto {
        options.backend = prefs.backend();
    }
    options.sample_rate = options.sample_rate.or(prefs.sample_rate);
    options.buffer_size = options.buffer_size.or(prefs.buffer_size);
    options.threads = options.threads.or(prefs.threads);
    let swept = faderframe_session::media::sweep_stale_scratch(Duration::from_secs(7 * 24 * 3600));
    if swept > 0 {
        tracing::info!("removed {swept} stale scratch media folder(s)");
    }
    let (mut session, error) = build_session(&options, &prefs);
    session.editor.snap = prefs.snap;
    session.editor.follow_playhead = prefs.follow_playhead;
    if let Err(e) = session.dispatch(faderframe_session::Action::SetRecordSettings(
        prefs.record_settings(),
    )) {
        tracing::warn!("recording settings: {e}");
    }
    session.set_sync_settings(prefs.sync_settings());
    session.set_video_settings(prefs.video_settings());
    // Video frames go to the display as dmabufs it takes.
    #[cfg(target_os = "linux")]
    if let Some(display) = gtk::gdk::Display::default() {
        let f = display.dmabuf_formats();
        let formats = (0..f.n_formats()).map(|i| f.format(i)).collect();
        faderframe_video::zero_copy::set_display_formats(formats);
        faderframe_video::zero_copy::set_allowed(prefs.video_zero_copy);
    }
    // The picture on a DeckLink output, when one was chosen and is there.
    if let Some((device, mode)) = prefs.video_output.clone()
        && faderframe_video::output::decklink_devices()
            .iter()
            .any(|(n, _)| *n == device)
        && let Err(e) = session.set_picture_output(Some(faderframe_video::output::Sink::DeckLink {
            device,
            mode,
        }))
    {
        tracing::warn!("picture output: {e}");
    }
    session.editor.show_edit_toolbar = prefs.show_edit_toolbar;
    // MIDI keyboards and controllers (FADERFRAME_NO_MIDI=1 keeps the
    // system's devices closed, e.g. for scripted runs).
    if std::env::var_os("FADERFRAME_NO_MIDI").is_none() {
        session.start_midi(&faderframe_session::MidiPreferences {
            disabled_inputs: prefs.midi_disabled_inputs.clone(),
            disabled_outputs: prefs.midi_disabled_outputs.clone(),
            clock_outputs: prefs.midi_clock_outputs.clone(),
            mtc_outputs: prefs.midi_mtc_outputs.clone(),
        });
        // Control surfaces (OSC too: scripted runs keep them all closed).
        session.set_control_surfaces(prefs.control_surfaces.clone());
    }
    let state = AppState::new(app, session, options);
    transport_keys::install(&state);
    let window = window::build(&state);
    actions::install(&state);
    recording::install_actions(&state);
    listen::install_actions(&state);
    console::install_actions(&state);
    actions::install_window_keys(&state, &window);
    dialogs::install_close_guard(&state, &window);
    dock::realize(&state);
    window.present();
    tracing::info!(
        "display backend: {}",
        gtk::prelude::WidgetExt::display(&window).type_().name()
    );
    state.start_audio();
    *state.plugin_scan.borrow_mut() = Some(plugins::scan_in_background());
    if let Some(e) = error {
        state.report(faderframe_session::SessionError::Other(e), true);
    }
    // Smoke-testing aid: FADERFRAME_STARTUP_ACTIONS="play,wait:2000,stop"
    // activates application actions shortly after start-up, in order;
    // `wait:<ms>` pauses between them.
    if let Ok(list) = std::env::var("FADERFRAME_STARTUP_ACTIONS") {
        let steps: Vec<String> = list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        run_startup_actions(app.clone(), steps, 0, 700);
    }
    let weak = Rc::downgrade(&state);
    glib::timeout_add_local(Duration::from_millis(16), move || match weak.upgrade() {
        Some(s) => {
            s.tick();
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
    state
}

fn run_startup_actions(app: gtk::Application, steps: Vec<String>, i: usize, delay_ms: u64) {
    glib::timeout_add_local_once(Duration::from_millis(delay_ms), move || {
        let mut i = i;
        while let Some(step) = steps.get(i) {
            i += 1;
            if let Some(ms) = step.strip_prefix("wait:").and_then(|v| v.parse().ok()) {
                run_startup_actions(app, steps, i, ms);
                return;
            }
            tracing::info!("startup action: {step}");
            // `name:argument` passes a string parameter.
            match step.split_once(':') {
                Some((name, arg)) => app.activate_action(name, Some(&arg.to_variant())),
                None => app.activate_action(step, None),
            }
        }
    });
}

/// Run the application; returns the process exit code.
pub fn run(options: RunOptions) -> glib::ExitCode {
    plugins::install();
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let holder: Rc<RefCell<Option<Rc<AppState>>>> = Rc::default();
    app.connect_startup(|_| {
        style::install(&faderframe_ui_canvas::Theme::by_id(
            &prefs::Preferences::load().theme,
        ));
        icons::install();
    });
    {
        let holder = Rc::clone(&holder);
        app.connect_activate(move |app| {
            if let Some(s) = holder.borrow().as_ref() {
                if let Some(w) = s.window.borrow().as_ref() {
                    w.present();
                }
                return;
            }
            let state = activate(app, &options);
            *holder.borrow_mut() = Some(state);
        });
    }
    {
        let holder = Rc::clone(&holder);
        app.connect_shutdown(move |_| {
            if let Some(s) = holder.borrow_mut().take() {
                s.session.borrow_mut().stop_audio();
            }
        });
    }
    app.run_with_args(&["faderframe"])
}
pub use gtk::glib::ExitCode;

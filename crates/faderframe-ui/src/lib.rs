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
mod dialogs;
pub mod dock;
pub mod painter;
mod placeholder;
mod plugin_browser;
mod plugin_window;
mod plugins;
mod preferences;
pub mod prefs;
mod recording;
mod render;
mod screenshot;
pub mod state;
mod style;
mod transport_display;
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

fn build_session(options: &RunOptions) -> (Session, Option<String>) {
    let config = EngineConfig {
        sample_rate: options.sample_rate.unwrap_or(48_000),
        ..EngineConfig::default()
    };
    let make = || -> Result<Session, faderframe_session::SessionError> {
        if options.empty {
            let mut s = Session::new(Project::new("Untitled", config.sample_rate), None, config)?;
            s.new_project(false)?;
            Ok(s)
        } else {
            Session::demo(config)
        }
    };
    let mut session = match make() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("cannot create the demo session: {e}");
            #[allow(clippy::expect_used)]
            let s = Session::new(Project::new("Untitled", config.sample_rate), None, config)
                .expect("an empty session can always be created");
            s
        }
    };
    let mut error = None;
    if let Some(path) = &options.project
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
    let swept = faderframe_session::media::sweep_stale_scratch(Duration::from_secs(7 * 24 * 3600));
    if swept > 0 {
        tracing::info!("removed {swept} stale scratch media folder(s)");
    }
    let (mut session, error) = build_session(&options);
    session.editor.snap = prefs.snap;
    session.editor.follow_playhead = prefs.follow_playhead;
    if let Err(e) = session.dispatch(faderframe_session::Action::SetRecordSettings(
        prefs.record_settings(),
    )) {
        tracing::warn!("recording settings: {e}");
    }
    let state = AppState::new(app, session, options);
    let window = window::build(&state);
    actions::install(&state);
    recording::install_actions(&state);
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
    app.connect_startup(|_| style::install());
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

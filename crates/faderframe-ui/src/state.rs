//! Shared GUI state: the session plus the GTK objects that present it.

use crate::canvas::CanvasWidget;
use crate::dock::DockState;
use crate::window::Chrome;
use faderframe_audio::AudioBackend;
use faderframe_session::{Action, AudioPreferences, NoticeLevel, Session, SessionError};
use faderframe_ui_canvas::Theme;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    /// JACK if a server is reachable, otherwise the silent dummy device.
    #[default]
    Auto,
    Jack,
    Dummy,
}

impl BackendChoice {
    pub fn label(self) -> &'static str {
        match self {
            BackendChoice::Auto => "Automatic (JACK, else silent)",
            BackendChoice::Jack => "JACK / PipeWire-JACK",
            BackendChoice::Dummy => "No audio device (silent)",
        }
    }

    pub fn backends(self) -> Vec<Box<dyn AudioBackend>> {
        let jack = || Box::new(faderframe_audio_jack::JackBackend) as Box<dyn AudioBackend>;
        let dummy = || Box::new(faderframe_audio::dummy::DummyBackend) as Box<dyn AudioBackend>;
        match self {
            BackendChoice::Auto => vec![jack(), dummy()],
            BackendChoice::Jack => vec![jack()],
            BackendChoice::Dummy => vec![dummy()],
        }
    }
}

/// Command-line / startup options.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub backend: BackendChoice,
    pub project: Option<PathBuf>,
    pub empty: bool,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
}

pub struct AppState {
    pub app: gtk::Application,
    pub session: RefCell<Session>,
    pub theme: Theme,
    pub options: RefCell<RunOptions>,
    pub dock: RefCell<DockState>,
    pub window: RefCell<Option<gtk::ApplicationWindow>>,
    pub chrome: RefCell<Option<Chrome>>,
    canvases: RefCell<Vec<glib::WeakRef<CanvasWidget>>>,
    layout_rev: Cell<u64>,
    last_tick: Cell<Option<Instant>>,
    frame: Cell<u64>,
}

impl AppState {
    pub fn new(app: &gtk::Application, session: Session, options: RunOptions) -> Rc<Self> {
        Rc::new(Self {
            app: app.clone(),
            layout_rev: Cell::new(session.layout_revision()),
            session: RefCell::new(session),
            theme: Theme::studio(),
            options: RefCell::new(options),
            dock: RefCell::new(DockState::default()),
            window: RefCell::new(None),
            chrome: RefCell::new(None),
            canvases: RefCell::new(Vec::new()),
            last_tick: Cell::new(None),
            frame: Cell::new(0),
        })
    }

    pub fn register_canvas(&self, c: &CanvasWidget) {
        self.canvases.borrow_mut().push(c.downgrade());
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
        let result = self.session.borrow_mut().dispatch(action);
        if let Err(e) = result {
            self.report(e, false);
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
        let rev = self.session.borrow().layout_revision();
        if rev != self.layout_rev.get() {
            self.layout_rev.set(rev);
            crate::dock::realize(self);
        }
        self.redraw_all();
        self.update_chrome(true);
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
        if let Ok(mut s) = self.session.try_borrow_mut() {
            s.tick(dt);
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

//! GTK host for [`CanvasView`]s.
//!
//! `CanvasWidget` is a custom `gtk::Widget` subclass. It paints its view
//! through [`SnapshotPainter`] in `snapshot()`, turns GTK event-controller
//! signals into toolkit-neutral [`ViewEvent`]s, executes the view's
//! [`HostRequest`]s (native popover menus, inline text entry) and forwards
//! emitted actions to the [`AppState`]. The same widget instance (and so the
//! same view state) is re-parented when its view moves between docks and
//! windows.

use crate::painter::{PathCache, SnapshotPainter, TextCache};
use crate::state::AppState;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Cursor, EventCx, FileChoice, HostRequest, Key, MenuItem, Modifiers, Point,
    PointerButton, ScrollAxis, Size, ViewEvent,
};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub type DynView = Box<dyn CanvasView<Session, Action>>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CanvasWidget {
        pub view: RefCell<Option<DynView>>,
        pub app: RefCell<Weak<AppState>>,
        pub text_cache: RefCell<TextCache>,
        pub path_cache: RefCell<PathCache>,
        pub dragging: Cell<bool>,
        pub drag_button: Cell<u32>,
        pub drag_origin: Cell<(f64, f64)>,
        pub last_cursor: Cell<Option<Cursor>>,
        pub hadj: RefCell<Option<gtk::Adjustment>>,
        pub vadj: RefCell<Option<gtk::Adjustment>>,
        /// The scrollbars (their margins follow the view's scrolled area).
        pub hbar: RefCell<Option<gtk::Scrollbar>>,
        pub vbar: RefCell<Option<gtk::Scrollbar>>,
        /// The bars lie over the canvas: they also keep clear of each other.
        pub overlaid: Cell<bool>,
        pub syncing: Cell<bool>,
        /// The view a drag that left this one hovers over (it carries a
        /// payload).
        pub payload_target: RefCell<Option<glib::WeakRef<super::CanvasWidget>>>,
        /// The payload left the window: a native drag carries it to other
        /// windows (this view's own drag is over).
        pub native_drag: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CanvasWidget {
        const NAME: &'static str = "FaderFrameCanvas";
        type Type = super::CanvasWidget;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CanvasWidget {}

    impl WidgetImpl for CanvasWidget {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let Some(app) = self.app.borrow().upgrade() else {
                return;
            };
            let Ok(session) = app.session.try_borrow() else {
                // Painting while the session is being mutated would be a
                // re-entrancy bug; skip the frame instead of panicking.
                widget.queue_draw();
                return;
            };
            let size = Size::new(widget.width() as f32, widget.height() as f32);
            // Dense views on the GPU (Preferences → General).
            if app.gpu_painter.get()
                && let Some(view) = self.view.borrow_mut().as_mut()
                && view.dense()
            {
                let w: &gtk::Widget = widget.upcast_ref();
                let scale =
                    w.native()
                        .and_then(|n| n.surface())
                        .map_or(w.scale_factor() as f64, |s| s.scale()) as f32;
                let theme = app.theme.borrow();
                let started = paint_stats::enabled().then(std::time::Instant::now);
                let mut counts = None;
                let painted = crate::gpu::paint(snapshot, size.w, size.h, scale, |p| {
                    if started.is_some() {
                        let mut counting = paint_stats::Counting::new(p);
                        view.paint(&mut counting, size, &session, &theme);
                        counts = Some(counting.counts);
                    } else {
                        view.paint(p, size, &session, &theme);
                    }
                });
                if painted {
                    if let (Some(t), Some(c)) = (started, counts) {
                        paint_stats::note(w, t.elapsed(), c, true);
                    }
                    return;
                }
            }
            self.text_cache.borrow_mut().begin_frame();
            self.path_cache.borrow_mut().begin_frame();
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, size.w, size.h));
            let w: &gtk::Widget = widget.upcast_ref();
            let mut painter = SnapshotPainter::new(snapshot, w, &self.text_cache, &self.path_cache);
            if paint_stats::enabled() {
                let started = std::time::Instant::now();
                let mut counting = paint_stats::Counting::new(&mut painter);
                if let Some(view) = self.view.borrow_mut().as_mut() {
                    view.paint(&mut counting, size, &session, &app.theme.borrow());
                }
                let counts = counting.counts;
                paint_stats::note(w, started.elapsed(), counts, false);
            } else if let Some(view) = self.view.borrow_mut().as_mut() {
                view.paint(&mut painter, size, &session, &app.theme.borrow());
            }
            snapshot.pop();
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let min = self
                .view
                .borrow()
                .as_ref()
                .map_or(Size::new(50.0, 50.0), |v| v.min_size());
            let m = match orientation {
                gtk::Orientation::Horizontal => min.w,
                _ => min.h,
            } as i32;
            (m, m, -1, -1)
        }
    }
}

glib::wrapper! {
    pub struct CanvasWidget(ObjectSubclass<imp::CanvasWidget>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn modifiers(state: gdk::ModifierType) -> Modifiers {
    Modifiers {
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        meta: state.intersects(gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK),
    }
}

fn button(b: u32) -> PointerButton {
    match b {
        1 => PointerButton::Primary,
        2 => PointerButton::Middle,
        3 => PointerButton::Secondary,
        other => PointerButton::Other(other),
    }
}

fn map_key(key: gdk::Key) -> Key {
    match key {
        gdk::Key::space => Key::Space,
        gdk::Key::Return | gdk::Key::KP_Enter => Key::Enter,
        gdk::Key::Escape => Key::Escape,
        gdk::Key::Delete | gdk::Key::KP_Delete => Key::Delete,
        gdk::Key::BackSpace => Key::Backspace,
        gdk::Key::Tab | gdk::Key::ISO_Left_Tab => Key::Tab,
        gdk::Key::F1 => Key::F(1),
        gdk::Key::F2 => Key::F(2),
        gdk::Key::F3 => Key::F(3),
        gdk::Key::F4 => Key::F(4),
        gdk::Key::F5 => Key::F(5),
        gdk::Key::F6 => Key::F(6),
        gdk::Key::F7 => Key::F(7),
        gdk::Key::F8 => Key::F(8),
        gdk::Key::F9 => Key::F(9),
        gdk::Key::F10 => Key::F(10),
        gdk::Key::F11 => Key::F(11),
        gdk::Key::F12 => Key::F(12),
        gdk::Key::Left => Key::Left,
        gdk::Key::Right => Key::Right,
        gdk::Key::Up => Key::Up,
        gdk::Key::Down => Key::Down,
        gdk::Key::Home => Key::Home,
        gdk::Key::End => Key::End,
        gdk::Key::Page_Up => Key::PageUp,
        gdk::Key::Page_Down => Key::PageDown,
        k => k.to_unicode().map_or(Key::Other, Key::Char),
    }
}

fn cursor_name(c: Cursor) -> &'static str {
    match c {
        Cursor::Default => "default",
        Cursor::Pointer => "pointer",
        Cursor::Grab => "grab",
        Cursor::Grabbing => "grabbing",
        Cursor::ResizeHorizontal => "ew-resize",
        Cursor::ResizeVertical => "ns-resize",
        Cursor::Text => "text",
        Cursor::Crosshair => "crosshair",
        Cursor::Move => "move",
    }
}

impl CanvasWidget {
    /// The skin changed: the view takes the new theme and repaints.
    pub fn set_theme(&self, theme: &faderframe_ui_canvas::Theme) {
        if let Some(view) = self.imp().view.borrow_mut().as_mut() {
            view.set_theme(theme);
        }
        self.queue_resize();
        self.queue_draw();
    }

    pub fn new(app: &Rc<AppState>, view: DynView) -> Self {
        let w: Self = glib::Object::new();
        let imp = w.imp();
        *imp.view.borrow_mut() = Some(view);
        *imp.app.borrow_mut() = Rc::downgrade(app);
        w.set_focusable(true);
        w.set_hexpand(true);
        w.set_vexpand(true);
        w.set_has_tooltip(true);
        w.add_css_class("canvas");
        w.install_controllers();
        w.install_tick();
        w.connect_scale_factor_notify(|w| w.imp().text_cache.borrow_mut().clear());
        w
    }

    fn app(&self) -> Option<Rc<AppState>> {
        self.imp().app.borrow().upgrade()
    }

    /// Paint the view into a fresh render node, independent of GTK's
    /// redraw state (screenshots of a window that is not on screen).
    pub fn render_node(&self) -> Option<gtk::gsk::RenderNode> {
        let app = self.app()?;
        let session = app.session.try_borrow().ok()?;
        let size = Size::new(self.width() as f32, self.height() as f32);
        if size.w <= 0.0 || size.h <= 0.0 {
            return None;
        }
        let snapshot = gtk::Snapshot::new();
        let imp = self.imp();
        imp.text_cache.borrow_mut().begin_frame();
        imp.path_cache.borrow_mut().begin_frame();
        snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, size.w, size.h));
        let w: &gtk::Widget = self.upcast_ref();
        {
            let mut painter = SnapshotPainter::new(&snapshot, w, &imp.text_cache, &imp.path_cache);
            if let Some(view) = imp.view.borrow_mut().as_mut() {
                view.paint(&mut painter, size, &session, &app.theme.borrow());
            }
        }
        snapshot.pop();
        snapshot.to_node()
    }

    fn size(&self) -> Size {
        Size::new(self.width() as f32, self.height() as f32)
    }

    /// Deliver an event to the view and act on the result.
    pub fn deliver(&self, ev: ViewEvent) -> bool {
        let Some(app) = self.app() else { return false };
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let (handled, redraw, cursor) = {
            let Ok(session) = app.session.try_borrow() else {
                return false;
            };
            let mut cx = EventCx::new(&mut actions, &mut requests);
            let handled = match self.imp().view.borrow_mut().as_mut() {
                Some(v) => v.event(&ev, self.size(), &session, &mut cx),
                None => false,
            };
            (handled, cx.wants_redraw(), cx.cursor())
        };
        if let Some(c) = cursor
            && self.imp().last_cursor.get() != Some(c)
        {
            self.imp().last_cursor.set(Some(c));
            self.set_cursor_from_name(Some(cursor_name(c)));
        }
        for req in requests {
            self.handle_request(&app, req);
        }
        let had_actions = !actions.is_empty();
        for a in actions {
            app.dispatch(a);
        }
        if redraw || had_actions {
            self.queue_draw();
        }
        handled
    }

    /// What the view's drag carries to other views.
    fn payload(&self) -> Option<String> {
        let app = self.app()?;
        let session = app.session.try_borrow().ok()?;
        self.imp().view.borrow().as_ref()?.drag_payload(&session)
    }

    /// Another canvas at `(x, y)` (this one's coordinates) and the point in
    /// its coordinates.
    fn canvas_at(&self, x: f64, y: f64) -> Option<(CanvasWidget, Point)> {
        let root = self.root()?;
        let at = self.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))?;
        let hit = root.pick(
            f64::from(at.x()),
            f64::from(at.y()),
            gtk::PickFlags::DEFAULT,
        )?;
        let target = hit
            .ancestor(CanvasWidget::static_type())
            .and_then(|w| w.downcast::<CanvasWidget>().ok())?;
        if &target == self {
            return None;
        }
        let local = root.compute_point(&target, &at)?;
        Some((target, Point::new(local.x(), local.y())))
    }

    fn hover_payload(&self, payload: Option<(&str, Point)>) {
        let Some(app) = self.app() else { return };
        let redraw = {
            let Ok(session) = app.session.try_borrow() else {
                return;
            };
            match self.imp().view.borrow_mut().as_mut() {
                Some(v) => v.hover_payload(payload, self.size(), &session),
                None => false,
            }
        };
        if redraw {
            self.queue_draw();
        }
    }

    /// Is `(x, y)` (this view's coordinates) outside its window?
    fn outside_window(&self, x: f64, y: f64) -> bool {
        let Some(root) = self.root() else {
            return false;
        };
        let Some(at) = self.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return false;
        };
        let (w, h) = (f64::from(root.width()), f64::from(root.height()));
        let (ax, ay) = (f64::from(at.x()), f64::from(at.y()));
        ax < 0.0 || ay < 0.0 || ax >= w || ay >= h
    }

    /// The payload drag left the window: it goes on as a native drag that
    /// other windows' canvases take (their views' `drop_payload`); this
    /// view's own drag is undone.
    fn start_native_drag(&self, g: &gtk::GestureDrag, x: f64, y: f64) {
        let Some(payload) = self.payload() else {
            return;
        };
        let (Some(native), Some(device)) = (self.native(), g.device()) else {
            return;
        };
        let Some(surface) = native.surface() else {
            return;
        };
        let Some(at) = self.compute_point(&native, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        let (sx, sy) = native.surface_transform();
        let content = gdk::ContentProvider::for_value(&payload.to_value());
        let Some(drag) = gdk::Drag::begin(
            &surface,
            &device,
            &content,
            gdk::DragAction::COPY,
            f64::from(at.x()) + sx,
            f64::from(at.y()) + sy,
        ) else {
            return;
        };
        self.imp().native_drag.set(true);
        let done = glib::clone!(
            #[weak(rename_to = w)]
            self,
            move || w.imp().native_drag.set(false)
        );
        let done = Rc::new(done);
        drag.connect_dnd_finished({
            let done = done.clone();
            move |_| done()
        });
        drag.connect_cancel(move |_, _| done());
        if let Some(o) = self
            .imp()
            .payload_target
            .borrow_mut()
            .take()
            .and_then(|w| w.upgrade())
        {
            o.hover_payload(None);
        }
        let Some(app) = self.app() else { return };
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        {
            let mut cx = EventCx::new(&mut actions, &mut requests);
            if let Some(v) = self.imp().view.borrow_mut().as_mut() {
                v.cancel_drag(&mut cx);
            }
        }
        for a in actions {
            app.dispatch(a);
        }
        self.queue_draw();
    }

    /// Follow a drag that left this view with a payload: the view under it
    /// shows where it would go.
    fn track_payload(&self, x: f64, y: f64) {
        let inside = x >= 0.0 && y >= 0.0 && x < self.width() as f64 && y < self.height() as f64;
        let target = if inside {
            None
        } else {
            self.payload()
                .and_then(|p| self.canvas_at(x, y).map(|t| (p, t)))
        };
        let old = self
            .imp()
            .payload_target
            .borrow_mut()
            .take()
            .and_then(|w| w.upgrade());
        match target {
            Some((payload, (t, at))) => {
                if let Some(o) = old.filter(|o| *o != t) {
                    o.hover_payload(None);
                }
                t.hover_payload(Some((&payload, at)));
                *self.imp().payload_target.borrow_mut() = Some(t.downgrade());
            }
            None => {
                if let Some(o) = old {
                    o.hover_payload(None);
                }
            }
        }
    }

    /// The drag ended at `(x, y)`: another view under it takes the
    /// payload (this view's drag is cancelled). True when one did.
    fn drop_payload_at(&self, x: f64, y: f64) -> bool {
        if let Some(o) = self
            .imp()
            .payload_target
            .borrow_mut()
            .take()
            .and_then(|w| w.upgrade())
        {
            o.hover_payload(None);
        }
        let inside = x >= 0.0 && y >= 0.0 && x < self.width() as f64 && y < self.height() as f64;
        if inside {
            return false;
        }
        let Some(payload) = self.payload() else {
            return false;
        };
        let Some((target, at)) = self.canvas_at(x, y) else {
            return false;
        };
        let Some(app) = self.app() else { return false };
        let action = {
            let Ok(session) = app.session.try_borrow() else {
                return false;
            };
            match target.imp().view.borrow_mut().as_mut() {
                Some(v) => v.drop_payload(&payload, at, target.size(), &session),
                None => None,
            }
        };
        let Some(action) = action else {
            return false;
        };
        // This view's drag is undone, then the other view's action runs.
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        {
            let mut cx = EventCx::new(&mut actions, &mut requests);
            if let Some(v) = self.imp().view.borrow_mut().as_mut() {
                v.cancel_drag(&mut cx);
            }
        }
        for a in actions {
            app.dispatch(a);
        }
        app.dispatch(action);
        self.queue_draw();
        target.queue_draw();
        true
    }

    fn handle_request(&self, app: &Rc<AppState>, req: HostRequest<Action>) {
        match req {
            HostRequest::GrabFocus => {
                self.grab_focus();
            }
            HostRequest::ContextMenu { at, items } => show_menu(self.upcast_ref(), at, items, app),
            HostRequest::TextInput {
                at,
                initial,
                commit,
            } => {
                let popover = gtk::Popover::new();
                popover.set_has_arrow(true);
                let entry = gtk::Entry::new();
                entry.set_text(&initial);
                entry.set_width_chars(16);
                popover.set_child(Some(&entry));
                popover.set_parent(self);
                popover.set_pointing_to(Some(&gdk::Rectangle::new(
                    at.x as i32,
                    at.y as i32,
                    at.w.max(1.0) as i32,
                    at.h.max(1.0) as i32,
                )));
                let commit = Rc::new(commit);
                let weak_app = Rc::downgrade(app);
                entry.connect_activate(glib::clone!(
                    #[weak]
                    popover,
                    move |e| {
                        let text = e.text().to_string();
                        popover.popdown();
                        if let (Some(action), Some(app)) = (commit(&text), weak_app.upgrade()) {
                            app.dispatch(action);
                        }
                    }
                ));
                popover.connect_closed(|p| {
                    let p = p.clone();
                    glib::idle_add_local_once(move || p.unparent());
                });
                popover.popup();
                entry.grab_focus();
                entry.select_region(0, -1);
            }
            HostRequest::ChooseFiles { choice, commit } => {
                let window = self.root().and_downcast::<gtk::Window>();
                let weak_app = Rc::downgrade(app);
                let finish = move |paths: Vec<std::path::PathBuf>| {
                    if paths.is_empty() {
                        return;
                    }
                    if let (Some(action), Some(app)) = (commit(paths), weak_app.upgrade()) {
                        app.dispatch(action);
                    }
                };
                match choice {
                    FileChoice::Open { title, filters } => {
                        let list = gio::ListStore::new::<gtk::FileFilter>();
                        for (name, patterns) in &filters {
                            let f = gtk::FileFilter::new();
                            f.set_name(Some(name));
                            for p in patterns {
                                f.add_pattern(p);
                            }
                            list.append(&f);
                        }
                        let dialog = gtk::FileDialog::builder()
                            .title(title.as_str())
                            .modal(true)
                            .filters(&list)
                            .build();
                        dialog.open_multiple(window.as_ref(), gio::Cancellable::NONE, move |res| {
                            let Ok(files) = res else { return };
                            let paths = (0..files.n_items())
                                .filter_map(|i| files.item(i).and_downcast::<gio::File>())
                                .filter_map(|f| f.path())
                                .collect();
                            finish(paths);
                        });
                    }
                    FileChoice::Folder { title, initial } => {
                        let dialog = gtk::FileDialog::builder()
                            .title(title.as_str())
                            .modal(true)
                            .build();
                        if let Some(dir) = initial.filter(|d| d.is_dir()) {
                            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
                        }
                        dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |res| {
                            if let Some(p) = res.ok().and_then(|f| f.path()) {
                                finish(vec![p]);
                            }
                        });
                    }
                }
            }
        }
    }

    fn install_controllers(&self) {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |g, n, x, y| {
                let b = g.current_button();
                w.imp().drag_button.set(b);
                w.deliver(ViewEvent::PointerDown {
                    pos: Point::new(x as f32, y as f32),
                    button: button(b),
                    modifiers: modifiers(g.current_event_state()),
                    clicks: n.max(1) as u32,
                });
            }
        ));
        self.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |_, x, y| {
                w.imp().dragging.set(true);
                w.imp().drag_origin.set((x, y));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |g, dx, dy| {
                let (x, y) = w.imp().drag_origin.get();
                w.deliver(ViewEvent::PointerMove {
                    pos: Point::new((x + dx) as f32, (y + dy) as f32),
                    modifiers: modifiers(g.current_event_state()),
                    dragging: true,
                });
                if w.imp().native_drag.get() {
                    return;
                }
                w.track_payload(x + dx, y + dy);
                if w.outside_window(x + dx, y + dy) {
                    w.start_native_drag(g, x + dx, y + dy);
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |g, dx, dy| {
                w.imp().dragging.set(false);
                let (x, y) = w.imp().drag_origin.get();
                // Dropped on another view that takes what is dragged (a
                // native drag carries it elsewhere instead).
                if !w.imp().native_drag.get() {
                    w.drop_payload_at(x + dx, y + dy);
                }
                w.deliver(ViewEvent::PointerUp {
                    pos: Point::new((x + dx) as f32, (y + dy) as f32),
                    button: button(w.imp().drag_button.get()),
                    modifiers: modifiers(g.current_event_state()),
                });
            }
        ));
        self.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |c, x, y| {
                if !w.imp().dragging.get() {
                    w.deliver(ViewEvent::PointerMove {
                        pos: Point::new(x as f32, y as f32),
                        modifiers: modifiers(c.current_event_state()),
                        dragging: false,
                    });
                }
            }
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |_| {
                w.deliver(ViewEvent::PointerLeave);
            }
        ));
        self.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |c, dx, dy| {
                let precise = c.unit() == gdk::ScrollUnit::Surface;
                let pos = w.pointer_position();
                let handled = w.deliver(ViewEvent::Scroll {
                    pos,
                    dx: dx as f32,
                    dy: dy as f32,
                    modifiers: modifiers(c.current_event_state()),
                    precise,
                });
                if handled {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        self.add_controller(scroll);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let k = map_key(key);
                // Space belongs to the window transport controller.
                if k == Key::Space {
                    return glib::Propagation::Proceed;
                }
                if w.deliver(ViewEvent::Key {
                    key: k,
                    modifiers: modifiers(state),
                }) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        self.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |_| {
                w.deliver(ViewEvent::FocusLost);
            }
        ));
        self.add_controller(focus);

        // Files dragged in from a file manager (or another app).
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let hover = |w: &CanvasWidget, pos: Option<Point>| -> gdk::DragAction {
            let Some(app) = w.app() else {
                return gdk::DragAction::empty();
            };
            let Ok(session) = app.session.try_borrow() else {
                return gdk::DragAction::empty();
            };
            let accepted = w
                .imp()
                .view
                .borrow_mut()
                .as_mut()
                .is_some_and(|v| v.drag_files(pos, w.size(), &session));
            w.queue_draw();
            if accepted {
                gdk::DragAction::COPY
            } else {
                gdk::DragAction::empty()
            }
        };
        drop.connect_enter(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_, x, y| hover(&w, Some(Point::new(x as f32, y as f32)))
        ));
        drop.connect_motion(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_, x, y| hover(&w, Some(Point::new(x as f32, y as f32)))
        ));
        drop.connect_leave(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |_| {
                hover(&w, None);
            }
        ));
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            false,
            move |_, value, x, y| {
                let Ok(list) = value.get::<gdk::FileList>() else {
                    return false;
                };
                let files: Vec<std::path::PathBuf> =
                    list.files().iter().filter_map(|f| f.path()).collect();
                let Some(app) = w.app() else { return false };
                let action = {
                    let Ok(session) = app.session.try_borrow() else {
                        return false;
                    };
                    w.imp().view.borrow_mut().as_mut().and_then(|v| {
                        v.drop_files(&files, Point::new(x as f32, y as f32), w.size(), &session)
                    })
                };
                w.queue_draw();
                match action {
                    Some(a) => {
                        app.dispatch(a);
                        true
                    }
                    None => false,
                }
            }
        ));
        self.add_controller(drop);

        // Payloads (clips) dragged from another window's views.
        let payload = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::COPY);
        payload.set_preload(true);
        let over = |w: &CanvasWidget, t: &gtk::DropTarget, pos: Option<Point>| -> gdk::DragAction {
            let text = t.value().and_then(|v| v.get::<String>().ok());
            let Some(text) = text.filter(|s| s.starts_with("clips:")) else {
                return gdk::DragAction::empty();
            };
            w.hover_payload(pos.map(|p| (text.as_str(), p)));
            gdk::DragAction::COPY
        };
        payload.connect_enter(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |t, x, y| over(&w, t, Some(Point::new(x as f32, y as f32)))
        ));
        payload.connect_motion(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |t, x, y| over(&w, t, Some(Point::new(x as f32, y as f32)))
        ));
        payload.connect_leave(glib::clone!(
            #[weak(rename_to = w)]
            self,
            move |_| w.hover_payload(None)
        ));
        payload.connect_drop(glib::clone!(
            #[weak(rename_to = w)]
            self,
            #[upgrade_or]
            false,
            move |_, value, x, y| {
                w.hover_payload(None);
                let Ok(text) = value.get::<String>() else {
                    return false;
                };
                let Some(app) = w.app() else { return false };
                let action = {
                    let Ok(session) = app.session.try_borrow() else {
                        return false;
                    };
                    w.imp().view.borrow_mut().as_mut().and_then(|v| {
                        v.drop_payload(&text, Point::new(x as f32, y as f32), w.size(), &session)
                    })
                };
                w.queue_draw();
                match action {
                    Some(a) => {
                        app.dispatch(a);
                        true
                    }
                    None => false,
                }
            }
        ));
        self.add_controller(payload);

        self.connect_query_tooltip(|w, x, y, keyboard, tooltip| {
            if keyboard {
                return false;
            }
            let Some(app) = w.app() else { return false };
            let Ok(session) = app.session.try_borrow() else {
                return false;
            };
            let text = w
                .imp()
                .view
                .borrow()
                .as_ref()
                .and_then(|v| v.tooltip(Point::new(x as f32, y as f32), w.size(), &session));
            match text {
                Some(t) => {
                    tooltip.set_text(Some(&t));
                    true
                }
                None => false,
            }
        });
    }

    fn pointer_position(&self) -> Point {
        self.root()
            .and_then(|root| {
                let surface = root.native()?.surface()?;
                let seat = self.display().default_seat()?;
                let pointer = seat.pointer()?;
                let (x, y, _) = surface.device_position(&pointer)?;
                let p = root.compute_point(self, &graphene::Point::new(x as f32, y as f32))?;
                Some(Point::new(p.x(), p.y()))
            })
            .unwrap_or_default()
    }

    fn install_tick(&self) {
        self.add_tick_callback(|w, _clock| {
            if let Some(app) = w.app()
                && let Ok(session) = app.session.try_borrow()
            {
                let animate = w
                    .imp()
                    .view
                    .borrow()
                    .as_ref()
                    .is_some_and(|v| v.wants_frames(&session));
                drop(session);
                if animate {
                    w.queue_draw();
                }
                w.sync_scrollbars();
            }
            glib::ControlFlow::Continue
        });
    }

    /// Attach native scrollbars driven by the view's scroll state.
    pub fn bind_scrollbars(&self, hbar: Option<gtk::Scrollbar>, vbar: Option<gtk::Scrollbar>) {
        let hadj = hbar.as_ref().map(|b| b.adjustment());
        let vadj = vbar.as_ref().map(|b| b.adjustment());
        // Held bars: the view leaves the scrolling to the hand (it stops
        // following the playhead). Seen in the capture phase, so the bar's
        // own dragging is untouched and the release arrives wherever the
        // pointer has gone.
        for (bar, axis) in [
            (hbar.as_ref(), ScrollAxis::Horizontal),
            (vbar.as_ref(), ScrollAxis::Vertical),
        ] {
            let Some(bar) = bar else { continue };
            let hold = gtk::EventControllerLegacy::new();
            hold.set_propagation_phase(gtk::PropagationPhase::Capture);
            hold.connect_event(glib::clone!(
                #[weak(rename_to = w)]
                self,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, event| {
                    let held = match event.event_type() {
                        gdk::EventType::ButtonPress | gdk::EventType::TouchBegin => true,
                        gdk::EventType::ButtonRelease
                        | gdk::EventType::TouchEnd
                        | gdk::EventType::TouchCancel
                        | gdk::EventType::GrabBroken => false,
                        _ => return glib::Propagation::Proceed,
                    };
                    if let Ok(mut view) = w.imp().view.try_borrow_mut()
                        && let Some(v) = view.as_mut()
                    {
                        v.scroll_held(axis, held);
                    }
                    glib::Propagation::Proceed
                }
            ));
            bar.add_controller(hold);
        }
        *self.imp().hbar.borrow_mut() = hbar;
        *self.imp().vbar.borrow_mut() = vbar;
        for (adj, axis) in [
            (hadj.as_ref(), ScrollAxis::Horizontal),
            (vadj.as_ref(), ScrollAxis::Vertical),
        ] {
            if let Some(adj) = adj {
                adj.connect_value_changed(glib::clone!(
                    #[weak(rename_to = w)]
                    self,
                    move |a| {
                        if w.imp().syncing.get() {
                            return;
                        }
                        if let Some(v) = w.imp().view.borrow_mut().as_mut() {
                            v.set_scroll(axis, a.value() as f32);
                        }
                        w.queue_draw();
                    }
                ));
            }
        }
        *self.imp().hadj.borrow_mut() = hadj;
        *self.imp().vadj.borrow_mut() = vadj;
    }

    fn sync_scrollbars(&self) {
        let Some(app) = self.app() else { return };
        let Ok(session) = app.session.try_borrow() else {
            return;
        };
        let size = self.size();
        let imp = self.imp();
        imp.syncing.set(true);
        for (adj, axis) in [
            (&imp.hadj, ScrollAxis::Horizontal),
            (&imp.vadj, ScrollAxis::Vertical),
        ] {
            let Some(adj) = adj.borrow().clone() else {
                continue;
            };
            let info = imp
                .view
                .borrow()
                .as_ref()
                .and_then(|v| v.scroll_info(axis, size, &session));
            // The bar spans the scrolled part only.
            let bar = match axis {
                ScrollAxis::Horizontal => imp.hbar.borrow().clone(),
                ScrollAxis::Vertical => imp.vbar.borrow().clone(),
            };
            if let Some(bar) = &bar
                && imp.overlaid.get()
            {
                let needed = info.is_some_and(|i| i.content > i.viewport + 1.0);
                if bar.is_visible() != needed {
                    bar.set_visible(needed);
                }
            }
            if let (Some(bar), Some(i)) = (bar, info) {
                // Over the canvas the two bars meet in the bottom-right
                // corner: the horizontal one stops at the vertical one.
                let corner = if imp.overlaid.get() && axis == ScrollAxis::Horizontal {
                    imp.vbar
                        .borrow()
                        .as_ref()
                        .filter(|v| {
                            v.is_visible() && v.adjustment().upper() > v.adjustment().page_size()
                        })
                        .map_or(0.0, |v| v.width() as f32)
                } else {
                    0.0
                };
                let (start, end) = (i.start.round() as i32, (i.end + corner).round() as i32);
                match axis {
                    ScrollAxis::Horizontal => {
                        if bar.margin_start() != start {
                            bar.set_margin_start(start);
                        }
                        if bar.margin_end() != end {
                            bar.set_margin_end(end);
                        }
                    }
                    ScrollAxis::Vertical => {
                        if bar.margin_top() != start {
                            bar.set_margin_top(start);
                        }
                        if bar.margin_bottom() != end {
                            bar.set_margin_bottom(end);
                        }
                    }
                }
            }
            match info {
                Some(i) if i.content > i.viewport + 1.0 => {
                    let upper = i.content as f64;
                    let page = i.viewport as f64;
                    let value = (i.offset as f64).min(upper - page).max(0.0);
                    if (adj.upper() - upper).abs() > 0.5
                        || (adj.page_size() - page).abs() > 0.5
                        || (adj.value() - value).abs() > 0.5
                    {
                        adj.configure(value, 0.0, upper, 24.0, page * 0.9, page);
                    }
                }
                _ => {
                    if adj.upper() != 0.0 {
                        adj.configure(0.0, 0.0, 0.0, 1.0, 1.0, 0.0);
                    }
                }
            }
        }
        imp.syncing.set(false);
    }
}

/// Native popover menu for a view's context menu request. Every submenu
/// level is a page of one stack, built up front: the stack is as large as
/// its largest level, so the popover is placed once with room for all of
/// them and moving between levels never resizes or moves it (a popover
/// that grows by the edge of the screen may otherwise be closed by the
/// compositor).
pub fn show_menu(
    parent: &gtk::Widget,
    at: Point,
    items: Vec<MenuItem<Action>>,
    app: &Rc<AppState>,
) {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.add_css_class("ff-menu");
    let stack = gtk::Stack::new();
    stack.set_hhomogeneous(true);
    stack.set_vhomogeneous(true);
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    stack.set_transition_duration(150);
    add_menu_pages(&stack, &popover, &items, &[], None, app);
    stack.set_visible_child_name(&page_name(&[]));
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(520);
    scroller.set_child(Some(&stack));
    popover.set_child(Some(&scroller));
    popover.set_parent(parent);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(at.x as i32, at.y as i32, 1, 1)));
    popover.connect_closed(|p| {
        OPEN_MENU.with(|m| m.borrow_mut().take());
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    OPEN_MENU.with(|m| *m.borrow_mut() = Some((popover.clone(), stack)));
    popover.popup();
}

thread_local! {
    /// The context menu open now (for scripted checks).
    static OPEN_MENU: RefCell<Option<(gtk::Popover, gtk::Stack)>> = const { RefCell::new(None) };
}

/// The open context menu and the name of the level it shows.
pub fn open_menu() -> Option<(gtk::Popover, String)> {
    OPEN_MENU.with(|m| {
        m.borrow().as_ref().map(|(p, s)| {
            (
                p.clone(),
                s.visible_child_name()
                    .map_or_else(String::new, |n| n.to_string()),
            )
        })
    })
}

/// Click the entry of the open context menu's shown level labelled
/// `label` — or, failing that, the first whose label contains it (as a
/// pointer click would; scripted checks).
pub fn activate_menu_entry(label: &str) -> bool {
    let Some(page) = OPEN_MENU.with(|m| m.borrow().as_ref().and_then(|(_, s)| s.visible_child()))
    else {
        return false;
    };
    let mut entries = Vec::new();
    let mut child = page.first_child();
    while let Some(c) = child {
        if let Ok(button) = c.clone().downcast::<gtk::Button>()
            && let Some(text) = button
                .child()
                .and_then(|l| l.downcast::<gtk::Label>().ok())
                .map(|l| l.text())
        {
            entries.push((button, text));
        }
        child = c.next_sibling();
    }
    // Without the check mark and the submenu arrow.
    let bare = |t: &str| {
        t.trim_start_matches(['✓', ' '])
            .trim_end_matches(['›', ' '])
            .to_string()
    };
    let exact = entries.iter().position(|(_, t)| bare(t) == label);
    let found = exact.or_else(|| entries.iter().position(|(_, t)| t.contains(label)));
    match found {
        Some(i) => {
            entries[i].0.emit_clicked();
            true
        }
        None => false,
    }
}

/// The stack page of the menu level at `path` (indices into submenus).
fn page_name(path: &[usize]) -> String {
    let mut name = String::from("menu");
    for i in path {
        name.push('-');
        name.push_str(&i.to_string());
    }
    name
}

/// A page for the level `items` at `path` (with a way back to its parent
/// when it has a `title`), then pages for its submenus.
fn add_menu_pages(
    stack: &gtk::Stack,
    popover: &gtk::Popover,
    items: &[MenuItem<Action>],
    path: &[usize],
    title: Option<&str>,
    app: &Rc<AppState>,
) {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let row = |text: &str| {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        let button = gtk::Button::new();
        button.set_child(Some(&label));
        button.add_css_class("flat");
        button
    };
    if let Some(title) = title {
        let back = row(&format!("‹  {title}"));
        let up = page_name(&path[..path.len() - 1]);
        back.connect_clicked(glib::clone!(
            #[weak]
            stack,
            move |_| stack.set_visible_child_name(&up)
        ));
        list.append(&back);
        list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }
    for (i, item) in items.iter().enumerate() {
        if item.separator_before && list.first_child().is_some() {
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        let mark = match item.checked {
            Some(true) => "✓  ",
            Some(false) => "    ",
            None => "",
        };
        let more = if item.children.is_empty() {
            ""
        } else {
            "  ›"
        };
        let button = row(&format!("{mark}{}{more}", item.label));
        if !item.children.is_empty() {
            let mut deeper = path.to_vec();
            deeper.push(i);
            let name = page_name(&deeper);
            add_menu_pages(
                stack,
                popover,
                &item.children,
                &deeper,
                Some(&item.label),
                app,
            );
            button.connect_clicked(glib::clone!(
                #[weak]
                stack,
                move |_| stack.set_visible_child_name(&name)
            ));
        } else {
            match item.action.clone() {
                Some(action) => {
                    let weak = Rc::downgrade(app);
                    button.connect_clicked(glib::clone!(
                        #[weak]
                        popover,
                        move |_| {
                            popover.popdown();
                            if let Some(app) = weak.upgrade() {
                                app.dispatch(action.clone());
                            }
                        }
                    ));
                }
                None => button.set_sensitive(false),
            }
        }
        list.append(&button);
    }
    stack.add_named(&list, Some(&page_name(path)));
}

/// A canvas plus optional native scrollbars, the unit the dock places.
#[derive(Clone)]
pub struct ViewHost {
    pub root: gtk::Grid,
    pub canvas: CanvasWidget,
}

impl ViewHost {
    pub fn new(app: &Rc<AppState>, view: DynView, horizontal: bool, vertical: bool) -> Self {
        let canvas = CanvasWidget::new(app, view);
        let root = gtk::Grid::new();
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.attach(&canvas, 0, 0, 1, 1);
        let hbar = horizontal.then(|| {
            let adj = gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0);
            let bar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&adj));
            root.attach(&bar, 0, 1, 1, 1);
            bar
        });
        let vbar = vertical.then(|| {
            let adj = gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0);
            let bar = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&adj));
            root.attach(&bar, 1, 0, 1, 1);
            bar
        });
        canvas.bind_scrollbars(hbar, vbar);
        Self { root, canvas }
    }

    /// A host whose scrollbars lie over the view's edges instead of beside
    /// it: the view paints the whole area (the arranger's header column runs
    /// down to the bottom, its ruler to the right edge) and the bars cover
    /// only the scrolled part, inset by the view's `ScrollInfo`.
    pub fn overlaid(app: &Rc<AppState>, view: DynView) -> Self {
        let canvas = CanvasWidget::new(app, view);
        let root = gtk::Grid::new();
        root.set_hexpand(true);
        root.set_vexpand(true);
        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        overlay.set_child(Some(&canvas));
        let hbar = gtk::Scrollbar::new(
            gtk::Orientation::Horizontal,
            Some(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0)),
        );
        hbar.set_valign(gtk::Align::End);
        let vbar = gtk::Scrollbar::new(
            gtk::Orientation::Vertical,
            Some(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0)),
        );
        vbar.set_halign(gtk::Align::End);
        overlay.add_overlay(&hbar);
        overlay.add_overlay(&vbar);
        root.attach(&overlay, 0, 0, 1, 1);
        canvas.imp().overlaid.set(true);
        canvas.bind_scrollbars(Some(hbar), Some(vbar));
        Self { root, canvas }
    }
}

/// Development aid: with `FADERFRAME_PAINT_STATS` set, every canvas logs
/// how long its view took to paint (building the frame's render nodes) and
/// the window's frame rate, every two seconds.
mod paint_stats {
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    pub fn enabled() -> bool {
        thread_local!(static ON: bool = std::env::var_os("FADERFRAME_PAINT_STATS").is_some());
        ON.with(|on| *on)
    }

    #[derive(Default)]
    struct Stats {
        frames: u32,
        total: Duration,
        max: Duration,
        counts: [u64; KINDS.len()],
    }

    const KINDS: [&str; 12] = [
        "rect",
        "rounded",
        "border",
        "fill",
        "stroke",
        "gradient-fill",
        "image",
        "shadow",
        "inset",
        "text",
        "clip",
        "transform",
    ];

    /// Forwards to a painter and counts the primitives by kind.
    pub struct Counting<'a> {
        inner: &'a mut dyn faderframe_ui_canvas::Painter,
        pub counts: [u64; KINDS.len()],
    }

    impl<'a> Counting<'a> {
        pub fn new(inner: &'a mut dyn faderframe_ui_canvas::Painter) -> Self {
            Self {
                inner,
                counts: [0; KINDS.len()],
            }
        }
    }

    use faderframe_ui_canvas::{Color, Image, Paint, Path, Rect, TextStyle};
    impl faderframe_ui_canvas::Painter for Counting<'_> {
        fn fill_rect(&mut self, rect: Rect, paint: &Paint) {
            self.counts[0] += 1;
            self.inner.fill_rect(rect, paint);
        }
        fn fill_rounded(&mut self, rect: Rect, radius: f32, paint: &Paint) {
            self.counts[1] += 1;
            self.inner.fill_rounded(rect, radius, paint);
        }
        fn stroke_rounded(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
            self.counts[2] += 1;
            self.inner.stroke_rounded(rect, radius, width, color);
        }
        fn fill_path(&mut self, path: &Path, color: Color) {
            self.counts[3] += 1;
            self.inner.fill_path(path, color);
        }
        fn stroke_path(&mut self, path: &Path, width: f32, color: Color) {
            self.counts[4] += 1;
            self.inner.stroke_path(path, width, color);
        }
        fn fill_path_paint(&mut self, path: &Path, paint: &Paint) {
            self.counts[5] += 1;
            self.inner.fill_path_paint(path, paint);
        }
        fn image(&mut self, image: &Image, src: Rect, dst: Rect, brightness: f32) {
            self.counts[6] += 1;
            self.inner.image(image, src, dst, brightness);
        }
        fn push_transform(&mut self, dx: f32, dy: f32, scale: f32) {
            self.counts[11] += 1;
            self.inner.push_transform(dx, dy, scale);
        }
        fn pop_transform(&mut self) {
            self.inner.pop_transform();
        }
        fn shadow(&mut self, rect: Rect, radius: f32, color: Color, dx: f32, dy: f32, blur: f32) {
            self.counts[7] += 1;
            self.inner.shadow(rect, radius, color, dx, dy, blur);
        }
        fn inset_shadow(
            &mut self,
            rect: Rect,
            radius: f32,
            color: Color,
            dx: f32,
            dy: f32,
            blur: f32,
        ) {
            self.counts[8] += 1;
            self.inner.inset_shadow(rect, radius, color, dx, dy, blur);
        }
        fn text(&mut self, text: &str, rect: Rect, style: &TextStyle) {
            self.counts[9] += 1;
            self.inner.text(text, rect, style);
        }
        fn text_width(&mut self, text: &str, style: &TextStyle) -> f32 {
            self.inner.text_width(text, style)
        }
        fn push_clip(&mut self, rect: Rect) {
            self.counts[10] += 1;
            self.inner.push_clip(rect);
        }
        fn pop_clip(&mut self) {
            self.inner.pop_clip();
        }
        fn scale_factor(&self) -> f32 {
            self.inner.scale_factor()
        }
    }

    thread_local! {
        static STATS: RefCell<(HashMap<String, Stats>, Option<Instant>)> =
            RefCell::new((HashMap::new(), None));
    }

    pub fn note(widget: &gtk::Widget, took: Duration, counts: [u64; KINDS.len()], gpu: bool) {
        let name = format!(
            "{}x{}@{:p}{}",
            widget.width(),
            widget.height(),
            widget.as_ptr(),
            if gpu { " (GPU)" } else { "" }
        );
        let fps = widget.frame_clock().map_or(0.0, |c| c.fps());
        STATS.with(|s| {
            let (stats, since) = &mut *s.borrow_mut();
            let e = stats.entry(name).or_default();
            e.frames += 1;
            e.total += took;
            e.max = e.max.max(took);
            for (a, b) in e.counts.iter_mut().zip(counts) {
                *a += b;
            }
            let start = *since.get_or_insert_with(Instant::now);
            if start.elapsed() >= Duration::from_secs(2) {
                for (name, st) in stats.drain() {
                    let per_frame: Vec<String> = KINDS
                        .iter()
                        .zip(st.counts)
                        .filter(|(_, c)| *c > 0)
                        .map(|(k, c)| format!("{k} {}", c / u64::from(st.frames.max(1))))
                        .collect();
                    tracing::info!(
                        "paint {name}: {} frames, mean {:.2} ms, max {:.2} ms, {fps:.0} fps; per frame {}",
                        st.frames,
                        st.total.as_secs_f64() * 1e3 / f64::from(st.frames.max(1)),
                        st.max.as_secs_f64() * 1e3,
                        per_frame.join(", "),
                    );
                }
                *since = Some(Instant::now());
            }
        });
    }
}

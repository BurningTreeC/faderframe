//! Views' controls as GTK accessibles (AT-SPI on Linux, UIA/NSAccessibility
//! where GTK bridges them): each [`AccessNode`] a GObject implementing
//! `gtk::Accessible` under the canvas that shows it, so a screen reader
//! walks a mixer's strips, a track's buttons, a list's rows.
//!
//! The canvas keeps the tree ([`AccessTree`]): rebuilt when the view's
//! controls change shape (other ids, other order), else updated in place —
//! names, values and states, which the screen reader hears change. It also
//! keeps the keyboard focus among the controls ([`AccessTree::focus`]),
//! told to GTK as the focused platform state.

use faderframe_session::Action;
use faderframe_ui_canvas::{AccessNode, AccessRole, Point};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::AccessObject)]
    pub struct AccessObject {
        #[property(get, set, override_interface = gtk::Accessible, builder(gtk::AccessibleRole::Generic))]
        pub accessible_role: Cell<gtk::AccessibleRole>,
        pub parent: RefCell<Option<glib::WeakRef<gtk::Accessible>>>,
        pub children: RefCell<Vec<super::AccessObject>>,
        pub next: RefCell<Option<glib::WeakRef<gtk::Accessible>>>,
        /// Relative to the parent.
        pub bounds: Cell<(i32, i32, i32, i32)>,
        pub focusable: Cell<bool>,
        pub focused: Cell<bool>,
        pub context: RefCell<Option<gtk::ATContext>>,
        /// What was last told to GTK: (label, description, value, checked,
        /// selected).
        pub told: RefCell<Told>,
    }

    impl Default for AccessObject {
        fn default() -> Self {
            Self {
                accessible_role: Cell::new(gtk::AccessibleRole::Generic),
                parent: RefCell::default(),
                children: RefCell::default(),
                next: RefCell::default(),
                bounds: Cell::default(),
                focusable: Cell::default(),
                focused: Cell::default(),
                context: RefCell::default(),
                told: RefCell::default(),
            }
        }
    }

    #[derive(Default, PartialEq, Clone)]
    pub struct Told {
        pub label: String,
        pub description: String,
        pub value: Option<(f64, f64, f64, String)>,
        pub checked: Option<bool>,
        pub selected: Option<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AccessObject {
        const NAME: &'static str = "FaderFrameAccessNode";
        type Type = super::AccessObject;
        type ParentType = glib::Object;
        type Interfaces = (gtk::Accessible,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for AccessObject {}

    impl AccessibleImpl for AccessObject {
        fn at_context(&self) -> Option<gtk::ATContext> {
            let mut c = self.context.borrow_mut();
            if c.is_none() {
                let display = gtk::gdk::Display::default()?;
                *c = gtk::ATContext::create(self.accessible_role.get(), &*self.obj(), &display);
            }
            c.clone()
        }

        fn accessible_parent(&self) -> Option<gtk::Accessible> {
            self.parent.borrow().as_ref()?.upgrade()
        }

        fn first_accessible_child(&self) -> Option<gtk::Accessible> {
            self.children.borrow().first().map(|c| c.clone().upcast())
        }

        fn next_accessible_sibling(&self) -> Option<gtk::Accessible> {
            self.next.borrow().as_ref()?.upgrade()
        }

        fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
            Some(self.bounds.get())
        }

        fn platform_state(&self, state: gtk::AccessiblePlatformState) -> bool {
            match state {
                gtk::AccessiblePlatformState::Focusable => self.focusable.get(),
                gtk::AccessiblePlatformState::Focused => self.focused.get(),
                _ => false,
            }
        }
    }
}

glib::wrapper! {
    /// One of a view's controls as an accessible object.
    pub struct AccessObject(ObjectSubclass<imp::AccessObject>)
        @implements gtk::Accessible;
}

fn role(r: AccessRole) -> gtk::AccessibleRole {
    use gtk::AccessibleRole as G;
    match r {
        AccessRole::Group => G::Group,
        AccessRole::Region => G::Region,
        AccessRole::List => G::List,
        AccessRole::ListItem => G::ListItem,
        AccessRole::Table => G::Table,
        AccessRole::Row => G::Row,
        AccessRole::Cell => G::Cell,
        AccessRole::ColumnHeader => G::ColumnHeader,
        AccessRole::Button => G::Button,
        AccessRole::ToggleButton => G::ToggleButton,
        AccessRole::CheckBox => G::Checkbox,
        AccessRole::Slider => G::Slider,
        AccessRole::SpinButton => G::SpinButton,
        AccessRole::Meter => G::Meter,
        AccessRole::Label => G::Label,
        AccessRole::Heading => G::Heading,
        AccessRole::TabList => G::TabList,
        AccessRole::Tab => G::Tab,
        AccessRole::Toolbar => G::Toolbar,
    }
}

fn tristate(on: bool) -> gtk::AccessibleTristate {
    if on {
        gtk::AccessibleTristate::True
    } else {
        gtk::AccessibleTristate::False
    }
}

impl AccessObject {
    fn new(node: &AccessNode<Action>) -> Self {
        let o: Self = glib::Object::builder()
            .property("accessible-role", role(node.role))
            .build();
        o.imp().focusable.set(node.focusable());
        o
    }

    /// Tell GTK what changed since last time.
    fn tell(&self, node: &AccessNode<Action>, origin: Point) {
        let b = node.bounds;
        self.imp().bounds.set((
            (b.x - origin.x).round() as i32,
            (b.y - origin.y).round() as i32,
            b.w.round().max(1.0) as i32,
            b.h.round().max(1.0) as i32,
        ));
        let now = imp::Told {
            label: node.label.clone(),
            description: node.description.clone(),
            value: node
                .value
                .as_ref()
                .map(|v| (v.now, v.min, v.max, v.text.clone())),
            checked: node.checked,
            selected: node.selected,
        };
        let told = self.imp().told.borrow().clone();
        if now == told && !told.label.is_empty() {
            return;
        }
        if now.label != told.label {
            self.update_property(&[gtk::accessible::Property::Label(&now.label)]);
        }
        if now.description != told.description {
            self.update_property(&[gtk::accessible::Property::Description(&now.description)]);
        }
        if now.value != told.value
            && let Some((v, lo, hi, text)) = &now.value
        {
            self.update_property(&[
                gtk::accessible::Property::ValueMin(*lo),
                gtk::accessible::Property::ValueMax(*hi),
                gtk::accessible::Property::ValueNow(*v),
                gtk::accessible::Property::ValueText(text),
            ]);
        }
        if now.checked != told.checked
            && let Some(on) = now.checked
        {
            let state = if node.role == AccessRole::ToggleButton {
                gtk::accessible::State::Pressed(tristate(on))
            } else {
                gtk::accessible::State::Checked(tristate(on))
            };
            self.update_state(&[state]);
        }
        if now.selected != told.selected {
            self.update_state(&[gtk::accessible::State::Selected(now.selected)]);
        }
        *self.imp().told.borrow_mut() = now;
    }

    fn set_focused(&self, on: bool) {
        if self.imp().focused.replace(on) != on {
            platform_focus_changed(self.upcast_ref());
        }
    }
}

/// `gtk_accessible_update_platform_state` (GTK 4.18): tells the screen
/// reader that a non-widget accessible took or lost the focus. Looked up in
/// the running process, so FaderFrame still runs on GTK 4.14 (where the
/// canvas's active-descendant relation carries the focus alone).
type UpdatePlatformState =
    unsafe extern "C" fn(*mut gtk::ffi::GtkAccessible, gtk::ffi::GtkAccessiblePlatformState);

fn update_platform_state() -> Option<UpdatePlatformState> {
    static F: std::sync::OnceLock<Option<UpdatePlatformState>> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        #[cfg(unix)]
        let lib = libloading::os::unix::Library::this();
        #[cfg(windows)]
        let lib = libloading::os::windows::Library::open_already_loaded("libgtk-4-1.dll").ok()?;
        // SAFETY: where GTK has the symbol (4.18 and later) it has this
        // signature (gtk/gtkaccessible.h); the library is the one already
        // loaded into the process, which stays loaded.
        let f =
            unsafe { lib.get::<UpdatePlatformState>(b"gtk_accessible_update_platform_state\0") }
                .ok()
                .map(|s| *s);
        std::mem::forget(lib);
        f
    })
}

fn platform_focus_changed(a: &gtk::Accessible) {
    use gtk::glib::translate::{IntoGlib, ToGlibPtr};
    if let Some(f) = update_platform_state() {
        let ptr: *mut gtk::ffi::GtkAccessible = a.to_glib_none().0;
        // SAFETY: `ptr` is a live GtkAccessible (borrowed for the call) and
        // the state a valid enum value.
        unsafe { f(ptr, gtk::AccessiblePlatformState::Focused.into_glib()) };
    }
}

/// A shape of the tree: its ids and roles in order (a rebuild when it
/// changes).
fn shape(nodes: &[AccessNode<Action>], h: &mut u64) {
    for n in nodes {
        *h = h
            .wrapping_mul(0x0100_0000_01b3)
            .wrapping_add(n.id ^ (n.role as u64) << 56);
        *h = h.wrapping_mul(31).wrapping_add(n.children.len() as u64);
        shape(&n.children, h);
    }
}

/// A canvas's accessible controls and the keyboard focus among them.
#[derive(Default)]
pub struct AccessTree {
    roots: Vec<AccessObject>,
    by_id: HashMap<u64, AccessObject>,
    shape: Option<u64>,
    /// The control the keyboard is on.
    pub focus: Option<u64>,
    /// The keyboard moves among the controls (Tab into the view): the
    /// focus ring shows, Tab and Enter and the arrows are the controls'.
    pub keyboard: bool,
    /// The newest tree from the view.
    pub nodes: Vec<AccessNode<Action>>,
}

impl AccessTree {
    /// The canvas's accessible children (the view's top-level controls).
    pub fn roots(&self) -> Vec<gtk::Accessible> {
        self.roots.iter().map(|o| o.clone().upcast()).collect()
    }

    /// Take the view's newest tree; `host` is the canvas, `after` what
    /// follows the controls among its accessible children (its first child
    /// widget, popovers).
    pub fn update(
        &mut self,
        nodes: Vec<AccessNode<Action>>,
        host: &gtk::Accessible,
        after: Option<&gtk::Accessible>,
    ) {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        shape(&nodes, &mut h);
        if self.shape != Some(h) {
            self.roots.clear();
            self.by_id.clear();
            self.roots = build(&nodes, host, after, Point::new(0.0, 0.0), &mut self.by_id);
            self.shape = Some(h);
        }
        tell_all(&nodes, &self.by_id, Point::new(0.0, 0.0));
        // The focus follows its control, or goes when it went.
        if let Some(f) = self.focus
            && faderframe_ui_canvas::access::find_node(&nodes, f).is_none()
        {
            self.focus = None;
        }
        self.nodes = nodes;
        self.tell_focus(host);
    }

    /// Move the keyboard focus to `id` (or off).
    pub fn set_focus(&mut self, id: Option<u64>, host: &gtk::Accessible) {
        self.focus = id;
        self.tell_focus(host);
    }

    /// The focused control as the canvas's active descendant and focused
    /// platform state.
    fn tell_focus(&self, host: &gtk::Accessible) {
        let on = self.focus.filter(|_| self.keyboard);
        // The old control lets go before the new one takes the focus.
        for (id, o) in &self.by_id {
            if on != Some(*id) {
                o.set_focused(false);
            }
        }
        if let Some(o) = on.and_then(|i| self.by_id.get(&i)) {
            o.set_focused(true);
        }
        match on.and_then(|i| self.by_id.get(&i)) {
            Some(o) => {
                host.update_relation(&[gtk::accessible::Relation::ActiveDescendant(o.upcast_ref())])
            }
            None => host.reset_relation(gtk::AccessibleRelation::ActiveDescendant),
        }
    }

    /// The node the keyboard is on.
    pub fn focused(&self) -> Option<&AccessNode<Action>> {
        faderframe_ui_canvas::access::find_node(&self.nodes, self.focus?)
    }

    /// The next (or previous) control in Tab order from the focus; `None`
    /// past the end.
    pub fn step(&self, forward: bool) -> Option<u64> {
        let order = faderframe_ui_canvas::access::focus_order(&self.nodes);
        let ids: Vec<u64> = order.iter().map(|n| n.id).collect();
        let at = self.focus.and_then(|f| ids.iter().position(|i| *i == f));
        match (at, forward) {
            (None, true) => ids.first().copied(),
            (None, false) => ids.last().copied(),
            (Some(i), true) => ids.get(i + 1).copied(),
            (Some(i), false) => i.checked_sub(1).and_then(|i| ids.get(i).copied()),
        }
    }

    pub fn has_controls(&self) -> bool {
        !faderframe_ui_canvas::access::focus_order(&self.nodes).is_empty()
    }
}

fn build(
    nodes: &[AccessNode<Action>],
    parent: &gtk::Accessible,
    after: Option<&gtk::Accessible>,
    origin: Point,
    by_id: &mut HashMap<u64, AccessObject>,
) -> Vec<AccessObject> {
    let objects: Vec<AccessObject> = nodes.iter().map(AccessObject::new).collect();
    for (i, (o, n)) in objects.iter().zip(nodes).enumerate() {
        let imp = o.imp();
        *imp.parent.borrow_mut() = Some(parent.downgrade());
        let next: Option<gtk::Accessible> = match objects.get(i + 1) {
            Some(n) => Some(n.clone().upcast()),
            None => after.cloned(),
        };
        *imp.next.borrow_mut() = next.as_ref().map(|n| n.downgrade());
        let me: gtk::Accessible = o.clone().upcast();
        let children = build(
            &n.children,
            &me,
            None,
            Point::new(n.bounds.x, n.bounds.y),
            by_id,
        );
        *imp.children.borrow_mut() = children;
        o.tell(n, origin);
        by_id.insert(n.id, o.clone());
    }
    objects
}

fn tell_all(nodes: &[AccessNode<Action>], by_id: &HashMap<u64, AccessObject>, origin: Point) {
    for n in nodes {
        if let Some(o) = by_id.get(&n.id) {
            o.tell(n, origin);
        }
        tell_all(&n.children, by_id, Point::new(n.bounds.x, n.bounds.y));
    }
}

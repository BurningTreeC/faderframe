//! What a view tells assistive technology (screen readers): its controls
//! as a tree of [`AccessNode`]s — role, name, value, state, where it is —
//! and what the keyboard does with each (Enter/Space activates, the arrows
//! step a value). The canvas host turns the tree into the toolkit's
//! accessible objects under the view, moves a keyboard focus along it
//! (Tab, Shift+Tab) with a focus ring, and sends the actions.
//!
//! Views build it from what they paint, so it stays in step with the
//! screen; ids are stable (a track's fader keeps its id while the mixer
//! shows the track) so the focus and a screen reader's place survive
//! repaints.
//!
//! What only the view can do for a control — open its menu, a text entry
//! or a chooser, change its own presentation (a filter, a selection) — the
//! keyboard asks of the view ([`AccessPress`],
//! [`crate::CanvasView::access_press`]); [`key_on`] says what a key does on
//! a node, for the host and for tests ([`press_key`]).

use crate::{CanvasView, EventCx, HostRequest, Key, Modifiers, Rect, Size};

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessRole {
    /// A part of a view with controls (a mixer strip, a track header).
    Group,
    /// A named part of a view (a lane, the ruler).
    Region,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    ColumnHeader,
    Button,
    ToggleButton,
    CheckBox,
    /// A continuous value (fader, pan, knob).
    Slider,
    /// A value typed or stepped (a position, a tempo).
    SpinButton,
    /// A level shown (meters).
    Meter,
    Label,
    Heading,
    TabList,
    Tab,
    Toolbar,
}

impl AccessRole {
    /// Roles a keyboard user moves to.
    pub fn takes_focus(self) -> bool {
        matches!(
            self,
            AccessRole::ListItem
                | AccessRole::Row
                | AccessRole::Cell
                | AccessRole::Button
                | AccessRole::ToggleButton
                | AccessRole::CheckBox
                | AccessRole::Slider
                | AccessRole::SpinButton
                | AccessRole::Tab
        )
    }
}

/// A node's value (sliders, spin buttons, meters).
#[derive(Clone, Debug, PartialEq)]
pub struct AccessValue {
    pub now: f64,
    pub min: f64,
    pub max: f64,
    /// As read out ("−6.0 dB", "L 30").
    pub text: String,
}

/// What the keyboard asks of the view itself for a node
/// ([`crate::CanvasView::access_press`]): what its press would do that
/// only the view can — a menu, a text entry, a chooser, its own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessPress {
    /// Enter on a node that asks the view ([`AccessNode::press`]).
    Activate,
    /// The node's menu ([`AccessNode::menu`]): Shift+F10 or the Menu key,
    /// and Enter when nothing else answers it (a menu button).
    Menu,
    /// Up/Right and Down/Left on a node whose steps are the view's
    /// ([`AccessNode::view_steps`]).
    Increment,
    Decrement,
}

/// What a key does on the control the keyboard is on ([`key_on`]).
#[derive(Clone, Debug, PartialEq)]
pub enum AccessKey<A> {
    /// An action for the session.
    Act(A),
    /// The view answers ([`crate::CanvasView::access_press`]).
    Press(AccessPress),
}

/// One control or part of a view.
#[derive(Clone, Debug)]
pub struct AccessNode<A> {
    /// Stable within the view ([`access_id`]).
    pub id: u64,
    pub role: AccessRole,
    pub label: String,
    pub description: String,
    pub value: Option<AccessValue>,
    /// Toggles and check boxes.
    pub checked: Option<bool>,
    pub selected: Option<bool>,
    /// In view coordinates.
    pub bounds: Rect,
    /// Enter / Space.
    pub activate: Option<A>,
    /// Up / Right, Down / Left: one step.
    pub increment: Option<A>,
    pub decrement: Option<A>,
    /// Enter asks the view ([`AccessPress::Activate`]) when there is no
    /// `activate`.
    pub press: bool,
    /// It has a menu ([`AccessPress::Menu`]; told as a popup).
    pub menu: bool,
    /// The arrows ask the view ([`AccessPress::Increment`],
    /// [`AccessPress::Decrement`]) when there are no step actions.
    pub view_steps: bool,
    /// Does nothing now (told as unavailable; the keys pass it by).
    pub disabled: bool,
    /// A Tab stop with nothing to do: a reading the keyboard visits (a
    /// meter, a summary) to hear it.
    pub stop: bool,
    /// Stays put when the view scrolls sideways (`fixed_x`) or up and down
    /// (`fixed_y`) — a toolbar's buttons, a header column: the keyboard
    /// never scrolls the view to bring it in.
    pub fixed_x: bool,
    pub fixed_y: bool,
    pub children: Vec<AccessNode<A>>,
}

impl<A> AccessNode<A> {
    pub fn new(id: u64, role: AccessRole, label: impl Into<String>) -> Self {
        Self {
            id,
            role,
            label: label.into(),
            description: String::new(),
            value: None,
            checked: None,
            selected: None,
            bounds: Rect::default(),
            activate: None,
            increment: None,
            decrement: None,
            press: false,
            menu: false,
            view_steps: false,
            disabled: false,
            stop: false,
            fixed_x: false,
            fixed_y: false,
            children: Vec::new(),
        }
    }

    pub fn at(mut self, bounds: Rect) -> Self {
        self.bounds = bounds;
        self
    }

    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn value(mut self, now: f64, min: f64, max: f64, text: impl Into<String>) -> Self {
        self.value = Some(AccessValue {
            now,
            min,
            max,
            text: text.into(),
        });
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }

    pub fn selected(mut self, on: bool) -> Self {
        self.selected = Some(on);
        self
    }

    pub fn on_activate(mut self, action: A) -> Self {
        self.activate = Some(action);
        self
    }

    pub fn on_step(mut self, up: A, down: A) -> Self {
        self.increment = Some(up);
        self.decrement = Some(down);
        self
    }

    /// Enter asks the view ([`AccessPress::Activate`]).
    pub fn on_press(mut self) -> Self {
        self.press = true;
        self
    }

    /// It has a menu the view opens ([`AccessPress::Menu`]).
    pub fn with_menu(mut self) -> Self {
        self.menu = true;
        self
    }

    /// The arrows ask the view ([`AccessPress::Increment`] and
    /// [`AccessPress::Decrement`]).
    pub fn view_steps(mut self) -> Self {
        self.view_steps = true;
        self
    }

    pub fn disabled(mut self, off: bool) -> Self {
        self.disabled = off;
        self
    }

    /// The keyboard visits it though it does nothing (a reading).
    pub fn tab_stop(mut self) -> Self {
        self.stop = true;
        self
    }

    /// Stays put however the view scrolls (a toolbar's).
    pub fn fixed(mut self) -> Self {
        self.fixed_x = true;
        self.fixed_y = true;
        self
    }

    /// Stays put when the view scrolls sideways (a column of row headers).
    pub fn fixed_x(mut self) -> Self {
        self.fixed_x = true;
        self
    }

    /// Stays put when the view scrolls up and down (a row of column
    /// headers).
    pub fn fixed_y(mut self) -> Self {
        self.fixed_y = true;
        self
    }

    pub fn child(mut self, node: AccessNode<A>) -> Self {
        self.children.push(node);
        self
    }

    pub fn with_children(mut self, nodes: impl IntoIterator<Item = AccessNode<A>>) -> Self {
        self.children.extend(nodes);
        self
    }

    /// A keyboard user can move here.
    pub fn focusable(&self) -> bool {
        self.role.takes_focus()
            || self.activate.is_some()
            || self.increment.is_some()
            || self.decrement.is_some()
            || self.press
            || self.menu
            || self.view_steps
            || self.stop
    }

    /// The nodes depth-first: the order Tab moves in.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a AccessNode<A>>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }

    /// The node with `id` in this subtree.
    pub fn find(&self, id: u64) -> Option<&AccessNode<A>> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    /// What a screen reader says for it: name, value, state.
    pub fn spoken(&self) -> String {
        let mut s = self.label.clone();
        if let Some(v) = &self.value {
            s.push_str(", ");
            s.push_str(&v.text);
        }
        if let Some(on) = self.checked {
            s.push_str(if on { ", on" } else { ", off" });
        }
        s
    }
}

/// What `key` does on `node`: Enter its action, else the view's press, else
/// its menu; the arrows (and Page Up/Down, once each) a step; Shift+F10 and
/// the Menu key its menu. `None`: nothing there (a disabled node: nothing at
/// all) — the key goes to the view.
pub fn key_on<A: Clone>(
    node: &AccessNode<A>,
    key: Key,
    modifiers: Modifiers,
) -> Option<AccessKey<A>> {
    if node.disabled {
        return None;
    }
    let menu = node.menu.then_some(AccessKey::Press(AccessPress::Menu));
    let step = |action: &Option<A>, press: AccessPress| {
        action
            .clone()
            .map(AccessKey::Act)
            .or_else(|| node.view_steps.then_some(AccessKey::Press(press)))
    };
    match key {
        Key::Enter => node
            .activate
            .clone()
            .map(AccessKey::Act)
            .or_else(|| {
                node.press
                    .then_some(AccessKey::Press(AccessPress::Activate))
            })
            .or(menu),
        Key::Up | Key::Right | Key::PageUp => step(&node.increment, AccessPress::Increment),
        Key::Down | Key::Left | Key::PageDown => step(&node.decrement, AccessPress::Decrement),
        Key::F(10) if modifiers.shift => menu,
        Key::Menu => menu,
        _ => None,
    }
}

/// The keyboard presses `key` on control `id` of `nodes` (as the host does,
/// for tests): the actions for the session and the view's requests to the
/// host. `None`: the key does nothing there.
pub fn press_key<M, A: Clone, V: CanvasView<M, A> + ?Sized>(
    view: &mut V,
    nodes: &[AccessNode<A>],
    id: u64,
    key: Key,
    modifiers: Modifiers,
    size: Size,
    model: &M,
) -> Option<(Vec<A>, Vec<HostRequest<A>>)> {
    let node = find_node(nodes, id)?;
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    match key_on(node, key, modifiers)? {
        AccessKey::Act(a) => actions.push(a),
        AccessKey::Press(p) => {
            let mut cx = EventCx::new(&mut actions, &mut requests);
            view.access_press(id, p, size, model, &mut cx);
        }
    }
    Some((actions, requests))
}

/// The focus order of a view's tree.
pub fn focus_order<A>(nodes: &[AccessNode<A>]) -> Vec<&AccessNode<A>> {
    let mut all = Vec::new();
    for n in nodes {
        n.walk(&mut all);
    }
    all.retain(|n| n.focusable());
    all
}

/// The node with `id` in a view's tree.
pub fn find_node<A>(nodes: &[AccessNode<A>], id: u64) -> Option<&AccessNode<A>> {
    nodes.iter().find_map(|n| n.find(id))
}

/// A stable id from parts (FNV-1a): a kind and the ids of what it shows,
/// e.g. `access_id(&[FADER, track.0])`.
pub fn access_id(parts: &[u64]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// A stable id from a string part and numbers (names of things without
/// ids).
pub fn access_id_str(name: &str, parts: &[u64]) -> u64 {
    let mut h = access_id(parts);
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_order_is_depth_first_over_controls() {
        let strip = AccessNode::<u8>::new(1, AccessRole::Group, "Bass")
            .child(AccessNode::new(2, AccessRole::Slider, "Volume").value(
                -6.0,
                -60.0,
                12.0,
                "−6.0 dB",
            ))
            .child(
                AccessNode::new(3, AccessRole::ToggleButton, "Mute")
                    .checked(true)
                    .on_activate(7),
            );
        let other = AccessNode::new(4, AccessRole::Group, "Drums").child(AccessNode::new(
            5,
            AccessRole::ToggleButton,
            "Mute",
        ));
        let tree = [strip, other];
        let order: Vec<u64> = focus_order(&tree).iter().map(|n| n.id).collect();
        assert_eq!(order, [2, 3, 5]);
        let mute = find_node(&tree, 3).unwrap();
        assert_eq!(mute.spoken(), "Mute, on");
        assert_eq!(find_node(&tree, 2).unwrap().spoken(), "Volume, −6.0 dB");
        assert_ne!(access_id(&[1, 2]), access_id(&[2, 1]));
        assert_ne!(access_id_str("a", &[1]), access_id_str("b", &[1]));
    }

    /// Enter: the action, else the view's press, else the menu; the arrows
    /// steps, the view's when it has none; Shift+F10 and the Menu key the
    /// menu; nothing on a disabled node.
    #[test]
    fn keys_reach_actions_presses_and_menus() {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        let button = AccessNode::<u8>::new(1, AccessRole::Button, "Grid ▾").with_menu();
        assert!(button.focusable());
        assert_eq!(
            key_on(&button, Key::Enter, Modifiers::NONE),
            Some(AccessKey::Press(AccessPress::Menu))
        );
        assert_eq!(
            key_on(&button, Key::F(10), shift),
            Some(AccessKey::Press(AccessPress::Menu))
        );
        assert_eq!(key_on(&button, Key::F(10), Modifiers::NONE), None);
        let row = AccessNode::<u8>::new(2, AccessRole::ListItem, "Song")
            .on_press()
            .with_menu()
            .view_steps();
        assert_eq!(
            key_on(&row, Key::Enter, Modifiers::NONE),
            Some(AccessKey::Press(AccessPress::Activate))
        );
        assert_eq!(
            key_on(&row, Key::Menu, Modifiers::NONE),
            Some(AccessKey::Press(AccessPress::Menu))
        );
        assert_eq!(
            key_on(&row, Key::PageDown, Modifiers::NONE),
            Some(AccessKey::Press(AccessPress::Decrement))
        );
        let slider = AccessNode::new(3, AccessRole::Slider, "Trim")
            .on_step(4u8, 5)
            .on_activate(6);
        assert_eq!(
            key_on(&slider, Key::Right, Modifiers::NONE),
            Some(AccessKey::Act(4))
        );
        assert_eq!(
            key_on(&slider, Key::Enter, Modifiers::NONE),
            Some(AccessKey::Act(6))
        );
        assert_eq!(key_on(&slider, Key::Menu, Modifiers::NONE), None);
        let off = slider.disabled(true);
        assert!(off.focusable(), "still found, told as unavailable");
        assert_eq!(key_on(&off, Key::Enter, Modifiers::NONE), None);
    }
}

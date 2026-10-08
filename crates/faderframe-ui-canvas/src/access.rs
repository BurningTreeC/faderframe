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

use crate::Rect;

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
}

use crate::Point;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Super/Command.
    pub meta: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };

    /// Fine adjustment (shift or ctrl held) for knobs and faders.
    pub fn fine(&self) -> bool {
        self.shift || self.ctrl
    }

    /// "Add to selection" / "toggle" modifier.
    pub fn toggle(&self) -> bool {
        self.ctrl || self.meta
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Other(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Space,
    Enter,
    Escape,
    Delete,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Char(char),
    Other,
}

/// Toolkit-independent input event in view-local logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewEvent {
    PointerDown {
        pos: Point,
        button: PointerButton,
        modifiers: Modifiers,
        /// 1 = single click, 2 = double click ...
        clicks: u32,
    },
    /// Pointer motion; `dragging` while the primary button is held (the view
    /// keeps receiving motion outside its bounds during a drag).
    PointerMove {
        pos: Point,
        modifiers: Modifiers,
        dragging: bool,
    },
    PointerUp {
        pos: Point,
        button: PointerButton,
        modifiers: Modifiers,
    },
    PointerLeave,
    /// `dx`/`dy` in wheel steps (`precise == false`) or pixels (touchpads).
    Scroll {
        pos: Point,
        dx: f32,
        dy: f32,
        modifiers: Modifiers,
        precise: bool,
    },
    Key {
        key: Key,
        modifiers: Modifiers,
    },
    FocusLost,
}

/// Pointer shape requested by a view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Default,
    Pointer,
    Grab,
    Grabbing,
    ResizeHorizontal,
    ResizeVertical,
    Text,
    Crosshair,
    Move,
}

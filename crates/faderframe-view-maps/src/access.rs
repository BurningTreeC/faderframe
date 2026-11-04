//! The map editor for screen readers and the keyboard: the toolbar's
//! buttons (the map chooser opens its menu with Enter, Shift+F10 or the
//! Menu key; Rename, Import and Export their entry or chooser), the line
//! saying who uses the map (or what is being learnt), and the table — a
//! row per articulation, combination or drum sound, read as its whole
//! (name, group, type, what it sends; a sound's pitch, note, channel…),
//! Enter selecting it (Delete then removes it, Alt+arrows move it), its
//! menu the row's. Each of its cells is a control of its own: names and
//! lists are typed (Enter), numbers are spin buttons the arrows step and
//! Enter types, choices open their menu (the arrows step a group or a
//! channel), a type or Learn switches with Enter.
//!
//! Said by itself: the map the editor shows changing.

use crate::cells::{Doc, Use, art_output, range};
use crate::{Button, Col, MapsView, Row};
use faderframe_session::maps::MapKind;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessPress, AccessRole, CanvasView, EventCx, HostRequest, Point, Rect, Size,
    access_id,
};

/// Kinds of node.
const TOOLBAR: u64 = 0x3A90;
const BUTTON: u64 = 0x3A91;
const INFO: u64 = 0x3A92;
const TABLE: u64 = 0x3A93;
const ROW: u64 = 0x3A94;
const CELL: u64 = 0x3A95;
const HEADER: u64 = 0x3A96;

/// What the keyboard can ask the view for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Control {
    Button(Button),
    Row(Row),
    Cell(Row, Col),
}

fn button_id(b: Button) -> u64 {
    access_id(&[BUTTON, b as u64])
}

/// A row's own part of its ids: an articulation's id, a sound's pitch (they
/// stay with the row as it moves), a combination's place.
fn row_key(doc: Doc<'_>, row: Row) -> [u64; 2] {
    match (doc, row) {
        (Doc::Art(m), Row::Articulation(i)) => [
            1,
            m.articulations
                .get(i)
                .map_or(u64::MAX, |a| u64::from(a.id.0)),
        ],
        (_, Row::Combination(i)) => [2, i as u64],
        (Doc::Drum(m), Row::Sound(i)) => {
            [3, m.sounds.get(i).map_or(u64::MAX, |s| u64::from(s.pitch))]
        }
        _ => [4, 0],
    }
}

fn row_id(doc: Doc<'_>, row: Row) -> u64 {
    let [k, v] = row_key(doc, row);
    access_id(&[ROW, k, v])
}

fn cell_id(doc: Doc<'_>, row: Row, col: Col) -> u64 {
    let [k, v] = row_key(doc, row);
    access_id(&[CELL, k, v, col as u64])
}

/// The cells of a row that are controls, in column order.
fn row_cells(doc: Doc<'_>, row: Row) -> Vec<Col> {
    let mut out: Vec<Col> = doc
        .columns()
        .iter()
        .map(|c| c.0)
        .map(|c| match (row, c) {
            (Row::Combination(_), Col::Name) => Col::Members,
            (_, c) => c,
        })
        .filter(|c| !matches!(doc.use_of(row, *c), Use::None | Use::Handle))
        .collect();
    out.dedup();
    out
}

/// A column's name as read.
fn col_name(col: Col) -> &'static str {
    match col {
        Col::Handle => "Move",
        Col::Name => "Name",
        Col::Symbol => "Symbol",
        Col::Group => "Group",
        Col::Kind => "Type",
        Col::Keyswitches => "Keyswitches",
        Col::Program => "Program",
        Col::Controls => "Controllers",
        Col::Channel => "Channel",
        Col::Transpose => "Transpose",
        Col::Velocity => "Velocity",
        Col::Length => "Length",
        Col::Members => "Articulations",
        Col::Pitch => "Pitch",
        Col::Output => "Plays",
        Col::Port => "Port",
        Col::Grid => "Grid",
        Col::Learn => "Learn pitch",
    }
}

/// What a cell reads as: its value in words where the table shows a dash.
fn cell_words(doc: Doc<'_>, row: Row, col: Col, model: &Session) -> String {
    let text = doc.text(row, col, None, model);
    let text = text.trim_start_matches(['→', '•']).trim().to_string();
    let blank = text.is_empty() || text == "–";
    match (doc, col) {
        (Doc::Art(_), Col::Channel) if blank => "the track's".into(),
        (Doc::Drum(_), Col::Channel) if blank => "the note's".into(),
        (_, Col::Program) if blank => "none".into(),
        (_, Col::Output) if blank => "its pitch".into(),
        (_, Col::Port) if blank => "the track's output".into(),
        (_, Col::Grid) if blank => "the editor's".into(),
        (_, Col::Keyswitches | Col::Controls | Col::Symbol) if blank => "none".into(),
        (_, Col::Transpose) => format!("{text} semitones"),
        _ => text,
    }
}

impl MapsView {
    /// A row as read.
    fn row_words(doc: Doc<'_>, row: Row, model: &Session) -> (String, String) {
        match (doc, row) {
            (Doc::Art(m), Row::Articulation(i)) => {
                let Some(a) = m.articulations.get(i) else {
                    return (String::new(), String::new());
                };
                let kind = match a.kind {
                    faderframe_project::articulation::ArticulationKind::Attribute => "attribute",
                    faderframe_project::articulation::ArticulationKind::Direction => "direction",
                };
                let mut label = a.name.clone();
                if !a.symbol.is_empty() {
                    label.push_str(&format!(" ({})", a.symbol));
                }
                label.push_str(&format!(", group {}, {kind}", a.group + 1));
                (label, format!("sends {}", a.output.describe()))
            }
            (Doc::Art(m), Row::Combination(i)) => {
                let members = crate::cells::members_text(m, i);
                let sends = art_output(m, row).map_or_else(String::new, |o| o.describe());
                (
                    format!("Combination {members}"),
                    format!("sends {sends} instead of what each sends"),
                )
            }
            (Doc::Drum(m), Row::Sound(i)) => {
                let Some(s) = m.sounds.get(i) else {
                    return (String::new(), String::new());
                };
                let mut label = format!(
                    "{}, on {} ({})",
                    s.name,
                    crate::text::note_name(s.pitch),
                    s.pitch
                );
                let mut more = Vec::new();
                if let Some(n) = s.output_note.filter(|n| *n != s.pitch) {
                    label.push_str(&format!(", plays {}", crate::text::note_name(n)));
                }
                if let Some(c) = s.channel {
                    more.push(format!("channel {}", c + 1));
                }
                if let Some(p) = &s.port {
                    more.push(format!("to {}", crate::cells::port_name(model, p)));
                }
                if let Some(g) = s.quantize {
                    more.push(format!("grid {}", g.label()));
                }
                if model.map_learning() == Some((m.id, s.pitch)) {
                    label.push_str(", learning its pitch");
                }
                (label, more.join(", "))
            }
            _ => (String::new(), String::new()),
        }
    }

    fn cell_node(
        &self,
        doc: Doc<'_>,
        row: Row,
        col: Col,
        r: Rect,
        model: &Session,
    ) -> AccessNode<Action> {
        let id = cell_id(doc, row, col);
        let words = cell_words(doc, row, col, model);
        let name = col_name(col);
        let cell = self.cell(doc, Some(row), col, r);
        match doc.use_of(row, col) {
            Use::Number => {
                let (lo, hi) = range(col);
                let now = doc.number(row, col).unwrap_or(doc.start(row, col));
                AccessNode::new(id, AccessRole::SpinButton, name)
                    .at(cell)
                    .value(f64::from(now), f64::from(lo), f64::from(hi), words)
                    .described("The arrows step it; Enter types it")
                    .view_steps()
                    .on_press()
            }
            Use::Menu => {
                let mut n = AccessNode::new(id, AccessRole::Button, format!("{name}: {words}"))
                    .at(cell)
                    .described("Enter: choose")
                    .with_menu();
                if matches!(col, Col::Group | Col::Channel) {
                    n = n.view_steps();
                }
                n
            }
            Use::Toggle if col == Col::Learn => {
                let learning = match (doc, row) {
                    (Doc::Drum(m), Row::Sound(i)) => m
                        .sounds
                        .get(i)
                        .is_some_and(|s| model.map_learning() == Some((m.id, s.pitch))),
                    _ => false,
                };
                AccessNode::new(
                    id,
                    AccessRole::ToggleButton,
                    "Learn the pitch from MIDI input",
                )
                .at(cell)
                .checked(learning)
                .described("Then play the key on a MIDI keyboard or pad")
                .on_press()
            }
            Use::Toggle => AccessNode::new(id, AccessRole::Button, format!("{name}: {words}"))
                .at(cell)
                .described("Enter: switch attribute and direction")
                .on_press(),
            _ => AccessNode::new(id, AccessRole::Cell, format!("{name}: {words}"))
                .at(cell)
                .described("Enter: type it")
                .on_press(),
        }
    }

    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let buttons = Self::buttons(size, model).into_iter().map(|(b, r)| {
            let label = Self::button_label(b, model);
            let name = match b {
                Button::Map => match model.map_editor() {
                    Some(m) => {
                        format!("Map: {}, {}", label.trim_end_matches(" ▾"), m.kind().noun())
                    }
                    None => "Map: none".into(),
                },
                _ => label,
            };
            let mut n = AccessNode::new(button_id(b), AccessRole::Button, name)
                .at(r)
                .fixed()
                .disabled(Self::button_off(b, model));
            if let Some(tip) = self.tooltip(r.center(), size, model) {
                n = n.described(tip);
            }
            if b == Button::Map {
                n.with_menu()
            } else {
                n.on_press()
            }
        });
        let mut out = vec![
            AccessNode::new(
                access_id(&[TOOLBAR]),
                AccessRole::Toolbar,
                "Map editor toolbar",
            )
            .at(Rect::new(0.0, 0.0, size.w, super::HEADER_H))
            .fixed()
            .with_children(buttons),
            AccessNode::new(
                access_id(&[INFO]),
                AccessRole::Label,
                Self::info_text(model).0,
            )
            .at(Rect::new(0.0, super::HEADER_H, size.w, super::INFO_H))
            .fixed(),
        ];
        let Some(doc) = Self::doc(model) else {
            return out;
        };
        let names = Rect::new(
            0.0,
            super::HEADER_H + super::INFO_H,
            size.w,
            super::COLUMNS_H,
        );
        let headers = doc
            .columns()
            .iter()
            .filter(|c| !c.1.is_empty())
            .map(|(c, label, _)| {
                AccessNode::new(
                    access_id(&[HEADER, *c as u64]),
                    AccessRole::ColumnHeader,
                    *label,
                )
                .at(self.cell(doc, None, *c, names))
                .fixed_y()
            });
        let rows = self.rows(doc);
        let items = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| **r != Row::Combinations)
            .map(|(i, row)| {
                let r = self.row_rect(i, size);
                let (label, more) = Self::row_words(doc, *row, model);
                let cells = row_cells(doc, *row)
                    .into_iter()
                    .map(|c| self.cell_node(doc, *row, c, r, model));
                AccessNode::new(row_id(doc, *row), AccessRole::Row, label)
                    .at(r)
                    .described(more)
                    .selected(self.selected == Some(*row))
                    .on_press()
                    .with_menu()
                    .with_children(cells)
            });
        let what = match doc {
            Doc::Art(m) => format!("Articulations of {}", m.name),
            Doc::Drum(m) => format!("Sounds of {}", m.name),
        };
        out.push(
            AccessNode::new(access_id(&[TABLE]), AccessRole::Table, what)
                .at(Self::table(size))
                .with_children(headers)
                .with_children(items),
        );
        out
    }

    /// The control a node id names.
    fn control_of(&self, id: u64, size: Size, model: &Session) -> Option<Control> {
        if let Some((b, _)) = Self::buttons(size, model)
            .into_iter()
            .find(|(b, _)| button_id(*b) == id)
        {
            return Some(Control::Button(b));
        }
        let doc = Self::doc(model)?;
        for row in doc.rows() {
            if row_id(doc, row) == id {
                return Some(Control::Row(row));
            }
            if let Some(c) = row_cells(doc, row)
                .into_iter()
                .find(|c| cell_id(doc, row, *c) == id)
            {
                return Some(Control::Cell(row, c));
            }
        }
        None
    }

    pub(crate) fn press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(control) = self.control_of(id, size, model) else {
            return false;
        };
        if let Control::Button(b) = control {
            let Some((_, r)) = Self::buttons(size, model)
                .into_iter()
                .find(|(x, _)| *x == b)
            else {
                return false;
            };
            if Self::button_off(b, model)
                || !matches!(press, AccessPress::Activate | AccessPress::Menu)
            {
                return false;
            }
            self.press_button(b, r, model, cx);
            return true;
        }
        let Some(doc) = Self::doc(model) else {
            return false;
        };
        let rows = self.rows(doc);
        let (row, col) = match control {
            Control::Row(r) => (r, None),
            Control::Cell(r, c) => (r, Some(c)),
            Control::Button(_) => return false,
        };
        let Some(i) = rows.iter().position(|r| *r == row) else {
            return false;
        };
        self.selected = Some(row);
        self.keep_in_view(i, size);
        cx.redraw();
        let r = self.row_rect(i, size);
        let under = |r: Rect| Point::new(r.x, r.bottom());
        match (col, press) {
            (None, AccessPress::Activate) => {}
            (None, AccessPress::Menu) => cx.request(HostRequest::ContextMenu {
                at: under(r),
                items: self.row_menu(doc, row, model),
            }),
            (Some(c), AccessPress::Increment | AccessPress::Decrement) => {
                let up = press == AccessPress::Increment;
                let Some(a) = doc.step(row, c, up) else {
                    return false;
                };
                cx.emit(a);
            }
            (Some(c), AccessPress::Activate) => {
                let cell = self.cell(doc, Some(row), c, r);
                return self.press_cell(doc, row, c, cell, model, cx);
            }
            (Some(c), AccessPress::Menu) => {
                let cell = self.cell(doc, Some(row), c, r);
                let items = crate::cells::cell_menu(doc, row, c, model)
                    .unwrap_or_else(|| self.row_menu(doc, row, model));
                cx.request(HostRequest::ContextMenu {
                    at: under(cell),
                    items,
                });
            }
            _ => return false,
        }
        true
    }

    /// The map shown changing.
    pub(crate) fn news(&mut self, model: &Session) -> Vec<String> {
        let now = model.map_editor();
        let Some(before) = self.told.replace(now) else {
            return Vec::new();
        };
        if before == now {
            return Vec::new();
        }
        match now {
            Some(map) => {
                let kind = match map.kind() {
                    MapKind::Articulation => "articulation map",
                    MapKind::Drum => "drum map",
                };
                vec![format!(
                    "Editing the {kind} {}",
                    model.map_name(map).unwrap_or_default()
                )]
            }
            None => vec!["No map to edit".into()],
        }
    }
}

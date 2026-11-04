//! The two tables' cells: what each column holds per row, how it is
//! edited ([`Use`]) and the map an edit makes. Every edit is the whole map
//! changed (`MapOp::EditArticulationMap` / `EditDrumMap`: one undo step);
//! an edit that changes nothing makes no action.

use crate::text::{
    controls_text, is_blank, keyswitches_text, note_name, parse_controls, parse_int, parse_key,
    parse_keyswitches, semitones,
};
use crate::{Col, Row};
use faderframe_project::articulation::{
    ArticulationKind, ArticulationMap, ArticulationOutput, Combination, GROUPS,
};
use faderframe_project::drum_map::{DrumMap, DrumSound};
use faderframe_session::maps::MapOp;
use faderframe_session::{Action, Session};
use faderframe_timeline::GridDivision;
use faderframe_ui_canvas::MenuItem;

/// The articulation table: the articulations' own columns, then what each
/// sends (combination rows share those; their articulations span Name …
/// Type).
pub(crate) const ART_COLUMNS: [(Col, &str, f32); 12] = [
    (Col::Handle, "", 22.0),
    (Col::Name, "Name", 140.0),
    (Col::Symbol, "Symbol", 76.0),
    (Col::Group, "Group", 54.0),
    (Col::Kind, "Type", 92.0),
    (Col::Keyswitches, "Keyswitches", 132.0),
    (Col::Program, "Program", 66.0),
    (Col::Controls, "Controllers", 128.0),
    (Col::Channel, "Channel", 64.0),
    (Col::Transpose, "Transpose", 74.0),
    (Col::Velocity, "Velocity", 66.0),
    (Col::Length, "Length", 64.0),
];

/// The drum table: the sounds in the map's order.
pub(crate) const DRUM_COLUMNS: [(Col, &str, f32); 8] = [
    (Col::Handle, "", 22.0),
    (Col::Pitch, "Pitch", 76.0),
    (Col::Name, "Name", 190.0),
    (Col::Output, "Plays", 66.0),
    (Col::Channel, "Channel", 70.0),
    (Col::Port, "Port", 170.0),
    (Col::Grid, "Grid", 92.0),
    (Col::Learn, "", 84.0),
];

/// How a cell is edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    None,
    /// Dragged to move the row.
    Handle,
    /// A double-click (or Enter) types it.
    Text,
    /// Dragged up and down, the arrows step it, a double-click types it.
    Number,
    /// A click (or Enter) opens its menu; the arrows step it.
    Menu,
    /// A click (or Enter) switches it.
    Toggle,
}

/// The map shown.
#[derive(Clone, Copy, Debug)]
pub enum Doc<'a> {
    Art(&'a ArticulationMap),
    Drum(&'a DrumMap),
}

impl<'a> Doc<'a> {
    pub fn columns(self) -> &'static [(Col, &'static str, f32)] {
        match self {
            Doc::Art(_) => &ART_COLUMNS,
            Doc::Drum(_) => &DRUM_COLUMNS,
        }
    }

    pub fn name(self) -> &'a str {
        match self {
            Doc::Art(m) => &m.name,
            Doc::Drum(m) => &m.name,
        }
    }

    /// The rows in the map's order.
    pub fn rows(self) -> Vec<Row> {
        match self {
            Doc::Art(m) => (0..m.articulations.len())
                .map(Row::Articulation)
                .chain(std::iter::once(Row::Combinations))
                .chain((0..m.combinations.len()).map(Row::Combination))
                .collect(),
            Doc::Drum(m) => (0..m.sounds.len()).map(Row::Sound).collect(),
        }
    }

    /// How many rows move by dragging (the articulations, the sounds).
    pub fn movable(self) -> usize {
        match self {
            Doc::Art(m) => m.articulations.len(),
            Doc::Drum(m) => m.sounds.len(),
        }
    }

    /// The row exists in the map.
    pub fn has(self, row: Row) -> bool {
        match (self, row) {
            (Doc::Art(m), Row::Articulation(i)) => i < m.articulations.len(),
            (Doc::Art(_), Row::Combinations) => true,
            (Doc::Art(m), Row::Combination(i)) => i < m.combinations.len(),
            (Doc::Drum(m), Row::Sound(i)) => i < m.sounds.len(),
            _ => false,
        }
    }

    pub fn use_of(self, row: Row, col: Col) -> Use {
        match (self, row) {
            (Doc::Art(_), Row::Articulation(_)) => match col {
                Col::Handle => Use::Handle,
                Col::Name | Col::Symbol | Col::Keyswitches | Col::Controls => Use::Text,
                Col::Group | Col::Channel => Use::Menu,
                Col::Kind => Use::Toggle,
                Col::Program | Col::Transpose | Col::Velocity | Col::Length => Use::Number,
                _ => Use::None,
            },
            (Doc::Art(_), Row::Combination(_)) => match col {
                Col::Members => Use::Menu,
                Col::Keyswitches | Col::Controls => Use::Text,
                Col::Channel => Use::Menu,
                Col::Program | Col::Transpose | Col::Velocity | Col::Length => Use::Number,
                _ => Use::None,
            },
            (Doc::Drum(_), Row::Sound(_)) => match col {
                Col::Handle => Use::Handle,
                Col::Pitch | Col::Output => Use::Number,
                Col::Name => Use::Text,
                Col::Channel | Col::Port | Col::Grid => Use::Menu,
                Col::Learn => Use::Toggle,
                _ => Use::None,
            },
            _ => Use::None,
        }
    }

    /// A number's stored value (`None`: unset — as the track, as written).
    pub fn number(self, row: Row, col: Col) -> Option<i32> {
        match self {
            Doc::Art(m) => {
                if let (Row::Articulation(i), Col::Group) = (row, col) {
                    return m.articulations.get(i).map(|a| i32::from(a.group));
                }
                let o = art_output(m, row)?;
                match col {
                    Col::Program => o.program.map(i32::from),
                    Col::Channel => o.channel.map(i32::from),
                    Col::Transpose => Some(i32::from(o.transpose)),
                    Col::Velocity => Some(i32::from(o.velocity)),
                    Col::Length => Some(i32::from(o.length)),
                    _ => None,
                }
            }
            Doc::Drum(m) => {
                let Row::Sound(i) = row else { return None };
                let s = m.sounds.get(i)?;
                match col {
                    Col::Pitch => Some(i32::from(s.pitch)),
                    Col::Output => s.output_note.map(i32::from),
                    Col::Channel => s.channel.map(i32::from),
                    _ => None,
                }
            }
        }
    }

    /// Where stepping or dragging an unset number starts.
    pub fn start(self, row: Row, col: Col) -> i32 {
        self.number(row, col).unwrap_or(match (self, col) {
            (Doc::Drum(_), Col::Output) => self.number(row, Col::Pitch).unwrap_or(60),
            _ => 0,
        })
    }

    /// The text of a cell (`dragged`: a number being dragged, shown in
    /// place of the stored one).
    pub fn text(self, row: Row, col: Col, dragged: Option<i32>, model: &Session) -> String {
        let number = |stored: Option<i32>| dragged.or(stored);
        match self {
            Doc::Art(m) => {
                if row == Row::Combinations {
                    return String::new();
                }
                if let Row::Articulation(i) = row {
                    let Some(a) = m.articulations.get(i) else {
                        return String::new();
                    };
                    match col {
                        Col::Name => return a.name.clone(),
                        Col::Symbol => return a.symbol.clone(),
                        Col::Group => {
                            return number(Some(i32::from(a.group)))
                                .map_or_else(String::new, |g| (g + 1).to_string());
                        }
                        Col::Kind => return kind_text(a.kind).into(),
                        _ => {}
                    }
                }
                if let (Row::Combination(i), Col::Members) = (row, col) {
                    return members_text(m, i);
                }
                let Some(o) = art_output(m, row) else {
                    return String::new();
                };
                match col {
                    Col::Keyswitches => keyswitches_text(&o.keyswitches),
                    Col::Controls => controls_text(&o.controls),
                    Col::Program => number(o.program.map(i32::from))
                        .map_or_else(|| "–".into(), |p| (p + 1).to_string()),
                    Col::Channel => number(o.channel.map(i32::from))
                        .map_or_else(|| "–".into(), |c| (c + 1).to_string()),
                    Col::Transpose => semitones(number(Some(i32::from(o.transpose))).unwrap_or(0)),
                    Col::Velocity => {
                        format!("{}%", number(Some(i32::from(o.velocity))).unwrap_or(100))
                    }
                    Col::Length => {
                        format!("{}%", number(Some(i32::from(o.length))).unwrap_or(100))
                    }
                    _ => String::new(),
                }
            }
            Doc::Drum(m) => {
                let Row::Sound(i) = row else {
                    return String::new();
                };
                let Some(s) = m.sounds.get(i) else {
                    return String::new();
                };
                let key = |k: i32| note_name(k.clamp(0, 127) as u8);
                match col {
                    Col::Pitch => {
                        let p = number(Some(i32::from(s.pitch))).unwrap_or(0);
                        format!("{} · {p}", key(p))
                    }
                    Col::Name => s.name.clone(),
                    Col::Output => {
                        number(s.output_note.map(i32::from)).map_or_else(|| "–".into(), key)
                    }
                    Col::Channel => number(s.channel.map(i32::from))
                        .map_or_else(|| "–".into(), |c| (c + 1).to_string()),
                    Col::Port => s
                        .port
                        .as_deref()
                        .map_or_else(|| "–".into(), |p| port_name(model, p)),
                    Col::Grid => s.quantize.map_or_else(|| "–".into(), GridDivision::label),
                    Col::Learn => "Learn".into(),
                    _ => String::new(),
                }
            }
        }
    }

    /// The number set (`None`: unset), as an action; `None` when nothing
    /// changes.
    pub fn with_number(self, row: Row, col: Col, v: Option<i32>) -> Option<Action> {
        match self {
            Doc::Art(m) => art_number(m, row, col, v),
            Doc::Drum(m) => drum_number(m, row, col, v),
        }
    }

    /// One step up or down from the stored value (an optional number steps
    /// down from its least to unset, and up from unset to its start).
    pub fn step(self, row: Row, col: Col, up: bool) -> Option<Action> {
        let (lo, hi) = range(col);
        let v = match (self.number(row, col), up) {
            (None, true) => Some(self.start(row, col)),
            (None, false) => return None,
            (Some(v), true) => Some((v + 1).min(hi)),
            (Some(v), false) if v <= lo && optional(self, col) => None,
            (Some(v), false) => Some((v - 1).max(lo)),
        };
        self.with_number(row, col, v)
    }
}

/// An optional number: unset is one of its values.
fn optional(doc: Doc<'_>, col: Col) -> bool {
    match doc {
        Doc::Art(_) => matches!(col, Col::Program | Col::Channel),
        Doc::Drum(_) => matches!(col, Col::Output | Col::Channel),
    }
}

/// A number column's range (stored values).
pub fn range(col: Col) -> (i32, i32) {
    match col {
        Col::Group => (0, GROUPS as i32 - 1),
        Col::Channel => (0, 15),
        Col::Transpose => (-48, 48),
        Col::Velocity | Col::Length => (1, 250),
        _ => (0, 127),
    }
}

/// A dragged number's change per step of the pointer.
pub fn drag_step(col: Col) -> i32 {
    match col {
        Col::Velocity | Col::Length => 5,
        _ => 1,
    }
}

fn kind_text(k: ArticulationKind) -> &'static str {
    match k {
        ArticulationKind::Direction => "→ Direction",
        ArticulationKind::Attribute => "• Attribute",
    }
}

/// A combination's articulations ("Legato + Sustain").
pub fn members_text(m: &ArticulationMap, i: usize) -> String {
    m.combinations.get(i).map_or_else(String::new, |c| {
        c.articulations
            .iter()
            .filter_map(|id| m.get(*id).map(|a| a.name.as_str()))
            .collect::<Vec<_>>()
            .join(" + ")
    })
}

/// A port's name (`key` when the output is not there now).
pub fn port_name(model: &Session, key: &str) -> String {
    model
        .midi_outputs()
        .into_iter()
        .find(|o| o.key == key)
        .map_or_else(|| format!("{key} (not connected)"), |o| o.name)
}

/// What an articulation row or a combination row sends.
pub fn art_output(m: &ArticulationMap, row: Row) -> Option<&ArticulationOutput> {
    match row {
        Row::Articulation(i) => m.articulations.get(i).map(|a| &a.output),
        Row::Combination(i) => m.combinations.get(i).map(|c| &c.output),
        _ => None,
    }
}

fn art_output_mut(m: &mut ArticulationMap, row: Row) -> Option<&mut ArticulationOutput> {
    match row {
        Row::Articulation(i) => m.articulations.get_mut(i).map(|a| &mut a.output),
        Row::Combination(i) => m.combinations.get_mut(i).map(|c| &mut c.output),
        _ => None,
    }
}

/// The articulation map changed by `f`, as an edit (`None`: unchanged).
pub fn art_edit(m: &ArticulationMap, f: impl FnOnce(&mut ArticulationMap)) -> Option<Action> {
    let mut new = m.clone();
    f(&mut new);
    (new != *m).then(|| Action::Maps(MapOp::EditArticulationMap(Box::new(new))))
}

/// The drum map changed by `f`, as an edit (`None`: unchanged).
pub fn drum_edit(m: &DrumMap, f: impl FnOnce(&mut DrumMap)) -> Option<Action> {
    let mut new = m.clone();
    f(&mut new);
    (new != *m).then(|| Action::Maps(MapOp::EditDrumMap(Box::new(new))))
}

fn art_number(m: &ArticulationMap, row: Row, col: Col, v: Option<i32>) -> Option<Action> {
    let (lo, hi) = range(col);
    let v = v.map(|v| v.clamp(lo, hi));
    art_edit(m, |m| {
        if let (Row::Articulation(i), Col::Group) = (row, col) {
            if let (Some(a), Some(g)) = (m.articulations.get_mut(i), v) {
                a.group = g as u8;
            }
            return;
        }
        let Some(o) = art_output_mut(m, row) else {
            return;
        };
        match col {
            Col::Program => o.program = v.map(|p| p as u8),
            Col::Channel => o.channel = v.map(|c| c as u8),
            Col::Transpose => o.transpose = v.unwrap_or(0) as i8,
            Col::Velocity => o.velocity = v.unwrap_or(100) as u8,
            Col::Length => o.length = v.unwrap_or(100) as u8,
            _ => {}
        }
    })
}

fn drum_number(m: &DrumMap, row: Row, col: Col, v: Option<i32>) -> Option<Action> {
    let Row::Sound(i) = row else { return None };
    let (lo, hi) = range(col);
    let v = v.map(|v| v.clamp(lo, hi) as u8);
    drum_edit(m, |m| match col {
        Col::Pitch => {
            if let Some(p) = v {
                m.set_pitch(i, p);
            }
        }
        Col::Output => {
            if let Some(s) = m.sounds.get_mut(i) {
                // Its own pitch again: as written.
                s.output_note = v.filter(|n| *n != s.pitch);
            }
        }
        Col::Channel => {
            if let Some(s) = m.sounds.get_mut(i) {
                s.channel = v;
            }
        }
        _ => {}
    })
}

/// What typing `text` into a cell of the articulation map makes.
pub fn art_text(m: &ArticulationMap, row: Row, col: Col, text: &str) -> Option<Action> {
    let t = text.trim();
    let optional = |lo: i32, hi: i32| -> Option<Option<i32>> {
        if is_blank(t) {
            return Some(None);
        }
        parse_int(t).filter(|v| (lo..=hi).contains(v)).map(Some)
    };
    match col {
        Col::Name => {
            let Row::Articulation(i) = row else {
                return None;
            };
            if t.is_empty() {
                return None;
            }
            art_edit(m, |m| {
                if let Some(a) = m.articulations.get_mut(i) {
                    a.name = t.to_string();
                }
            })
        }
        Col::Symbol => {
            let Row::Articulation(i) = row else {
                return None;
            };
            art_edit(m, |m| {
                if let Some(a) = m.articulations.get_mut(i) {
                    a.symbol = t.to_string();
                }
            })
        }
        Col::Keyswitches => {
            let keys = parse_keyswitches(t)?;
            art_edit(m, |m| {
                if let Some(o) = art_output_mut(m, row) {
                    o.keyswitches = keys;
                }
            })
        }
        Col::Controls => {
            let controls = parse_controls(t)?;
            art_edit(m, |m| {
                if let Some(o) = art_output_mut(m, row) {
                    o.controls = controls;
                }
            })
        }
        // Shown from 1.
        Col::Program => art_number(m, row, col, optional(1, 128)?.map(|p| p - 1)),
        Col::Channel => art_number(m, row, col, optional(1, 16)?.map(|c| c - 1)),
        Col::Group => {
            let g = parse_int(t).filter(|g| (1..=GROUPS as i32).contains(g))?;
            art_number(m, row, col, Some(g - 1))
        }
        Col::Transpose | Col::Velocity | Col::Length => {
            let (lo, hi) = range(col);
            let v = parse_int(t).filter(|v| (lo..=hi).contains(v))?;
            art_number(m, row, col, Some(v))
        }
        _ => None,
    }
}

/// What typing `text` into a cell of the drum map makes.
pub fn drum_text(m: &DrumMap, row: Row, col: Col, text: &str) -> Option<Action> {
    let Row::Sound(i) = row else { return None };
    let t = text.trim();
    match col {
        Col::Name => {
            if t.is_empty() {
                return None;
            }
            drum_edit(m, |m| {
                if let Some(s) = m.sounds.get_mut(i) {
                    s.name = t.to_string();
                }
            })
        }
        Col::Pitch => {
            // "C1 · 36" as shown reads as its key.
            let key = parse_key(t.split('·').next().unwrap_or(t))?;
            drum_number(m, row, col, Some(i32::from(key)))
        }
        Col::Output => {
            let v = if is_blank(t) {
                None
            } else {
                Some(i32::from(parse_key(t)?))
            };
            drum_number(m, row, col, v)
        }
        Col::Channel => {
            let v = if is_blank(t) {
                None
            } else {
                Some(parse_int(t).filter(|c| (1..=16).contains(c))? - 1)
            };
            drum_number(m, row, col, v)
        }
        _ => None,
    }
}

/// The articulation map with the articulation's type switched.
pub fn toggle_kind(m: &ArticulationMap, i: usize) -> Option<Action> {
    art_edit(m, |m| {
        if let Some(a) = m.articulations.get_mut(i) {
            a.kind = match a.kind {
                ArticulationKind::Attribute => ArticulationKind::Direction,
                ArticulationKind::Direction => ArticulationKind::Attribute,
            };
        }
    })
}

/// A cell's menu (`None`: the cell has none).
pub fn cell_menu(
    doc: Doc<'_>,
    row: Row,
    col: Col,
    model: &Session,
) -> Option<Vec<MenuItem<Action>>> {
    if doc.use_of(row, col) != Use::Menu {
        return None;
    }
    match (doc, col) {
        (Doc::Art(m), Col::Group) => {
            let now = doc.number(row, col);
            Some(
                (0..GROUPS as i32)
                    .map(|g| {
                        let label = format!("Group {}", g + 1);
                        match art_number(m, row, col, Some(g)) {
                            Some(a) => MenuItem::new(label, a).checked(now == Some(g)),
                            None => MenuItem::disabled(label).checked(now == Some(g)),
                        }
                    })
                    .collect(),
            )
        }
        (_, Col::Channel) => {
            let none = match doc {
                Doc::Art(_) => "As the Track's Notes",
                Doc::Drum(_) => "As the Note's",
            };
            let now = doc.number(row, col);
            let item = |label: String, v: Option<i32>| match doc.with_number(row, col, v) {
                Some(a) => MenuItem::new(label, a).checked(now == v),
                None => MenuItem::disabled(label).checked(now == v),
            };
            let mut items = vec![item(none.into(), None)];
            items.extend((0..16).map(|c| {
                let i = item(format!("Channel {}", c + 1), Some(c));
                if c == 0 { i.separated() } else { i }
            }));
            Some(items)
        }
        (Doc::Art(m), Col::Members) => {
            let Row::Combination(i) = row else {
                return None;
            };
            Some(members_menu(m, i))
        }
        (Doc::Drum(m), Col::Port) => {
            let Row::Sound(i) = row else { return None };
            let now = m.sounds.get(i).and_then(|s| s.port.clone());
            let set = |port: Option<String>| {
                drum_edit(m, |m| {
                    if let Some(s) = m.sounds.get_mut(i) {
                        s.port = port;
                    }
                })
            };
            let item = |label: String, port: Option<String>| {
                let on = now == port;
                match set(port) {
                    Some(a) => MenuItem::new(label, a).checked(on),
                    None => MenuItem::disabled(label).checked(on),
                }
            };
            let mut items = vec![item("The Track's Output".into(), None)];
            let outputs = model.midi_outputs();
            for (k, o) in outputs.iter().enumerate() {
                let i = item(o.name.clone(), Some(o.key.clone()));
                items.push(if k == 0 { i.separated() } else { i });
            }
            // A port not there now stays offered as it is.
            if let Some(p) = now
                .as_ref()
                .filter(|p| !outputs.iter().any(|o| o.key == **p))
            {
                items.push(item(format!("{p} (not connected)"), Some(p.clone())));
            }
            Some(items)
        }
        (Doc::Drum(m), Col::Grid) => {
            let Row::Sound(i) = row else { return None };
            let now = m.sounds.get(i).and_then(|s| s.quantize);
            let item = |label: String, grid: Option<GridDivision>| {
                let action = drum_edit(m, |m| {
                    if let Some(s) = m.sounds.get_mut(i) {
                        s.quantize = grid;
                    }
                });
                match action {
                    Some(a) => MenuItem::new(label, a).checked(now == grid),
                    None => MenuItem::disabled(label).checked(now == grid),
                }
            };
            let mut items = vec![item("The Editor's Grid".into(), None)];
            for (k, g) in grids().into_iter().enumerate() {
                let i = item(g.label(), Some(g));
                items.push(if k == 0 { i.separated() } else { i });
            }
            Some(items)
        }
        _ => None,
    }
}

/// The grids a drum sound can draw on.
pub fn grids() -> Vec<GridDivision> {
    let mut out = vec![GridDivision::Bar, GridDivision::Beat];
    out.extend([2u16, 4, 8, 16, 32, 64].map(GridDivision::Note));
    out.extend([4u16, 8, 16, 32].map(GridDivision::Triplet));
    out.extend([4u16, 8, 16].map(GridDivision::Dotted));
    out
}

/// A combination's articulations, a submenu per group: one of the
/// group's, or none of it (while two would be left).
pub fn members_menu(m: &ArticulationMap, i: usize) -> Vec<MenuItem<Action>> {
    let Some(c) = m.combinations.get(i) else {
        return Vec::new();
    };
    let group_of = |id| m.get(id).map(|a| usize::from(a.group));
    m.groups()
        .into_iter()
        .map(|g| {
            let g = usize::from(g);
            let now = c
                .articulations
                .iter()
                .copied()
                .find(|a| group_of(*a) == Some(g));
            let set = |pick: Option<faderframe_project::articulation::ArticulationId>| {
                art_edit(m, |m| {
                    let group_of =
                        |m: &ArticulationMap, id| m.get(id).map(|a| usize::from(a.group));
                    let mut members: Vec<_> = m.combinations[i]
                        .articulations
                        .iter()
                        .copied()
                        .filter(|a| group_of(m, *a) != Some(g))
                        .collect();
                    members.extend(pick);
                    members.sort_by_key(|a| group_of(m, *a));
                    m.combinations[i].articulations = members;
                })
            };
            let mut items: Vec<MenuItem<Action>> = m
                .articulations
                .iter()
                .filter(|a| usize::from(a.group) == g)
                .map(|a| match set(Some(a.id)) {
                    Some(x) => MenuItem::new(a.name.clone(), x).checked(now == Some(a.id)),
                    None => MenuItem::disabled(a.name.clone()).checked(now == Some(a.id)),
                })
                .collect();
            let none = if now.is_some() && c.articulations.len() <= 2 {
                MenuItem::disabled("None (a combination needs two)")
            } else {
                match set(None) {
                    Some(x) => MenuItem::new("None", x).checked(now.is_none()),
                    None => MenuItem::disabled("None").checked(now.is_none()),
                }
            };
            items.push(none.separated());
            MenuItem::submenu(format!("Group {}", g + 1), items)
        })
        .collect()
}

// --- rows --------------------------------------------------------------------------

/// A name like `base n` the list has none of.
fn fresh_name(base: &str, taken: &[&str]) -> String {
    (taken.len() + 1..)
        .map(|n| format!("{base} {n}"))
        .find(|n| !taken.iter().any(|t| t.eq_ignore_ascii_case(n)))
        .unwrap_or_else(|| base.to_string())
}

/// A new articulation after the `after`th (else last): a direction in the
/// group of the one before it, sending nothing yet.
pub fn add_articulation(m: &ArticulationMap, after: Option<usize>) -> Option<Action> {
    art_edit(m, |m| {
        let taken: Vec<&str> = m.articulations.iter().map(|a| a.name.as_str()).collect();
        let name = fresh_name("Articulation", &taken);
        let at = after.map_or(m.articulations.len(), |i| {
            (i + 1).min(m.articulations.len())
        });
        let group = at
            .checked_sub(1)
            .and_then(|i| m.articulations.get(i))
            .map_or(0, |a| a.group);
        m.add(
            name,
            "",
            group,
            ArticulationKind::Direction,
            ArticulationOutput::default(),
        );
        if let Some(a) = m.articulations.pop() {
            m.articulations.insert(at, a);
        }
    })
}

/// A copy of the `i`th articulation right after it.
pub fn duplicate_articulation(m: &ArticulationMap, i: usize) -> Option<Action> {
    let a = m.articulations.get(i)?.clone();
    art_edit(m, |m| {
        let id = m.next_id();
        m.articulations.insert(
            i + 1,
            faderframe_project::articulation::Articulation {
                id,
                name: format!("{} copy", a.name),
                ..a
            },
        );
    })
}

pub fn remove_articulation(m: &ArticulationMap, i: usize) -> Option<Action> {
    if i >= m.articulations.len() {
        return None;
    }
    art_edit(m, |m| {
        m.articulations.remove(i);
        m.tidy();
    })
}

/// A new combination: the first articulations of the first two groups.
pub fn add_combination(m: &ArticulationMap) -> Option<Action> {
    let state = m.default_state();
    let members: Vec<_> = state.iter().flatten().copied().take(2).collect();
    if members.len() < 2 {
        return None;
    }
    art_edit(m, |m| {
        m.combinations.push(Combination {
            articulations: members,
            output: ArticulationOutput::default(),
        });
    })
}

pub fn remove_combination(m: &ArticulationMap, i: usize) -> Option<Action> {
    if i >= m.combinations.len() {
        return None;
    }
    art_edit(m, |m| {
        m.combinations.remove(i);
    })
}

/// A new sound after the `after`th (else last), on the first free key
/// from the one after it (named as General MIDI names that key).
pub fn add_sound(m: &DrumMap, after: Option<usize>) -> Option<Action> {
    let at = after.map_or(m.sounds.len(), |i| (i + 1).min(m.sounds.len()));
    let near = at
        .checked_sub(1)
        .and_then(|i| m.sounds.get(i))
        .map_or(36, |s| s.pitch.saturating_add(1));
    let pitch = m.free_pitch(near)?;
    let taken: Vec<&str> = m.sounds.iter().map(|s| s.name.as_str()).collect();
    let name = faderframe_midi::gm::drum_name(pitch)
        .filter(|n| !taken.contains(n))
        .map_or_else(|| fresh_name("Sound", &taken), str::to_string);
    drum_edit(m, |m| m.sounds.insert(at, DrumSound::new(pitch, name)))
}

/// A copy of the `i`th sound right after it, on the next free key.
pub fn duplicate_sound(m: &DrumMap, i: usize) -> Option<Action> {
    let s = m.sounds.get(i)?.clone();
    let pitch = m.free_pitch(s.pitch.saturating_add(1))?;
    drum_edit(m, |m| {
        m.sounds.insert(
            i + 1,
            DrumSound {
                pitch,
                name: format!("{} copy", s.name),
                ..s
            },
        );
    })
}

pub fn remove_sound(m: &DrumMap, i: usize) -> Option<Action> {
    if i >= m.sounds.len() {
        return None;
    }
    drum_edit(m, |m| {
        m.sounds.remove(i);
    })
}

/// The `from`th movable row put at `to`.
pub fn move_row(doc: Doc<'_>, from: usize, to: usize) -> Option<Action> {
    if from == to || from >= doc.movable() || to >= doc.movable() {
        return None;
    }
    match doc {
        Doc::Art(m) => art_edit(m, |m| {
            let a = m.articulations.remove(from);
            m.articulations.insert(to, a);
        }),
        Doc::Drum(m) => drum_edit(m, |m| {
            let s = m.sounds.remove(from);
            m.sounds.insert(to, s);
        }),
    }
}

/// The map renamed.
pub fn rename(doc: Doc<'_>, name: &str) -> Option<Action> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    match doc {
        Doc::Art(m) => art_edit(m, |m| m.name = name.to_string()),
        Doc::Drum(m) => drum_edit(m, |m| m.name = name.to_string()),
    }
}

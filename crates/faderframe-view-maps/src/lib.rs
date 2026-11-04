//! The map editor: an articulation map or a drum map, a row each, every
//! property in a column.
//!
//! * An articulation map lists its articulations — name, symbol, group,
//!   type (a click switches attribute ⇄ direction) — and what each sends:
//!   keyswitches ("C0, D0@90"), a program, controller values ("1=64"), the
//!   channel its notes go out on, transpose, velocity and length in
//!   percent. Below them the combinations: articulations of different
//!   groups sending something of their own (their articulations chosen per
//!   group from a menu).
//! * A drum map lists its sounds in the map's order: the pitch they are
//!   written on, name, the note they play, channel, MIDI port, the grid
//!   drawing keeps to on their row, and Learn (the next key played on a
//!   MIDI input becomes the sound's pitch).
//!
//! A double-click types a cell, a vertical drag changes a number (shown at
//! once, committed on release), a click opens a cell's menu; rows move by
//! dragging their handle (committed on release) or Alt+Up/Down, are added
//! from the toolbar or a row's menu, deleted with Delete. Every change is
//! one undo step: the whole map replaced (`MapOp::EditArticulationMap`,
//! `MapOp::EditDrumMap`). The toolbar chooses the map (or makes a new
//! one), renames it, imports a map file (FaderFrame's JSON, a Cubase
//! `.expressionmap`), exports it as JSON and saves it as a user map.
//! Which map shows is the session's (`Session::map_editor`): the one opened
//! from a track menu's "Edit Map…", else the selected track's.

#![forbid(unsafe_code)]

mod access;
mod cells;
pub mod text;

pub use cells::{Doc, Use};

use faderframe_project::articulation::ArticulationMap;
use faderframe_project::drum_map::DrumMap;
use faderframe_session::maps::{MapKind, MapOp, MapRef};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    Cursor, EventCx, FileChoice, FontFamily, HostRequest, Key, MenuItem, Painter, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 34.0;
/// The line under the toolbar: who uses the map, learning.
const INFO_H: f32 = 20.0;
const COLUMNS_H: f32 = 22.0;
pub const ROW_H: f32 = 24.0;
/// Pixels of vertical drag per step of a number.
pub const DRAG_PX: f32 = 4.0;
/// Narrower than this, the toolbar leaves its title out for the buttons.
const TITLE_FROM: f32 = 960.0;

/// A row of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Row {
    /// The `i`th articulation.
    Articulation(usize),
    /// The line over the combinations.
    Combinations,
    /// The `i`th combination.
    Combination(usize),
    /// The `i`th drum sound.
    Sound(usize),
}

impl Row {
    /// Its place among the rows that move (articulations, sounds).
    pub fn movable(self) -> Option<usize> {
        match self {
            Row::Articulation(i) | Row::Sound(i) => Some(i),
            _ => None,
        }
    }

    fn with_index(self, i: usize) -> Row {
        match self {
            Row::Articulation(_) => Row::Articulation(i),
            Row::Combination(_) => Row::Combination(i),
            Row::Sound(_) => Row::Sound(i),
            Row::Combinations => Row::Combinations,
        }
    }
}

/// A column of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    /// Drag here to move the row.
    Handle,
    Name,
    Symbol,
    Group,
    /// Attribute or direction.
    Kind,
    Keyswitches,
    Program,
    Controls,
    Channel,
    Transpose,
    Velocity,
    Length,
    /// A combination's articulations (over Name … Type).
    Members,
    /// The key a drum sound's notes are written on.
    Pitch,
    /// The note it plays.
    Output,
    Port,
    Grid,
    /// Learn the pitch from MIDI input.
    Learn,
}

pub(crate) use Column as Col;

/// The toolbar's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    /// The map shown (its menu chooses another or makes one).
    Map,
    Rename,
    /// + Articulation / + Sound.
    Add,
    AddCombination,
    Import,
    Export,
    SaveUser,
}

/// What the pointer is dragging.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// A row by its handle (shown where it would go; moved on release).
    Row { from: usize, to: usize },
    /// A number (shown at once, set on release).
    Value {
        row: Row,
        col: Col,
        y0: f32,
        base: i32,
        value: i32,
    },
}

pub struct MapsView {
    theme: Theme,
    scroll: f32,
    scroll_x: f32,
    hover: Option<Row>,
    selected: Option<Row>,
    drag: Option<Drag>,
    /// The map last painted (the selection and scrolling go with it).
    shown: Option<MapRef>,
    /// The map a screen reader was last told of ([`access`]).
    told: Option<Option<MapRef>>,
}

impl MapsView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            scroll_x: 0.0,
            hover: None,
            selected: None,
            drag: None,
            shown: None,
            told: None,
        }
    }

    /// The map shown, from the project.
    pub fn doc(model: &Session) -> Option<Doc<'_>> {
        match model.map_editor()? {
            MapRef::Articulation(id) => model.project().articulation_map(id).map(Doc::Art),
            MapRef::Drum(id) => model.project().drum_map(id).map(Doc::Drum),
        }
    }

    /// The row selected (still in the map).
    pub fn selected(&self) -> Option<Row> {
        self.selected
    }

    // --- geometry ----------------------------------------------------------------

    fn table(size: Size) -> Rect {
        let top = HEADER_H + INFO_H;
        Rect::new(0.0, top, size.w, (size.h - top).max(0.0))
    }

    /// The rows' area (under the column names).
    fn body(size: Size) -> Rect {
        let t = Self::table(size);
        Rect::new(t.x, t.y + COLUMNS_H, t.w, (t.h - COLUMNS_H).max(0.0))
    }

    /// The `i`th row shown.
    pub fn row_rect(&self, i: usize, size: Size) -> Rect {
        let b = Self::body(size);
        Rect::new(b.x, b.y + i as f32 * ROW_H - self.scroll, b.w, ROW_H)
    }

    /// The width of the columns.
    fn content_w(doc: Doc<'_>) -> f32 {
        doc.columns().iter().map(|c| c.2).sum()
    }

    /// Column `col`'s place across `r` (a row, or the names' line);
    /// a combination's articulations span Name … Type.
    pub fn cell(&self, doc: Doc<'_>, row: Option<Row>, col: Col, r: Rect) -> Rect {
        let span = matches!(row, Some(Row::Combination(_)));
        let (col, last) = match col {
            Col::Members | Col::Name | Col::Symbol | Col::Group | Col::Kind if span => {
                (Col::Name, Col::Kind)
            }
            c => (c, c),
        };
        let mut x = r.x - self.scroll_x;
        let mut start = None;
        for (c, _, w) in doc.columns() {
            if *c == col {
                start = Some(x);
            }
            x += w;
            if *c == last
                && let Some(s) = start
            {
                return Rect::new(s, r.y, x - s, r.h);
            }
        }
        Rect::new(x, r.y, 0.0, r.h)
    }

    /// The column at `x` of a row.
    fn column_at(&self, doc: Doc<'_>, row: Row, x: f32) -> Option<Col> {
        let mut left = -self.scroll_x;
        for (c, _, w) in doc.columns() {
            if x >= left && x < left + w {
                let span = matches!(row, Row::Combination(_))
                    && matches!(c, Col::Name | Col::Symbol | Col::Group | Col::Kind);
                return Some(if span { Col::Members } else { *c });
            }
            left += w;
        }
        None
    }

    /// The rows in the order shown (a row being dragged where it would go).
    pub fn rows(&self, doc: Doc<'_>) -> Vec<Row> {
        let mut rows = doc.rows();
        if let Some(Drag::Row { from, to }) = self.drag
            && from < doc.movable()
            && to < doc.movable()
        {
            let r = rows.remove(from);
            rows.insert(to, r);
        }
        rows
    }

    /// The place of the row under `pos` among those shown.
    pub fn row_at(&self, pos: Point, size: Size, count: usize) -> Option<usize> {
        let b = Self::body(size);
        if !b.contains(pos) {
            return None;
        }
        let i = ((pos.y - b.y + self.scroll) / ROW_H).floor();
        (i >= 0.0 && (i as usize) < count).then_some(i as usize)
    }

    fn clamp(&mut self, doc: Option<Doc<'_>>, size: Size) {
        let (count, w) = doc.map_or((0, 0.0), |d| (d.rows().len(), Self::content_w(d)));
        let b = Self::body(size);
        self.scroll = self
            .scroll
            .clamp(0.0, (count as f32 * ROW_H - b.h).max(0.0));
        self.scroll_x = self.scroll_x.clamp(0.0, (w - size.w).max(0.0));
    }

    pub(crate) fn keep_in_view(&mut self, i: usize, size: Size) {
        let b = Self::body(size);
        let top = i as f32 * ROW_H;
        if top < self.scroll {
            self.scroll = top;
        } else if top + ROW_H > self.scroll + b.h {
            self.scroll = top + ROW_H - b.h;
        }
    }

    /// The toolbar's buttons and where they are (only those that apply).
    pub fn buttons(size: Size, model: &Session) -> Vec<(Button, Rect)> {
        let doc = Self::doc(model);
        let h = HEADER_H - 12.0;
        let width = |label: &str| label.chars().count() as f32 * 6.4 + 22.0;
        let mut out = Vec::new();
        let mut x = if Self::titled(size) { 108.0 } else { 12.0 };
        let map_label = Self::button_label(Button::Map, model);
        let w = width(&map_label).clamp(120.0, 240.0);
        out.push((Button::Map, Rect::new(x, 6.0, w, h)));
        x += w + 8.0;
        let Some(doc) = doc else {
            return out;
        };
        let mut left = vec![Button::Rename, Button::Add];
        if matches!(doc, Doc::Art(_)) {
            left.push(Button::AddCombination);
        }
        for b in left {
            let w = width(&Self::button_label(b, model));
            out.push((b, Rect::new(x, 6.0, w, h)));
            x += w + 4.0;
        }
        // The files' buttons at the right, never over those on the left
        // (past the edge when the view is too narrow for both).
        let files = [Button::Import, Button::Export, Button::SaveUser];
        let widths: Vec<f32> = files
            .iter()
            .map(|b| width(&Self::button_label(*b, model)))
            .collect();
        let total = widths.iter().sum::<f32>() + 4.0 * (files.len() - 1) as f32;
        let mut at = (size.w - 12.0 - total).max(x + 8.0);
        for (b, w) in files.into_iter().zip(widths) {
            out.push((b, Rect::new(at, 6.0, w, h)));
            at += w + 4.0;
        }
        out
    }

    /// Wide enough for the title before the buttons.
    fn titled(size: Size) -> bool {
        size.w >= TITLE_FROM
    }

    pub fn button_label(b: Button, model: &Session) -> String {
        let doc = Self::doc(model);
        match b {
            Button::Map => match doc {
                Some(d) => format!("{} ▾", d.name()),
                None => "No Map ▾".into(),
            },
            Button::Rename => "Rename…".into(),
            Button::Add => match doc {
                Some(Doc::Drum(_)) => "+ Sound".into(),
                _ => "+ Articulation".into(),
            },
            Button::AddCombination => "+ Combination".into(),
            Button::Import => "Import…".into(),
            Button::Export => "Export…".into(),
            Button::SaveUser => "Save as User Map".into(),
        }
    }

    /// The button does nothing now.
    fn button_off(b: Button, model: &Session) -> bool {
        match (b, Self::doc(model)) {
            (Button::AddCombination, Some(Doc::Art(m))) => cells::add_combination(m).is_none(),
            (Button::Add, Some(Doc::Drum(m))) => cells::add_sound(m, None).is_none(),
            _ => false,
        }
    }

    // --- actions -------------------------------------------------------------------

    /// The map chooser: the project's maps, new ones, deleting this one.
    fn map_menu(model: &Session) -> Vec<MenuItem<Action>> {
        let shown = model.map_editor();
        let track = model.map_editor_track();
        let p = model.project();
        let open = |map: MapRef| Action::Maps(MapOp::OpenEditor { map, track: None });
        let mut items = Vec::new();
        for m in &p.articulation_maps {
            let map = MapRef::Articulation(m.id);
            items.push(
                MenuItem::new(format!("{} (Articulations)", m.name), open(map))
                    .checked(shown == Some(map)),
            );
        }
        for (k, m) in p.drum_maps.iter().enumerate() {
            let map = MapRef::Drum(m.id);
            let item =
                MenuItem::new(format!("{} (Drums)", m.name), open(map)).checked(shown == Some(map));
            items.push(if k == 0 && !items.is_empty() {
                item.separated()
            } else {
                item
            });
        }
        let new: Vec<MenuItem<Action>> = model
            .new_map_choices(track)
            .into_iter()
            .map(|c| {
                let label = c
                    .label
                    .strip_prefix("New ")
                    .map_or(c.label.clone(), str::to_string);
                let item = MenuItem::new(label, c.action);
                if c.group_start {
                    item.separated()
                } else {
                    item
                }
            })
            .collect();
        let new = MenuItem::submenu("New", new);
        items.push(if items.is_empty() {
            new
        } else {
            new.separated()
        });
        if let Some(map) = shown {
            let (label, op) = match map {
                MapRef::Articulation(id) => ("Delete This Map", MapOp::RemoveArticulationMap(id)),
                MapRef::Drum(id) => ("Delete This Map", MapOp::RemoveDrumMap(id)),
            };
            items.push(MenuItem::new(label, Action::Maps(op)).separated());
        }
        items
    }

    /// The map's name typed.
    fn rename_input(model: &Session, at: Rect) -> Option<HostRequest<Action>> {
        let initial = Self::doc(model)?.name().to_string();
        let owned = Owned::of(Self::doc(model)?);
        Some(HostRequest::TextInput {
            at,
            initial,
            commit: Box::new(move |t| cells::rename(owned.doc(), t)),
        })
    }

    /// A map file chosen: read into the project (for the track edited for).
    fn import_request(model: &Session) -> Option<HostRequest<Action>> {
        let kind = model
            .map_editor()
            .map_or(MapKind::Articulation, MapRef::kind);
        let track = model.map_editor_track();
        let (title, filters) = match kind {
            MapKind::Articulation => (
                "Import an Articulation Map",
                vec![
                    (
                        "Articulation maps".to_string(),
                        vec!["*.json".to_string(), "*.expressionmap".to_string()],
                    ),
                    ("All files".to_string(), vec!["*".to_string()]),
                ],
            ),
            MapKind::Drum => (
                "Import a Drum Map",
                vec![
                    ("Drum maps".to_string(), vec!["*.json".to_string()]),
                    ("All files".to_string(), vec!["*".to_string()]),
                ],
            ),
        };
        Some(HostRequest::ChooseFiles {
            choice: FileChoice::Open {
                title: title.into(),
                filters,
            },
            commit: Box::new(move |mut paths| {
                let path = paths.pop()?;
                Some(Action::Maps(match kind {
                    MapKind::Articulation => MapOp::ImportArticulationMap { track, path },
                    MapKind::Drum => MapOp::ImportDrumMap { track, path },
                }))
            }),
        })
    }

    /// Where to write the map as JSON.
    fn export_request(model: &Session) -> Option<HostRequest<Action>> {
        let map = model.map_editor()?;
        let name = format!("{}.json", Self::doc(model)?.name().trim());
        Some(HostRequest::ChooseFiles {
            choice: FileChoice::Save {
                title: match map {
                    MapRef::Articulation(_) => "Export the Articulation Map".into(),
                    MapRef::Drum(_) => "Export the Drum Map".into(),
                },
                name,
                filters: vec![("FaderFrame maps".into(), vec!["*.json".into()])],
            },
            commit: Box::new(move |mut paths| {
                let mut path = paths.pop()?;
                if path.extension().is_none() {
                    path.set_extension("json");
                }
                let path = Some(path);
                Some(Action::Maps(match map {
                    MapRef::Articulation(map) => MapOp::SaveArticulationMap { map, path },
                    MapRef::Drum(map) => MapOp::SaveDrumMap { map, path },
                }))
            }),
        })
    }

    /// A button pressed (`under`: where its menu or entry goes).
    pub(crate) fn press_button(
        &mut self,
        b: Button,
        r: Rect,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let under = Point::new(r.x, r.bottom());
        let doc = Self::doc(model);
        match b {
            Button::Map => cx.request(HostRequest::ContextMenu {
                at: under,
                items: Self::map_menu(model),
            }),
            Button::Rename => {
                if let Some(req) = Self::rename_input(model, r) {
                    cx.request(req);
                }
            }
            Button::Add => self.add_row(doc, model, cx),
            Button::AddCombination => {
                if let Some(Doc::Art(m)) = doc
                    && let Some(a) = cells::add_combination(m)
                {
                    self.selected = Some(Row::Combination(m.combinations.len()));
                    cx.emit(a);
                }
            }
            Button::Import => {
                if let Some(req) = Self::import_request(model) {
                    cx.request(req);
                }
            }
            Button::Export => {
                if let Some(req) = Self::export_request(model) {
                    cx.request(req);
                }
            }
            Button::SaveUser => {
                if let Some(map) = model.map_editor() {
                    cx.emit(Action::Maps(match map {
                        MapRef::Articulation(map) => MapOp::SaveArticulationMap { map, path: None },
                        MapRef::Drum(map) => MapOp::SaveDrumMap { map, path: None },
                    }));
                }
            }
        }
    }

    /// A new articulation or sound after the selected one (else last).
    fn add_row(&mut self, doc: Option<Doc<'_>>, _model: &Session, cx: &mut EventCx<'_, Action>) {
        let after = self.selected.and_then(Row::movable);
        let (action, at) = match doc {
            Some(Doc::Art(m)) => (
                cells::add_articulation(m, after),
                after.map_or(m.articulations.len(), |i| i + 1),
            ),
            Some(Doc::Drum(m)) => (
                cells::add_sound(m, after),
                after.map_or(m.sounds.len(), |i| i + 1),
            ),
            None => (None, 0),
        };
        if let (Some(a), Some(doc)) = (action, doc) {
            self.selected = Some(match doc {
                Doc::Art(_) => Row::Articulation(at),
                Doc::Drum(_) => Row::Sound(at),
            });
            cx.emit(a);
        }
    }

    /// Delete the row.
    fn remove_action(doc: Doc<'_>, row: Row) -> Option<Action> {
        match (doc, row) {
            (Doc::Art(m), Row::Articulation(i)) => cells::remove_articulation(m, i),
            (Doc::Art(m), Row::Combination(i)) => cells::remove_combination(m, i),
            (Doc::Drum(m), Row::Sound(i)) => cells::remove_sound(m, i),
            _ => None,
        }
    }

    /// Typing into a cell: the entry and what its text makes.
    pub(crate) fn text_input(
        &self,
        doc: Doc<'_>,
        row: Row,
        col: Col,
        at: Rect,
        model: &Session,
    ) -> Option<HostRequest<Action>> {
        if !matches!(doc.use_of(row, col), Use::Text | Use::Number) {
            return None;
        }
        let initial = match (col, doc.number(row, col)) {
            // The number alone, as typed.
            (Col::Pitch | Col::Output, Some(k)) => text::note_name(k.clamp(0, 127) as u8),
            (Col::Program | Col::Channel, None) | (Col::Output, None) => String::new(),
            (Col::Velocity | Col::Length | Col::Transpose, Some(v)) => v.to_string(),
            _ => doc.text(row, col, None, model),
        };
        let owned = Owned::of(doc);
        Some(HostRequest::TextInput {
            at,
            initial,
            commit: Box::new(move |t| match &owned {
                Owned::Art(m) => cells::art_text(m, row, col, t),
                Owned::Drum(m) => cells::drum_text(m, row, col, t),
            }),
        })
    }

    /// What a click on a cell does that is not typing or dragging: a
    /// switch, a menu, learning.
    pub(crate) fn press_cell(
        &mut self,
        doc: Doc<'_>,
        row: Row,
        col: Col,
        at: Rect,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match doc.use_of(row, col) {
            Use::Toggle => match (doc, row, col) {
                (Doc::Art(m), Row::Articulation(i), Col::Kind) => {
                    if let Some(a) = cells::toggle_kind(m, i) {
                        cx.emit(a);
                    }
                    true
                }
                (Doc::Drum(m), Row::Sound(i), Col::Learn) => {
                    if let Some(a) = Self::learn_action(m, i, model) {
                        cx.emit(a);
                    }
                    true
                }
                _ => false,
            },
            Use::Menu => match cells::cell_menu(doc, row, col, model) {
                Some(items) => {
                    cx.request(HostRequest::ContextMenu {
                        at: Point::new(at.x, at.bottom()),
                        items,
                    });
                    true
                }
                None => false,
            },
            Use::Text | Use::Number => match self.text_input(doc, row, col, at, model) {
                Some(req) => {
                    cx.request(req);
                    true
                }
                None => false,
            },
            Use::Handle | Use::None => false,
        }
    }

    /// Learn the sound's pitch, or stop learning it.
    pub(crate) fn learn_action(m: &DrumMap, i: usize, model: &Session) -> Option<Action> {
        let pitch = m.sounds.get(i)?.pitch;
        Some(Action::Maps(
            if model.map_learning() == Some((m.id, pitch)) {
                MapOp::CancelLearn
            } else {
                MapOp::LearnDrumPitch { map: m.id, pitch }
            },
        ))
    }

    /// A row's menu.
    pub(crate) fn row_menu(
        &self,
        doc: Doc<'_>,
        row: Row,
        model: &Session,
    ) -> Vec<MenuItem<Action>> {
        let opt = |label: &str, a: Option<Action>| match a {
            Some(a) => MenuItem::new(label, a),
            None => MenuItem::disabled(label),
        };
        let mut items = Vec::new();
        match (doc, row) {
            (Doc::Art(m), Row::Articulation(i)) => {
                items.push(opt(
                    "Add Articulation Below",
                    cells::add_articulation(m, Some(i)),
                ));
                items.push(opt("Duplicate", cells::duplicate_articulation(m, i)));
                items.push(
                    opt(
                        "Move Up",
                        i.checked_sub(1).and_then(|to| cells::move_row(doc, i, to)),
                    )
                    .separated(),
                );
                items.push(opt("Move Down", cells::move_row(doc, i, i + 1)));
                if let Some(g) = cells::cell_menu(doc, row, Col::Group, model) {
                    items.push(MenuItem::submenu("Group", g).separated());
                }
                if let Some(a) = m.articulations.get(i) {
                    let kind = |k| {
                        let label = match k {
                            faderframe_project::articulation::ArticulationKind::Attribute => {
                                "Attribute (on a note)"
                            }
                            faderframe_project::articulation::ArticulationKind::Direction => {
                                "Direction (from here on)"
                            }
                        };
                        let action = cells::art_edit(m, |m| m.articulations[i].kind = k);
                        opt(label, action).checked(a.kind == k)
                    };
                    items.push(MenuItem::submenu(
                        "Type",
                        vec![
                            kind(faderframe_project::articulation::ArticulationKind::Attribute),
                            kind(faderframe_project::articulation::ArticulationKind::Direction),
                        ],
                    ));
                }
                if let Some(c) = cells::cell_menu(doc, row, Col::Channel, model) {
                    items.push(MenuItem::submenu("Channel", c));
                }
                items
                    .push(opt("Delete Articulation", cells::remove_articulation(m, i)).separated());
            }
            (Doc::Art(m), Row::Combination(i)) => {
                items.push(MenuItem::submenu(
                    "Articulations",
                    cells::members_menu(m, i),
                ));
                if let Some(c) = cells::cell_menu(doc, row, Col::Channel, model) {
                    items.push(MenuItem::submenu("Channel", c));
                }
                items.push(opt("Delete Combination", cells::remove_combination(m, i)).separated());
            }
            (Doc::Art(m), Row::Combinations) => {
                items.push(opt("Add Combination", cells::add_combination(m)));
            }
            (Doc::Drum(m), Row::Sound(i)) => {
                items.push(opt("Add Sound Below", cells::add_sound(m, Some(i))));
                items.push(opt("Duplicate", cells::duplicate_sound(m, i)));
                items.push(
                    opt(
                        "Move Up",
                        i.checked_sub(1).and_then(|to| cells::move_row(doc, i, to)),
                    )
                    .separated(),
                );
                items.push(opt("Move Down", cells::move_row(doc, i, i + 1)));
                let learning = m
                    .sounds
                    .get(i)
                    .is_some_and(|s| model.map_learning() == Some((m.id, s.pitch)));
                items.push(
                    opt(
                        if learning {
                            "Stop Learning"
                        } else {
                            "Learn Pitch from MIDI Input"
                        },
                        Self::learn_action(m, i, model),
                    )
                    .separated(),
                );
                for (label, col) in [
                    ("Channel", Col::Channel),
                    ("Port", Col::Port),
                    ("Grid", Col::Grid),
                ] {
                    if let Some(c) = cells::cell_menu(doc, row, col, model) {
                        items.push(MenuItem::submenu(label, c));
                    }
                }
                items.push(opt("Delete Sound", cells::remove_sound(m, i)).separated());
            }
            _ => {}
        }
        items
    }

    /// The menu of the table's empty part: add a row.
    fn empty_menu(doc: Doc<'_>) -> Vec<MenuItem<Action>> {
        let opt = |label: &str, a: Option<Action>| match a {
            Some(a) => MenuItem::new(label, a),
            None => MenuItem::disabled(label),
        };
        match doc {
            Doc::Art(m) => vec![
                opt("Add Articulation", cells::add_articulation(m, None)),
                opt("Add Combination", cells::add_combination(m)),
            ],
            Doc::Drum(m) => vec![opt("Add Sound", cells::add_sound(m, None))],
        }
    }

    // --- painting ----------------------------------------------------------------

    fn button(&self, p: &mut dyn Painter, r: Rect, label: &str, accent: bool, off: bool) {
        let th = &self.theme;
        if accent && !off {
            p.fill_rounded(r, 4.0, &th.ui.accent.with_alpha(0.25).into());
            p.stroke_rounded(r, 4.0, 1.0, th.ui.accent);
        } else {
            p.fill_rounded(r, 4.0, &th.ui.surface_alt.into());
            p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        }
        let ink = if off { th.ui.text_faint } else { th.ui.text };
        p.text(
            label,
            r.inset_xy(6.0, 0.0),
            &TextStyle::new(th.fonts.small, ink).center(),
        );
    }

    /// The line under the toolbar.
    fn info_text(model: &Session) -> (String, bool) {
        if let Some((map, pitch)) = model.map_learning() {
            let name = model
                .project()
                .drum_map(map)
                .and_then(|m| m.sound(pitch))
                .map_or("the sound", |s| s.name.as_str());
            return (
                format!("Learning: play the key for {name} on a MIDI input · Esc stops"),
                true,
            );
        }
        let Some(map) = model.map_editor() else {
            return (
                "Choose a map above, or a track's Articulation Map / Drum Map menu → Edit Map…"
                    .into(),
                false,
            );
        };
        let users: Vec<String> = model
            .map_users(map)
            .into_iter()
            .filter_map(|t| model.project().track(t).map(|t| t.name.clone()))
            .collect();
        let kind = match map.kind() {
            MapKind::Articulation => "Articulation map",
            MapKind::Drum => "Drum map",
        };
        let used = if users.is_empty() {
            "used by no track".to_string()
        } else {
            format!("used by {}", users.join(", "))
        };
        let hint = match map.kind() {
            MapKind::Articulation => {
                "one articulation of each group is in effect; a group's first plays before any direction"
            }
            MapKind::Drum => "notes stay on their pitch and play the sound's note and channel",
        };
        (format!("{kind} · {used} · {hint}"), false)
    }

    fn paint_row(
        &self,
        p: &mut dyn Painter,
        doc: Doc<'_>,
        row: Row,
        i: usize,
        size: Size,
        model: &Session,
    ) {
        let th = &self.theme;
        let r = self.row_rect(i, size);
        let dragged_row =
            matches!(self.drag, Some(Drag::Row { to, .. }) if row.movable().is_some() && to == i);
        if row == Row::Combinations {
            p.fill(r, th.ui.surface.with_alpha(0.7));
            p.hline(r.x, r.right(), r.y + 0.5, th.ui.border);
            let text = match doc {
                Doc::Art(m) if m.groups().len() < 2 => {
                    "Combinations · articulations of two groups sending something of their own (give articulations groups first)"
                }
                _ => {
                    "Combinations · articulations of different groups sending something of their own instead of what each sends"
                }
            };
            p.text(
                text,
                r.inset_xy(10.0, 0.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).bold(),
            );
            return;
        }
        if self.selected == Some(row) || dragged_row {
            p.fill(r, th.ui.selection.with_alpha(0.35));
        } else if self.hover == Some(row) {
            p.fill(r, th.ui.text.with_alpha(0.06));
        } else if i % 2 == 1 {
            p.fill(r, th.ui.text.with_alpha(0.025));
        }
        let learning = match (doc, row) {
            (Doc::Drum(m), Row::Sound(k)) => m
                .sounds
                .get(k)
                .is_some_and(|s| model.map_learning() == Some((m.id, s.pitch))),
            _ => false,
        };
        let held = model.held_midi_keys();
        for (col, _, _) in doc.columns() {
            let col = match (row, *col) {
                (Row::Combination(_), Col::Name) => Col::Members,
                (Row::Combination(_), Col::Symbol | Col::Group | Col::Kind) => continue,
                (_, c) => c,
            };
            let c = self.cell(doc, Some(row), col, r);
            if c.right() < 0.0 || c.x > size.w {
                continue;
            }
            match col {
                Col::Handle => {
                    if row.movable().is_some() {
                        let g = c.centered(8.0, 10.0);
                        for k in 0..3 {
                            let y = g.y + 1.0 + k as f32 * 4.0;
                            p.hline(g.x, g.right(), y, th.ui.text_faint);
                        }
                    }
                    continue;
                }
                Col::Learn => {
                    let chip = c.inset_xy(6.0, 4.0);
                    if learning {
                        p.fill_rounded(chip, 4.0, &th.arranger.record.with_alpha(0.8).into());
                    }
                    p.stroke_rounded(chip, 4.0, 1.0, th.ui.border);
                    p.text(
                        if learning { "Play a key…" } else { "Learn" },
                        chip,
                        &TextStyle::new(
                            th.fonts.small,
                            if learning { th.ui.text } else { th.ui.text_dim },
                        )
                        .center(),
                    );
                    continue;
                }
                _ => {}
            }
            let dragged = match self.drag {
                Some(Drag::Value {
                    row: dr,
                    col: dc,
                    value,
                    ..
                }) if dr == row && dc == col => Some(value),
                _ => None,
            };
            let text = doc.text(row, col, dragged, model);
            if text.is_empty() {
                continue;
            }
            let unset = text == "–";
            let ink = if unset {
                th.ui.text_faint
            } else if col == Col::Kind || col == Col::Members {
                th.ui.accent
            } else {
                th.ui.text
            };
            let style = TextStyle::new(th.fonts.small, ink);
            let style = match col {
                Col::Keyswitches | Col::Controls | Col::Pitch | Col::Output => {
                    style.family(FontFamily::Mono)
                }
                Col::Program
                | Col::Channel
                | Col::Transpose
                | Col::Velocity
                | Col::Length
                | Col::Group => style.right(),
                _ => style,
            };
            // A drum sound's key held on a MIDI input: lit.
            if col == Col::Pitch
                && let Some(k) = doc.number(row, Col::Pitch)
                && held & (1u128 << (k as u32 & 127)) != 0
            {
                p.fill_rounded(c.inset(2.0), 3.0, &th.ui.accent.with_alpha(0.35).into());
            }
            if doc.use_of(row, col) == Use::Menu && col != Col::Members {
                p.text(
                    "▾",
                    Rect::new(c.right() - 14.0, c.y, 12.0, c.h),
                    &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
                );
                p.text(&text, Rect::new(c.x + 6.0, c.y, c.w - 22.0, c.h), &style);
            } else {
                p.text(&text, c.inset_xy(6.0, 0.0), &style);
            }
        }
    }
}

/// The map shown, owned (for a text entry's answer).
enum Owned {
    Art(ArticulationMap),
    Drum(DrumMap),
}

impl Owned {
    fn of(doc: Doc<'_>) -> Self {
        match doc {
            Doc::Art(m) => Owned::Art(m.clone()),
            Doc::Drum(m) => Owned::Drum(m.clone()),
        }
    }

    fn doc(&self) -> Doc<'_> {
        match self {
            Owned::Art(m) => Doc::Art(m),
            Owned::Drum(m) => Doc::Drum(m),
        }
    }
}

impl faderframe_ui_canvas::CanvasView<Session, Action> for MapsView {
    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        self.access_nodes(size, model)
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Map Editor".into())
    }

    fn access_press(
        &mut self,
        id: u64,
        press: faderframe_ui_canvas::AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.press(id, press, size, model, cx)
    }

    fn access_news(&mut self, model: &Session) -> Vec<String> {
        self.news(model)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.theme = theme.clone();
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        let shown = model.map_editor();
        if shown != self.shown {
            self.shown = shown;
            self.selected = None;
            self.drag = None;
            self.scroll = 0.0;
            self.scroll_x = 0.0;
        }
        let doc = Self::doc(model);
        if let (Some(d), Some(r)) = (doc, self.selected)
            && !d.has(r)
        {
            self.selected = None;
        }
        self.clamp(doc, size);
        // The toolbar.
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        if Self::titled(size) {
            p.text(
                "Map Editor",
                Rect::new(12.0, 0.0, 92.0, HEADER_H),
                &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
            );
        }
        for (b, r) in Self::buttons(size, model) {
            let label = Self::button_label(b, model);
            let accent = matches!(b, Button::Add | Button::AddCombination);
            self.button(p, r, &label, accent, Self::button_off(b, model));
        }
        // Who uses it, or learning.
        let info = Rect::new(0.0, HEADER_H, size.w, INFO_H);
        let (text, learning) = Self::info_text(model);
        p.fill(info, th.ui.surface.with_alpha(0.5));
        p.text(
            &text,
            info.inset_xy(12.0, 0.0),
            &TextStyle::new(
                th.fonts.small,
                if learning {
                    th.arranger.record
                } else {
                    th.ui.text_dim
                },
            ),
        );
        let Some(doc) = doc else {
            p.text(
                "No map: choose one with the button above (New makes one)",
                Self::table(size).inset(16.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
            return;
        };
        // The column names.
        let names = Rect::new(0.0, HEADER_H + INFO_H, size.w, COLUMNS_H);
        p.fill(names, th.ui.surface.with_alpha(0.6));
        p.hline(0.0, size.w, names.bottom() - 0.5, th.ui.border);
        p.push_clip(names);
        for (col, label, _) in doc.columns() {
            let r = self.cell(doc, None, *col, names);
            if !label.is_empty() {
                let style = TextStyle::new(th.fonts.small, th.ui.text_dim).bold();
                p.text(label, r.inset_xy(6.0, 0.0), &style);
            }
            p.vline(
                r.right() - 0.5,
                names.y + 5.0,
                names.bottom() - 5.0,
                th.ui.border,
            );
        }
        p.pop_clip();
        let body = Self::body(size);
        let rows = self.rows(doc);
        p.push_clip(body);
        let first = (self.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (((self.scroll + body.h) / ROW_H).ceil() as usize).min(rows.len());
        for (i, row) in rows.iter().enumerate().take(last).skip(first) {
            self.paint_row(p, doc, *row, i, size, model);
        }
        // Where a dragged row goes.
        if let Some(Drag::Row { to, .. }) = self.drag {
            let r = self.row_rect(to, size);
            p.stroke_rounded(r.inset(1.0), 3.0, 1.5, th.ui.accent);
        }
        let empty = match doc {
            Doc::Art(m) if m.articulations.is_empty() => {
                Some("No articulations: “+ Articulation” adds one")
            }
            Doc::Drum(m) if m.sounds.is_empty() => Some("No sounds: “+ Sound” adds one"),
            _ => None,
        };
        if let Some(t) = empty {
            let r = Rect::new(body.x, body.y + rows.len() as f32 * ROW_H, body.w, 40.0);
            p.text(
                t,
                r,
                &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
            );
        }
        p.pop_clip();
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let doc = Self::doc(model);
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                clicks,
                ..
            } => {
                for (b, r) in Self::buttons(size, model) {
                    if r.contains(pos) {
                        if !Self::button_off(b, model) {
                            self.press_button(b, r, model, cx);
                        }
                        return true;
                    }
                }
                let Some(doc) = doc else {
                    return false;
                };
                let rows = self.rows(doc);
                let Some(i) = self.row_at(pos, size, rows.len()) else {
                    if Self::body(size).contains(pos) {
                        self.selected = None;
                        cx.redraw();
                        return true;
                    }
                    return false;
                };
                cx.request(HostRequest::GrabFocus);
                let row = rows[i];
                self.selected = (row != Row::Combinations).then_some(row);
                cx.redraw();
                let Some(col) = self.column_at(doc, row, pos.x) else {
                    return true;
                };
                let cell = self.cell(doc, Some(row), col, self.row_rect(i, size));
                match doc.use_of(row, col) {
                    Use::Handle => {
                        if let Some(from) = row.movable() {
                            self.drag = Some(Drag::Row { from, to: from });
                        }
                    }
                    Use::Number if clicks < 2 => {
                        self.drag = Some(Drag::Value {
                            row,
                            col,
                            y0: pos.y,
                            base: doc.start(row, col),
                            value: doc.start(row, col),
                        });
                    }
                    Use::Text | Use::Number if clicks < 2 => {}
                    _ => {
                        self.press_cell(doc, row, col, cell, model, cx);
                    }
                }
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let Some(doc) = doc else {
                    return false;
                };
                let rows = self.rows(doc);
                match self.row_at(pos, size, rows.len()) {
                    Some(i) => {
                        let row = rows[i];
                        self.selected = (row != Row::Combinations).then_some(row);
                        cx.redraw();
                        cx.request(HostRequest::ContextMenu {
                            at: pos,
                            items: self.row_menu(doc, row, model),
                        });
                        true
                    }
                    None if Self::body(size).contains(pos) => {
                        cx.request(HostRequest::ContextMenu {
                            at: pos,
                            items: Self::empty_menu(doc),
                        });
                        true
                    }
                    None => false,
                }
            }
            ViewEvent::PointerMove { pos, dragging, .. } => {
                if dragging && let Some(d) = self.drag.as_mut() {
                    match d {
                        Drag::Row { to, .. } => {
                            let n = doc.map_or(0, Doc::movable);
                            let b = Self::body(size);
                            let i = ((pos.y - b.y + self.scroll) / ROW_H).floor().max(0.0) as usize;
                            let i = i.min(n.saturating_sub(1));
                            if i != *to {
                                *to = i;
                                cx.redraw();
                            }
                            cx.set_cursor(Cursor::Grabbing);
                        }
                        Drag::Value {
                            col,
                            y0,
                            base,
                            value,
                            ..
                        } => {
                            let (lo, hi) = cells::range(*col);
                            let steps = ((*y0 - pos.y) / DRAG_PX).round() as i32;
                            let v = (*base + steps * cells::drag_step(*col)).clamp(lo, hi);
                            if v != *value {
                                *value = v;
                                cx.redraw();
                            }
                            cx.set_cursor(Cursor::ResizeVertical);
                        }
                    }
                    return true;
                }
                let Some(doc) = doc else {
                    return false;
                };
                let rows = self.rows(doc);
                let at = self.row_at(pos, size, rows.len());
                let hover = at.map(|i| rows[i]).filter(|r| *r != Row::Combinations);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                let cursor = match hover.and_then(|r| Some((r, self.column_at(doc, r, pos.x)?))) {
                    Some((r, c)) => match doc.use_of(r, c) {
                        Use::Handle => Cursor::Grab,
                        Use::Number => Cursor::ResizeVertical,
                        Use::Menu | Use::Toggle => Cursor::Pointer,
                        Use::Text => Cursor::Text,
                        Use::None => Cursor::Default,
                    },
                    None => Cursor::Default,
                };
                cx.set_cursor(cursor);
                false
            }
            ViewEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => {
                let Some(d) = self.drag.take() else {
                    return false;
                };
                cx.redraw();
                let Some(doc) = doc else {
                    return true;
                };
                match d {
                    Drag::Row { from, to } => {
                        if let Some(a) = cells::move_row(doc, from, to) {
                            self.selected = self.selected.map(|r| r.with_index(to));
                            cx.emit(a);
                        }
                    }
                    Drag::Value {
                        row,
                        col,
                        base,
                        value,
                        ..
                    } => {
                        if value != base
                            && let Some(a) = doc.with_number(row, col, Some(value))
                        {
                            cx.emit(a);
                        }
                    }
                }
                true
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                dx,
                dy,
                modifiers,
                precise,
                ..
            } => {
                let unit = if precise { 1.0 } else { ROW_H * 3.0 };
                if modifiers.shift {
                    self.scroll_x += dy * unit;
                } else {
                    self.scroll += dy * unit;
                    self.scroll_x += dx * unit;
                }
                self.clamp(doc, size);
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => self.key(key, modifiers, size, model, cx),
            ViewEvent::FocusLost => {
                if self.drag.take().is_some() {
                    cx.redraw();
                }
                false
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        for (b, r) in Self::buttons(size, model) {
            if r.contains(pos) {
                return Some(
                    match b {
                        Button::Map => "The map edited: choose another, make a new one, delete it",
                        Button::Rename => "Rename the map",
                        Button::Add => "Add a row after the selected one (+)",
                        Button::AddCombination => {
                            "Add a combination: articulations of different groups sending something of their own"
                        }
                        Button::Import => {
                            "Read a map file into the project: FaderFrame's JSON or a Cubase expression map"
                        }
                        Button::Export => "Write the map as a JSON file",
                        Button::SaveUser => "Save the map among your own maps (offered in every project)",
                    }
                    .into(),
                );
            }
        }
        let doc = Self::doc(model)?;
        let rows = self.rows(doc);
        let row = rows[self.row_at(pos, size, rows.len())?];
        let col = self.column_at(doc, row, pos.x)?;
        Some(
            match (col, doc.use_of(row, col)) {
                (_, Use::Handle) => "Drag to move the row (Alt+Up/Down)",
                (Col::Kind, _) => {
                    "Attribute: set on a note, for that note only · Direction: set in the clip's lane, from there on · click to switch"
                }
                (Col::Group, _) => {
                    "One articulation of each group is in effect at a time; groups combine"
                }
                (Col::Keyswitches, _) => {
                    "Keys pressed before the notes: “C0, D0@90” (a velocity after @) · double-click to type"
                }
                (Col::Controls, _) => {
                    "Controller values sent to switch: “1=64, 68=127” · double-click to type"
                }
                (Col::Members, _) => "The articulations combined, one of each group · click to choose",
                (Col::Pitch, _) => {
                    "The key its notes are written on (another sound there swaps) · drag or double-click to type"
                }
                (Col::Output, _) => "The note it plays (– : its pitch) · drag or double-click to type",
                (Col::Learn, _) => "Learn the pitch: the next key played on a MIDI input",
                (Col::Port, _) => "An external MIDI output it goes to instead of the track's",
                (Col::Grid, _) => "The grid drawing on its row keeps to",
                (_, Use::Number) => "Drag up or down, or double-click to type",
                (_, Use::Text) => "Double-click to type",
                (_, Use::Menu) => "Click to choose",
                _ => return None,
            }
            .into(),
        )
    }

    fn wants_frames(&self, _model: &Session) -> bool {
        self.drag.is_some()
    }

    fn min_size(&self) -> Size {
        Size::new(520.0, 140.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        let doc = Self::doc(model)?;
        let body = Self::body(size);
        Some(match axis {
            ScrollAxis::Vertical => ScrollInfo {
                content: doc.rows().len() as f32 * ROW_H,
                viewport: body.h,
                offset: self.scroll,
                start: body.y,
                end: 0.0,
            },
            ScrollAxis::Horizontal => ScrollInfo {
                content: Self::content_w(doc),
                viewport: size.w,
                offset: self.scroll_x,
                start: 0.0,
                end: 0.0,
            },
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        match axis {
            ScrollAxis::Vertical => self.scroll = offset.max(0.0),
            ScrollAxis::Horizontal => self.scroll_x = offset.max(0.0),
        }
    }
}

impl MapsView {
    /// The view's own keys: the arrows choose a row (Alt moves it), Delete
    /// deletes it, + adds one after it, F2 or Enter types its name, Escape
    /// stops learning.
    fn key(
        &mut self,
        key: Key,
        modifiers: faderframe_ui_canvas::Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if key == Key::Escape {
            if self.drag.take().is_some() {
                cx.redraw();
                return true;
            }
            if model.map_learning().is_some() {
                cx.emit(Action::Maps(MapOp::CancelLearn));
                return true;
            }
            return false;
        }
        let Some(doc) = Self::doc(model) else {
            return false;
        };
        let rows: Vec<Row> = doc
            .rows()
            .into_iter()
            .filter(|r| *r != Row::Combinations)
            .collect();
        let at = self
            .selected
            .and_then(|s| rows.iter().position(|r| *r == s));
        let place = |r: Row| doc.rows().iter().position(|x| *x == r).unwrap_or(0);
        match key {
            Key::Up | Key::Down if modifiers.alt => {
                let Some(from) = self.selected.and_then(Row::movable) else {
                    return false;
                };
                let to = if key == Key::Up {
                    from.checked_sub(1)
                } else {
                    Some(from + 1)
                };
                if let Some(a) = to.and_then(|to| cells::move_row(doc, from, to)) {
                    let to = to.unwrap_or(from);
                    self.selected = self.selected.map(|r| r.with_index(to));
                    self.keep_in_view(to, size);
                    cx.emit(a);
                }
                true
            }
            Key::Up | Key::Down if !rows.is_empty() => {
                let i = match (key, at) {
                    (Key::Up, Some(i)) => i.saturating_sub(1),
                    (Key::Down, Some(i)) => (i + 1).min(rows.len() - 1),
                    _ => 0,
                };
                self.selected = Some(rows[i]);
                self.keep_in_view(place(rows[i]), size);
                cx.redraw();
                true
            }
            Key::Delete | Key::Backspace => {
                let Some(row) = self.selected else {
                    return false;
                };
                if let Some(a) = Self::remove_action(doc, row) {
                    cx.emit(a);
                }
                true
            }
            Key::Char('+') => {
                self.add_row(Some(doc), model, cx);
                true
            }
            Key::F(2) | Key::Enter => {
                let Some(row) = self.selected.filter(|r| r.movable().is_some()) else {
                    return false;
                };
                let i = place(row);
                let cell = self.cell(doc, Some(row), Col::Name, self.row_rect(i, size));
                if let Some(req) = self.text_input(doc, row, Col::Name, cell, model) {
                    cx.request(req);
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;

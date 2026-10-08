//! The MIDI event list: every event of the edited MIDI clip in time order
//! — notes, control changes, pitch bend, channel pressure, polyphonic key
//! pressure, program changes, per-note expression points and SysEx — one
//! row each, every property in a column: position, channel, key or
//! controller number, velocity or value, length, end, release velocity,
//! mute, and a SysEx message's bytes. A double-click on a field types it, a
//! vertical drag on a number changes it (committed once on release), a
//! click in M mutes a note; rows are added from "+ Add" or a row's menu,
//! deleted with Delete. Note rows share the session's note selection (the
//! piano roll shows the same notes picked); the row at the playhead is
//! marked and kept in view while playing.

#![forbid(unsafe_code)]

use faderframe_core::{ClipId, NoteId};
use faderframe_project::{ExpressionKind, MidiController};
use faderframe_session::editing::{format_position, parse_position};
use faderframe_session::midi_events::{
    EventField, EventKind, EventRef, EventRow, EventValue, NewEvent, parse_sysex,
};
use faderframe_session::{Action, NoteOp, SelectMode, Session, TransportAction};
use faderframe_timeline::{MusicalTime, TICKS_PER_QUARTER};
use faderframe_ui_canvas::{
    Cursor, EventCx, FontFamily, HostRequest, Key, MenuItem, Painter, Point, PointerButton, Rect,
    ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 34.0;
const COLUMNS_H: f32 = 22.0;
const ROW_H: f32 = 22.0;
/// Pixels of vertical drag per step of a value.
const DRAG_PX: f32 = 4.0;

/// A column of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// A click moves the playhead to the event; the playhead's row is
    /// marked here.
    Locate,
    Position,
    Type,
    Channel,
    /// The key (notes, poly pressure, expression points) or controller
    /// number (control changes).
    Data1,
    /// The velocity or value.
    Data2,
    Length,
    End,
    /// A note's release velocity.
    Release,
    /// A note's mute.
    Mute,
    /// The controller's or program's name; a SysEx message's bytes.
    Info,
}

const COLUMNS: [(Column, &str, f32); 11] = [
    (Column::Locate, "", 26.0),
    (Column::Position, "Position", 108.0),
    (Column::Type, "Type", 118.0),
    (Column::Channel, "Ch", 36.0),
    (Column::Data1, "Key / No.", 70.0),
    (Column::Data2, "Value", 76.0),
    (Column::Length, "Length", 76.0),
    (Column::End, "End", 108.0),
    (Column::Release, "Off Vel.", 60.0),
    (Column::Mute, "M", 26.0),
    (Column::Info, "", 0.0),
];

impl Column {
    fn field(self) -> Option<EventField> {
        Some(match self {
            Column::Position => EventField::Position,
            Column::Channel => EventField::Channel,
            Column::Data1 => EventField::Data1,
            Column::Data2 => EventField::Data2,
            Column::Length => EventField::Length,
            Column::End => EventField::End,
            Column::Release => EventField::Release,
            Column::Mute => EventField::Muted,
            Column::Info => EventField::Bytes,
            Column::Locate | Column::Type => return None,
        })
    }

    /// Changed by dragging.
    fn numeric(self) -> bool {
        matches!(
            self,
            Column::Channel | Column::Data1 | Column::Data2 | Column::Release
        )
    }
}

/// Which kinds of events show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Filter {
    notes: bool,
    controllers: bool,
    expression: bool,
    sysex: bool,
}

impl Filter {
    fn shows(self, kind: EventKind) -> bool {
        match kind {
            EventKind::Note => self.notes,
            EventKind::Controller(_) => self.controllers,
            EventKind::Expression(_) => self.expression,
            EventKind::Sysex => self.sysex,
        }
    }
}

/// A number being dragged: shown at once, committed on release.
#[derive(Clone, Copy, Debug)]
struct ValueDrag {
    event: EventRef,
    column: Column,
    kind: EventKind,
    y0: f32,
    base: f64,
    value: f64,
}

/// The header's controls.
struct Header {
    notes: Rect,
    controllers: Rect,
    expression: Rect,
    sysex: Rect,
    add: Rect,
}

pub struct EventsView {
    theme: Theme,
    scroll: f32,
    hover: Option<usize>,
    /// Selected controller values, expression points and SysEx (note rows
    /// follow the session's note selection).
    selected: Vec<EventRef>,
    /// Where Shift-clicks extend from.
    anchor: Option<usize>,
    filter: Filter,
    drag: Option<ValueDrag>,
    /// The playhead's row when last painted (kept in view while playing).
    current: Option<usize>,
    /// The clip last shown (its own selection goes with it).
    shown: Option<ClipId>,
}

impl EventsView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            hover: None,
            selected: Vec::new(),
            anchor: None,
            filter: Filter {
                notes: true,
                controllers: true,
                expression: true,
                sysex: true,
            },
            drag: None,
            current: None,
            shown: None,
        }
    }

    /// The clip listed: the MIDI clip in the editor, else the first
    /// selected MIDI clip.
    pub fn clip(model: &Session) -> Option<ClipId> {
        let midi = |c: &ClipId| {
            model
                .project()
                .clip(*c)
                .is_some_and(|c| c.as_midi().is_some())
        };
        model
            .editor_clip()
            .filter(midi)
            .or_else(|| model.selection.clips.iter().copied().find(midi))
    }

    /// The rows shown.
    pub fn rows(&self, model: &Session) -> Vec<EventRow> {
        let Some(clip) = Self::clip(model) else {
            return Vec::new();
        };
        model
            .midi_events(clip)
            .into_iter()
            .filter(|r| self.filter.shows(r.kind))
            .collect()
    }

    fn header(size: Size) -> Header {
        let y = 6.0;
        let h = HEADER_H - 12.0;
        let add = Rect::new(size.w - 12.0 - 58.0, y, 58.0, h);
        let sysex = Rect::new(add.x - 10.0 - 54.0, y, 54.0, h);
        let expression = Rect::new(sysex.x - 4.0 - 80.0, y, 80.0, h);
        let controllers = Rect::new(expression.x - 4.0 - 84.0, y, 84.0, h);
        let notes = Rect::new(controllers.x - 4.0 - 54.0, y, 54.0, h);
        Header {
            notes,
            controllers,
            expression,
            sysex,
            add,
        }
    }

    fn list(size: Size) -> Rect {
        let top = HEADER_H + COLUMNS_H;
        Rect::new(0.0, top, size.w, (size.h - top).max(0.0))
    }

    pub fn row_rect(&self, i: usize, size: Size) -> Rect {
        let list = Self::list(size);
        Rect::new(
            list.x,
            list.y + i as f32 * ROW_H - self.scroll,
            list.w,
            ROW_H,
        )
    }

    /// Column `col` of the row (or header) `r`.
    pub fn cell(col: Column, r: Rect) -> Rect {
        let mut x = r.x;
        for (c, _, w) in COLUMNS {
            let w = if c == Column::Info {
                (r.x + r.w - x).max(0.0)
            } else {
                w
            };
            if c == col {
                return Rect::new(x, r.y, w, r.h);
            }
            x += w;
        }
        Rect::new(x, r.y, 0.0, r.h)
    }

    fn column_at(x: f32) -> Column {
        let mut left = 0.0;
        for (c, _, w) in COLUMNS {
            if c == Column::Info || x < left + w {
                return c;
            }
            left += w;
        }
        Column::Info
    }

    /// The row under `pos`.
    pub fn row_at(&self, pos: Point, size: Size, count: usize) -> Option<usize> {
        let list = Self::list(size);
        if !list.contains(pos) {
            return None;
        }
        let i = ((pos.y - list.y + self.scroll) / ROW_H).floor();
        (i >= 0.0 && (i as usize) < count).then_some(i as usize)
    }

    fn clamp(&mut self, count: usize, size: Size) {
        let max = (count as f32 * ROW_H - Self::list(size).h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn is_selected(&self, row: &EventRow, model: &Session) -> bool {
        match row.event {
            EventRef::Note(id) => model.selection.notes.contains(&id),
            e => self.selected.contains(&e),
        }
    }

    /// The selected rows' events (in list order).
    fn selection(&self, rows: &[EventRow], model: &Session) -> Vec<EventRef> {
        rows.iter()
            .filter(|r| self.is_selected(r, model))
            .map(|r| r.event)
            .collect()
    }

    /// Select exactly `events`: the notes through the session (so the
    /// piano roll shows them), the rest here.
    fn select(&mut self, events: &[EventRef], cx: &mut EventCx<'_, Action>) {
        let notes: Vec<NoteId> = events
            .iter()
            .filter_map(|e| match e {
                EventRef::Note(id) => Some(*id),
                _ => None,
            })
            .collect();
        self.selected = events
            .iter()
            .copied()
            .filter(|e| !matches!(e, EventRef::Note(_)))
            .collect();
        cx.emit(Action::SelectNotes {
            notes,
            mode: SelectMode::Replace,
        });
        cx.redraw();
    }

    /// The row at the playhead: the last event at or before it, while the
    /// playhead is inside the clip.
    fn playhead_row(rows: &[EventRow], clip: ClipId, model: &Session) -> Option<usize> {
        let c = model.project().clip(clip)?;
        let at = model.playhead();
        if at < c.start || at > c.start + c.as_midi()?.length {
            return None;
        }
        rows.iter().rposition(|r| r.at <= at)
    }

    /// Where a new event goes: at the first selected row, else the playhead
    /// inside the clip, else the clip's start; on that row's channel (else
    /// the first event's); and the note a new expression point is for (the
    /// selected one, else the one sounding there).
    fn new_event_place(
        &self,
        rows: &[EventRow],
        clip: ClipId,
        model: &Session,
    ) -> (MusicalTime, u8, Option<(NoteId, u8)>) {
        let picked = rows.iter().find(|r| self.is_selected(r, model));
        let channel = picked
            .or_else(|| rows.iter().find(|r| r.channel.is_some()))
            .and_then(|r| r.channel)
            .unwrap_or(0);
        let at = match picked {
            Some(r) => r.at,
            None => model.project().clip(clip).map_or(MusicalTime::ZERO, |c| {
                let end = c.start + c.as_midi().map_or(MusicalTime::ZERO, |m| m.length);
                let p = model.playhead();
                if p >= c.start && p < end { p } else { c.start }
            }),
        };
        let note_of = |r: &EventRow| match r.event {
            EventRef::Note(id) | EventRef::Expression { note: id, .. } => {
                Some((id, r.data1.unwrap_or(60) as u8))
            }
            _ => None,
        };
        let note = picked.and_then(note_of).or_else(|| {
            rows.iter()
                .filter(|r| r.kind == EventKind::Note)
                .find(|r| r.at <= at && r.length.is_some_and(|l| r.at + l > at))
                .and_then(note_of)
        });
        (at, channel, note)
    }

    /// The dragged value of this cell, if it is being dragged.
    fn dragged(&self, row: &EventRow, col: Column) -> Option<f64> {
        self.drag
            .filter(|d| d.event == row.event && d.column == col)
            .map(|d| d.value)
    }

    /// The text of a cell (with a dragged value in place).
    fn cell_text(&self, row: &EventRow, col: Column, model: &Session) -> String {
        let dragged = self.dragged(row, col).map(|v| v.round() as i64);
        let number = |stored: Option<u16>| dragged.or(stored.map(i64::from));
        let position = |at: MusicalTime| {
            let p = model.project();
            let samples = p.timeline.to_samples(at, f64::from(p.sample_rate.max(1)));
            format_position(p, samples, model.editor.main_counter)
        };
        match col {
            Column::Locate | Column::Mute => String::new(),
            Column::Position => position(row.at),
            Column::Type => kind_label(row.kind),
            Column::Channel => dragged
                .or(row.channel.map(i64::from))
                .map_or_else(String::new, |c| (c + 1).to_string()),
            Column::Data1 => match row.kind {
                EventKind::Note
                | EventKind::Expression(_)
                | EventKind::Controller(MidiController::PolyPressure { .. }) => {
                    number(row.data1).map_or_else(String::new, |k| note_name(k.clamp(0, 127) as u8))
                }
                EventKind::Controller(MidiController::Cc { .. }) => {
                    number(row.data1).map_or_else(String::new, |n| n.to_string())
                }
                _ => String::new(),
            },
            Column::Data2 => match row.kind {
                EventKind::Controller(MidiController::PitchBend) => {
                    number(row.data2).map_or_else(String::new, |v| bend_text(v - 8192))
                }
                EventKind::Controller(MidiController::Program) => {
                    number(row.data2).map_or_else(String::new, |v| (v + 1).to_string())
                }
                EventKind::Expression(kind) => self
                    .dragged(row, col)
                    .map(|v| v as f32)
                    .or(row.amount)
                    .map_or_else(String::new, |v| kind.format(v)),
                EventKind::Sysex => format!("{} bytes", row.bytes.len()),
                _ => number(row.data2).map_or_else(String::new, |v| v.to_string()),
            },
            Column::Length => row.length.map_or_else(String::new, length_text),
            Column::End => row
                .length
                .map_or_else(String::new, |l| position(row.at + l)),
            Column::Release => match (row.kind, dragged.or(row.release.map(i64::from))) {
                (EventKind::Note, Some(v)) => v.to_string(),
                (EventKind::Note, None) => "–".into(),
                _ => String::new(),
            },
            Column::Info => match row.kind {
                EventKind::Controller(MidiController::Cc { number }) => {
                    // The name of the number being dragged.
                    let number = self
                        .dragged(row, Column::Data1)
                        .map_or(number, |v| v.round().clamp(0.0, 127.0) as u8);
                    let label = MidiController::Cc { number }.label();
                    if label.starts_with("CC ") {
                        String::new()
                    } else {
                        label
                    }
                }
                EventKind::Controller(MidiController::Program) => {
                    let program = number(row.data2).unwrap_or(0).clamp(0, 127) as u8;
                    faderframe_midi::gm::program_name(row.channel.unwrap_or(0), program)
                        .map_or_else(String::new, |n| format!("GM: {n}"))
                }
                EventKind::Sysex => hex(&row.bytes),
                _ => String::new(),
            },
        }
    }

    /// The range of a dragged number (stored values).
    fn range(kind: EventKind, col: Column) -> (f64, f64) {
        match (col, kind) {
            (Column::Channel, _) => (0.0, 15.0),
            (Column::Data2, EventKind::Note) => (1.0, 127.0),
            (Column::Data2, EventKind::Controller(c)) => (0.0, f64::from(c.max())),
            (Column::Data2, EventKind::Expression(k)) => {
                let (lo, hi) = k.range();
                (f64::from(lo), f64::from(hi))
            }
            _ => (0.0, 127.0),
        }
    }

    /// A dragged number's change per [`DRAG_PX`].
    fn drag_step(kind: EventKind, col: Column) -> f64 {
        match (col, kind) {
            (Column::Data2, EventKind::Controller(MidiController::PitchBend)) => 64.0,
            (Column::Data2, EventKind::Expression(k)) => match k {
                ExpressionKind::Pitch => 0.05,
                ExpressionKind::Volume => 0.5,
                _ => 0.01,
            },
            _ => 1.0,
        }
    }

    /// Where a drag starts from.
    fn stored(row: &EventRow, col: Column) -> Option<f64> {
        match (col, row.kind) {
            (Column::Channel, _) => row.channel.map(f64::from),
            (Column::Data1, _) => row.data1.map(f64::from),
            (Column::Data2, EventKind::Expression(_)) => row.amount.map(f64::from),
            (Column::Data2, _) => row.data2.map(f64::from),
            // A release velocity not set starts from the usual 64.
            (Column::Release, EventKind::Note) => Some(f64::from(row.release.unwrap_or(64))),
            _ => None,
        }
    }

    /// The value a drag ends with.
    fn drag_value(kind: EventKind, v: f64) -> EventValue {
        match kind {
            EventKind::Expression(_) => EventValue::Amount(v as f32),
            _ => EventValue::Number(v.round() as i64),
        }
    }

    /// The field can change for this row.
    pub fn editable(row: &EventRow, col: Column) -> bool {
        use EventKind as K;
        use MidiController as C;
        matches!(
            (col, row.kind),
            (Column::Position, _)
                | (Column::Channel, K::Note | K::Controller(_))
                | (
                    Column::Data1,
                    K::Note | K::Controller(C::Cc { .. } | C::PolyPressure { .. })
                )
                | (Column::Data2, K::Note | K::Controller(_) | K::Expression(_))
                | (
                    Column::Length | Column::End | Column::Release | Column::Mute,
                    K::Note
                )
                | (Column::Info, K::Sysex)
        )
    }

    /// Typing into a cell: the action its text makes.
    fn text_input(
        &self,
        row: &EventRow,
        col: Column,
        at: Rect,
        model: &Session,
    ) -> Option<HostRequest<Action>> {
        let clip = Self::clip(model)?;
        let field = col.field()?;
        let event = row.event;
        let kind = row.kind;
        let p = model.project();
        let timeline = p.timeline.clone();
        let rate = p.sample_rate;
        let tc = p.timecode.unwrap_or_default();
        let unit = model.editor.main_counter;
        let initial = match col {
            Column::Release if row.release.is_none() => String::new(),
            Column::Data2 if matches!(kind, EventKind::Expression(_)) => {
                row.amount.map_or_else(String::new, |v| format!("{v:.2}"))
            }
            _ => self.cell_text(row, col, model),
        };
        let commit = move |text: &str| -> Option<Action> {
            let int = |t: &str| t.trim().replace('−', "-").parse::<i64>().ok();
            let value = match col {
                Column::Position | Column::End => {
                    EventValue::Time(parse_position(text, unit, &timeline, rate, tc)?)
                }
                Column::Length => EventValue::Time(parse_length(text)?),
                Column::Channel => {
                    let c = int(text)?;
                    (1..=16).contains(&c).then_some(EventValue::Number(c - 1))?
                }
                Column::Data1 => EventValue::Number(match kind {
                    EventKind::Controller(MidiController::Cc { .. }) => {
                        int(text).filter(|n| (0..=127).contains(n))?
                    }
                    _ => i64::from(parse_key(text)?),
                }),
                Column::Data2 => match kind {
                    EventKind::Controller(MidiController::PitchBend) => {
                        EventValue::Number(parse_signed(text)?.clamp(-8192, 8191) + 8192)
                    }
                    EventKind::Controller(MidiController::Program) => {
                        let p = int(text)?;
                        (1..=128)
                            .contains(&p)
                            .then_some(EventValue::Number(p - 1))?
                    }
                    EventKind::Expression(k) => EventValue::Amount(parse_amount(k, text)?),
                    _ => EventValue::Number(int(text)?),
                },
                Column::Release => match text.trim() {
                    "" | "-" | "–" | "—" => EventValue::Number(-1),
                    t => EventValue::Number(int(t).filter(|v| (0..=127).contains(v))?),
                },
                Column::Info => EventValue::Bytes(parse_sysex(text)?),
                _ => return None,
            };
            Some(Action::EditMidiEvents {
                clip,
                events: vec![event],
                field,
                value,
            })
        };
        Some(HostRequest::TextInput {
            at,
            initial,
            commit: Box::new(commit),
        })
    }

    /// "+ Add": every kind of event at `at` (an expression point needs a
    /// note: the selected one or the one sounding there).
    fn add_items(
        clip: ClipId,
        at: MusicalTime,
        channel: u8,
        note: Option<(NoteId, u8)>,
    ) -> Vec<MenuItem<Action>> {
        let add = |label: &str, what: NewEvent| {
            MenuItem::new(
                label,
                Action::AddMidiEvent {
                    clip,
                    what,
                    at,
                    channel,
                },
            )
        };
        let cc = |n: u8| {
            let c = MidiController::Cc { number: n };
            let label = c.label();
            let label = if label.starts_with("CC ") {
                label
            } else {
                format!("CC {n} · {label}")
            };
            add(&label, NewEvent::Controller(c))
        };
        let common = [0u8, 32, 1, 2, 7, 10, 11, 64, 66, 67, 71, 74];
        let mut controllers: Vec<MenuItem<Action>> = common.into_iter().map(cc).collect();
        controllers.push(
            MenuItem::submenu(
                "Other",
                (0..4u8)
                    .map(|g| {
                        MenuItem::submenu(
                            format!("CC {}–{}", g * 32, g * 32 + 31),
                            (g * 32..g * 32 + 32).map(cc).collect(),
                        )
                    })
                    .collect(),
            )
            .separated(),
        );
        let key = note.map_or(60, |(_, k)| k);
        let expression = match note {
            Some((id, key)) => MenuItem::submenu(
                format!("Note Expression ({})", note_name(key)),
                ExpressionKind::ALL
                    .into_iter()
                    .map(|kind| add(kind.label(), NewEvent::Expression { note: id, kind }))
                    .collect(),
            ),
            None => MenuItem::disabled("Note Expression (select a note)"),
        };
        vec![
            add("Note", NewEvent::Note),
            MenuItem::submenu("Control Change", controllers),
            add(
                "Program Change",
                NewEvent::Controller(MidiController::Program),
            ),
            add(
                "Pitch Bend",
                NewEvent::Controller(MidiController::PitchBend),
            ),
            add(
                "Channel Pressure",
                NewEvent::Controller(MidiController::ChannelPressure),
            ),
            add(
                &format!("Poly Pressure ({})", note_name(key)),
                NewEvent::Controller(MidiController::PolyPressure { key }),
            ),
            expression,
            add("SysEx", NewEvent::Sysex).separated(),
        ]
    }

    /// A row's menu: the playhead, the channel, mute, release velocity,
    /// add, delete — for the selected rows when it is one of them.
    fn row_menu(
        &mut self,
        at: Point,
        i: usize,
        rows: &[EventRow],
        clip: ClipId,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let row = &rows[i];
        let events = if self.is_selected(row, model) {
            self.selection(rows, model)
        } else {
            self.select(&[row.event], cx);
            self.anchor = Some(i);
            vec![row.event]
        };
        let notes: Vec<NoteId> = events
            .iter()
            .filter_map(|e| match e {
                EventRef::Note(id) => Some(*id),
                _ => None,
            })
            .collect();
        let channelled: Vec<EventRef> = events
            .iter()
            .copied()
            .filter(|e| matches!(e, EventRef::Note(_) | EventRef::Controller { .. }))
            .collect();
        let shared = {
            let mut chans = rows
                .iter()
                .filter(|r| channelled.contains(&r.event))
                .filter_map(|r| r.channel);
            chans.next().filter(|first| chans.all(|c| c == *first))
        };
        let mut items = vec![MenuItem::new(
            "Move Playhead Here",
            Action::Transport(TransportAction::Locate(row.at)),
        )];
        if !channelled.is_empty() {
            items.push(
                MenuItem::submenu(
                    "Channel",
                    (0..16u8)
                        .map(|c| {
                            MenuItem::new(
                                format!("Channel {}", c + 1),
                                Action::EditMidiEvents {
                                    clip,
                                    events: channelled.clone(),
                                    field: EventField::Channel,
                                    value: EventValue::Number(i64::from(c)),
                                },
                            )
                            .checked(shared == Some(c))
                        })
                        .collect(),
                )
                .separated(),
            );
        }
        if !notes.is_empty() {
            items.push(MenuItem::new(
                "Mute / Unmute Notes",
                Action::NoteOperation {
                    clip,
                    notes: notes.clone(),
                    op: NoteOp::ToggleMuted,
                },
            ));
            let note_events: Vec<EventRef> = notes.iter().map(|n| EventRef::Note(*n)).collect();
            let release = |label: String, v: i64| {
                MenuItem::new(
                    label,
                    Action::EditMidiEvents {
                        clip,
                        events: note_events.clone(),
                        field: EventField::Release,
                        value: EventValue::Number(v),
                    },
                )
            };
            items.push(MenuItem::submenu(
                "Release Velocity",
                std::iter::once(release("None (note-off at 0)".into(), -1))
                    .chain(
                        [0i64, 32, 64, 96, 127]
                            .into_iter()
                            .map(|v| release(v.to_string(), v)),
                    )
                    .collect(),
            ));
        }
        let note = match row.event {
            EventRef::Note(id) | EventRef::Expression { note: id, .. } => {
                Some((id, row.data1.unwrap_or(60) as u8))
            }
            _ => None,
        };
        items.push(
            MenuItem::submenu(
                "Add Here",
                Self::add_items(clip, row.at, row.channel.unwrap_or(0), note),
            )
            .separated(),
        );
        let n = events.len();
        items.push(
            MenuItem::new(
                if n == 1 {
                    "Delete Event".to_string()
                } else {
                    format!("Delete {n} Events")
                },
                Action::RemoveMidiEvents { clip, events },
            )
            .separated(),
        );
        cx.request(HostRequest::ContextMenu { at, items });
    }

    fn keep_in_view(&mut self, i: usize, size: Size) {
        let list = Self::list(size);
        let top = i as f32 * ROW_H;
        if top < self.scroll {
            self.scroll = top;
        } else if top + ROW_H > self.scroll + list.h {
            self.scroll = top + ROW_H - list.h;
        }
    }

    fn chip(&self, p: &mut dyn Painter, r: Rect, label: &str, on: bool) {
        let th = &self.theme;
        if on {
            p.fill_rounded(r, 4.0, &th.ui.selection.with_alpha(0.45).into());
        }
        p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        p.text(
            label,
            r,
            &TextStyle::new(th.fonts.small, if on { th.ui.text } else { th.ui.text_dim }).center(),
        );
    }
}

impl faderframe_ui_canvas::CanvasView<Session, Action> for EventsView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.theme = theme.clone();
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        let clip = Self::clip(model);
        if clip != self.shown {
            self.shown = clip;
            self.selected.clear();
            self.anchor = None;
            self.scroll = 0.0;
        }
        // The header: the clip, the filters, "+ Add".
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Event List",
            Rect::new(12.0, 0.0, 90.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let h = Self::header(size);
        let what = clip
            .and_then(|c| model.project().clip(c))
            .map(|c| {
                let track = model
                    .project()
                    .track(c.track)
                    .map_or(String::new(), |t| format!(" · {}", t.name));
                format!("‘{}’{track}", c.name)
            })
            .unwrap_or_default();
        p.text(
            &what,
            Rect::new(104.0, 0.0, (h.notes.x - 112.0).max(0.0), HEADER_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        self.chip(p, h.notes, "Notes", self.filter.notes);
        self.chip(p, h.controllers, "Controllers", self.filter.controllers);
        self.chip(p, h.expression, "Expression", self.filter.expression);
        self.chip(p, h.sysex, "SysEx", self.filter.sysex);
        if clip.is_some() {
            p.fill_rounded(h.add, 4.0, &th.ui.accent.with_alpha(0.25).into());
            p.stroke_rounded(h.add, 4.0, 1.0, th.ui.accent);
            p.text(
                "+ Add",
                h.add,
                &TextStyle::new(th.fonts.small, th.ui.text).bold().center(),
            );
        }
        // The column names.
        let names = Rect::new(0.0, HEADER_H, size.w, COLUMNS_H);
        p.fill(names, th.ui.surface.with_alpha(0.6));
        p.hline(0.0, size.w, HEADER_H + COLUMNS_H - 0.5, th.ui.border);
        for (col, label, _) in COLUMNS {
            let r = Self::cell(col, names);
            if !label.is_empty() {
                let style = TextStyle::new(th.fonts.small, th.ui.text_dim).bold();
                let style = if col == Column::Mute {
                    style.center()
                } else {
                    style
                };
                p.text(label, r.inset_xy(6.0, 0.0), &style);
            }
            if col != Column::Info {
                p.vline(
                    r.x + r.w - 0.5,
                    names.y + 5.0,
                    names.y + names.h - 5.0,
                    th.ui.border,
                );
            }
        }
        let Some(clip) = clip else {
            p.text(
                "No MIDI clip: open one in the piano roll (double-click it in the arranger) or select one",
                Self::list(size).inset(16.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
            return;
        };
        let rows = self.rows(model);
        // The playhead's row, kept in view while playing.
        let current = Self::playhead_row(&rows, clip, model);
        if current != self.current {
            self.current = current;
            if let (Some(i), true) = (current, model.transport().playing) {
                self.keep_in_view(i, size);
            }
        }
        self.clamp(rows.len(), size);
        let list = Self::list(size);
        if rows.is_empty() {
            p.text(
                "No events shown: “+ Add” puts one at the playhead",
                list.inset(16.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
            return;
        }
        p.push_clip(list);
        let first = (self.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (((self.scroll + list.h) / ROW_H).ceil() as usize).min(rows.len());
        for (i, row) in rows.iter().enumerate().take(last).skip(first) {
            let r = self.row_rect(i, size);
            if self.is_selected(row, model) {
                p.fill(r, th.ui.selection.with_alpha(0.35));
            } else if self.hover == Some(i) {
                p.fill(r, th.ui.text.with_alpha(0.06));
            } else if i % 2 == 1 {
                p.fill(r, th.ui.text.with_alpha(0.025));
            }
            let locate = Self::cell(Column::Locate, r);
            if current == Some(i) {
                p.fill(Rect::new(r.x, r.y, 3.0, r.h), th.ui.accent);
                p.text(
                    "▶",
                    locate,
                    &TextStyle::new(th.fonts.small, th.ui.accent).center(),
                );
            } else if self.hover == Some(i) {
                p.text(
                    "▷",
                    locate,
                    &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
                );
            }
            // A note's mute: a small button, lit when muted.
            if row.kind == EventKind::Note {
                let m = Self::cell(Column::Mute, r).centered(14.0, 14.0);
                if row.muted {
                    p.fill_rounded(m, 3.0, &th.ui.accent.with_alpha(0.8).into());
                }
                p.stroke_rounded(m, 3.0, 1.0, th.ui.border);
                p.text(
                    "M",
                    m,
                    &TextStyle::new(
                        th.fonts.small,
                        if row.muted {
                            th.ui.text
                        } else {
                            th.ui.text_faint
                        },
                    )
                    .center(),
                );
            }
            let ink = if row.muted {
                th.ui.text_faint
            } else {
                th.ui.text
            };
            for (col, _, _) in COLUMNS.iter().skip(1) {
                let text = self.cell_text(row, *col, model);
                if text.is_empty() {
                    continue;
                }
                let c = Self::cell(*col, r).inset_xy(6.0, 0.0);
                let style = match col {
                    Column::Type => TextStyle::new(th.fonts.small, kind_color(row.kind, th)),
                    Column::Info => TextStyle::new(th.fonts.small, th.ui.text_dim),
                    Column::Release if row.release.is_none() => {
                        TextStyle::new(th.fonts.small, th.ui.text_faint)
                    }
                    _ => TextStyle::new(th.fonts.small, ink),
                };
                let style = if matches!(col, Column::Position | Column::Length | Column::End)
                    || (*col == Column::Info && row.kind == EventKind::Sysex)
                {
                    style.family(FontFamily::Mono)
                } else {
                    style
                };
                let style = if col.numeric() { style.right() } else { style };
                p.text(&text, c, &style);
            }
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
        let clip = Self::clip(model);
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks,
            } => {
                let h = Self::header(size);
                for (r, which) in [
                    (h.notes, 0),
                    (h.controllers, 1),
                    (h.expression, 2),
                    (h.sysex, 3),
                ] {
                    if r.contains(pos) {
                        let f = &mut self.filter;
                        let on = match which {
                            0 => &mut f.notes,
                            1 => &mut f.controllers,
                            2 => &mut f.expression,
                            _ => &mut f.sysex,
                        };
                        *on = !*on;
                        cx.redraw();
                        return true;
                    }
                }
                let Some(clip) = clip else {
                    return false;
                };
                let rows = self.rows(model);
                if h.add.contains(pos) {
                    let (at, channel, note) = self.new_event_place(&rows, clip, model);
                    cx.request(HostRequest::ContextMenu {
                        at: Point::new(h.add.x, h.add.y + h.add.h),
                        items: Self::add_items(clip, at, channel, note),
                    });
                    return true;
                }
                let Some(i) = self.row_at(pos, size, rows.len()) else {
                    return false;
                };
                cx.request(HostRequest::GrabFocus);
                let row = &rows[i];
                let col = Self::column_at(pos.x);
                if col == Column::Locate {
                    cx.emit(Action::Transport(TransportAction::Locate(row.at)));
                }
                // M: mute or unmute the note at once.
                if col == Column::Mute && row.kind == EventKind::Note {
                    cx.emit(Action::EditMidiEvents {
                        clip,
                        events: vec![row.event],
                        field: EventField::Muted,
                        value: EventValue::Number(i64::from(!row.muted)),
                    });
                    return true;
                }
                if clicks >= 2 && Self::editable(row, col) {
                    let cell = Self::cell(col, self.row_rect(i, size));
                    if let Some(req) = self.text_input(row, col, cell, model) {
                        cx.request(req);
                    }
                    return true;
                }
                // Selection: Shift extends, Ctrl toggles, else this row.
                let picked = self.is_selected(row, model);
                if modifiers.shift {
                    let a = self.anchor.unwrap_or(i).min(rows.len() - 1);
                    let (lo, hi) = (a.min(i), a.max(i));
                    let events: Vec<EventRef> = rows[lo..=hi].iter().map(|r| r.event).collect();
                    self.select(&events, cx);
                } else if modifiers.ctrl {
                    let mut events = self.selection(&rows, model);
                    if picked {
                        events.retain(|e| *e != row.event);
                    } else {
                        events.push(row.event);
                    }
                    self.select(&events, cx);
                    self.anchor = Some(i);
                } else {
                    if !picked {
                        self.select(&[row.event], cx);
                    }
                    self.anchor = Some(i);
                }
                if col.numeric()
                    && Self::editable(row, col)
                    && let Some(base) = Self::stored(row, col)
                {
                    self.drag = Some(ValueDrag {
                        event: row.event,
                        column: col,
                        kind: row.kind,
                        y0: pos.y,
                        base,
                        value: base,
                    });
                }
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let Some(clip) = clip else {
                    return false;
                };
                let rows = self.rows(model);
                let Some(i) = self.row_at(pos, size, rows.len()) else {
                    if Self::list(size).contains(pos) {
                        let (at, channel, note) = self.new_event_place(&rows, clip, model);
                        cx.request(HostRequest::ContextMenu {
                            at: pos,
                            items: vec![MenuItem::submenu(
                                "Add",
                                Self::add_items(clip, at, channel, note),
                            )],
                        });
                        return true;
                    }
                    return false;
                };
                self.row_menu(pos, i, &rows, clip, model, cx);
                true
            }
            ViewEvent::PointerMove { pos, dragging, .. } => {
                if let (Some(d), true) = (self.drag.as_mut(), dragging) {
                    let (lo, hi) = Self::range(d.kind, d.column);
                    let steps = f64::from(((d.y0 - pos.y) / DRAG_PX).round());
                    let v = (d.base + steps * Self::drag_step(d.kind, d.column)).clamp(lo, hi);
                    if v != d.value {
                        d.value = v;
                        cx.redraw();
                    }
                    cx.set_cursor(Cursor::ResizeVertical);
                    return true;
                }
                let rows = self.rows(model);
                let hover = self.row_at(pos, size, rows.len());
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                let col = Self::column_at(pos.x);
                let cursor = match hover.map(|i| &rows[i]) {
                    Some(r) if col.numeric() && Self::editable(r, col) => Cursor::ResizeVertical,
                    Some(r) if col == Column::Mute && Self::editable(r, col) => Cursor::Pointer,
                    Some(_) if col == Column::Locate => Cursor::Pointer,
                    _ => Cursor::Default,
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
                if d.value != d.base
                    && let (Some(clip), Some(field)) = (clip, d.column.field())
                {
                    cx.emit(Action::EditMidiEvents {
                        clip,
                        events: vec![d.event],
                        field,
                        value: Self::drag_value(d.kind, d.value),
                    });
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll { dy, precise, .. } => {
                self.scroll += if precise { dy } else { dy * ROW_H * 3.0 };
                let rows = self.rows(model);
                self.clamp(rows.len(), size);
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => {
                let Some(clip) = clip else {
                    return false;
                };
                let rows = self.rows(model);
                match key {
                    Key::Delete | Key::Backspace => {
                        let events = self.selection(&rows, model);
                        if events.is_empty() {
                            return false;
                        }
                        self.selected.clear();
                        cx.emit(Action::RemoveMidiEvents { clip, events });
                        true
                    }
                    Key::Up | Key::Down if !rows.is_empty() => {
                        let at = rows
                            .iter()
                            .rposition(|r| self.is_selected(r, model))
                            .or(self.anchor);
                        let i = match (key, at) {
                            (Key::Up, Some(i)) => i.saturating_sub(1),
                            (Key::Down, Some(i)) => (i + 1).min(rows.len() - 1),
                            _ => 0,
                        };
                        if modifiers.shift {
                            let a = self.anchor.unwrap_or(i);
                            let (lo, hi) = (a.min(i), a.max(i));
                            let events: Vec<EventRef> =
                                rows[lo..=hi].iter().map(|r| r.event).collect();
                            self.select(&events, cx);
                        } else {
                            self.select(&[rows[i].event], cx);
                            self.anchor = Some(i);
                        }
                        self.keep_in_view(i, size);
                        true
                    }
                    Key::Char('a') if modifiers.ctrl => {
                        let events: Vec<EventRef> = rows.iter().map(|r| r.event).collect();
                        self.select(&events, cx);
                        true
                    }
                    Key::Escape => {
                        self.select(&[], cx);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let h = Self::header(size);
        for (r, tip) in [
            (h.notes, "Show notes"),
            (
                h.controllers,
                "Show controller values: control changes, program changes, pitch bend, channel and poly pressure",
            ),
            (
                h.expression,
                "Show the points of the notes' expression curves",
            ),
            (h.sysex, "Show SysEx messages"),
            (
                h.add,
                "Add an event at the selected event, else at the playhead",
            ),
        ] {
            if r.contains(pos) {
                return Some(tip.into());
            }
        }
        let rows = self.rows(model);
        let row = &rows[self.row_at(pos, size, rows.len())?];
        let col = Self::column_at(pos.x);
        let editable = Self::editable(row, col);
        Some(match col {
            Column::Locate => "Move the playhead to this event".into(),
            Column::Mute if editable => {
                if row.muted {
                    "Muted: click to unmute".into()
                } else {
                    "Click to mute the note".into()
                }
            }
            Column::Info if editable => {
                format!(
                    "{} · double-click to type its bytes in hex",
                    hex(&row.bytes)
                )
            }
            Column::Length if editable => {
                "Length in beats.ticks (960 ticks a beat) or a note value like 1/8 · double-click to type".into()
            }
            Column::Release if editable => {
                "Release (note-off) velocity: drag or double-click to type; empty = none".into()
            }
            _ if col.numeric() && editable => "Drag up or down, or double-click to type".into(),
            _ if editable => "Double-click to type".into(),
            _ => return None,
        })
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.transport().playing || self.drag.is_some()
    }

    fn min_size(&self) -> Size {
        Size::new(420.0, 140.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let count = self.rows(model).len();
        let list = Self::list(size);
        Some(ScrollInfo {
            content: count as f32 * ROW_H,
            viewport: list.h,
            offset: self.scroll,
            start: list.y,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll = offset.max(0.0);
        }
    }
}

fn kind_label(kind: EventKind) -> String {
    match kind {
        EventKind::Note => "Note".into(),
        EventKind::Controller(MidiController::Cc { .. }) => "Control Change".into(),
        EventKind::Controller(MidiController::PitchBend) => "Pitch Bend".into(),
        EventKind::Controller(MidiController::ChannelPressure) => "Channel Pressure".into(),
        EventKind::Controller(MidiController::PolyPressure { .. }) => "Poly Pressure".into(),
        EventKind::Controller(MidiController::Program) => "Program Change".into(),
        EventKind::Expression(k) => format!("Note {}", k.label()),
        EventKind::Sysex => "SysEx".into(),
    }
}

fn kind_color(kind: EventKind, th: &Theme) -> faderframe_ui_canvas::Color {
    match kind {
        EventKind::Note => th.ui.text,
        EventKind::Controller(_) => th.ui.accent,
        EventKind::Expression(_) | EventKind::Sysex => th.ui.text_dim,
    }
}

/// Bytes as hex ("F0 7E 7F 06 01 F7").
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// "C4" style (middle C = C4 = MIDI 60), as the piano roll names keys.
pub fn note_name(key: u8) -> String {
    format!("{}{}", NAMES[(key % 12) as usize], key as i32 / 12 - 1)
}

/// A typed key: a MIDI number, or a name like "C4", "F#3", "Bb-1".
pub fn parse_key(text: &str) -> Option<u8> {
    let t = text.trim();
    if let Ok(n) = t.parse::<i64>() {
        return u8::try_from(n).ok().filter(|k| *k <= 127);
    }
    let mut chars = t.chars();
    let base: i64 = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave) = if let Some(r) = rest.strip_prefix(['#', '♯']) {
        (1, r)
    } else if let Some(r) = rest.strip_prefix(['b', '♭']) {
        (-1, r)
    } else {
        (0, rest)
    };
    let octave: i64 = octave.trim().replace('−', "-").parse().ok()?;
    u8::try_from((octave + 1) * 12 + base + accidental)
        .ok()
        .filter(|k| *k <= 127)
}

/// A pitch bend offset from the centre ("0", "+512", "−300").
fn bend_text(d: i64) -> String {
    match d {
        0 => "0".into(),
        d if d > 0 => format!("+{d}"),
        d => format!("−{}", -d),
    }
}

fn parse_signed(text: &str) -> Option<i64> {
    let t = text.trim().replace('−', "-");
    t.strip_prefix('+').unwrap_or(&t).parse().ok()
}

/// A typed expression value in its kind's unit: "+1.5 st", "−3 dB", pan as
/// "C", "L 30", "R 30" or −1…1, else a plain number.
pub fn parse_amount(kind: ExpressionKind, text: &str) -> Option<f32> {
    let t = text.trim().replace('−', "-");
    if kind == ExpressionKind::Pan {
        let up = t.to_ascii_uppercase();
        if up == "C" {
            return Some(0.0);
        }
        for (side, sign) in [("L", -1.0f32), ("R", 1.0)] {
            if let Some(rest) = up.strip_prefix(side) {
                let v: f32 = rest.trim().parse().ok()?;
                return Some((sign * v / 100.0).clamp(-1.0, 1.0));
            }
        }
    }
    let t = t
        .trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace())
        .trim();
    let v: f32 = t.strip_prefix('+').unwrap_or(t).parse().ok()?;
    let (lo, hi) = kind.range();
    v.is_finite().then_some(v.clamp(lo, hi))
}

/// A length as beats.ticks (960 ticks a quarter, like the positions).
pub fn length_text(t: MusicalTime) -> String {
    let ticks = t.ticks().max(0);
    let beats = ticks / TICKS_PER_QUARTER;
    let rest = (ticks % TICKS_PER_QUARTER) * 960 / TICKS_PER_QUARTER;
    format!("{beats}.{rest:03}")
}

/// A typed length: "beats.ticks", "beats", or a note value "1/8".
pub fn parse_length(text: &str) -> Option<MusicalTime> {
    let t = text.trim();
    let ticks = if let Some((a, b)) = t.split_once('/') {
        let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        if b <= 0.0 {
            return None;
        }
        (4.0 * a / b * TICKS_PER_QUARTER as f64).round() as i64
    } else {
        let (beats, rest) = t.split_once(['.', ':']).unwrap_or((t, "0"));
        let beats: i64 = if beats.is_empty() {
            0
        } else {
            beats.parse().ok()?
        };
        let rest: i64 = if rest.is_empty() {
            0
        } else {
            rest.parse().ok()?
        };
        if !(0..960).contains(&rest) || beats < 0 {
            return None;
        }
        beats * TICKS_PER_QUARTER + rest * TICKS_PER_QUARTER / 960
    };
    (ticks > 0).then_some(MusicalTime::from_ticks(ticks))
}

#[cfg(test)]
mod tests;

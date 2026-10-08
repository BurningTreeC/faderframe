//! The MIDI event list (`faderframe-view-events`): every event of a clip in
//! time order — notes (position, channel, key, velocity, length or end,
//! release velocity, mute), controller values (control changes, pitch bend,
//! channel pressure, polyphonic key pressure, program changes), per-note
//! expression points and SysEx (its bytes) — each editable by field, added
//! and removed, one undo step each; and the channel of whole clips set at
//! once (their notes and their controller lanes).

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, NoteId};
use faderframe_project::{
    ClipContent, Command, ControllerLane, ControllerPoint, ExpressionKind, ExpressionPoint,
    MidiClip, MidiController, MidiNote, NoteExpression, SysexEvent,
};
use faderframe_timeline::MusicalTime;

/// One event of a clip, as the list addresses it (stable across edits of
/// other events).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventRef {
    Note(NoteId),
    /// A controller value: its lane (controller, channel) and its time
    /// (from the clip start).
    Controller {
        controller: MidiController,
        channel: u8,
        time: MusicalTime,
    },
    /// A point of a note's expression curve (its time from the note's
    /// start).
    Expression {
        note: NoteId,
        kind: ExpressionKind,
        time: MusicalTime,
    },
    /// A SysEx message by its place among the clip's (they are sorted).
    Sysex(usize),
}

/// What an event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Note,
    Controller(MidiController),
    Expression(ExpressionKind),
    Sysex,
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRow {
    pub event: EventRef,
    pub kind: EventKind,
    /// The project position.
    pub at: MusicalTime,
    /// The channel (an expression point's: its note's).
    pub channel: Option<u8>,
    /// The key (notes, poly pressure, expression points: their note's) or
    /// controller number (control changes).
    pub data1: Option<u16>,
    /// The velocity (notes) or the value (controllers; a program 0–127).
    pub data2: Option<u16>,
    /// An expression point's value, in its kind's unit.
    pub amount: Option<f32>,
    pub length: Option<MusicalTime>,
    /// A note's release velocity.
    pub release: Option<u8>,
    pub muted: bool,
    /// A SysEx message.
    pub bytes: Vec<u8>,
}

/// What an edit changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventField {
    Position,
    Channel,
    /// Key or controller number.
    Data1,
    /// Velocity or value (an expression point's: [`EventValue::Amount`]).
    Data2,
    Length,
    /// A note's end (a project position): its length follows.
    End,
    /// A note's release velocity ([`EventValue::Number`] below 0: none).
    Release,
    /// A note's mute (0 or 1).
    Muted,
    /// A SysEx message's bytes.
    Bytes,
}

/// The value an edit sets.
#[derive(Clone, Debug, PartialEq)]
pub enum EventValue {
    /// A project position (Position, End) or a duration (Length).
    Time(MusicalTime),
    Number(i64),
    /// An expression point's value.
    Amount(f32),
    /// A whole SysEx message, `F0 … F7`.
    Bytes(Vec<u8>),
}

/// What a new event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewEvent {
    Note,
    Controller(MidiController),
    /// A point on a note's curve (at the playhead, within the note).
    Expression {
        note: NoteId,
        kind: ExpressionKind,
    },
    /// A SysEx message (an identity request, to type over).
    Sysex,
}

/// The message a new SysEx event starts with: the universal identity
/// request.
pub const NEW_SYSEX: [u8; 6] = [0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7];

/// A typed SysEx message: hex bytes ("F0 43 10 4C … F7", commas, `0x`
/// allowed), `F0`/`F7` added when left out; `None` unless every data byte
/// is 7-bit.
pub fn parse_sysex(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for word in text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
    {
        let w = word
            .strip_prefix("0x")
            .or_else(|| word.strip_prefix("0X"))
            .unwrap_or(word);
        if w.is_empty() || w.len() > 2 {
            return None;
        }
        bytes.push(u8::from_str_radix(w, 16).ok()?);
    }
    if bytes.first() != Some(&0xF0) {
        bytes.insert(0, 0xF0);
    }
    if bytes.last() != Some(&0xF7) || bytes.len() < 2 {
        bytes.push(0xF7);
    }
    valid_sysex(&bytes).then_some(bytes)
}

/// `F0`, 7-bit data, `F7`.
pub fn valid_sysex(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && bytes[0] == 0xF0
        && bytes[bytes.len() - 1] == 0xF7
        && bytes[1..bytes.len() - 1].iter().all(|b| *b < 0x80)
}

fn not_midi() -> SessionError {
    SessionError::Other("not a MIDI clip".into())
}

/// The lane's point at `time`.
fn point_at(lane: &ControllerLane, time: MusicalTime) -> Option<usize> {
    lane.points.iter().position(|p| p.time == time)
}

/// Put `point` into the clip's lane for `controller` on `channel`, in time
/// order (a point already at that time is replaced).
fn put_point(m: &mut MidiClip, controller: MidiController, channel: u8, point: ControllerPoint) {
    let lane = m.lane_mut(controller, channel);
    lane.points.retain(|p| p.time != point.time);
    let at = lane.points.partition_point(|p| p.time < point.time);
    lane.points.insert(at, point);
}

/// Put an expression point into `curve` in time order (replacing one at
/// that time).
fn put_expression(curve: &mut Vec<ExpressionPoint>, point: ExpressionPoint) {
    curve.retain(|p| p.time != point.time);
    let at = curve.partition_point(|p| p.time < point.time);
    curve.insert(at, point);
}

/// Lanes left without points go; lanes that now share a controller and a
/// channel become one (their points merged in time order, the later lane's
/// winning at the same time).
fn tidy_lanes(m: &mut MidiClip) {
    let mut merged: Vec<ControllerLane> = Vec::with_capacity(m.controllers.len());
    for lane in m.controllers.drain(..) {
        match merged
            .iter_mut()
            .find(|l| l.controller == lane.controller && l.channel == lane.channel)
        {
            Some(into) => {
                for p in lane.points {
                    into.points.retain(|q| q.time != p.time);
                    into.points.push(p);
                }
                into.points.sort_by_key(|p| p.time);
            }
            None => merged.push(lane),
        }
    }
    merged.retain(|l| !l.points.is_empty());
    merged.sort_by_key(|l| (l.controller, l.channel));
    m.controllers = merged;
    m.expressions.retain(|e| !e.is_empty());
}

/// A controller moved to another key or number (`None`: it has none).
fn renumbered(controller: MidiController, n: i64) -> Option<MidiController> {
    let n = n.clamp(0, 127) as u8;
    match controller {
        MidiController::Cc { .. } => Some(MidiController::Cc { number: n }),
        MidiController::PolyPressure { .. } => Some(MidiController::PolyPressure { key: n }),
        _ => None,
    }
}

/// Change one field of one event of `m` (a clip starting at `start`).
fn edit_event(
    m: &mut MidiClip,
    start: MusicalTime,
    event: EventRef,
    field: EventField,
    value: &EventValue,
) {
    let number = match value {
        EventValue::Number(n) => Some(*n),
        _ => None,
    };
    let time = match value {
        EventValue::Time(t) => Some(*t),
        _ => None,
    };
    match event {
        EventRef::Note(id) => {
            let Some(n) = m.notes.iter_mut().find(|n| n.id == id) else {
                return;
            };
            match (field, number, time) {
                (EventField::Position, _, Some(t)) => n.start = (t - start).max(MusicalTime::ZERO),
                (EventField::Length, _, Some(t)) => n.length = t.max(MusicalTime::from_ticks(1)),
                (EventField::End, _, Some(t)) => {
                    n.length = (t - start - n.start).max(MusicalTime::from_ticks(1));
                }
                (EventField::Channel, Some(c), _) => n.channel = c.clamp(0, 15) as u8,
                (EventField::Data1, Some(k), _) => n.key = k.clamp(0, 127) as u8,
                (EventField::Data2, Some(v), _) => n.velocity = v.clamp(1, 127) as u8,
                (EventField::Release, Some(v), _) => {
                    n.release = (v >= 0).then(|| v.min(127) as u8);
                }
                (EventField::Muted, Some(v), _) => n.muted = v != 0,
                _ => return,
            }
            m.notes.sort_by_key(|n| (n.start, n.key));
        }
        EventRef::Controller {
            controller,
            channel,
            time: at,
        } => {
            let Some(lane) = m.lane(controller, channel) else {
                return;
            };
            let Some(i) = point_at(lane, at) else {
                return;
            };
            let mut point = lane.points[i];
            let (mut to_controller, mut to_channel) = (controller, channel);
            match (field, number, time) {
                (EventField::Position, _, Some(t)) => {
                    point.time = (t - start).max(MusicalTime::ZERO);
                }
                (EventField::Channel, Some(c), _) => to_channel = c.clamp(0, 15) as u8,
                (EventField::Data1, Some(c), _) => match renumbered(controller, c) {
                    Some(to) => to_controller = to,
                    None => return,
                },
                (EventField::Data2, Some(v), _) => {
                    point.value = v.clamp(0, i64::from(to_controller.max())) as u16;
                }
                _ => return,
            }
            m.lane_mut(controller, channel).points.remove(i);
            put_point(m, to_controller, to_channel, point);
            tidy_lanes(m);
        }
        EventRef::Expression {
            note,
            kind,
            time: at,
        } => {
            let Some(n) = m.notes.iter().find(|n| n.id == note).copied() else {
                return;
            };
            let Some(e) = m.expressions.iter_mut().find(|e| e.note == note) else {
                return;
            };
            let curve = e.curve_mut(kind);
            let Some(i) = curve.iter().position(|p| p.time == at) else {
                return;
            };
            let mut point = curve[i];
            match (field, value) {
                (EventField::Position, EventValue::Time(t)) => {
                    point.time = (*t - start - n.start).clamp(MusicalTime::ZERO, n.length);
                }
                (EventField::Data2, EventValue::Amount(v)) => {
                    let (lo, hi) = kind.range();
                    point.value = v.clamp(lo, hi);
                }
                _ => return,
            }
            curve.remove(i);
            put_expression(curve, point);
        }
        EventRef::Sysex(i) => {
            if i >= m.sysex.len() {
                return;
            }
            match (field, value) {
                (EventField::Position, EventValue::Time(t)) => {
                    m.sysex[i].time = (*t - start).max(MusicalTime::ZERO);
                }
                (EventField::Bytes, EventValue::Bytes(b)) if valid_sysex(b) => {
                    m.sysex[i].data = b.clone();
                }
                _ => return,
            }
            m.sysex.sort_by_key(|s| s.time);
        }
    }
}

impl Session {
    /// The MIDI clip `clip` and its start.
    fn midi_of(&self, clip: ClipId) -> Result<(MusicalTime, MidiClip)> {
        let c = self
            .project
            .clip(clip)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        Ok((c.start, c.as_midi().ok_or_else(not_midi)?.clone()))
    }

    /// Write `m` back as `clip`'s content (one undo step labelled `label`).
    fn set_midi(
        &mut self,
        clip: ClipId,
        start: MusicalTime,
        m: MidiClip,
        label: &str,
    ) -> Result<()> {
        self.batch(
            label,
            vec![Command::SetClipContent {
                clip,
                start,
                content: Box::new(ClipContent::Midi(m)),
            }],
        )
    }

    /// Clip `clip`'s events in time order (at the same time: notes, their
    /// expression points, controller values, SysEx).
    pub fn midi_events(&self, clip: ClipId) -> Vec<EventRow> {
        let Ok((start, m)) = self.midi_of(clip) else {
            return Vec::new();
        };
        let row = |event, kind, at| EventRow {
            event,
            kind,
            at,
            channel: None,
            data1: None,
            data2: None,
            amount: None,
            length: None,
            release: None,
            muted: false,
            bytes: Vec::new(),
        };
        let mut rows: Vec<(u8, EventRow)> = Vec::new();
        for n in &m.notes {
            rows.push((
                0,
                EventRow {
                    channel: Some(n.channel),
                    data1: Some(u16::from(n.key)),
                    data2: Some(u16::from(n.velocity)),
                    length: Some(n.length),
                    release: n.release,
                    muted: n.muted,
                    ..row(EventRef::Note(n.id), EventKind::Note, start + n.start)
                },
            ));
            let Some(e) = m.expression(n.id) else {
                continue;
            };
            for kind in ExpressionKind::ALL {
                for p in e.curve(kind) {
                    rows.push((
                        1,
                        EventRow {
                            channel: Some(n.channel),
                            data1: Some(u16::from(n.key)),
                            amount: Some(p.value),
                            muted: n.muted,
                            ..row(
                                EventRef::Expression {
                                    note: n.id,
                                    kind,
                                    time: p.time,
                                },
                                EventKind::Expression(kind),
                                start + n.start + p.time,
                            )
                        },
                    ));
                }
            }
        }
        for lane in &m.controllers {
            for p in &lane.points {
                rows.push((
                    2,
                    EventRow {
                        channel: Some(lane.channel),
                        data1: match lane.controller {
                            MidiController::Cc { number } => Some(u16::from(number)),
                            MidiController::PolyPressure { key } => Some(u16::from(key)),
                            _ => None,
                        },
                        data2: Some(p.value),
                        ..row(
                            EventRef::Controller {
                                controller: lane.controller,
                                channel: lane.channel,
                                time: p.time,
                            },
                            EventKind::Controller(lane.controller),
                            start + p.time,
                        )
                    },
                ));
            }
        }
        for (i, s) in m.sysex.iter().enumerate() {
            rows.push((
                3,
                EventRow {
                    bytes: s.data.clone(),
                    ..row(EventRef::Sysex(i), EventKind::Sysex, start + s.time)
                },
            ));
        }
        rows.sort_by_key(|(order, r)| (r.at, *order, r.data1));
        rows.into_iter().map(|(_, r)| r).collect()
    }

    /// Change one field of events (the same value for each), one undo
    /// step.
    pub(crate) fn edit_midi_events(
        &mut self,
        clip: ClipId,
        events: &[EventRef],
        field: EventField,
        value: &EventValue,
    ) -> Result<()> {
        let (start, mut m) = self.midi_of(clip)?;
        let before = m.clone();
        for &event in events {
            edit_event(&mut m, start, event, field, value);
        }
        if m == before {
            return Ok(());
        }
        self.set_midi(clip, start, m, "Edit MIDI Event")
    }

    /// Remove events (notes with their expression).
    pub(crate) fn remove_midi_events(&mut self, clip: ClipId, events: &[EventRef]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let (start, mut m) = self.midi_of(clip)?;
        let notes: Vec<NoteId> = events
            .iter()
            .filter_map(|e| match e {
                EventRef::Note(id) => Some(*id),
                _ => None,
            })
            .collect();
        m.notes.retain(|n| !notes.contains(&n.id));
        m.expressions.retain(|x| !notes.contains(&x.note));
        for e in events {
            match *e {
                EventRef::Controller {
                    controller,
                    channel,
                    time,
                } => {
                    if let Some(lane) = m
                        .controllers
                        .iter_mut()
                        .find(|l| l.controller == controller && l.channel == channel)
                    {
                        lane.points.retain(|p| p.time != time);
                    }
                }
                EventRef::Expression { note, kind, time } => {
                    if let Some(x) = m.expressions.iter_mut().find(|x| x.note == note) {
                        x.curve_mut(kind).retain(|p| p.time != time);
                    }
                }
                EventRef::Note(_) | EventRef::Sysex(_) => {}
            }
        }
        let mut sysex: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                EventRef::Sysex(i) => Some(*i),
                _ => None,
            })
            .collect();
        sysex.sort_unstable();
        sysex.dedup();
        for i in sysex.into_iter().rev() {
            if i < m.sysex.len() {
                m.sysex.remove(i);
            }
        }
        tidy_lanes(&mut m);
        self.set_midi(clip, start, m, "Delete MIDI Events")
    }

    /// Add an event at project position `at` on `channel`: a note (middle
    /// C, velocity 100, a beat long), a controller value (64 for a control
    /// change or pressure, a program 1, pitch bend centred), a point on a
    /// note's expression curve (its value there; within the note), a SysEx
    /// message ([`NEW_SYSEX`]). Returns it.
    pub(crate) fn add_midi_event(
        &mut self,
        clip: ClipId,
        what: NewEvent,
        at: MusicalTime,
        channel: u8,
    ) -> Result<EventRef> {
        let (start, mut m) = self.midi_of(clip)?;
        let time = (at - start).max(MusicalTime::ZERO);
        let channel = channel.min(15);
        let event = match what {
            NewEvent::Note => {
                let id: NoteId = self.project.ids.allocate();
                m.notes.push(MidiNote {
                    id,
                    start: time,
                    length: MusicalTime::from_quarters(1.0),
                    key: 60,
                    velocity: 100,
                    channel,
                    muted: false,
                    release: None,
                });
                m.notes.sort_by_key(|n| (n.start, n.key));
                EventRef::Note(id)
            }
            NewEvent::Controller(controller) => {
                let value = match controller {
                    MidiController::PitchBend | MidiController::Program => controller.rest(),
                    _ => 64,
                };
                put_point(&mut m, controller, channel, ControllerPoint { time, value });
                EventRef::Controller {
                    controller,
                    channel,
                    time,
                }
            }
            NewEvent::Expression { note, kind } => {
                let n = m
                    .notes
                    .iter()
                    .find(|n| n.id == note)
                    .copied()
                    .ok_or_else(|| SessionError::Other("no such note".into()))?;
                let t = (time - n.start).clamp(MusicalTime::ZERO, n.length);
                let i = match m.expressions.iter().position(|e| e.note == note) {
                    Some(i) => i,
                    None => {
                        m.expressions.push(NoteExpression::new(note));
                        m.expressions.len() - 1
                    }
                };
                let e = &mut m.expressions[i];
                let value = e.value_at(kind, t);
                put_expression(e.curve_mut(kind), ExpressionPoint { time: t, value });
                EventRef::Expression {
                    note,
                    kind,
                    time: t,
                }
            }
            NewEvent::Sysex => {
                let at = m.sysex.partition_point(|s| s.time <= time);
                m.sysex.insert(
                    at,
                    SysexEvent {
                        time,
                        data: NEW_SYSEX.to_vec(),
                    },
                );
                EventRef::Sysex(at)
            }
        };
        self.set_midi(clip, start, m, "Add MIDI Event")?;
        Ok(event)
    }

    /// Put every note and controller value of the MIDI clips among `clips`
    /// on `channel` (0–15), one undo step.
    pub(crate) fn set_midi_channel(&mut self, clips: &[ClipId], channel: u8) -> Result<()> {
        let channel = channel.min(15);
        let mut commands = Vec::new();
        for &clip in clips {
            let Ok((start, mut m)) = self.midi_of(clip) else {
                continue;
            };
            let before = m.clone();
            for n in &mut m.notes {
                n.channel = channel;
            }
            for lane in &mut m.controllers {
                lane.channel = channel;
            }
            tidy_lanes(&mut m);
            if m != before {
                commands.push(Command::SetClipContent {
                    clip,
                    start,
                    content: Box::new(ClipContent::Midi(m)),
                });
            }
        }
        self.batch(&format!("MIDI Channel {}", channel + 1), commands)
    }

    /// The channel every note and controller value of the clips is on, if
    /// they share one (for a menu's check).
    pub fn midi_channel_of(&self, clips: &[ClipId]) -> Option<u8> {
        let mut found = None;
        for &clip in clips {
            let Ok((_, m)) = self.midi_of(clip) else {
                continue;
            };
            for c in m
                .notes
                .iter()
                .map(|n| n.channel)
                .chain(m.controllers.iter().map(|l| l.channel))
            {
                match found {
                    None => found = Some(c),
                    Some(f) if f != c => return None,
                    _ => {}
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_sysex() {
        assert_eq!(
            parse_sysex("F0 43 10 4C 00 00 7E 00 F7"),
            Some(vec![0xF0, 0x43, 0x10, 0x4C, 0, 0, 0x7E, 0, 0xF7])
        );
        assert_eq!(
            parse_sysex("7e,7f, 0x09 01"),
            Some(vec![0xF0, 0x7E, 0x7F, 9, 1, 0xF7])
        );
        assert_eq!(parse_sysex(""), Some(vec![0xF0, 0xF7]));
        assert_eq!(parse_sysex("F0 80 F7"), None, "data bytes are 7-bit");
        assert_eq!(parse_sysex("F0 ZZ F7"), None);
        assert_eq!(parse_sysex("F0 123 F7"), None);
    }
}

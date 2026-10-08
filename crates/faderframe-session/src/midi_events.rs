//! The MIDI event list (`faderframe-view-events`): a clip's events in time
//! order — notes, controller values (control changes, pitch bend, channel
//! pressure) and SysEx — each editable by field, added and removed, one
//! undo step each; and the channel of whole clips set at once (their notes
//! and their controller lanes).

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, NoteId};
use faderframe_project::{
    ClipContent, Command, ControllerLane, ControllerPoint, MidiClip, MidiController, MidiNote,
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
    /// A SysEx message by its place among the clip's (they are sorted).
    Sysex(usize),
}

/// What an event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Note,
    Controller(MidiController),
    Sysex,
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRow {
    pub event: EventRef,
    pub kind: EventKind,
    /// The project position.
    pub at: MusicalTime,
    pub channel: Option<u8>,
    /// The key (notes) or controller number (control changes).
    pub data1: Option<u16>,
    /// The velocity (notes) or the value.
    pub data2: Option<u16>,
    pub length: Option<MusicalTime>,
    pub muted: bool,
    /// A SysEx message's bytes, in hex.
    pub bytes: String,
}

/// What an edit changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventField {
    Position,
    Channel,
    /// Key or controller number.
    Data1,
    /// Velocity or value.
    Data2,
    Length,
}

/// The value an edit sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventValue {
    /// A project position (Position) or a duration (Length).
    Time(MusicalTime),
    Number(i64),
}

/// What a new event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewEvent {
    Note,
    Controller(MidiController),
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
}

/// Change one field of one event of `m` (a clip starting at `start`).
fn edit_event(
    m: &mut MidiClip,
    start: MusicalTime,
    event: EventRef,
    field: EventField,
    value: EventValue,
) {
    let number = |v: EventValue| match v {
        EventValue::Number(n) => Some(n),
        EventValue::Time(_) => None,
    };
    let time = |v: EventValue| match v {
        EventValue::Time(t) => Some(t),
        EventValue::Number(_) => None,
    };
    match event {
        EventRef::Note(id) => {
            let Some(n) = m.notes.iter_mut().find(|n| n.id == id) else {
                return;
            };
            match field {
                EventField::Position => {
                    if let Some(t) = time(value) {
                        n.start = (t - start).max(MusicalTime::ZERO);
                    }
                }
                EventField::Length => {
                    if let Some(t) = time(value) {
                        n.length = t.max(MusicalTime::from_ticks(1));
                    }
                }
                EventField::Channel => {
                    if let Some(c) = number(value) {
                        n.channel = c.clamp(0, 15) as u8;
                    }
                }
                EventField::Data1 => {
                    if let Some(k) = number(value) {
                        n.key = k.clamp(0, 127) as u8;
                    }
                }
                EventField::Data2 => {
                    if let Some(v) = number(value) {
                        n.velocity = v.clamp(1, 127) as u8;
                    }
                }
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
            match field {
                EventField::Position => {
                    if let Some(t) = time(value) {
                        point.time = (t - start).max(MusicalTime::ZERO);
                    }
                }
                EventField::Channel => {
                    if let Some(c) = number(value) {
                        to_channel = c.clamp(0, 15) as u8;
                    }
                }
                EventField::Data1 => {
                    if let (MidiController::Cc { .. }, Some(c)) = (controller, number(value)) {
                        to_controller = MidiController::Cc {
                            number: c.clamp(0, 127) as u8,
                        };
                    }
                }
                EventField::Data2 => {
                    if let Some(v) = number(value) {
                        point.value = v.clamp(0, i64::from(to_controller.max())) as u16;
                    }
                }
                EventField::Length => return,
            }
            m.lane_mut(controller, channel).points.remove(i);
            put_point(m, to_controller, to_channel, point);
            tidy_lanes(m);
        }
        EventRef::Sysex(i) => {
            if field != EventField::Position || i >= m.sysex.len() {
                return;
            }
            if let Some(t) = time(value) {
                m.sysex[i].time = (t - start).max(MusicalTime::ZERO);
                m.sysex.sort_by_key(|s| s.time);
            }
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

    /// Clip `clip`'s events in time order (notes before controller values
    /// before SysEx at the same time).
    pub fn midi_events(&self, clip: ClipId) -> Vec<EventRow> {
        let Ok((start, m)) = self.midi_of(clip) else {
            return Vec::new();
        };
        let mut rows: Vec<(u8, EventRow)> = Vec::new();
        for n in &m.notes {
            rows.push((
                0,
                EventRow {
                    event: EventRef::Note(n.id),
                    kind: EventKind::Note,
                    at: start + n.start,
                    channel: Some(n.channel),
                    data1: Some(u16::from(n.key)),
                    data2: Some(u16::from(n.velocity)),
                    length: Some(n.length),
                    muted: n.muted,
                    bytes: String::new(),
                },
            ));
        }
        for lane in &m.controllers {
            for p in &lane.points {
                rows.push((
                    1,
                    EventRow {
                        event: EventRef::Controller {
                            controller: lane.controller,
                            channel: lane.channel,
                            time: p.time,
                        },
                        kind: EventKind::Controller(lane.controller),
                        at: start + p.time,
                        channel: Some(lane.channel),
                        data1: match lane.controller {
                            MidiController::Cc { number } => Some(u16::from(number)),
                            _ => None,
                        },
                        data2: Some(p.value),
                        length: None,
                        muted: false,
                        bytes: String::new(),
                    },
                ));
            }
        }
        for (i, s) in m.sysex.iter().enumerate() {
            rows.push((
                2,
                EventRow {
                    event: EventRef::Sysex(i),
                    kind: EventKind::Sysex,
                    at: start + s.time,
                    channel: None,
                    data1: None,
                    data2: None,
                    length: None,
                    muted: false,
                    bytes: s
                        .data
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" "),
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
        value: EventValue,
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
            if let EventRef::Controller {
                controller,
                channel,
                time,
            } = *e
                && let Some(lane) = m
                    .controllers
                    .iter_mut()
                    .find(|l| l.controller == controller && l.channel == channel)
            {
                lane.points.retain(|p| p.time != time);
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
    /// C, velocity 100, a beat long) or a controller value at the
    /// controller's resting value (64 for a control change). Returns it.
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
                });
                m.notes.sort_by_key(|n| (n.start, n.key));
                EventRef::Note(id)
            }
            NewEvent::Controller(controller) => {
                let value = match controller {
                    MidiController::Cc { .. } => 64,
                    other => other.rest(),
                };
                put_point(&mut m, controller, channel, ControllerPoint { time, value });
                EventRef::Controller {
                    controller,
                    channel,
                    time,
                }
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

//! MIDI 2.0 Clip Files (`.midi2`, [`faderframe_midi::clipfile`]).
//!
//! Export writes one MIDI clip in MIDI 2.0: velocities at 16 bits,
//! controllers at 32, programs with their banks (the clip's bank-select
//! lanes folded in), the notes' per-note expression as per-note pitch bends
//! and controllers (pressure as poly pressure), SysEx; the project's tempo,
//! meter and key over the clip, and the clip's name, as flex data.
//!
//! Import reads MIDI 2.0 and MIDI 1.0 messages alike into one clip on a new
//! instrument track: per-note controllers and pitch, and poly pressure
//! while its note sounds, become note expression; controllers become lanes
//! at MIDI 1.0's resolution; tempo, meter and key go to the project when it
//! is imported at the start of an empty one.

use crate::midi::{RecExpression, RecNote, native_expressions};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_midi::MidiEvent;
use faderframe_midi::clipfile::ClipFile;
use faderframe_midi::theory::{Key, Scale};
use faderframe_midi::ump::{
    Flex, Form, Message, Ump, Voice2, per_note_expression, per_note_message, scale_down, scale_up,
    sysex7, text_of, text_pieces, unit_of,
};
use faderframe_project::harmony::KeyChange;
use faderframe_project::{
    Clip, ClipContent, Command, ControllerLane, ControllerPoint, ExpressionKind, MidiClip,
    MidiController, MidiNote, SysexEvent, Track, TrackColor, TrackKind,
};
use faderframe_timeline::{MusicalTime, TICKS_PER_QUARTER, TimeSignature};
use std::collections::HashMap;
use std::path::Path;

/// Ticks per quarter of written files.
const FILE_TPQ: u16 = 960;

/// Is `path` a MIDI 2.0 clip file (by its extension)?
pub fn is_midi2_clip_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("midi2"))
}

fn err(e: impl std::fmt::Display) -> SessionError {
    SessionError::Other(format!("MIDI 2.0 clip file: {e}"))
}

/// Letters as the key signature message numbers them (A = 1 … G = 7).
const LETTER_OF_PC_MAJOR: [(i8, u8); 12] = [
    (0, 3),  // C
    (-5, 4), // Db
    (2, 4),  // D
    (-3, 5), // Eb
    (4, 5),  // E
    (-1, 6), // F
    (6, 6),  // F#
    (1, 7),  // G
    (-4, 1), // Ab
    (3, 1),  // A
    (-2, 2), // Bb
    (5, 2),  // B
];
const LETTER_OF_PC_MINOR: [(i8, u8); 12] = [
    (-3, 3), // C
    (4, 3),  // C#
    (-1, 4), // D
    (-6, 5), // Eb
    (1, 5),  // E
    (-4, 6), // F
    (3, 6),  // F#
    (-2, 7), // G
    (5, 7),  // G#
    (0, 1),  // A
    (-5, 2), // Bb
    (2, 2),  // B
];

/// A major or minor key as (sharps, tonic letter).
fn key_signature(key: Key) -> Option<(i8, u8)> {
    let table = match key.scale {
        Scale::Major => &LETTER_OF_PC_MAJOR,
        Scale::Minor => &LETTER_OF_PC_MINOR,
        _ => return None,
    };
    Some(table[usize::from(key.root % 12)])
}

/// The key of (sharps, tonic letter): major when the letter is the major
/// key's tonic, minor when it is the relative minor's.
fn key_of_signature(sharps: i8, tonic: u8) -> Option<Key> {
    let find = |table: &[(i8, u8); 12]| table.iter().position(|e| *e == (sharps, tonic));
    if let Some(pc) = find(&LETTER_OF_PC_MAJOR) {
        return Some(Key::new(pc as u8, Scale::Major));
    }
    find(&LETTER_OF_PC_MINOR).map(|pc| Key::new(pc as u8, Scale::Minor))
}

/// Notes sounding: (channel, key) → (start, velocity), oldest first.
type Open = HashMap<(u8, u8), Vec<(i64, u8)>>;

fn close(
    open: &mut Open,
    notes: &mut Vec<RecNote>,
    channel: u8,
    key: u8,
    end: i64,
    release: Option<u8>,
) {
    let Some(v) = open.get_mut(&(channel, key)).filter(|v| !v.is_empty()) else {
        return;
    };
    let (start, velocity) = v.remove(0);
    notes.push(RecNote {
        start,
        end: end.max(start + 1),
        key,
        velocity,
        channel,
        pass: 0,
        release,
    });
}

fn sounding(open: &Open, channel: u8, key: u8) -> bool {
    open.get(&(channel, key)).is_some_and(|v| !v.is_empty())
}

fn add_point(
    lanes: &mut Vec<ControllerLane>,
    c: MidiController,
    channel: u8,
    time: MusicalTime,
    value: u16,
) {
    let i = match lanes
        .iter()
        .position(|l| l.controller == c && l.channel == channel)
    {
        Some(i) => i,
        None => {
            lanes.push(ControllerLane::new(c, channel));
            lanes.len() - 1
        }
    };
    lanes[i].points.push(ControllerPoint { time, value });
}

/// What the import holds while it reads.
#[derive(Default)]
struct Reading {
    open: Open,
    notes: Vec<RecNote>,
    lanes: Vec<ControllerLane>,
}

impl Reading {
    /// A MIDI 1.0 channel message at `pos` (`time` in the clip).
    fn midi1(&mut self, ev: MidiEvent, pos: i64, time: MusicalTime) {
        match ev {
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } if velocity > 0 => self
                .open
                .entry((channel, key))
                .or_default()
                .push((pos, velocity)),
            MidiEvent::NoteOn { channel, key, .. } => {
                close(&mut self.open, &mut self.notes, channel, key, pos, None)
            }
            MidiEvent::NoteOff {
                channel,
                key,
                velocity,
            } => close(
                &mut self.open,
                &mut self.notes,
                channel,
                key,
                pos,
                Some(velocity),
            ),
            ev => {
                if let Some((c, ch, v)) = MidiController::of_event(ev) {
                    add_point(&mut self.lanes, c, ch, time, v);
                }
            }
        }
    }
}

fn channel_len(status: u8) -> usize {
    if matches!(status & 0xF0, 0xC0 | 0xD0) {
        2
    } else {
        3
    }
}

fn midi2(channel: u8, voice: Voice2) -> Ump {
    Message::Midi2 {
        group: 0,
        channel,
        voice,
    }
    .to_ump()
}

impl Session {
    /// The clip Export MIDI 2.0 Clip writes: the first selected MIDI clip,
    /// else the first MIDI clip of the first selected track.
    pub fn midi2_export_clip(&self) -> Option<ClipId> {
        let p = &self.project;
        let is_midi = |c: &ClipId| p.clip(*c).is_some_and(|c| c.as_midi().is_some());
        let mut selected: Vec<ClipId> = self
            .selection
            .clips
            .iter()
            .copied()
            .filter(is_midi)
            .collect();
        selected.sort_by_key(|c| p.clip(*c).map(|c| c.start));
        selected.first().copied().or_else(|| {
            p.tracks
                .iter()
                .filter(|t| self.selection.tracks.contains(&t.id))
                .find_map(|t| {
                    p.clips_of(t.id)
                        .into_iter()
                        .filter(|c| c.as_midi().is_some())
                        .min_by_key(|c| c.start)
                        .map(|c| c.id)
                })
        })
    }

    /// Write a MIDI clip as a MIDI 2.0 clip file.
    pub fn export_midi2_clip(&self, path: &Path, clip: ClipId) -> Result<()> {
        let p = &self.project;
        let c = p.clip(clip).ok_or_else(|| err("no such clip"))?;
        let m = c
            .as_midi()
            .ok_or_else(|| err("only MIDI clips are written"))?;
        let tick = |t: MusicalTime| -> u64 {
            (i128::from(t.ticks().max(0)) * i128::from(FILE_TPQ) / i128::from(TICKS_PER_QUARTER))
                as u64
        };
        // (tick, order at the tick, packet): offs, settings, ons, expression.
        let mut events: Vec<(u64, u8, Ump)> = Vec::new();
        let tl = &p.timeline;
        let flex = |f: Flex| Message::Flex(f).to_ump();
        // Tempo and meter over the clip.
        events.push((
            0,
            1,
            flex(Flex::Tempo {
                group: 0,
                tens_of_ns: Flex::tens_of_ns(tl.tempo.bpm_at(c.start)),
            }),
        ));
        for pt in tl.tempo.points() {
            if pt.position > c.start && pt.position < c.start + m.length {
                events.push((
                    tick(pt.position - c.start),
                    1,
                    flex(Flex::Tempo {
                        group: 0,
                        tens_of_ns: Flex::tens_of_ns(pt.bpm),
                    }),
                ));
            }
        }
        let signature = |at: MusicalTime| {
            let s = tl.meter.signature_at(at);
            flex(Flex::TimeSignature {
                group: 0,
                numerator: s.numerator,
                denominator_power: s.denominator.trailing_zeros() as u8,
            })
        };
        events.push((0, 1, signature(c.start)));
        let mut bar = tl.meter.bar_at(c.start) + 1;
        while tl.meter.bar_start(bar) < c.start + m.length {
            let at = tl.meter.bar_start(bar);
            if tl.meter.signature_at(at) != tl.meter.signature_at(tl.meter.bar_start(bar - 1)) {
                events.push((tick(at - c.start), 1, signature(at)));
            }
            bar += 1;
        }
        if let Some((sharps, tonic)) = p.key_at(c.start).and_then(key_signature) {
            events.push((
                0,
                1,
                flex(Flex::KeySignature {
                    group: 0,
                    sharps,
                    tonic,
                }),
            ));
        }
        // The clip's name (metadata text, MIDI Clip Name).
        for (form, bytes) in text_pieces::<12>(&c.name) {
            events.push((
                0,
                0,
                flex(Flex::Text {
                    group: 0,
                    form,
                    bank: 1,
                    status: 3,
                    bytes,
                }),
            ));
        }
        // Notes and their expression.
        let mut by_id = HashMap::new();
        for n in m.notes.iter().filter(|n| !n.muted && n.start < m.length) {
            by_id.insert(n.id, *n);
            let attribute = faderframe_midi::ump::Attribute::default();
            events.push((
                tick(n.start),
                2,
                midi2(
                    n.channel,
                    Voice2::NoteOn {
                        note: n.key,
                        velocity: scale_up(u32::from(n.velocity.clamp(1, 127)), 7, 16) as u16,
                        attribute,
                    },
                ),
            ));
            let end = (n.start + n.length).min(m.length);
            events.push((
                tick(end),
                0,
                midi2(
                    n.channel,
                    Voice2::NoteOff {
                        note: n.key,
                        velocity: scale_up(u32::from(n.release.unwrap_or(0).min(127)), 7, 16)
                            as u16,
                        attribute,
                    },
                ),
            ));
        }
        let step = MusicalTime(TICKS_PER_QUARTER / 128);
        for e in &m.expressions {
            let Some(n) = by_id.get(&e.note) else {
                continue;
            };
            for kind in ExpressionKind::ALL {
                if e.curve(kind).is_empty() {
                    continue;
                }
                let out = |v: f32| per_note_message(n.key, kind.native(), f64::from(v));
                let mut last = e.value_at(kind, MusicalTime::ZERO);
                events.push((tick(n.start), 3, midi2(n.channel, out(last))));
                let mut t = step;
                while t < n.length && n.start + t < m.length {
                    let v = e.value_at(kind, t);
                    if (v - last).abs() >= kind.resolution() {
                        events.push((tick(n.start + t), 3, midi2(n.channel, out(v))));
                        last = v;
                    }
                    t += step;
                }
            }
        }
        // Controllers (bank selects folded into the programs).
        let bank = |ch: u8, number: u8, at: MusicalTime| {
            m.lane(MidiController::Cc { number }, ch).and_then(|l| {
                l.points
                    .iter()
                    .rev()
                    .find(|p| p.time <= at)
                    .map(|p| p.value.min(127) as u8)
            })
        };
        for lane in &m.controllers {
            let ch = lane.channel;
            for pt in lane.points.iter().filter(|pt| pt.time < m.length) {
                let v = u32::from(pt.value);
                let voice = match lane.controller {
                    MidiController::Cc { number: 0 | 32 } => continue,
                    MidiController::Cc { number } => Voice2::ControlChange {
                        index: number,
                        value: scale_up(v.min(127), 7, 32),
                    },
                    MidiController::PitchBend => Voice2::PitchBend(scale_up(v.min(0x3FFF), 14, 32)),
                    MidiController::ChannelPressure => {
                        Voice2::ChannelPressure(scale_up(v.min(127), 7, 32))
                    }
                    MidiController::Program => Voice2::ProgramChange {
                        program: v.min(127) as u8,
                        bank: match (bank(ch, 0, pt.time), bank(ch, 32, pt.time)) {
                            (None, None) => None,
                            (msb, lsb) => Some((msb.unwrap_or(0), lsb.unwrap_or(0))),
                        },
                    },
                    MidiController::PolyPressure { key } => Voice2::PolyPressure {
                        note: key,
                        value: scale_up(v.min(127), 7, 32),
                    },
                };
                events.push((tick(pt.time), 1, midi2(ch, voice)));
            }
        }
        for x in m.sysex.iter().filter(|x| x.time < m.length) {
            let data = x.data.strip_prefix(&[0xF0]).unwrap_or(&x.data);
            let data = data.strip_suffix(&[0xF7]).unwrap_or(data);
            for p in sysex7(0, data) {
                events.push((tick(x.time), 1, p));
            }
        }
        events.sort_by_key(|(t, order, _)| (*t, *order));
        let file = ClipFile {
            ticks_per_quarter: FILE_TPQ,
            events: events.into_iter().map(|(t, _, p)| (t, p)).collect(),
            length: tick(m.length),
        };
        std::fs::write(path, file.write()).map_err(|e| err(format!("{}: {e}", path.display())))
    }

    /// Import a MIDI 2.0 clip file at `at` (see the module docs); with
    /// `tempo`, at the start, its tempo, meter and key become the
    /// project's. Returns the new track.
    pub fn import_midi2_clip(
        &mut self,
        path: &Path,
        at: MusicalTime,
        tempo: bool,
    ) -> Result<TrackId> {
        let bytes = std::fs::read(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
        let file = ClipFile::read(&bytes).map_err(err)?;
        let tpq = i128::from(file.ticks_per_quarter.max(1));
        let musical =
            |t: u64| MusicalTime((i128::from(t) * i128::from(TICKS_PER_QUARTER) / tpq) as i64);
        let mut name_bytes = Vec::new();
        let mut name = None;
        let mut tempos: Vec<(MusicalTime, f64)> = Vec::new();
        let mut meters: Vec<(MusicalTime, TimeSignature)> = Vec::new();
        let mut key = None;
        // Notes as recorded ones (ticks as positions), their expression.
        let mut r = Reading::default();
        let mut expression: Vec<RecExpression> = Vec::new();
        let mut sysex: Vec<SysexEvent> = Vec::new();
        let mut sysex_buf: Option<(u64, Vec<u8>)> = None;
        for &(t, p) in &file.events {
            let pos = t as i64;
            let time = musical(t);
            match Message::parse(&p) {
                Message::Midi1 { bytes, .. } => {
                    if let Some(ev) = MidiEvent::from_bytes(&bytes[..channel_len(bytes[0])]) {
                        r.midi1(ev, pos, time);
                    }
                }
                Message::Midi2 { channel, voice, .. } => match voice {
                    Voice2::NoteOn {
                        note,
                        velocity,
                        attribute,
                    } => {
                        let v = (scale_down(u32::from(velocity), 16, 7) as u8).max(1);
                        r.open.entry((channel, note)).or_default().push((pos, v));
                        if let Some(pitch) = attribute.pitch() {
                            expression.push((
                                pos,
                                channel,
                                note,
                                ExpressionKind::Pitch,
                                (pitch - f64::from(note)) as f32,
                                0,
                            ));
                        }
                    }
                    Voice2::NoteOff { note, velocity, .. } => close(
                        &mut r.open,
                        &mut r.notes,
                        channel,
                        note,
                        pos,
                        Some(scale_down(u32::from(velocity), 16, 7) as u8),
                    ),
                    // Poly pressure while its note sounds: the note's.
                    Voice2::PolyPressure { note, value } if sounding(&r.open, channel, note) => {
                        expression.push((
                            pos,
                            channel,
                            note,
                            ExpressionKind::Pressure,
                            unit_of(value) as f32,
                            0,
                        ));
                    }
                    Voice2::PerNoteManagement { .. } => {}
                    v => {
                        if let Some((note, kind, value)) = per_note_expression(&v) {
                            expression.push((
                                pos,
                                channel,
                                note,
                                ExpressionKind::of_native(kind),
                                value as f32,
                                0,
                            ));
                        } else {
                            let (msgs, n) = v.to_midi1(channel);
                            for b in &msgs[..n] {
                                if let Some(ev) = MidiEvent::from_bytes(&b[..channel_len(b[0])]) {
                                    r.midi1(ev, pos, time);
                                }
                            }
                        }
                    }
                },
                Message::Sysex7 {
                    form, bytes, len, ..
                } => {
                    if matches!(form, Form::Complete | Form::Start) {
                        sysex_buf = Some((t, vec![0xF0]));
                    }
                    if let Some((_, buf)) = sysex_buf.as_mut() {
                        buf.extend_from_slice(&bytes[..usize::from(len.min(6))]);
                    }
                    if matches!(form, Form::Complete | Form::End)
                        && let Some((at, mut data)) = sysex_buf.take()
                    {
                        data.push(0xF7);
                        sysex.push(SysexEvent {
                            time: musical(at),
                            data,
                        });
                    }
                }
                Message::Flex(Flex::Tempo { tens_of_ns, .. }) => {
                    tempos.push((musical(t), Flex::bpm(tens_of_ns)));
                }
                Message::Flex(Flex::TimeSignature {
                    numerator,
                    denominator_power,
                    ..
                }) => {
                    if let Some(s) = TimeSignature::new(
                        numerator,
                        1u8.checked_shl(denominator_power.into()).unwrap_or(4),
                    ) {
                        meters.push((musical(t), s));
                    }
                }
                Message::Flex(Flex::KeySignature { sharps, tonic, .. }) if key.is_none() => {
                    key = key_of_signature(sharps, tonic);
                }
                Message::Flex(Flex::Text {
                    form,
                    bank: 1,
                    status: 3,
                    bytes,
                    ..
                }) => {
                    if matches!(form, Form::Complete | Form::Start) {
                        name_bytes.clear();
                    }
                    name_bytes.extend_from_slice(&bytes);
                    if matches!(form, Form::Complete | Form::End) {
                        name = Some(text_of(&name_bytes));
                    }
                }
                _ => {}
            }
        }
        // Notes still held at the end last until there.
        let end = file.length as i64;
        let held: Vec<(u8, u8)> = r.open.keys().copied().collect();
        for (ch, k) in held {
            while sounding(&r.open, ch, k) {
                close(&mut r.open, &mut r.notes, ch, k, end, None);
            }
        }
        let Reading {
            mut notes,
            mut lanes,
            ..
        } = r;
        if notes.is_empty() && lanes.is_empty() && sysex.is_empty() {
            return Err(err("the clip has no notes, controllers or SysEx"));
        }
        notes.sort_by_key(|n| (n.start, n.key));
        for l in &mut lanes {
            l.points.sort_by_key(|pt| pt.time);
        }
        let p = &mut self.project;
        let ids: Vec<faderframe_core::NoteId> = notes.iter().map(|_| p.ids.allocate()).collect();
        let expressions = native_expressions(&notes, &ids, &expression, 0.0, |pos, start| {
            musical(pos.max(0) as u64) - musical(start.max(0) as u64)
        });
        let midi_notes: Vec<MidiNote> = notes
            .iter()
            .zip(&ids)
            .map(|(n, &id)| {
                let start = musical(n.start as u64);
                MidiNote {
                    id,
                    start,
                    length: (musical(n.end as u64) - start).max(MusicalTime(1)),
                    key: n.key,
                    velocity: n.velocity.max(1),
                    channel: n.channel,
                    muted: false,
                    release: n.release,
                }
            })
            .collect();
        let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            path.file_stem().map_or_else(
                || "MIDI 2.0".to_string(),
                |s| s.to_string_lossy().to_string(),
            )
        });
        let length = musical(file.length)
            .max(
                midi_notes
                    .iter()
                    .map(|n| n.start + n.length)
                    .max()
                    .unwrap_or(MusicalTime::ZERO),
            )
            .max(MusicalTime::from_quarters(1.0));
        let mut cmds = Vec::new();
        if tempo && at == MusicalTime::ZERO {
            if !tempos.is_empty() || !meters.is_empty() {
                cmds.push(self.imported_timeline(tempos, meters));
            }
            if let Some(key) = key
                && self.project.keys.is_empty()
            {
                cmds.push(Command::SetKeys {
                    keys: vec![KeyChange {
                        at: MusicalTime::ZERO,
                        key,
                    }],
                });
            }
        }
        let p = &mut self.project;
        let index = p
            .tracks
            .iter()
            .rposition(|t| t.kind.has_clips())
            .map_or(0, |i| i + 1);
        let track_id: TrackId = p.ids.allocate();
        let color = TrackColor::palette(p.tracks.len());
        cmds.push(Command::AddTrack {
            track: Box::new(Track::new(
                track_id,
                TrackKind::Instrument,
                name.clone(),
                color,
            )),
            index,
        });
        let clip_id: ClipId = p.ids.allocate();
        cmds.push(Command::AddClip {
            clip: Box::new(Clip {
                id: clip_id,
                track: track_id,
                name: name.clone(),
                color: None,
                start: at,
                muted: false,
                content: ClipContent::Midi(MidiClip {
                    length,
                    notes: midi_notes,
                    controllers: lanes,
                    expressions,
                    sysex,
                }),
            }),
        });
        self.batch("Import MIDI 2.0 Clip", cmds)?;
        self.selection
            .select_tracks(&[track_id], crate::SelectMode::Replace);
        self.notify(
            NoticeLevel::Info,
            format!("imported the MIDI 2.0 clip ‘{name}’ — choose an instrument for it"),
        );
        Ok(track_id)
    }
}

//! Standard MIDI Files (`.mid`) in and out, through `midly`.
//!
//! Import: one new instrument track per file track (format 0 files are
//! split by MIDI channel), each with one clip holding the notes, controllers
//! (CC, pitch bend, channel pressure) and SysEx; optionally the file's tempo
//! map and time signatures. Export: a format-1 file with a tempo/meter
//! track and one track per instrument or MIDI track (or only the given
//! clips).

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::{
    Clip, ClipContent, Command, ControllerLane, ControllerPoint, MidiClip, MidiController,
    MidiNote, SysexEvent, Track, TrackColor, TrackKind,
};
use faderframe_timeline::{
    MeterChange, MusicalTime, TICKS_PER_QUARTER, TempoCurve, TempoPoint, TimeSignature,
};
use midly::num::{u4, u7, u14, u15, u24, u28};
use midly::{
    Format, Header, MetaMessage, MidiMessage, PitchBend, Smf, Timing, TrackEvent, TrackEventKind,
};
use std::collections::HashMap;
use std::path::Path;

/// Is `path` a Standard MIDI File (by its extension)?
pub fn is_midi_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "mid" | "midi" | "smf" | "kar"
        )
    })
}

/// Resolution of exported files (ticks per quarter).
const EXPORT_PPQ: u16 = 960;

fn err(e: impl std::fmt::Display) -> SessionError {
    SessionError::Other(format!("MIDI file: {e}"))
}

/// One destination track of an import.
#[derive(Default)]
struct Part {
    name: String,
    /// (absolute tick, event) in file order.
    events: Vec<(u64, TrackEventKind<'static>)>,
    sysex: Vec<(u64, Vec<u8>)>,
}

fn musical(tick: u64, ppq: u64) -> MusicalTime {
    MusicalTime((tick as i128 * TICKS_PER_QUARTER as i128 / ppq.max(1) as i128) as i64)
}

impl Session {
    /// Import a MIDI file at `at`; with `tempo`, the file's tempo map and
    /// time signatures replace the project's (only when importing at the
    /// start). Returns the new tracks.
    pub fn import_midi_file(
        &mut self,
        path: &Path,
        at: MusicalTime,
        tempo: bool,
    ) -> Result<Vec<TrackId>> {
        let bytes = std::fs::read(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
        let smf = Smf::parse(&bytes).map_err(err)?;
        let ppq = match smf.header.timing {
            Timing::Metrical(t) => t.as_int().max(1) as u64,
            // Timecode: ticks per second, read at 120 BPM.
            Timing::Timecode(fps, per_frame) => {
                ((fps.as_f32() * per_frame as f32) / 2.0).round().max(1.0) as u64
            }
        };
        let base = path
            .file_stem()
            .map_or_else(|| "MIDI".to_string(), |s| s.to_string_lossy().to_string());

        // Absolute ticks per file track; tempo and meter from all tracks.
        let mut tempos: Vec<(u64, f64)> = Vec::new();
        let mut meters: Vec<(u64, TimeSignature)> = Vec::new();
        let mut file_tracks: Vec<Part> = Vec::new();
        for (i, track) in smf.tracks.iter().enumerate() {
            let mut part = Part {
                name: format!("{base} {}", i + 1),
                ..Part::default()
            };
            let mut tick = 0u64;
            let mut pending: Option<(u64, Vec<u8>)> = None;
            for e in track {
                tick += e.delta.as_int() as u64;
                match e.kind {
                    TrackEventKind::Meta(MetaMessage::TrackName(n)) if !n.is_empty() => {
                        part.name = String::from_utf8_lossy(n).trim().to_string();
                    }
                    TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                        tempos.push((tick, 60_000_000.0 / us.as_int().max(1) as f64));
                    }
                    TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) => {
                        if let Some(sig) =
                            TimeSignature::new(n, 1u8.checked_shl(d as u32).unwrap_or(4))
                        {
                            meters.push((tick, sig));
                        }
                    }
                    TrackEventKind::SysEx(data) => {
                        let mut msg = vec![0xF0];
                        msg.extend_from_slice(data);
                        if msg.last() == Some(&0xF7) {
                            part.sysex.push((tick, msg));
                        } else {
                            pending = Some((tick, msg));
                        }
                    }
                    TrackEventKind::Escape(data) => {
                        if let Some((t, mut msg)) = pending.take() {
                            msg.extend_from_slice(data);
                            if msg.last() == Some(&0xF7) {
                                part.sysex.push((t, msg));
                            } else {
                                pending = Some((t, msg));
                            }
                        }
                    }
                    TrackEventKind::Midi { channel, message } => {
                        part.events
                            .push((tick, TrackEventKind::Midi { channel, message }));
                    }
                    TrackEventKind::Meta(_) => {}
                }
            }
            file_tracks.push(part);
        }

        // Format 0: one part per channel.
        let parts: Vec<Part> = if smf.header.format == Format::SingleTrack {
            let mut by_channel: Vec<(u8, Part)> = Vec::new();
            let mut sysex = Vec::new();
            for p in file_tracks {
                sysex.extend(p.sysex);
                for (tick, ev) in p.events {
                    let TrackEventKind::Midi { channel, .. } = ev else {
                        continue;
                    };
                    let ch = channel.as_int();
                    let i = match by_channel.iter().position(|(c, _)| *c == ch) {
                        Some(i) => i,
                        None => {
                            by_channel.push((
                                ch,
                                Part {
                                    name: if ch == 9 {
                                        format!("{base} Drums")
                                    } else {
                                        format!("{base} Ch {}", ch + 1)
                                    },
                                    ..Part::default()
                                },
                            ));
                            by_channel.len() - 1
                        }
                    };
                    by_channel[i].1.events.push((tick, ev));
                }
            }
            by_channel.sort_by_key(|(c, _)| *c);
            let mut parts: Vec<Part> = by_channel.into_iter().map(|(_, p)| p).collect();
            if let Some(first) = parts.first_mut() {
                first.sysex = sysex;
            } else if !sysex.is_empty() {
                parts.push(Part {
                    name: base.clone(),
                    sysex,
                    ..Part::default()
                });
            }
            parts
        } else {
            file_tracks
                .into_iter()
                .filter(|p| !p.events.is_empty() || !p.sysex.is_empty())
                .collect()
        };
        if parts.is_empty() {
            return Err(err("the file has no notes, controllers or SysEx"));
        }

        let mut cmds = Vec::new();
        if tempo && at == MusicalTime::ZERO && (!tempos.is_empty() || !meters.is_empty()) {
            let mut tl = self.project.timeline.clone();
            tempos.sort_by_key(|(t, _)| *t);
            while tl.tempo.points().len() > 1 {
                tl.tempo.remove_point(1);
            }
            let first = tempos
                .first()
                .filter(|(t, _)| *t == 0)
                .map_or(120.0, |(_, b)| *b);
            tl.tempo.set_initial_bpm(first.clamp(10.0, 999.0));
            for &(t, bpm) in tempos.iter().filter(|(t, _)| *t > 0) {
                tl.tempo.set_point(TempoPoint {
                    position: musical(t, ppq),
                    bpm: bpm.clamp(10.0, 999.0),
                    curve: TempoCurve::Constant,
                });
            }
            meters.sort_by_key(|(t, _)| *t);
            let mut meter = faderframe_timeline::TimeSignatureMap::new(
                meters
                    .first()
                    .filter(|(t, _)| *t == 0)
                    .map_or(TimeSignature::FOUR_FOUR, |(_, s)| *s),
            );
            for &(t, signature) in meters.iter().filter(|(t, _)| *t > 0) {
                let pos = musical(t, ppq);
                let mut bar = meter.bar_at(pos);
                if meter.bar_start(bar) != pos {
                    bar += 1; // changes land on bar lines
                }
                meter.set_change(MeterChange { bar, signature });
            }
            tl.meter = meter;
            cmds.push(Command::SetTimeline {
                timeline: Box::new(tl),
            });
        }

        let p = &mut self.project;
        let mut index = p
            .tracks
            .iter()
            .rposition(|t| t.kind.has_clips())
            .map_or(0, |i| i + 1);
        let mut created = Vec::new();
        let mut programs = 0usize;
        for part in parts {
            let mut notes: Vec<MidiNote> = Vec::new();
            let mut open: HashMap<(u8, u8), Vec<(u64, u8)>> = HashMap::new();
            let mut lanes: Vec<ControllerLane> = Vec::new();
            let mut last = part.sysex.iter().map(|(t, _)| *t).max().unwrap_or(0);
            let mut lane = |c: MidiController, ch: u8, tick: u64, value: u16| {
                let i = match lanes
                    .iter()
                    .position(|l| l.controller == c && l.channel == ch)
                {
                    Some(i) => i,
                    None => {
                        lanes.push(ControllerLane::new(c, ch));
                        lanes.len() - 1
                    }
                };
                lanes[i].points.push(ControllerPoint {
                    time: musical(tick, ppq),
                    value,
                });
            };
            for &(tick, ref ev) in &part.events {
                last = last.max(tick);
                let TrackEventKind::Midi { channel, message } = *ev else {
                    continue;
                };
                let ch = channel.as_int();
                match message {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                        open.entry((ch, key.as_int()))
                            .or_default()
                            .push((tick, vel.as_int()));
                    }
                    MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                        let k = key.as_int();
                        if let Some(stack) = open.get_mut(&(ch, k))
                            && !stack.is_empty()
                        {
                            let (start, vel) = stack.remove(0);
                            notes.push(MidiNote {
                                id: p.ids.allocate(),
                                start: musical(start, ppq),
                                length: (musical(tick, ppq) - musical(start, ppq))
                                    .max(MusicalTime(1)),
                                key: k,
                                velocity: vel,
                                channel: ch,
                                muted: false,
                            });
                        }
                    }
                    MidiMessage::Controller { controller, value } => lane(
                        MidiController::Cc {
                            number: controller.as_int(),
                        },
                        ch,
                        tick,
                        value.as_int() as u16,
                    ),
                    MidiMessage::PitchBend { bend } => {
                        lane(MidiController::PitchBend, ch, tick, bend.0.as_int());
                    }
                    MidiMessage::ChannelAftertouch { vel } => {
                        lane(
                            MidiController::ChannelPressure,
                            ch,
                            tick,
                            vel.as_int() as u16,
                        );
                    }
                    MidiMessage::ProgramChange { .. } => programs += 1,
                    MidiMessage::Aftertouch { .. } => {}
                }
            }
            // Notes still held at the end last until there.
            for ((ch, key), stack) in open {
                for (start, vel) in stack {
                    notes.push(MidiNote {
                        id: p.ids.allocate(),
                        start: musical(start, ppq),
                        length: (musical(last, ppq) - musical(start, ppq))
                            .max(MusicalTime::from_quarters(0.25)),
                        key,
                        velocity: vel,
                        channel: ch,
                        muted: false,
                    });
                }
            }
            notes.sort_by_key(|n| (n.start, n.key));
            for l in &mut lanes {
                l.points.sort_by_key(|pt| pt.time);
            }
            // Length: to the bar after the last event.
            let end = notes
                .iter()
                .map(|n| n.start + n.length)
                .chain([musical(last, ppq)])
                .max()
                .unwrap_or(MusicalTime::ZERO);
            let meter = &p.timeline.meter;
            let end_abs = at + end;
            let bar = meter.bar_at(end_abs);
            let bar_end = if meter.bar_start(bar) == end_abs && end > MusicalTime::ZERO {
                end_abs
            } else {
                meter.bar_start(bar + 1)
            };
            let length = (bar_end - at).max(MusicalTime::from_quarters(1.0));
            let track_id: TrackId = p.ids.allocate();
            let color = TrackColor::palette(p.tracks.len() + created.len());
            let track = Track::new(track_id, TrackKind::Instrument, part.name.clone(), color);
            cmds.push(Command::AddTrack {
                track: Box::new(track),
                index,
            });
            index += 1;
            let clip_id: ClipId = p.ids.allocate();
            cmds.push(Command::AddClip {
                clip: Box::new(Clip {
                    id: clip_id,
                    track: track_id,
                    name: part.name,
                    color: None,
                    start: at,
                    muted: false,
                    content: ClipContent::Midi(MidiClip {
                        length,
                        notes,
                        controllers: lanes,
                        expressions: Vec::new(),
                        sysex: part
                            .sysex
                            .into_iter()
                            .map(|(t, data)| SysexEvent {
                                time: musical(t, ppq),
                                data,
                            })
                            .collect(),
                    }),
                }),
            });
            created.push(track_id);
        }
        self.batch("Import MIDI File", cmds)?;
        self.selection
            .select_tracks(&created, crate::SelectMode::Replace);
        let n = created.len();
        let mut text = format!(
            "imported {n} track{} from {} — choose an instrument for {}",
            if n == 1 { "" } else { "s" },
            path.file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().to_string()),
            if n == 1 { "it" } else { "each" }
        );
        if programs > 0 {
            text.push_str(" (program changes are not imported)");
        }
        self.notify(crate::NoticeLevel::Info, text);
        Ok(created)
    }

    /// Write the MIDI of instrument and MIDI tracks (or only `clips`) as a
    /// format-1 MIDI file. Returns the number of tracks written.
    pub fn export_midi_file(&self, path: &Path, clips: Option<&[ClipId]>) -> Result<usize> {
        let p = &self.project;
        let tick = |t: MusicalTime| -> u64 {
            (t.ticks().max(0) as i128 * EXPORT_PPQ as i128 / TICKS_PER_QUARTER as i128) as u64
        };
        // Owned bytes the events borrow (names, SysEx).
        enum Ev {
            Midi(u8, MidiMessage),
            Name(Vec<u8>),
            Tempo(u32),
            Meter(u8, u8),
            SysEx(Vec<u8>),
        }
        // (tick, order at equal ticks: note-offs first, note-ons last)
        let mut tracks: Vec<Vec<(u64, u8, Ev)>> = Vec::new();
        let mut conductor = vec![(0u64, 0u8, Ev::Name(p.name.as_bytes().to_vec()))];
        for pt in p.timeline.tempo.points() {
            conductor.push((
                tick(pt.position),
                1,
                Ev::Tempo((60_000_000.0 / pt.bpm.max(1.0)).round() as u32),
            ));
        }
        for c in p.timeline.meter.changes() {
            conductor.push((
                tick(p.timeline.meter.bar_start(c.bar)),
                1,
                Ev::Meter(c.signature.numerator, c.signature.denominator),
            ));
        }
        tracks.push(conductor);
        for t in p
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Instrument | TrackKind::Midi))
        {
            let mut evs = vec![(0u64, 0u8, Ev::Name(t.name.as_bytes().to_vec()))];
            for c in p.clips_of(t.id) {
                if c.muted || clips.is_some_and(|only| !only.contains(&c.id)) {
                    continue;
                }
                let Some(m) = c.as_midi() else { continue };
                let end = c.start + m.length;
                for n in m.notes.iter().filter(|n| !n.muted && n.start < m.length) {
                    let on = c.start + n.start;
                    let off = (on + n.length).min(end);
                    let (ch, key, vel) =
                        (n.channel & 0x0F, u7::new(n.key), u7::new(n.velocity.max(1)));
                    evs.push((tick(on), 2, Ev::Midi(ch, MidiMessage::NoteOn { key, vel })));
                    evs.push((
                        tick(off),
                        0,
                        Ev::Midi(
                            ch,
                            MidiMessage::NoteOff {
                                key,
                                vel: u7::new(0),
                            },
                        ),
                    ));
                }
                for l in &m.controllers {
                    for pt in l.points.iter().filter(|pt| pt.time < m.length) {
                        let msg = match l.controller {
                            MidiController::Cc { number } => MidiMessage::Controller {
                                controller: u7::new(number),
                                value: u7::new(pt.value.min(127) as u8),
                            },
                            MidiController::PitchBend => MidiMessage::PitchBend {
                                bend: PitchBend(u14::new(pt.value.min(16383))),
                            },
                            MidiController::ChannelPressure => MidiMessage::ChannelAftertouch {
                                vel: u7::new(pt.value.min(127) as u8),
                            },
                        };
                        evs.push((tick(c.start + pt.time), 1, Ev::Midi(l.channel & 0x0F, msg)));
                    }
                }
                for s in m.sysex.iter().filter(|s| s.time < m.length) {
                    evs.push((
                        tick(c.start + s.time),
                        1,
                        Ev::SysEx(s.data.get(1..).unwrap_or(&[]).to_vec()),
                    ));
                }
            }
            if evs.len() > 1 {
                tracks.push(evs);
            }
        }
        let written = tracks.len() - 1;
        if written == 0 {
            return Err(err("there is no MIDI to export"));
        }
        let mut smf = Smf::new(Header::new(
            Format::Parallel,
            Timing::Metrical(u15::new(EXPORT_PPQ)),
        ));
        for evs in &mut tracks {
            evs.sort_by_key(|(t, order, _)| (*t, *order));
        }
        for evs in &tracks {
            let mut out: Vec<TrackEvent<'_>> = Vec::with_capacity(evs.len() + 1);
            let mut last = 0u64;
            for (t, _, ev) in evs {
                let delta = u28::new((t - last).min(0x0FFF_FFFF) as u32);
                last = *t;
                let kind = match ev {
                    Ev::Midi(ch, message) => TrackEventKind::Midi {
                        channel: u4::new(*ch),
                        message: *message,
                    },
                    Ev::Name(n) => TrackEventKind::Meta(MetaMessage::TrackName(n)),
                    Ev::Tempo(us) => TrackEventKind::Meta(MetaMessage::Tempo(u24::new(*us))),
                    Ev::Meter(n, d) => TrackEventKind::Meta(MetaMessage::TimeSignature(
                        *n,
                        d.max(&1).trailing_zeros() as u8,
                        24,
                        8,
                    )),
                    Ev::SysEx(data) => TrackEventKind::SysEx(data),
                };
                out.push(TrackEvent { delta, kind });
            }
            out.push(TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            });
            smf.tracks.push(out);
        }
        smf.save(path)
            .map_err(|e| err(format!("{}: {e}", path.display())))?;
        Ok(written)
    }
}

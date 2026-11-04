//! A Standard MIDI File read for the media browser: its channel messages
//! in quarters (every track merged), its tempo map, its length and key —
//! enough to list it and to preview it, nothing imported.

use faderframe_project::harmony::{self, Key, Sounding};
use faderframe_timeline::MusicalTime;
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use std::path::Path;

/// A MIDI file's notes and controls.
#[derive(Clone, Debug, PartialEq)]
pub struct SmfNotes {
    /// (quarters from the start, message bytes), in order.
    pub events: Vec<(f64, [u8; 3])>,
    /// (quarter, beats a minute): where the tempo changes (one at 0).
    pub tempos: Vec<(f64, f64)>,
    /// The last event's quarter.
    pub quarters: f64,
    /// 0, 1 or 2.
    pub format: u8,
    pub tracks: usize,
    pub channels: u16,
}

/// Quarters a minute when a file says nothing.
const DEFAULT_BPM: f64 = 120.0;

impl SmfNotes {
    /// The first tempo.
    pub fn bpm(&self) -> f64 {
        self.tempos.first().map_or(DEFAULT_BPM, |t| t.1)
    }

    /// Seconds to quarter `q` at the file's tempos.
    pub fn seconds_at(&self, q: f64) -> f64 {
        let mut s = 0.0;
        let mut at = 0.0;
        let mut bpm = DEFAULT_BPM;
        for &(tq, tb) in &self.tempos {
            if tq >= q {
                break;
            }
            s += (tq - at).max(0.0) * 60.0 / bpm;
            at = tq.max(at);
            bpm = tb;
        }
        s + (q - at).max(0.0) * 60.0 / bpm
    }

    /// The length in quarters, up to the next bar of four.
    pub fn bar_quarters(&self) -> f64 {
        ((self.quarters / 4.0).ceil() * 4.0).max(4.0)
    }

    pub fn seconds(&self) -> f64 {
        self.seconds_at(self.quarters)
    }

    /// The key its notes (drums aside) are in.
    pub fn key(&self) -> Option<Key> {
        let mut on: [[Option<f64>; 128]; 16] = [[None; 128]; 16];
        let mut notes = Vec::new();
        let at = |q: f64| MusicalTime::from_quarters(q);
        for &(q, b) in &self.events {
            let ch = usize::from(b[0] & 15);
            if ch == 9 {
                continue;
            }
            let key = usize::from(b[1] & 127);
            match b[0] & 0xF0 {
                0x90 if b[2] > 0 => on[ch][key] = Some(q),
                0x80 | 0x90 => {
                    if let Some(s) = on[ch][key].take() {
                        notes.push(Sounding {
                            start: at(s),
                            end: at(q.max(s + 1e-3)),
                            key: key as u8,
                        });
                    }
                }
                _ => {}
            }
        }
        harmony::detect_key(&notes)
    }

    /// How many notes start in each of `points` stretches (0…1 of the
    /// busiest): a thumbnail.
    pub fn density(&self, points: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; points.max(1)];
        let len = self.quarters.max(1e-9);
        for &(q, b) in &self.events {
            if b[0] & 0xF0 == 0x90 && b[2] > 0 {
                let i = ((q / len) * points as f64) as usize;
                out[i.min(points - 1)] += 1.0;
            }
        }
        let top = out.iter().copied().fold(0.0, f32::max);
        if top > 0.0 {
            for v in &mut out {
                *v /= top;
            }
        }
        out
    }
}

/// The bytes of a channel message.
fn bytes(channel: u8, m: MidiMessage) -> [u8; 3] {
    let ch = channel & 15;
    match m {
        MidiMessage::NoteOff { key, vel } => [0x80 | ch, key.as_int(), vel.as_int()],
        MidiMessage::NoteOn { key, vel } => [0x90 | ch, key.as_int(), vel.as_int()],
        MidiMessage::Aftertouch { key, vel } => [0xA0 | ch, key.as_int(), vel.as_int()],
        MidiMessage::Controller { controller, value } => {
            [0xB0 | ch, controller.as_int(), value.as_int()]
        }
        MidiMessage::ProgramChange { program } => [0xC0 | ch, program.as_int(), 0],
        MidiMessage::ChannelAftertouch { vel } => [0xD0 | ch, vel.as_int(), 0],
        MidiMessage::PitchBend { bend } => {
            let v = bend.0.as_int();
            [0xE0 | ch, (v & 127) as u8, (v >> 7) as u8]
        }
    }
}

/// Read `path`.
pub fn read(path: &Path) -> Result<SmfNotes, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let smf = Smf::parse(&data).map_err(|e| e.to_string())?;
    let ppq = match smf.header.timing {
        Timing::Metrical(t) => f64::from(t.as_int().max(1)),
        // Ticks a second, read at 120 BPM.
        Timing::Timecode(fps, per_frame) => {
            (f64::from(fps.as_f32()) * f64::from(per_frame) / 2.0).max(1.0)
        }
    };
    let mut events: Vec<(u64, [u8; 3])> = Vec::new();
    let mut tempos: Vec<(u64, f64)> = Vec::new();
    let mut channels = 0u16;
    for track in &smf.tracks {
        let mut tick = 0u64;
        for e in track {
            tick += u64::from(e.delta.as_int());
            match e.kind {
                TrackEventKind::Midi { channel, message } => {
                    channels |= 1 << channel.as_int();
                    events.push((tick, bytes(channel.as_int(), message)));
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    let us = f64::from(us.as_int().max(1));
                    tempos.push((tick, 60_000_000.0 / us));
                }
                _ => {}
            }
        }
    }
    // In time; note-offs before note-ons at the same tick.
    events.sort_by_key(|&(t, b)| (t, b[0] & 0xF0 == 0x90 && b[2] > 0));
    tempos.sort_by_key(|a| a.0);
    tempos.dedup_by_key(|t| t.0);
    let mut tempos: Vec<(f64, f64)> = tempos
        .into_iter()
        .map(|(t, bpm)| (t as f64 / ppq, bpm))
        .collect();
    if tempos.first().is_none_or(|t| t.0 > 0.0) {
        tempos.insert(0, (0.0, tempos.first().map_or(DEFAULT_BPM, |t| t.1)));
    }
    let events: Vec<(f64, [u8; 3])> = events
        .into_iter()
        .map(|(t, b)| (t as f64 / ppq, b))
        .collect();
    let quarters = events.last().map_or(0.0, |e| e.0);
    Ok(SmfNotes {
        events,
        tempos,
        quarters,
        format: match smf.header.format {
            midly::Format::SingleTrack => 0,
            midly::Format::Parallel => 1,
            midly::Format::Sequential => 2,
        },
        tracks: smf.tracks.len(),
        channels: channels.count_ones() as u16,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use midly::num::{u4, u7, u24, u28};
    use midly::{Format, Header, TrackEvent};

    /// A one-track file: `notes` as (quarter, key, quarters long) at `bpm`.
    pub(crate) fn write(path: &Path, bpm: f64, notes: &[(f64, u8, f64)]) {
        let ppq = 480u16;
        let mut evs: Vec<(u64, TrackEventKind<'static>)> = vec![(
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new((60_000_000.0 / bpm) as u32))),
        )];
        for &(at, key, len) in notes {
            let t = |q: f64| (q * f64::from(ppq)).round() as u64;
            let msg = |on: bool| TrackEventKind::Midi {
                channel: u4::new(0),
                message: if on {
                    MidiMessage::NoteOn {
                        key: u7::new(key),
                        vel: u7::new(100),
                    }
                } else {
                    MidiMessage::NoteOff {
                        key: u7::new(key),
                        vel: u7::new(0),
                    }
                },
            };
            evs.push((t(at), msg(true)));
            evs.push((t(at + len), msg(false)));
        }
        evs.sort_by_key(|e| e.0);
        let mut last = 0;
        let mut track: Vec<TrackEvent<'static>> = evs
            .into_iter()
            .map(|(t, kind)| {
                let delta = u28::new((t - last) as u32);
                last = t;
                TrackEvent { delta, kind }
            })
            .collect();
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        let smf = Smf {
            header: Header::new(
                Format::SingleTrack,
                Timing::Metrical(midly::num::u15::new(ppq)),
            ),
            tracks: vec![track],
        };
        let mut out = Vec::new();
        smf.write_std(&mut out).unwrap();
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn a_file_reads_as_quarters_with_its_tempo_and_key() {
        let dir = std::env::temp_dir().join(format!("ff-browser-smf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("riff.mid");
        // A minor arpeggio, twice, at 90 BPM.
        let mut notes = Vec::new();
        for bar in 0..2 {
            for (i, k) in [57u8, 60, 64, 69].iter().enumerate() {
                notes.push((f64::from(bar) * 4.0 + i as f64, *k, 0.9));
            }
        }
        write(&path, 90.0, &notes);
        let smf = read(&path).unwrap();
        assert_eq!(smf.format, 0);
        assert!((smf.bpm() - 90.0).abs() < 0.01);
        assert_eq!(smf.events.len(), 16);
        assert!((smf.quarters - 7.9).abs() < 0.01);
        assert_eq!(smf.bar_quarters(), 8.0);
        assert!((smf.seconds_at(4.0) - 4.0 * 60.0 / 90.0).abs() < 1e-4);
        let key = smf.key().unwrap();
        assert_eq!(key.root, 9, "{}", key.name());
        let d = smf.density(8);
        assert_eq!(d.len(), 8);
        assert!(d.iter().all(|v| *v > 0.0));
        let _ = std::fs::remove_file(&path);
    }
}

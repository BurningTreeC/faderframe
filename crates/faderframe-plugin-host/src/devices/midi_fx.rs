//! What the MIDI effects (arpeggiator, chord, scale, note echo) share: a
//! schedule of events due later, the notes sounding at the output (counted,
//! so two sources of one key never leave it stuck), the song grid, and
//! passing on what they do not handle.
//!
//! Each effect keeps its own clock: samples since it started. Nothing
//! here allocates after construction.

use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_transport::TransportInfo;

/// An event due at `at` (the effect's clock), from `source` (an effect's
/// tag: the input note that caused it).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pending {
    pub at: u64,
    pub event: MidiEvent,
    pub source: u16,
}

/// Events due later, in a fixed amount of room.
pub(crate) struct Schedule {
    items: Box<[Pending]>,
    len: usize,
}

impl Schedule {
    pub fn new(capacity: usize) -> Self {
        let blank = Pending {
            at: 0,
            event: MidiEvent::NoteOff {
                channel: 0,
                key: 0,
                velocity: 0,
            },
            source: 0,
        };
        Self {
            items: vec![blank; capacity.max(1)].into_boxed_slice(),
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Add an event (false: no room — a note-off then has to go now).
    pub fn push(&mut self, at: u64, event: MidiEvent, source: u16) -> bool {
        if self.len == self.items.len() {
            return false;
        }
        self.items[self.len] = Pending { at, event, source };
        self.len += 1;
        true
    }

    /// The earliest event due before `end`, taken out.
    pub fn pop_due(&mut self, end: u64) -> Option<Pending> {
        let i = (0..self.len)
            .filter(|&i| self.items[i].at < end)
            .min_by_key(|&i| (self.items[i].at, is_on(&self.items[i].event)))?;
        let p = self.items[i];
        self.len -= 1;
        self.items[i] = self.items[self.len];
        Some(p)
    }

    /// Drop the events `keep` says no to.
    pub fn retain(&mut self, mut keep: impl FnMut(&Pending) -> bool) {
        let mut i = 0;
        while i < self.len {
            if keep(&self.items[i]) {
                i += 1;
            } else {
                self.len -= 1;
                self.items[i] = self.items[self.len];
            }
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

fn is_on(e: &MidiEvent) -> bool {
    matches!(e, MidiEvent::NoteOn { velocity, .. } if *velocity > 0)
}

/// The notes sounding at the output, counted per channel and key.
pub(crate) struct Sounding {
    count: Box<[[u8; 128]; 16]>,
}

impl Default for Sounding {
    fn default() -> Self {
        Self {
            count: Box::new([[0; 128]; 16]),
        }
    }
}

impl Sounding {
    /// Start a note (one already sounding is restarted: off, then on).
    pub fn on(&mut self, out: &mut MidiBuffer, offset: u32, channel: u8, key: u8, velocity: u8) {
        let c = &mut self.count[usize::from(channel & 15)][usize::from(key & 127)];
        if *c > 0 {
            let _ = out.push(TimedMidiEvent::new(
                offset,
                MidiEvent::NoteOff {
                    channel,
                    key,
                    velocity: 0,
                },
            ));
        }
        *c = c.saturating_add(1);
        let _ = out.push(TimedMidiEvent::new(
            offset,
            MidiEvent::NoteOn {
                channel,
                key,
                velocity: velocity.max(1),
            },
        ));
    }

    /// End a note: it stops once every start of it has ended.
    pub fn off(&mut self, out: &mut MidiBuffer, offset: u32, channel: u8, key: u8) {
        let c = &mut self.count[usize::from(channel & 15)][usize::from(key & 127)];
        if *c == 0 {
            return;
        }
        *c -= 1;
        if *c == 0 {
            let _ = out.push(TimedMidiEvent::new(
                offset,
                MidiEvent::NoteOff {
                    channel,
                    key,
                    velocity: 0,
                },
            ));
        }
    }

    /// Everything off now.
    pub fn all_off(&mut self, out: &mut MidiBuffer, offset: u32) {
        for (channel, keys) in self.count.iter_mut().enumerate() {
            for (key, c) in keys.iter_mut().enumerate() {
                if *c > 0 {
                    *c = 0;
                    let _ = out.push(TimedMidiEvent::new(
                        offset,
                        MidiEvent::NoteOff {
                            channel: channel as u8,
                            key: key as u8,
                            velocity: 0,
                        },
                    ));
                }
            }
        }
    }

    #[cfg(test)]
    pub fn is_sounding(&self, channel: u8, key: u8) -> bool {
        self.count[usize::from(channel & 15)][usize::from(key & 127)] > 0
    }
}

/// Play a scheduled event at `offset`.
pub(crate) fn play(sounding: &mut Sounding, out: &mut MidiBuffer, offset: u32, event: MidiEvent) {
    match event {
        MidiEvent::NoteOn {
            channel,
            key,
            velocity,
        } if velocity > 0 => sounding.on(out, offset, channel, key, velocity),
        MidiEvent::NoteOn { channel, key, .. } | MidiEvent::NoteOff { channel, key, .. } => {
            sounding.off(out, offset, channel, key);
        }
        other => {
            let _ = out.push(TimedMidiEvent::new(offset, other));
        }
    }
}

/// A note-on (with velocity) or a note-off, as `(on, channel, key,
/// velocity)`; `None` for anything else.
pub(crate) fn note(e: &MidiEvent) -> Option<(bool, u8, u8, u8)> {
    match *e {
        MidiEvent::NoteOn {
            channel,
            key,
            velocity,
        } => Some((velocity > 0, channel, key, velocity)),
        MidiEvent::NoteOff { channel, key, .. } => Some((false, channel, key, 0)),
        _ => None,
    }
}

/// Pass an event the effect does not handle (controllers, bends, …) on.
pub(crate) fn pass(out: &mut MidiBuffer, input: &MidiBuffer, e: &TimedMidiEvent) {
    match e.event {
        MidiEvent::SysEx(_) => {
            let _ = out.push_from(input, *e);
        }
        _ => {
            let _ = out.push(*e);
        }
    }
}

/// Samples per quarter at the transport's tempo.
pub(crate) fn samples_per_quarter(t: &TransportInfo, sample_rate: f64) -> f64 {
    60.0 / t.tempo.max(1.0) * sample_rate
}

/// A source tag for an input note.
pub(crate) fn source(channel: u8, key: u8) -> u16 {
    u16::from(channel & 15) << 7 | u16::from(key & 127)
}

#[cfg(test)]
pub(crate) mod rig {
    //! Run a MIDI effect over timed notes and collect what it plays.

    use crate::{ParamValues, ParameterInfo, PluginProcessContext, PluginProcessor, ProcessConfig};
    use faderframe_audio_graph::NodeIo;
    use faderframe_core::ParameterId;
    use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
    use faderframe_transport::TransportInfo;

    pub const SR: f64 = 48_000.0;
    pub const BLOCK: usize = 256;

    pub fn config() -> ProcessConfig {
        ProcessConfig {
            sample_rate: SR,
            max_block_size: BLOCK as u32,
            sidechain: false,
            double_precision: false,
        }
    }

    pub fn params(infos: Vec<ParameterInfo>, set: &[(u32, f64)]) -> ParamValues {
        let p = ParamValues::new(infos);
        for (id, v) in set {
            p.set_by_id(ParameterId(*id), *v).unwrap_or_default();
        }
        p
    }

    /// Note on (`velocity` > 0) or off at a sample.
    pub fn on(at: u64, key: u8, velocity: u8) -> (u64, MidiEvent) {
        (
            at,
            MidiEvent::NoteOn {
                channel: 0,
                key,
                velocity,
            },
        )
    }

    pub fn off(at: u64, key: u8) -> (u64, MidiEvent) {
        (
            at,
            MidiEvent::NoteOff {
                channel: 0,
                key,
                velocity: 0,
            },
        )
    }

    /// Feed `input` (sorted by sample) for `samples`, the transport
    /// playing from quarter 0 at `tempo` (or stopped); the output events
    /// with their absolute sample.
    pub fn run(
        p: &mut dyn PluginProcessor,
        input: &[(u64, MidiEvent)],
        samples: u64,
        tempo: f64,
        playing: bool,
        harmony: &crate::Harmony,
    ) -> Vec<(u64, MidiEvent)> {
        let mut ins = vec![MidiBuffer::with_capacity(256)];
        let mut outs = vec![MidiBuffer::with_capacity(256)];
        let mut got = Vec::new();
        let mut at = 0u64;
        let mut transport = TransportInfo {
            playing,
            tempo,
            sample_rate: SR,
            ..TransportInfo::default()
        };
        while at < samples {
            ins[0].clear();
            for (t, e) in input
                .iter()
                .filter(|(t, _)| (at..at + BLOCK as u64).contains(t))
            {
                ins[0]
                    .push(TimedMidiEvent::new((t - at) as u32, *e))
                    .unwrap_or_default();
            }
            transport.sample_position = at as i64;
            transport.quarter_position = at as f64 / SR * tempo / 60.0;
            let ctx = PluginProcessContext {
                transport: &transport,
                param_events: &[],
                harmony,
            };
            let mut io = NodeIo {
                frames: BLOCK,
                audio_in: &[],
                audio_out: &mut [],
                events_in: &ins,
                events_out: &mut outs,
            };
            p.process(&ctx, &mut io);
            for e in outs[0].iter() {
                got.push((at + u64::from(e.sample_offset), e.event));
            }
            at += BLOCK as u64;
        }
        got
    }

    /// Note-ons as (sample, key, velocity).
    pub fn ons(events: &[(u64, MidiEvent)]) -> Vec<(u64, u8, u8)> {
        events
            .iter()
            .filter_map(|(t, e)| match *e {
                MidiEvent::NoteOn { key, velocity, .. } if velocity > 0 => {
                    Some((*t, key, velocity))
                }
                _ => None,
            })
            .collect()
    }

    /// Every note-on matched by a later note-off of its key.
    pub fn balanced(events: &[(u64, MidiEvent)]) -> bool {
        let mut count = [0i32; 128];
        for (_, e) in events {
            match *e {
                MidiEvent::NoteOn { key, velocity, .. } if velocity > 0 => count[key as usize] += 1,
                MidiEvent::NoteOn { key, .. } | MidiEvent::NoteOff { key, .. } => {
                    count[key as usize] -= 1
                }
                _ => {}
            }
        }
        count.iter().all(|c| *c == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schedule_gives_events_in_time_order_and_offs_first() {
        let mut s = Schedule::new(4);
        let on = MidiEvent::NoteOn {
            channel: 0,
            key: 60,
            velocity: 90,
        };
        let off = MidiEvent::NoteOff {
            channel: 0,
            key: 60,
            velocity: 0,
        };
        assert!(s.push(30, on, 1));
        assert!(s.push(10, on, 2));
        assert!(s.push(30, off, 3));
        assert!(s.push(50, off, 4));
        assert!(!s.push(60, off, 5), "full");
        let order: Vec<u16> = std::iter::from_fn(|| s.pop_due(40))
            .map(|p| p.source)
            .collect();
        assert_eq!(order, [2, 3, 1]);
        assert_eq!(s.len(), 1);
        s.retain(|p| p.source != 4);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn overlapping_starts_of_a_key_end_together() {
        let mut out = MidiBuffer::with_capacity(16);
        let mut s = Sounding::default();
        s.on(&mut out, 0, 0, 60, 100);
        s.on(&mut out, 5, 0, 60, 80);
        s.off(&mut out, 10, 0, 60);
        assert!(s.is_sounding(0, 60));
        s.off(&mut out, 20, 0, 60);
        s.off(&mut out, 30, 0, 60);
        let kinds: Vec<(u32, bool)> = out
            .iter()
            .map(|e| (e.sample_offset, matches!(e.event, MidiEvent::NoteOn { .. })))
            .collect();
        // On, retriggered (off, on), and one off when both have ended.
        assert_eq!(kinds, [(0, true), (5, false), (5, true), (20, false)]);
        s.on(&mut out, 40, 3, 64, 1);
        out.clear();
        s.all_off(&mut out, 0);
        assert_eq!(out.len(), 1);
    }
}

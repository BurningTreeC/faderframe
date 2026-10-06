//! Scale: notes kept in a key.
//!
//! The key is the key track's where the note is (or one set here). A note
//! outside it moves to the nearest note of the key (a tie goes down), the
//! next one up or down, or is not played at all; then everything can move
//! by scale degrees (a diatonic transposer) and octaves. A note's end, its
//! polyphonic pressure and per-note expressions follow it to where it
//! went.

use super::chord::{ROOTS, key_at};
use super::midi_fx::{self, Sounding};
use super::{on_off, param, pick, stepped};
use crate::tap::AnalysisTap;
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_midi::theory::{Key, Scale};
use faderframe_midi::{MidiEvent, TimedMidiEvent};
use std::sync::Arc;

pub mod id {
    pub const FOLLOW_KEY: u32 = 0;
    pub const ROOT: u32 = 1;
    pub const SCALE: u32 = 2;
    pub const MODE: u32 = 3;
    pub const DEGREES: u32 = 4;
    pub const OCTAVE: u32 = 5;
}

/// Published: the key in use (root, scale index), the last note in and
/// where it went (−1: blocked).
pub mod value {
    pub const ROOT: usize = 0;
    pub const SCALE: usize = 1;
    pub const IN: usize = 2;
    pub const OUT: usize = 3;
}
pub const TAP_VALUES: usize = 4;

pub const MODES: [&str; 4] = ["Nearest", "Up", "Down", "Block"];

/// No note (blocked).
const NONE: u8 = 255;

fn ranged(pid: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(pid, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    vec![
        stepped(id::FOLLOW_KEY, "Follow the Key Track", 1.0, 1.0),
        stepped(id::ROOT, "Key", 11.0, 0.0),
        stepped(id::SCALE, "Scale", (Scale::ALL.len() - 1) as f64, 0.0),
        stepped(id::MODE, "Out of Key", (MODES.len() - 1) as f64, 0.0),
        ranged(id::DEGREES, "Transpose (Degrees)", -14.0, 14.0, 0.0),
        ranged(id::OCTAVE, "Octave", -3.0, 3.0, 0.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    let signed = |v: f64| {
        let s = v.round() as i32;
        if s == 0 {
            "0".to_string()
        } else {
            format!("{s:+}").replace('-', "−")
        }
    };
    Some(match pid.0 {
        id::FOLLOW_KEY => on_off(v),
        id::ROOT => pick(&ROOTS, v),
        id::SCALE => Scale::ALL[(v.round().max(0.0) as usize).min(Scale::ALL.len() - 1)]
            .name()
            .into(),
        id::MODE => pick(&MODES, v),
        id::DEGREES | id::OCTAVE => signed(v),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

/// Where `key` goes in `scale_key` with the mode, degrees and octaves
/// (`None`: not played).
pub fn map(scale_key: Key, key: u8, mode: usize, degrees: i32, octave: i32) -> Option<u8> {
    let k = i32::from(key);
    let inside = if scale_key.contains(k) {
        k
    } else {
        match mode {
            1 => (k..k + 12).find(|n| scale_key.contains(*n))?,
            2 => (k - 12..=k).rev().find(|n| scale_key.contains(*n))?,
            3 => return None,
            _ => scale_key.snap(k),
        }
    };
    let moved = if degrees != 0 {
        scale_key.step(inside, degrees)
    } else {
        inside
    } + 12 * octave;
    (0..=127).contains(&moved).then_some(moved as u8)
}

pub struct ScaleProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    /// Where each input note (channel, key) went.
    to: Box<[u8; 2048]>,
    sounding: Sounding,
    panic: bool,
}

impl ScaleProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, _config: &ProcessConfig) -> Self {
        Self {
            params,
            tap,
            to: Box::new([NONE; 2048]),
            sounding: Sounding::default(),
            panic: false,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }
}

impl PluginProcessor for ScaleProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let Some(out) = io.events_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        out.clear();
        if self.panic {
            self.sounding.all_off(out, 0);
            self.to.fill(NONE);
            self.panic = false;
        }
        let follow = self.get(id::FOLLOW_KEY) >= 0.5;
        let mode = self.get(id::MODE).round().max(0.0) as usize;
        let degrees = self.get(id::DEGREES).round() as i32;
        let octave = self.get(id::OCTAVE).round() as i32;
        let (root, scale) = (self.get(id::ROOT), self.get(id::SCALE));
        let now = key_at(
            ctx.harmony,
            ctx.transport.sample_position,
            follow,
            root,
            scale,
        );
        self.tap.set_value(value::ROOT, f32::from(now.root));
        let index = Scale::ALL.iter().position(|s| *s == now.scale).unwrap_or(0);
        self.tap.set_value(value::SCALE, index as f32);
        let Some(input) = io.events_in.first() else {
            return ProcessStatus::Continue;
        };
        for e in input.iter() {
            let offset = e.sample_offset;
            match e.event {
                MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity,
                } if velocity > 0 => {
                    let src = usize::from(midi_fx::source(channel, key));
                    // A repeated key ends where the earlier one went.
                    if self.to[src] != NONE {
                        self.sounding.off(out, offset, channel, self.to[src]);
                    }
                    let sample = ctx.transport.sample_position + i64::from(offset);
                    let k = key_at(ctx.harmony, sample, follow, root, scale);
                    let mapped = map(k, key, mode, degrees, octave);
                    self.to[src] = mapped.unwrap_or(NONE);
                    self.tap.set_value(value::IN, f32::from(key));
                    self.tap
                        .set_value(value::OUT, mapped.map_or(-1.0, f32::from));
                    if let Some(m) = mapped {
                        self.sounding.on(out, offset, channel, m, velocity);
                    }
                }
                MidiEvent::NoteOn { channel, key, .. }
                | MidiEvent::NoteOff { channel, key, .. } => {
                    let src = usize::from(midi_fx::source(channel, key));
                    let m = std::mem::replace(&mut self.to[src], NONE);
                    if m != NONE {
                        self.sounding.off(out, offset, channel, m);
                    }
                }
                MidiEvent::PolyPressure {
                    channel,
                    key,
                    pressure,
                } => {
                    let m = self.to[usize::from(midi_fx::source(channel, key))];
                    if m != NONE {
                        let _ = out.push(TimedMidiEvent::new(
                            offset,
                            MidiEvent::PolyPressure {
                                channel,
                                key: m,
                                pressure,
                            },
                        ));
                    }
                }
                MidiEvent::NoteExpression {
                    channel,
                    key,
                    kind,
                    value,
                } => {
                    let m = self.to[usize::from(midi_fx::source(channel, key))];
                    if m != NONE {
                        let _ = out.push(TimedMidiEvent::new(
                            offset,
                            MidiEvent::NoteExpression {
                                channel,
                                key: m,
                                kind,
                                value,
                            },
                        ));
                    }
                }
                _ => midi_fx::pass(out, input, e),
            }
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.panic = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::midi_fx::rig::{self, balanced, off, on, ons};
    use crate::{Harmony, NO_HARMONY};

    #[test]
    fn notes_move_into_the_key() {
        let c = Key::new(0, Scale::Major);
        // C# (61): nearest is C (a tie goes down), up D, down C, blocked.
        assert_eq!(map(c, 61, 0, 0, 0), Some(60));
        assert_eq!(map(c, 61, 1, 0, 0), Some(62));
        assert_eq!(map(c, 61, 2, 0, 0), Some(60));
        assert_eq!(map(c, 61, 3, 0, 0), None);
        // In the key it stays; two degrees up from C is E; an octave down.
        assert_eq!(map(c, 64, 3, 0, 0), Some(64));
        assert_eq!(map(c, 60, 0, 2, 0), Some(64));
        assert_eq!(map(c, 60, 0, -1, -1), Some(47));
        assert_eq!(map(c, 120, 0, 0, 1), None, "past the top");
    }

    #[test]
    fn it_follows_the_key_track_and_note_ends_follow_the_notes() {
        let params = rig::params(parameters(), &[]);
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        let mut p = ScaleProcessor::new(params, tap, &rig::config());
        // C major, then E major from sample 10 000: G# stays there, not before.
        let harmony = Harmony {
            keys: vec![
                (0, Key::new(0, Scale::Major)),
                (10_000, Key::new(4, Scale::Major)),
            ],
            chords: Vec::new(),
        };
        let input = [
            on(0, 68, 100),
            off(5000, 68),
            on(12_000, 68, 100),
            off(15_000, 68),
        ];
        let got = rig::run(&mut p, &input, 20_000, 120.0, true, &harmony);
        let keys: Vec<u8> = ons(&got).iter().map(|n| n.1).collect();
        assert_eq!(keys, [67, 68]);
        assert!(balanced(&got));
        // Not following: C major set here everywhere (scale index 0).
        let params = rig::params(parameters(), &[(id::FOLLOW_KEY, 0.0)]);
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        let mut p = ScaleProcessor::new(params, tap, &rig::config());
        let got = rig::run(&mut p, &input, 20_000, 120.0, true, &harmony);
        let keys: Vec<u8> = ons(&got).iter().map(|n| n.1).collect();
        assert_eq!(keys, [67, 67]);
        assert!(balanced(&got));
        let _ = &NO_HARMONY;
    }
}

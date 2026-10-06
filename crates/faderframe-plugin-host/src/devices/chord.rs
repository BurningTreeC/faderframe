//! Chord: every note played becomes a chord.
//!
//! * Intervals — the note and up to five more at fixed distances
//!   (semitones; a major triad to start with).
//! * Scale triad / seventh — thirds stacked in the key: the key track's
//!   key where the note is (or a key set here), so one finger plays the
//!   key's chords.
//! * Chord track — the chord track's chord where the note is, voiced
//!   round the played key (the note alone where there is none).
//!
//! The chord can be strummed (each note later than the one before, from
//! the bottom or the top) and its added notes played softer. A note's
//! chord ends with it; notes of a strum not started yet do not start.

use super::midi_fx::{self, Schedule, Sounding};
use super::{on_off, param, pick, stepped};
use crate::tap::AnalysisTap;
use crate::{
    Harmony, ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_midi::theory::{Key, Scale};
use faderframe_midi::{MidiBuffer, MidiEvent};
use std::sync::Arc;

pub mod id {
    pub const MODE: u32 = 0;
    /// The intervals: 1 to 5.
    pub const SHIFT: u32 = 1;
    pub const FOLLOW_KEY: u32 = 6;
    pub const ROOT: u32 = 7;
    pub const SCALE: u32 = 8;
    pub const STRUM: u32 = 9;
    pub const DIRECTION: u32 = 10;
    pub const VELOCITY: u32 = 11;
}
/// The intervals.
pub const SHIFTS: usize = 5;

/// Published: the notes of the chord played last (−1 for the rest).
pub mod value {
    pub const NOTES: usize = 0;
}
pub const NOTES_SHOWN: usize = 8;
pub const TAP_VALUES: usize = NOTES_SHOWN;

pub const MODES: [&str; 4] = ["Intervals", "Scale Triad", "Scale Seventh", "Chord Track"];
pub const DIRECTIONS: [&str; 2] = ["Up", "Down"];
pub const ROOTS: [&str; 12] = [
    "C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B",
];

/// Notes a chord has at most.
const MAX_NOTES: usize = 8;

fn ranged(pid: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(pid, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let mut out = vec![stepped(id::MODE, "Mode", (MODES.len() - 1) as f64, 0.0)];
    for (i, default) in [4.0, 7.0, 0.0, 0.0, 0.0].into_iter().enumerate() {
        out.push(ranged(
            id::SHIFT + i as u32,
            &format!("Interval {}", i + 1),
            -24.0,
            24.0,
            default,
        ));
    }
    out.extend([
        stepped(id::FOLLOW_KEY, "Follow the Key Track", 1.0, 1.0),
        stepped(id::ROOT, "Key", 11.0, 0.0),
        stepped(id::SCALE, "Scale", (Scale::ALL.len() - 1) as f64, 0.0),
        param(id::STRUM, "Strum", 0.0, 200.0, 0.0, Milliseconds),
        stepped(id::DIRECTION, "Strum Direction", 1.0, 0.0),
        param(id::VELOCITY, "Added Velocity", 0.0, 1.0, 1.0, Percent),
    ]);
    out
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::MODE => pick(&MODES, v),
        p if (id::SHIFT..id::SHIFT + SHIFTS as u32).contains(&p) => {
            let s = v.round() as i32;
            if s == 0 {
                "Off".into()
            } else {
                format!("{s:+}").replace('-', "−")
            }
        }
        id::FOLLOW_KEY => on_off(v),
        id::ROOT => pick(&ROOTS, v),
        id::SCALE => Scale::ALL[(v.round().max(0.0) as usize).min(Scale::ALL.len() - 1)]
            .name()
            .into(),
        id::DIRECTION => pick(&DIRECTIONS, v),
        id::STRUM if v < 0.5 => "Off".into(),
        id::STRUM => format!("{v:.0} ms"),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

/// The key a device uses at `sample`: the key track's (following it, where
/// there is one) or its own.
pub(crate) fn key_at(harmony: &Harmony, sample: i64, follow: bool, root: f64, scale: f64) -> Key {
    if follow && let Some(k) = harmony.key_at(sample) {
        return k;
    }
    Key::new(
        (root.round().max(0.0) as u8) % 12,
        Scale::ALL[(scale.round().max(0.0) as usize).min(Scale::ALL.len() - 1)],
    )
}

/// The chord a note becomes (keys, low to high; the played key first in
/// `0`'s place is not promised).
pub fn chord_of(
    params: &ParamValues,
    harmony: &Harmony,
    sample: i64,
    key: u8,
) -> ([u8; MAX_NOTES], usize) {
    let get = |pid: u32| f64::from(params.get(pid as usize));
    let mut keys = [0u8; MAX_NOTES];
    let mut n = 0;
    let mut add = |k: i32| {
        if (0..=127).contains(&k) && n < MAX_NOTES && !keys[..n].contains(&(k as u8)) {
            keys[n] = k as u8;
            n += 1;
        }
    };
    let k = i32::from(key);
    let mode = get(id::MODE).round() as i32;
    match mode {
        1 | 2 => {
            let scale_key = key_at(
                harmony,
                sample,
                get(id::FOLLOW_KEY) >= 0.5,
                get(id::ROOT),
                get(id::SCALE),
            );
            if scale_key.scale.intervals().len() == 7 {
                let base = scale_key.snap(k);
                add(base);
                add(scale_key.step(base, 2));
                add(scale_key.step(base, 4));
                if mode == 2 {
                    add(scale_key.step(base, 6));
                }
            } else {
                add(k);
            }
        }
        3 => match harmony.chord_at(sample) {
            Some(c) => {
                for v in c.voicing(k) {
                    add(v);
                }
            }
            None => add(k),
        },
        _ => {
            add(k);
            for i in 0..SHIFTS {
                let s = get(id::SHIFT + i as u32).round() as i32;
                if s != 0 {
                    add(k + s);
                }
            }
        }
    }
    keys[..n].sort_unstable();
    (keys, n)
}

/// One input note's chord: its keys, and whether each has started.
#[derive(Clone, Copy, Default)]
struct Voiced {
    keys: [u8; MAX_NOTES],
    started: [bool; MAX_NOTES],
    n: u8,
}

pub struct ChordProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    sr: f64,
    voiced: Box<[Voiced; 2048]>,
    strum: Schedule,
    sounding: Sounding,
    clock: u64,
    panic: bool,
}

impl ChordProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        Self {
            params,
            tap,
            sr: config.sample_rate.max(1.0),
            voiced: Box::new([Voiced::default(); 2048]),
            strum: Schedule::new(1024),
            sounding: Sounding::default(),
            clock: 0,
            panic: false,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    fn release(&mut self, out: &mut MidiBuffer, offset: u32, channel: u8, key: u8) {
        let src = midi_fx::source(channel, key);
        // Strummed notes not started yet never start.
        self.strum.retain(|p| p.source != src);
        let v = std::mem::take(&mut self.voiced[usize::from(src)]);
        for i in 0..usize::from(v.n) {
            if v.started[i] {
                self.sounding.off(out, offset, channel, v.keys[i]);
            }
        }
    }

    fn press(
        &mut self,
        out: &mut MidiBuffer,
        ctx: &PluginProcessContext<'_>,
        offset: u32,
        channel: u8,
        key: u8,
        velocity: u8,
    ) {
        self.release(out, offset, channel, key);
        let sample = ctx.transport.sample_position + i64::from(offset);
        let (keys, n) = chord_of(&self.params, ctx.harmony, sample, key);
        let strum = self.get(id::STRUM) * 0.001 * self.sr;
        let down = self.get(id::DIRECTION) >= 0.5;
        let added = ((f64::from(velocity) * self.get(id::VELOCITY)).round() as u8).max(1);
        let src = midi_fx::source(channel, key);
        let mut v = Voiced {
            keys,
            started: [false; MAX_NOTES],
            n: n as u8,
        };
        for (i, &k) in keys[..n].iter().enumerate() {
            let vel = if k == key { velocity } else { added };
            let place = if down { n - 1 - i } else { i };
            let delay = (place as f64 * strum).round() as u64;
            if delay == 0 {
                self.sounding.on(out, offset, channel, k, vel);
                v.started[i] = true;
            } else {
                let at = self.clock + u64::from(offset) + delay;
                let e = MidiEvent::NoteOn {
                    channel,
                    key: k,
                    velocity: vel,
                };
                // Started when it plays (or now, without room to wait).
                if !self.strum.push(at, e, src) {
                    self.sounding.on(out, offset, channel, k, vel);
                    v.started[i] = true;
                }
            }
        }
        self.voiced[usize::from(src)] = v;
        for (i, k) in keys.iter().enumerate().take(NOTES_SHOWN) {
            let k = if i < n { f32::from(*k) } else { -1.0 };
            self.tap.set_value(value::NOTES + i, k);
        }
    }

    /// Strummed notes due before `to`.
    fn advance(&mut self, out: &mut MidiBuffer, from: usize, to: usize) {
        while let Some(p) = self.strum.pop_due(self.clock + to as u64) {
            let offset = p.at.saturating_sub(self.clock).max(from as u64) as u32;
            if let MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } = p.event
            {
                self.sounding.on(out, offset, channel, key, velocity);
                let v = &mut self.voiced[usize::from(p.source)];
                if let Some(i) = v.keys[..usize::from(v.n)].iter().position(|k| *k == key) {
                    v.started[i] = true;
                }
            }
        }
    }
}

impl PluginProcessor for ChordProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames;
        let Some(out) = io.events_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        out.clear();
        if self.panic {
            self.sounding.all_off(out, 0);
            self.strum.clear();
            self.voiced.fill(Voiced::default());
            self.panic = false;
        }
        let mut cursor = 0;
        if let Some(input) = io.events_in.first() {
            for e in input.iter() {
                let at = (e.sample_offset as usize).min(frames);
                self.advance(out, cursor, at);
                cursor = at;
                match midi_fx::note(&e.event) {
                    Some((true, channel, key, velocity)) => {
                        self.press(out, ctx, at as u32, channel, key, velocity);
                    }
                    Some((false, channel, key, _)) => self.release(out, at as u32, channel, key),
                    None => midi_fx::pass(out, input, e),
                }
            }
        }
        self.advance(out, cursor, frames);
        self.clock += frames as u64;
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.panic = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NO_HARMONY;
    use crate::devices::midi_fx::rig::{self, balanced, off, on, ons};
    use faderframe_midi::theory::{Chord, Quality};

    fn chord(set: &[(u32, f64)]) -> ChordProcessor {
        let params = rig::params(parameters(), set);
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        ChordProcessor::new(params, tap, &rig::config())
    }

    fn played(c: &mut ChordProcessor, key: u8, harmony: &Harmony) -> Vec<u8> {
        let got = rig::run(
            c,
            &[on(0, key, 100), off(4000, key)],
            8000,
            120.0,
            true,
            harmony,
        );
        assert!(balanced(&got));
        let mut k: Vec<u8> = ons(&got).iter().map(|n| n.1).collect();
        k.sort_unstable();
        k
    }

    #[test]
    fn intervals_scale_chords_and_the_chord_track() {
        // A major triad by default; a power chord with an octave.
        assert_eq!(played(&mut chord(&[]), 60, &NO_HARMONY), [60, 64, 67]);
        let mut p = chord(&[(id::SHIFT, 7.0), (id::SHIFT + 1, 12.0)]);
        assert_eq!(played(&mut p, 40, &NO_HARMONY), [40, 47, 52]);
        // Scale triads in A minor (set here): D → D F A, E → E G B.
        let set = [
            (id::MODE, 1.0),
            (id::FOLLOW_KEY, 0.0),
            (id::ROOT, 9.0),
            (id::SCALE, 1.0),
        ];
        assert_eq!(played(&mut chord(&set), 62, &NO_HARMONY), [62, 65, 69]);
        // Following the key track: D major there makes D F# A; sevenths add C#.
        let d_major = Harmony {
            keys: vec![(0, Key::new(2, Scale::Major))],
            chords: vec![(0, 100_000, Chord::new(5, Quality::Major))],
        };
        assert_eq!(
            played(&mut chord(&[(id::MODE, 1.0)]), 62, &d_major),
            [62, 66, 69]
        );
        assert_eq!(
            played(&mut chord(&[(id::MODE, 2.0)]), 62, &d_major),
            [62, 66, 69, 73]
        );
        // The chord track's F major, round the played key.
        let k = played(&mut chord(&[(id::MODE, 3.0)]), 64, &d_major);
        assert!(
            k.iter().all(|k| [5, 9, 0].contains(&(k % 12))) && k.len() == 3,
            "{k:?}"
        );
        // No chord there: the note alone.
        assert_eq!(
            played(&mut chord(&[(id::MODE, 3.0)]), 64, &NO_HARMONY),
            [64]
        );
    }

    #[test]
    fn strums_and_softer_added_notes_end_with_the_note() {
        // 10 ms between notes, from the top, added notes at half velocity.
        let mut c = chord(&[(id::STRUM, 10.0), (id::DIRECTION, 1.0), (id::VELOCITY, 0.5)]);
        let got = rig::run(
            &mut c,
            &[on(0, 60, 100), off(30_000, 60)],
            40_000,
            120.0,
            false,
            &NO_HARMONY,
        );
        let notes = ons(&got);
        assert_eq!(notes, [(0, 67, 50), (480, 64, 50), (960, 60, 100)]);
        assert!(balanced(&got));
        // Let go mid-strum: the notes not started never start.
        let mut c = chord(&[(id::STRUM, 100.0)]);
        let got = rig::run(
            &mut c,
            &[on(0, 60, 100), off(6000, 60)],
            20_000,
            120.0,
            false,
            &NO_HARMONY,
        );
        assert_eq!(ons(&got).len(), 2, "{got:?}");
        assert!(balanced(&got));
    }
}

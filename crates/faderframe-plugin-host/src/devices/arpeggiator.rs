//! Arpeggiator: the held notes played one after another.
//!
//! While the song plays, steps fall on its grid (the rate a division of a
//! quarter, every second step delayed by the swing); stopped, the grid
//! starts with the first key pressed. The first note of a new chord plays
//! as it is pressed unless the grid's next step is close. The notes are
//! the held keys over one to four octaves, in one of nine orders; each
//! step's note lasts the gate (a share of the step, past one: legato), at
//! the played velocity or a fixed one, and can repeat. Hold keeps playing
//! the last chord after the keys are let go, until a new one is pressed.
//! Everything that is not a note passes through.

use super::midi_fx::{self, Schedule, Sounding};
use super::{on_off, param, pick, stepped};
use crate::dsp::lfo::{DIVISIONS, division_name};
use crate::tap::AnalysisTap;
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_midi::MidiBuffer;
use std::sync::Arc;

pub mod id {
    pub const MODE: u32 = 0;
    pub const RATE: u32 = 1;
    pub const GATE: u32 = 2;
    pub const OCTAVES: u32 = 3;
    pub const SWING: u32 = 4;
    pub const VELOCITY: u32 = 5;
    pub const HOLD: u32 = 6;
    pub const REPEATS: u32 = 7;
}

/// Published: the note played last (−1: none), the steps played since the
/// chord started (repeats counted), the pattern's length, then the held
/// keys in the order pressed (−1 for the rest).
pub mod value {
    pub const NOTE: usize = 0;
    pub const STEP: usize = 1;
    pub const LENGTH: usize = 2;
    pub const HELD: usize = 3;
}
/// Held keys published.
pub const HELD_SHOWN: usize = 16;
pub const TAP_VALUES: usize = value::HELD + HELD_SHOWN;

pub const MODES: [&str; 9] = [
    "Up",
    "Down",
    "Up-Down",
    "Down-Up",
    "Converge",
    "Diverge",
    "As Played",
    "Random",
    "Chord",
];
const AS_PLAYED: usize = 6;
const RANDOM: usize = 7;
const CHORD: usize = 8;

/// The rate's default: 1/16.
const DEFAULT_RATE: f64 = 5.0;
/// Keys held at most.
const MAX_HELD: usize = 32;

fn ranged(pid: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(pid, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        stepped(id::MODE, "Mode", (MODES.len() - 1) as f64, 0.0),
        stepped(id::RATE, "Rate", (DIVISIONS.len() - 1) as f64, DEFAULT_RATE),
        param(id::GATE, "Gate", 0.05, 1.5, 0.8, Percent),
        ranged(id::OCTAVES, "Octaves", 1.0, 4.0, 1.0),
        param(id::SWING, "Swing", 0.0, 1.0, 0.0, Percent),
        ranged(id::VELOCITY, "Velocity", 0.0, 127.0, 0.0),
        stepped(id::HOLD, "Hold", 1.0, 0.0),
        ranged(id::REPEATS, "Repeats", 1.0, 4.0, 1.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::MODE => pick(&MODES, v),
        id::RATE => division_name(v.round().max(0.0) as usize).into(),
        id::OCTAVES => format!("{:.0}", v.round()),
        id::VELOCITY if v < 0.5 => "As Played".into(),
        id::VELOCITY => format!("{:.0}", v.round()),
        id::HOLD => on_off(v),
        id::REPEATS => format!("{:.0}×", v.round()),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

/// Which note of a pattern of `n` step `pos` plays (`None`: at random).
pub fn order_index(mode: usize, n: usize, pos: u64) -> Option<usize> {
    if n <= 1 {
        return Some(0);
    }
    let p = (pos % n as u64) as usize;
    let updown = |pos: u64| {
        let period = (2 * n - 2) as u64;
        let p = (pos % period) as usize;
        if p < n { p } else { 2 * n - 2 - p }
    };
    let converge = |p: usize| {
        if p.is_multiple_of(2) {
            p / 2
        } else {
            n - 1 - p / 2
        }
    };
    Some(match mode {
        1 => n - 1 - p,
        2 => updown(pos),
        3 => n - 1 - updown(pos),
        4 => converge(p),
        5 => converge(n - 1 - p),
        RANDOM => return None,
        _ => p,
    })
}

/// The pattern's notes for keys held in the order pressed: low to high
/// (or as played) over the octaves (for editors; allocates).
pub fn sequence(mode: usize, held: &[u8], octaves: usize) -> Vec<u8> {
    let mut base = held.to_vec();
    if mode != AS_PLAYED {
        base.sort_unstable();
    }
    (0..octaves.clamp(1, 4))
        .flat_map(|o| base.iter().map(move |k| u16::from(*k) + 12 * o as u16))
        .filter(|k| *k <= 127)
        .map(|k| k as u8)
        .collect()
}

/// A held key.
#[derive(Clone, Copy, Debug, Default)]
struct Held {
    key: u8,
    channel: u8,
    velocity: u8,
    order: u32,
}

/// A note of the pattern.
#[derive(Clone, Copy, Debug, Default)]
struct Step {
    key: u8,
    channel: u8,
    velocity: u8,
}

pub struct ArpeggiatorProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    sr: f64,
    held: [Held; MAX_HELD],
    n_held: usize,
    order: u32,
    /// Keys down now (with hold, the held set outlives them).
    down: usize,
    /// Hold: every key is up, the chord plays on until the next press.
    latched: bool,
    seq: [Step; MAX_HELD * 4],
    n_seq: usize,
    /// The held set changed: rebuild the pattern.
    dirty: bool,
    /// Steps played since the chord started, and repeats of this one.
    pos: u64,
    repeat: u32,
    /// Steps played since the chord started (editors follow it).
    steps: u64,
    last_random: usize,
    rng: u32,
    running: bool,
    /// Stopped: the clock time the free grid starts at, and its next step.
    free_start: u64,
    free_next: u64,
    offs: Schedule,
    sounding: Sounding,
    clock: u64,
    panic: bool,
}

impl ArpeggiatorProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        Self {
            params,
            tap,
            sr: config.sample_rate.max(1.0),
            held: [Held::default(); MAX_HELD],
            n_held: 0,
            order: 0,
            down: 0,
            latched: false,
            seq: [Step::default(); MAX_HELD * 4],
            n_seq: 0,
            dirty: false,
            pos: 0,
            repeat: 0,
            steps: 0,
            last_random: usize::MAX,
            rng: 0x2545_f491,
            running: false,
            free_start: 0,
            free_next: 0,
            offs: Schedule::new(1024),
            sounding: Sounding::default(),
            clock: 0,
            panic: false,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    fn mode(&self) -> usize {
        (self.get(id::MODE).round().max(0.0) as usize).min(MODES.len() - 1)
    }

    /// The pattern's notes: the held keys (low to high, or as played) over
    /// the octaves.
    fn rebuild(&mut self) {
        let mut base = self.held;
        let base = &mut base[..self.n_held];
        if self.mode() == AS_PLAYED {
            base.sort_by_key(|h| h.order);
        } else {
            base.sort_by_key(|h| h.key);
        }
        let octaves = self.get(id::OCTAVES).round().clamp(1.0, 4.0) as usize;
        self.n_seq = 0;
        for o in 0..octaves {
            for h in base.iter() {
                let key = u16::from(h.key) + 12 * o as u16;
                if key > 127 {
                    continue;
                }
                self.seq[self.n_seq] = Step {
                    key: key as u8,
                    channel: h.channel,
                    velocity: h.velocity,
                };
                self.n_seq += 1;
            }
        }
        self.dirty = false;
    }

    /// The index into the pattern for step `pos`.
    fn index(&mut self, pos: u64) -> usize {
        let n = self.n_seq;
        if let Some(i) = order_index(self.mode(), n, pos) {
            return i;
        }
        let mut i;
        loop {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 17;
            self.rng ^= self.rng << 5;
            i = self.rng as usize % n.max(1);
            if n <= 1 || i != self.last_random {
                break;
            }
        }
        self.last_random = i;
        i
    }

    /// Play the step at `offset` (the clock's `at`), its note(s) lasting
    /// `length` samples.
    fn step(&mut self, out: &mut MidiBuffer, offset: u32, length: u64) {
        if self.dirty {
            self.rebuild();
        }
        if self.n_seq == 0 {
            return;
        }
        let fixed = self.get(id::VELOCITY).round() as u8;
        let at = self.clock + u64::from(offset);
        let mut play = |s: &mut Self, st: Step| {
            let velocity = if fixed > 0 { fixed } else { st.velocity };
            s.sounding.on(out, offset, st.channel, st.key, velocity);
            let off = faderframe_midi::MidiEvent::NoteOff {
                channel: st.channel,
                key: st.key,
                velocity: 0,
            };
            if !s.offs.push(at + length.max(1), off, 0) {
                s.sounding.off(out, offset, st.channel, st.key);
            }
            s.tap.set_value(value::NOTE, f32::from(st.key));
        };
        if self.mode() == CHORD {
            // The whole chord, an octave further each step.
            let nb = self.n_held.max(1);
            let octave = (self.pos as usize) % (self.n_seq / nb).max(1);
            for i in 0..nb.min(self.n_seq) {
                let st = self.seq[(octave * nb + i).min(self.n_seq - 1)];
                play(self, st);
            }
        } else {
            let i = self.index(self.pos);
            let st = self.seq[i];
            play(self, st);
        }
        let repeats = self.get(id::REPEATS).round().clamp(1.0, 4.0) as u32;
        self.repeat += 1;
        if self.repeat >= repeats {
            self.repeat = 0;
            self.pos += 1;
        }
        self.steps += 1;
        self.tap.set_value(value::STEP, (self.steps % 4096) as f32);
    }

    /// Play the scheduled note-offs and grid steps in `from..to` (block
    /// offsets) in time order.
    fn advance(
        &mut self,
        out: &mut MidiBuffer,
        ctx: &PluginProcessContext<'_>,
        from: usize,
        to: usize,
    ) {
        let t = ctx.transport;
        let spq = midi_fx::samples_per_quarter(t, self.sr);
        let division =
            DIVISIONS[(self.get(id::RATE).round().max(0.0) as usize).min(DIVISIONS.len() - 1)].1;
        let step_s = (division * spq).max(1.0);
        let gate = (self.get(id::GATE) * step_s) as u64;
        let swing = self.get(id::SWING).clamp(0.0, 1.0) * 0.5;
        // The step times in from..to (block offsets).
        let mut steps = [0usize; 64];
        let mut n = 0;
        if self.running {
            if t.playing {
                let q_from = t.quarter_position + from as f64 / spq;
                let q_to = t.quarter_position + to as f64 / spq;
                let first = (q_from / division).floor() as i64 - 1;
                let mut k = first;
                loop {
                    let q = k as f64 * division + if k % 2 != 0 { swing * division } else { 0.0 };
                    if q >= q_to || n == steps.len() {
                        break;
                    }
                    if q >= q_from {
                        let off = ((q - t.quarter_position) * spq).round().max(0.0) as usize;
                        steps[n] = off.clamp(from, to.saturating_sub(1).max(from));
                        n += 1;
                    }
                    k += 1;
                }
            } else {
                let end = self.clock + to as u64;
                loop {
                    let k = self.free_next;
                    let at = self.free_start as f64
                        + k as f64 * step_s
                        + if k % 2 == 1 { swing * step_s } else { 0.0 };
                    let at = at.round() as u64;
                    if at >= end || n == steps.len() {
                        break;
                    }
                    if at >= self.clock + from as u64 {
                        steps[n] = (at - self.clock) as usize;
                        n += 1;
                    }
                    self.free_next += 1;
                }
            }
        }
        let mut s = 0;
        loop {
            let next_step = (s < n).then(|| steps[s]);
            let limit = self.clock + next_step.unwrap_or(to) as u64;
            // Offs due before the next step (or the segment's end).
            while let Some(p) = self.offs.pop_due(limit) {
                let off = p.at.saturating_sub(self.clock).max(from as u64) as u32;
                midi_fx::play(&mut self.sounding, out, off, p.event);
            }
            let Some(o) = next_step else { break };
            self.step(out, o as u32, gate);
            s += 1;
        }
    }

    fn held_changed(&mut self) {
        self.dirty = true;
        for i in 0..HELD_SHOWN {
            let v = if i < self.n_held {
                f32::from(self.held[i].key)
            } else {
                -1.0
            };
            self.tap.set_value(value::HELD + i, v);
        }
        if self.dirty {
            self.rebuild();
        }
        self.tap.set_value(value::LENGTH, self.n_seq as f32);
    }

    /// A key goes down at `offset`.
    fn press(
        &mut self,
        out: &mut MidiBuffer,
        ctx: &PluginProcessContext<'_>,
        offset: usize,
        channel: u8,
        key: u8,
        velocity: u8,
    ) {
        if self.latched {
            // A new chord replaces the one held.
            self.n_held = 0;
            self.latched = false;
            self.running = false;
        }
        self.down += 1;
        if self.held[..self.n_held]
            .iter()
            .any(|h| h.key == key && h.channel == channel)
        {
            return;
        }
        if self.n_held < MAX_HELD {
            self.order += 1;
            self.held[self.n_held] = Held {
                key,
                channel,
                velocity,
                order: self.order,
            };
            self.n_held += 1;
            self.held_changed();
        }
        if !self.running {
            // A new chord: from the pattern's start.
            self.running = true;
            self.pos = 0;
            self.repeat = 0;
            self.steps = 0;
            self.last_random = usize::MAX;
            let t = ctx.transport;
            let spq = midi_fx::samples_per_quarter(t, self.sr);
            let division = DIVISIONS
                [(self.get(id::RATE).round().max(0.0) as usize).min(DIVISIONS.len() - 1)]
            .1;
            let step_s = (division * spq).max(1.0);
            let gate = (self.get(id::GATE) * step_s) as u64;
            if t.playing {
                // Now, unless the grid's next step is near.
                let q = t.quarter_position + offset as f64 / spq;
                let next = (q / division).ceil() * division;
                if next - q > 0.25 * division {
                    self.step(out, offset as u32, gate);
                }
            } else {
                self.free_start = self.clock + offset as u64;
                self.free_next = 1;
                self.step(out, offset as u32, gate);
            }
        }
    }

    /// A key comes up.
    fn release(&mut self, channel: u8, key: u8) {
        self.down = self.down.saturating_sub(1);
        if self.get(id::HOLD) >= 0.5 {
            if self.down == 0 && self.n_held > 0 {
                self.latched = true;
            }
            return;
        }
        if let Some(i) = self.held[..self.n_held]
            .iter()
            .position(|h| h.key == key && h.channel == channel)
        {
            self.held.copy_within(i + 1..self.n_held, i);
            self.n_held -= 1;
            self.held_changed();
        }
        if self.n_held == 0 {
            self.running = false;
        }
    }
}

impl PluginProcessor for ArpeggiatorProcessor {
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
            self.offs.clear();
            self.panic = false;
        }
        // Hold let go while a chord is latched: it stops.
        if self.latched && self.get(id::HOLD) < 0.5 {
            self.latched = false;
            self.n_held = 0;
            self.running = false;
            self.held_changed();
        }
        if self.dirty {
            self.rebuild();
        }
        let mut cursor = 0;
        if let Some(input) = io.events_in.first() {
            for e in input.iter() {
                let at = (e.sample_offset as usize).min(frames);
                self.advance(out, ctx, cursor, at);
                cursor = at;
                match midi_fx::note(&e.event) {
                    Some((true, channel, key, velocity)) => {
                        self.press(out, ctx, at, channel, key, velocity);
                    }
                    Some((false, channel, key, _)) => self.release(channel, key),
                    None => midi_fx::pass(out, input, e),
                }
            }
        }
        self.advance(out, ctx, cursor, frames);
        if !self.running && self.offs.len() == 0 {
            self.tap.set_value(value::NOTE, -1.0);
        }
        self.clock += frames as u64;
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.panic = true;
        self.n_held = 0;
        self.down = 0;
        self.latched = false;
        self.running = false;
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NO_HARMONY;
    use crate::devices::midi_fx::rig::{self, SR, balanced, off, on, ons};

    fn arp(set: &[(u32, f64)]) -> ArpeggiatorProcessor {
        let params = rig::params(parameters(), set);
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        ArpeggiatorProcessor::new(params, tap, &rig::config())
    }

    /// A sixteenth at 120 BPM: 6000 samples.
    const SIXTEENTH: u64 = 6000;

    #[test]
    fn up_plays_the_chord_on_the_grid_and_lets_go() {
        let mut a = arp(&[]);
        // C E G held from the first beat for a bar's quarter, while playing.
        let input = [
            on(0, 60, 100),
            on(0, 64, 90),
            on(0, 67, 80),
            off(24_000, 60),
            off(24_000, 64),
            off(24_000, 67),
        ];
        let got = rig::run(&mut a, &input, 48_000, 120.0, true, &NO_HARMONY);
        let notes = ons(&got);
        let keys: Vec<u8> = notes.iter().map(|n| n.1).collect();
        assert_eq!(keys, [60, 64, 67, 60], "{notes:?}");
        // On the sixteenth grid, at the played velocities.
        for (i, n) in notes.iter().enumerate() {
            assert_eq!(n.0, i as u64 * SIXTEENTH, "{notes:?}");
        }
        assert_eq!(notes[1].2, 90);
        // Gate 80 %: each note ends 4800 samples after it starts.
        let first_off = got
            .iter()
            .find(|(_, e)| matches!(e, faderframe_midi::MidiEvent::NoteOff { key: 60, .. }))
            .unwrap()
            .0;
        assert_eq!(first_off, 4800);
        assert!(balanced(&got));
    }

    #[test]
    fn the_orders_and_octaves() {
        let chord = [on(0, 60, 100), on(0, 64, 100), on(0, 67, 100)];
        let keys = |set: &[(u32, f64)], steps: usize| {
            let mut a = arp(set);
            let got = rig::run(
                &mut a,
                &chord,
                SIXTEENTH * steps as u64,
                120.0,
                true,
                &NO_HARMONY,
            );
            ons(&got)
                .iter()
                .filter(|n| n.0 < SIXTEENTH * steps as u64)
                .map(|n| n.1)
                .collect::<Vec<u8>>()
        };
        assert_eq!(keys(&[(id::MODE, 1.0)], 4), [67, 64, 60, 67]);
        assert_eq!(keys(&[(id::MODE, 2.0)], 6), [60, 64, 67, 64, 60, 64]);
        assert_eq!(keys(&[(id::MODE, 4.0)], 3), [60, 67, 64]);
        assert_eq!(keys(&[(id::OCTAVES, 2.0)], 7), [60, 64, 67, 72, 76, 79, 60]);
        assert_eq!(keys(&[(id::REPEATS, 2.0)], 4), [60, 60, 64, 64]);
        // The chord, all at once, an octave up the next step.
        let k = keys(&[(id::MODE, 8.0), (id::OCTAVES, 2.0)], 2);
        assert_eq!(k, [60, 64, 67, 72, 76, 79]);
        // Random never repeats a note straight away.
        let k = keys(&[(id::MODE, 7.0)], 16);
        assert!(k.windows(2).all(|w| w[0] != w[1]), "{k:?}");
    }

    #[test]
    fn stopped_it_runs_from_the_first_key_and_hold_latches() {
        let mut a = arp(&[(id::HOLD, 1.0), (id::VELOCITY, 70.0)]);
        // Pressed at 1000 and released at 2000: held on.
        let input = [
            on(1000, 62, 100),
            off(2000, 62),
            on(30_000, 65, 100),
            off(31_000, 65),
        ];
        let got = rig::run(&mut a, &input, 48_000, 120.0, false, &NO_HARMONY);
        let notes = ons(&got);
        assert_eq!(notes[0], (1000, 62, 70));
        assert_eq!(notes[1].0, 1000 + SIXTEENTH);
        // A new key replaces the latched one.
        assert!(notes.iter().any(|n| n.1 == 65 && n.0 == 30_000));
        assert!(notes.iter().filter(|n| n.0 > 30_000).all(|n| n.1 == 65));
        // Turning hold off stops it.
        a.params
            .set_by_id(ParameterId(id::HOLD), 0.0)
            .unwrap_or_default();
        let later = rig::run(&mut a, &[], 24_000, 120.0, false, &NO_HARMONY);
        assert!(ons(&later).is_empty());
    }

    #[test]
    fn swing_delays_every_second_step_and_controllers_pass() {
        let mut a = arp(&[(id::SWING, 0.5)]);
        let cc = (
            100u64,
            faderframe_midi::MidiEvent::ControlChange {
                channel: 0,
                controller: 1,
                value: 64,
            },
        );
        let input = [on(0, 60, 100), cc];
        let got = rig::run(&mut a, &input, SIXTEENTH * 3, 120.0, true, &NO_HARMONY);
        let times: Vec<u64> = ons(&got).iter().map(|n| n.0).collect();
        assert_eq!(times, [0, SIXTEENTH + SIXTEENTH / 4, 2 * SIXTEENTH]);
        assert!(got.iter().any(
            |(t, e)| *t == 100 && matches!(e, faderframe_midi::MidiEvent::ControlChange { .. })
        ));
        let _ = SR;
    }
}

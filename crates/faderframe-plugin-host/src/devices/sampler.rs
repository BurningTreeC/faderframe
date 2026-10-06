//! Sampler: plays one sample across the keyboard, or an SFZ instrument.
//!
//! With a single sample its root key, loop (off, forward, while held:
//! start and end with a crossfade), start and end points, direction and
//! whether the pitch follows the keys are the device's settings; with an SFZ the
//! regions' own key and velocity ranges, roots, tuning, levels, pans,
//! loops, envelopes, release triggers, choke groups (`group`/`off_by`),
//! round robins (`seq_length`/`seq_position`) and random layers apply, the
//! device's settings on top. Every voice has an ADSR (a region's `ampeg_*`
//! where it has them), a filter (12 or 24 dB low pass, band, high pass)
//! the envelope can open, and reads its sample by cubic interpolation at
//! the ratio of the pitch and the sample's rate to the host's.

use super::keep_length::{KeepLength, Render};
use super::samples::{LoopMode, Sample, Shared, Zone};
use super::{on_off, param, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ParameterId, db_to_gain};
use faderframe_midi::MidiEvent;
use std::f32::consts::PI;
use std::sync::Arc;

pub mod id {
    pub const VOLUME: u32 = 0;
    pub const TRANSPOSE: u32 = 1;
    pub const TUNE: u32 = 2;
    pub const ATTACK: u32 = 3;
    pub const DECAY: u32 = 4;
    pub const SUSTAIN: u32 = 5;
    pub const RELEASE: u32 = 6;
    pub const CUTOFF: u32 = 7;
    pub const RESONANCE: u32 = 8;
    pub const FILTER_TYPE: u32 = 9;
    pub const FILTER_ENV: u32 = 10;
    pub const VELOCITY: u32 = 11;
    pub const ROOT: u32 = 12;
    pub const LOOP: u32 = 13;
    pub const LOOP_START: u32 = 14;
    pub const LOOP_END: u32 = 15;
    pub const START: u32 = 16;
    pub const CROSSFADE: u32 = 17;
    pub const POLYPHONY: u32 = 18;
    pub const PAN: u32 = 19;
    pub const REVERSE: u32 = 20;
    pub const KEY_TRACK: u32 = 21;
    /// 0: the pitch moves with the speed (repitch); 1: the length stays.
    pub const PITCH_MODE: u32 = 22;
    /// Where playing stops (0–1 of the sample; the start to here plays,
    /// backwards when reversed).
    pub const END: u32 = 23;
}

/// Published: voices sounding, the newest voice's key and where it plays
/// (0–1 of its sample) and which zone (−1 the single sample), the output's
/// peak (linear), and the keys held (see [`key_held`]).
pub mod value {
    pub const VOICES: usize = 0;
    pub const NOTE: usize = 1;
    pub const POSITION: usize = 2;
    pub const ZONE: usize = 3;
    pub const OUT_PEAK: usize = 4;
    /// The first of [`super::KEY_WORDS`] values of 24 key bits each.
    pub const KEYS: usize = 5;
}
/// Values that hold the held keys (24 keys each, exact in an `f32`).
pub const KEY_WORDS: usize = 6;
pub const TAP_VALUES: usize = value::KEYS + KEY_WORDS;

/// Whether `key` is held, from the published values (`value(i)` reads
/// value `i`).
pub fn key_held(value: impl Fn(usize) -> f32, key: u8) -> bool {
    let k = usize::from(key);
    let word = value(value::KEYS + k / 24) as u32;
    (word >> (k % 24)) & 1 == 1
}

pub const FILTERS: [&str; 4] = ["LP 12", "LP 24", "Band", "High"];
pub const LOOPS: [&str; 3] = ["Off", "Loop", "While Held"];
pub const PITCH_MODES: [&str; 2] = ["Repitch", "Keep Length"];

const MAX_VOICES: usize = 64;

fn ranged(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(id, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::VOLUME, "Volume", -48.0, 12.0, -6.0, Decibels),
        ranged(id::TRANSPOSE, "Transpose", -24.0, 24.0, 0.0),
        param(id::TUNE, "Tune", -100.0, 100.0, 0.0, None),
        param(id::ATTACK, "Attack", 0.5, 10_000.0, 1.0, Milliseconds),
        param(id::DECAY, "Decay", 1.0, 20_000.0, 1_000.0, Milliseconds),
        param(id::SUSTAIN, "Sustain", 0.0, 1.0, 1.0, Percent),
        param(id::RELEASE, "Release", 5.0, 20_000.0, 250.0, Milliseconds),
        param(id::CUTOFF, "Cutoff", 20.0, 20_000.0, 20_000.0, Hertz),
        param(id::RESONANCE, "Resonance", 0.0, 1.0, 0.1, Percent),
        stepped(id::FILTER_TYPE, "Filter Type", 3.0, 0.0),
        param(id::FILTER_ENV, "Filter Envelope", -1.0, 1.0, 0.0, Percent),
        param(id::VELOCITY, "Velocity", 0.0, 1.0, 0.8, Percent),
        ranged(id::ROOT, "Root Key", 0.0, 127.0, 60.0),
        stepped(id::LOOP, "Loop", 2.0, 0.0),
        param(id::LOOP_START, "Loop Start", 0.0, 1.0, 0.0, Percent),
        param(id::LOOP_END, "Loop End", 0.0, 1.0, 1.0, Percent),
        param(id::START, "Start", 0.0, 1.0, 0.0, Percent),
        param(
            id::CROSSFADE,
            "Loop Crossfade",
            0.0,
            500.0,
            10.0,
            Milliseconds,
        ),
        ranged(id::POLYPHONY, "Voices", 1.0, MAX_VOICES as f64, 32.0),
        param(id::PAN, "Pan", -1.0, 1.0, 0.0, None),
        stepped(id::REVERSE, "Reverse", 1.0, 0.0),
        stepped(id::KEY_TRACK, "Key Tracking", 1.0, 1.0),
        stepped(id::PITCH_MODE, "Pitch", 1.0, 0.0),
        param(id::END, "End", 0.0, 1.0, 1.0, Percent),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::FILTER_TYPE => pick(&FILTERS, v),
        id::LOOP => pick(&LOOPS, v),
        id::PITCH_MODE => pick(&PITCH_MODES, v),
        id::REVERSE | id::KEY_TRACK => on_off(v),
        id::ROOT => crate::devices::note_name(v.round() as i32),
        id::TRANSPOSE => format!("{:+.0} st", v.round()).replace("+0 st", "0 st"),
        id::TUNE => format!("{v:+.0} ¢").replace("+0 ¢", "0 ¢"),
        id::POLYPHONY => format!("{:.0}", v.round()),
        id::CUTOFF if v >= 19_500.0 => "Open".into(),
        id::PAN if v.abs() < 0.005 => "C".into(),
        id::PAN if v < 0.0 => format!("L {:.0}", -v * 100.0),
        id::PAN => format!("R {:.0}", v * 100.0),
        id::FILTER_ENV => format!("{:+.0} %", v * 100.0).replace("+0 %", "0 %"),
        _ => return None,
    })
}

/// Read channel `c` of a sample at a fractional frame (cubic Hermite).
#[inline]
pub(crate) fn read(s: &Sample, c: usize, pos: f64) -> f32 {
    let d = &s.data[c];
    let n = d.len();
    if n == 0 || pos < -1.0 || pos >= n as f64 {
        return 0.0;
    }
    let i = pos.floor() as isize;
    let t = (pos - i as f64) as f32;
    let at = |k: isize| -> f32 {
        if k < 0 || k as usize >= n {
            0.0
        } else {
            d[k as usize]
        }
    };
    let (y0, y1, y2, y3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

/// The voice's sample at its position (crossfaded into the loop start near
/// the loop's end), and on by `step`: the frame and whether that was the
/// end of the sample.
fn next_frame(v: &mut Voice, s: &Sample, step: f64) -> ([f32; 2], bool) {
    let stereo = s.stereo;
    let mut y = [
        read(s, 0, v.pos),
        if stereo { read(s, 1, v.pos) } else { 0.0 },
    ];
    if let Some((ls, le)) = v.looping
        && v.fade > 0.0
        && !v.reverse
        && v.pos > le - v.fade
    {
        let t = ((v.pos - (le - v.fade)) / v.fade) as f32;
        let back = v.pos - (le - ls);
        y[0] = y[0] * (1.0 - t) + read(s, 0, back) * t;
        if stereo {
            y[1] = y[1] * (1.0 - t) + read(s, 1, back) * t;
        }
    }
    if v.looping.is_none() {
        // Fade the last 3 ms before the end (an end point mid-waveform
        // would click).
        let left = if v.reverse {
            v.pos - v.end
        } else {
            v.end - v.pos
        };
        let fade = s.rate * 0.003;
        if left < fade {
            let g = (left / fade).max(0.0) as f32;
            y[0] *= g;
            y[1] *= g;
        }
    }
    if !stereo {
        y[1] = y[0];
    }
    let ended = if v.reverse {
        v.pos -= step;
        v.pos < v.end
    } else {
        v.pos += step;
        match v.looping {
            Some((ls, le)) if v.pos >= le => {
                v.pos -= le - ls;
                false
            }
            _ => v.pos >= v.end,
        }
    };
    (y, ended)
}

/// A Keep Length voice's sample at its own speed into `a`/`b`: the frames
/// it had (fewer once it ended).
fn feed_voice(v: &mut Voice, s: &Sample, step: f64, a: &mut [f32], b: &mut [f32]) -> usize {
    if v.fed_out {
        return 0;
    }
    for k in 0..a.len().min(b.len()) {
        let (y, ended) = next_frame(v, s, step);
        a[k] = y[0];
        b[k] = y[1];
        if ended {
            v.fed_out = true;
            return k + 1;
        }
    }
    a.len().min(b.len())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    stage: Stage,
    env: f32,
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
    /// The zone (`usize::MAX`: the single sample) and its sample.
    zone: usize,
    sample: usize,
    pos: f64,
    step: f64,
    /// Loop range (frames) when looping, and the crossfade before its end.
    looping: Option<(f64, f64)>,
    loop_mode: LoopMode,
    fade: f64,
    /// Plays to `end` (frames; going backwards: down to it).
    end: f64,
    reverse: bool,
    gain: [f32; 2],
    key: u8,
    channel: u8,
    velocity: f32,
    held: bool,
    pedal_held: bool,
    off_by: u32,
    svf: [[(f32, f32); 2]; 2],
    age: u64,
    /// Keep Length: the stretcher it plays through and its pitch factor;
    /// its sample has all been fed.
    slot: Option<u8>,
    pitch: f32,
    fed_out: bool,
}

impl Voice {
    const IDLE: Voice = Voice {
        stage: Stage::Idle,
        env: 0.0,
        attack_step: 1.0,
        decay_coef: 0.0,
        sustain: 1.0,
        release_coef: 0.0,
        zone: usize::MAX,
        sample: 0,
        pos: 0.0,
        step: 1.0,
        looping: None,
        loop_mode: LoopMode::NoLoop,
        fade: 0.0,
        end: 0.0,
        reverse: false,
        gain: [1.0; 2],
        key: 0,
        channel: 0,
        velocity: 0.0,
        held: false,
        pedal_held: false,
        off_by: 0,
        svf: [[(0.0, 0.0); 2]; 2],
        age: 0,
        slot: None,
        pitch: 1.0,
        fed_out: false,
    };

    fn release(&mut self) {
        self.held = false;
        if self.stage != Stage::Idle && self.loop_mode != LoopMode::OneShot {
            self.stage = Stage::Release;
        }
        // Loop while held: play on out of the loop.
        if self.loop_mode == LoopMode::Sustain {
            self.looping = None;
        }
    }
}

pub struct SamplerProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    watching: Watching,
    shared: Shared,
    sr: f64,
    voices: [Voice; MAX_VOICES],
    counter: u64,
    generation: u64,
    sustain: [bool; 16],
    bend: [f32; 16],
    bend_range: [f32; 16],
    rpn: [(u8, u8); 16],
    /// Round robin counters per key, and the random draw.
    rounds: [u32; 128],
    random: u32,
    /// Keys whose release triggers are due (from note-offs).
    pending_release: [(u8, u8, u8); 16],
    pending: usize,
    /// The block's two sides before they go out.
    mix: [Vec<f32>; 2],
    meters: [MeterTap; 2],
    /// Stretchers for Keep Length (made when the mode is on).
    keep: Option<KeepLength>,
}

impl SamplerProcessor {
    pub fn new(
        params: ParamValues,
        tap: Option<Arc<AnalysisTap>>,
        config: &ProcessConfig,
        shared: Shared,
    ) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        Self {
            tap,
            watching: Watching::new(sr as f32),
            shared,
            sr,
            voices: [Voice::IDLE; MAX_VOICES],
            counter: 0,
            generation: 0,
            sustain: [false; 16],
            bend: [0.0; 16],
            bend_range: [2.0; 16],
            rpn: [(127, 127); 16],
            rounds: [0; 128],
            random: 0x2545_f491,
            pending_release: [(0, 0, 0); 16],
            pending: 0,
            mix: [vec![0.0; block], vec![0.0; block]],
            meters: [MeterTap::new(sr as f32); 2],
            keep: (params.get(id::PITCH_MODE as usize) >= 0.5)
                .then(|| KeepLength::new(sr, block))
                .flatten(),
            params,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    fn free_voice(&self) -> usize {
        let poly = (self.get(id::POLYPHONY).round() as usize).clamp(1, MAX_VOICES);
        let voices = &self.voices[..poly];
        voices
            .iter()
            .position(|v| v.stage == Stage::Idle)
            .or_else(|| {
                voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.stage == Stage::Release)
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
            })
            .unwrap_or_else(|| {
                voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map_or(0, |(i, _)| i)
            })
    }

    /// Start a voice on `zone` (or the single sample) of `set`.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        set: &super::samples::SampleSet,
        zone: Option<&Zone>,
        zone_index: usize,
        sample: usize,
        channel: u8,
        key: u8,
        velocity: u8,
    ) {
        let Some(s) = set.samples.get(sample) else {
            return;
        };
        let sr = self.sr;
        let ms = |v: f64| (v * 0.001 * sr).max(1.0) as f32;
        let coef = |samples: f32| (-6.9 / samples).exp();
        let (a, d, sus, r) = (
            zone.and_then(|z| z.ampeg[0])
                .map_or(ms(self.get(id::ATTACK)), |t| ((t * sr) as f32).max(1.0)),
            zone.and_then(|z| z.ampeg[1])
                .map_or(ms(self.get(id::DECAY)), |t| ((t * sr) as f32).max(1.0)),
            zone.and_then(|z| z.ampeg[2])
                .unwrap_or(self.get(id::SUSTAIN))
                .clamp(0.0, 1.0) as f32,
            zone.and_then(|z| z.ampeg[3])
                .map_or(ms(self.get(id::RELEASE)), |t| ((t * sr) as f32).max(1.0)),
        );
        let frames = s.frames as f64;
        let single = zone.is_none();
        let root = zone.map_or(self.get(id::ROOT).round(), |z| z.root);
        let keytrack = zone.map_or(
            if self.get(id::KEY_TRACK) >= 0.5 {
                100.0
            } else {
                0.0
            },
            |z| z.keytrack,
        );
        let tune = zone.map_or(0.0, |z| z.tune)
            + self.get(id::TUNE)
            + 100.0 * self.get(id::TRANSPOSE).round();
        let semis = (f64::from(key) - root) * keytrack / 100.0 + tune / 100.0;
        let ratio = 2f64.powf(semis / 12.0) * s.rate / sr;
        let reverse = single && self.get(id::REVERSE) >= 0.5;
        let loop_mode = match zone.and_then(|z| z.loop_mode) {
            Some(m) => m,
            None if single => match self.get(id::LOOP).round() as i64 {
                1 => LoopMode::Continuous,
                2 => LoopMode::Sustain,
                _ => LoopMode::NoLoop,
            },
            None => {
                if zone.is_some_and(|z| z.loop_start.is_some() && z.loop_end.is_some()) {
                    LoopMode::Continuous
                } else {
                    LoopMode::NoLoop
                }
            }
        };
        let mut end = zone
            .and_then(|z| z.end)
            .map_or(frames, |e| (e as f64).min(frames));
        let start = if single {
            // The region between the start and end markers.
            let a = self.get(id::START).clamp(0.0, 0.99) * frames;
            end = (self.get(id::END).clamp(0.0, 1.0) * frames)
                .max(a + 1.0)
                .min(frames);
            a
        } else {
            zone.map_or(0.0, |z| z.offset as f64).min(end)
        };
        let looping = match loop_mode {
            LoopMode::Continuous | LoopMode::Sustain => {
                let (ls, le) = match zone {
                    Some(z) => (
                        z.loop_start.map_or(0.0, |v| v as f64),
                        z.loop_end.map_or(end, |v| (v as f64 + 1.0).min(end)),
                    ),
                    None => (
                        self.get(id::LOOP_START).clamp(0.0, 1.0) * frames,
                        (self.get(id::LOOP_END).clamp(0.0, 1.0) * frames).min(end),
                    ),
                };
                (le - ls > 16.0).then_some((ls, le))
            }
            _ => None,
        };
        let fade = looping.map_or(0.0, |(ls, le)| {
            (self.get(id::CROSSFADE) * 0.001 * s.rate)
                .min((le - ls) * 0.5)
                .min(ls)
        });
        let vel = f64::from(velocity) / 127.0;
        let vt = self.get(id::VELOCITY) * zone.map_or(1.0, |z| z.veltrack);
        let level = db_to_gain((self.get(id::VOLUME) + zone.map_or(0.0, |z| z.volume)) as f32)
            * (1.0 - vt + vt * vel * vel) as f32;
        let pan = (self.get(id::PAN) + zone.map_or(0.0, |z| z.pan)).clamp(-1.0, 1.0) as f32;
        let gain = [level * (1.0 - pan).min(1.0), level * (1.0 + pan).min(1.0)];
        let (choke, off_by) = zone.map_or((0, 0), |z| (z.group, z.off_by));
        // Choke: this group silences voices that it turns off.
        if choke > 0 {
            for v in &mut self.voices {
                if v.stage != Stage::Idle && v.off_by == choke {
                    v.stage = Stage::Release;
                    v.release_coef = (-6.9 / (0.005 * sr) as f32).exp();
                    v.loop_mode = LoopMode::NoLoop;
                }
            }
        }
        self.counter += 1;
        let idx = self.free_voice();
        // Keep Length: read at the sample's own speed, pitched by a
        // stretcher.
        let keep = self.get(id::PITCH_MODE) >= 0.5;
        let (step, pitch, slot) = match self.keep.as_mut().filter(|_| keep) {
            Some(pool) => {
                let ages: [u64; MAX_VOICES] = std::array::from_fn(|i| self.voices[i].age);
                let (slot, evicted) = pool.claim(idx, |i| ages[i]);
                if let Some(e) = evicted {
                    self.voices[e].stage = Stage::Idle;
                    self.voices[e].slot = None;
                }
                (s.rate / sr, 2f64.powf(semis / 12.0) as f32, Some(slot))
            }
            None => (ratio, 1.0, None),
        };
        self.voices[idx] = Voice {
            stage: Stage::Attack,
            env: 0.0,
            attack_step: 1.0 / a,
            decay_coef: coef(d),
            sustain: sus,
            release_coef: coef(r),
            zone: zone_index,
            sample,
            pos: if reverse { end - 1.0 } else { start },
            step,
            looping,
            loop_mode,
            fade,
            end: if reverse { start } else { end },
            reverse,
            gain,
            key,
            channel,
            velocity: vel as f32,
            held: true,
            pedal_held: false,
            off_by,
            svf: [[(0.0, 0.0); 2]; 2],
            age: self.counter,
            slot,
            pitch,
            fed_out: false,
        };
    }

    fn note_on(
        &mut self,
        set: &super::samples::SampleSet,
        channel: u8,
        key: u8,
        velocity: u8,
        release: bool,
    ) {
        if set.zones.is_empty() {
            if !release && let Some(Some(sample)) = set.slots.first() {
                self.start(set, None, usize::MAX, *sample, channel, key, velocity);
            }
            return;
        }
        let k = key as usize & 127;
        let round = if release {
            self.rounds[k].saturating_sub(1)
        } else {
            self.rounds[k]
        };
        if !release {
            self.rounds[k] = self.rounds[k].wrapping_add(1);
        }
        self.random ^= self.random << 13;
        self.random ^= self.random >> 17;
        self.random ^= self.random << 5;
        let rand = f64::from(self.random) / f64::from(u32::MAX);
        for (i, z) in set.zones.iter().enumerate() {
            if z.on_release != release || !z.takes(key, velocity) {
                continue;
            }
            if z.seq_length > 1 && round % z.seq_length + 1 != z.seq_position {
                continue;
            }
            if rand < z.lorand || rand >= z.hirand.max(z.lorand + 1e-9) {
                continue;
            }
            self.start(set, Some(z), i, z.sample, channel, key, velocity);
        }
    }

    fn note_off(&mut self, channel: u8, key: u8, velocity: u8) {
        let pedal = self.sustain[(channel & 15) as usize];
        for v in &mut self.voices {
            if v.key == key && v.channel == channel && v.held && v.stage != Stage::Idle {
                if pedal {
                    v.pedal_held = true;
                    v.held = false;
                } else {
                    v.release();
                }
            }
        }
        if self.pending < self.pending_release.len() {
            self.pending_release[self.pending] = (channel, key, velocity.max(64));
            self.pending += 1;
        }
    }

    fn handle(&mut self, set: Option<&super::samples::SampleSet>, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } => {
                if let Some(set) = set {
                    self.note_on(set, channel, key, velocity, false);
                }
            }
            MidiEvent::NoteOff {
                channel,
                key,
                velocity,
            } => self.note_off(channel, key, velocity),
            MidiEvent::ControlChange { controller, .. }
                if controller == MidiEvent::CC_ALL_NOTES_OFF
                    || controller == MidiEvent::CC_ALL_SOUND_OFF =>
            {
                self.sustain = [false; 16];
                for v in &mut self.voices {
                    v.release();
                }
            }
            MidiEvent::ControlChange {
                channel,
                controller: 64,
                value,
            } => {
                let c = (channel & 15) as usize;
                self.sustain[c] = value >= 64;
                if value < 64 {
                    for v in &mut self.voices {
                        if v.channel == channel && v.pedal_held {
                            v.pedal_held = false;
                            v.release();
                        }
                    }
                }
            }
            MidiEvent::PitchBend { channel, value } => {
                let c = (channel & 15) as usize;
                self.bend[c] = (value as f32 - 8192.0) / 8192.0 * self.bend_range[c];
            }
            MidiEvent::ControlChange {
                channel,
                controller: 101,
                value,
            } => self.rpn[(channel & 15) as usize].0 = value,
            MidiEvent::ControlChange {
                channel,
                controller: 100,
                value,
            } => self.rpn[(channel & 15) as usize].1 = value,
            MidiEvent::ControlChange {
                channel,
                controller: 6,
                value,
            } => {
                let c = (channel & 15) as usize;
                if self.rpn[c] == (0, 0) {
                    self.bend_range[c] = f32::from(value.max(1));
                }
            }
            _ => {}
        }
    }

    // Each frame writes both output sides at its index.
    #[allow(clippy::needless_range_loop)]
    fn render(
        &mut self,
        set: Option<&super::samples::SampleSet>,
        out: &mut [&mut [f32]; 2],
        start: usize,
        end: usize,
    ) {
        let Some(set) = set else { return };
        let sr = self.sr as f32;
        let kind = self.get(id::FILTER_TYPE).round() as i64;
        let cutoff = self.get(id::CUTOFF) as f32;
        let env_amt = self.get(id::FILTER_ENV) as f32 * 6.0;
        let filtering = cutoff < 19_500.0 || env_amt.abs() > 0.01 || kind != 0;
        let k = 2.0 - 1.9 * self.get(id::RESONANCE).clamp(0.0, 1.0) as f32;
        let bends = self.bend;
        let Self { voices, keep, .. } = self;
        for v in voices.iter_mut().filter(|v| v.stage != Stage::Idle) {
            let Some(s) = set.samples.get(v.sample) else {
                v.stage = Stage::Idle;
                continue;
            };
            let bend = 2f64.powf(f64::from(bends[(v.channel & 15) as usize]) / 12.0);
            // Keep Length: the segment from the voice's stretcher (the bend
            // moves its pitch, not its speed).
            let mut kept: Option<(&[f32], &[f32])> = None;
            if let (Some(slot), Some(pool)) = (v.slot, keep.as_mut()) {
                let (step, transpose) = (v.step, v.pitch * bend as f32);
                let mut feed = |a: &mut [f32], b: &mut [f32]| feed_voice(v, s, step, a, b);
                match pool.render(slot, transpose, end - start, &mut feed) {
                    Render::Wait => continue,
                    Render::Done => {
                        v.stage = Stage::Idle;
                        continue;
                    }
                    Render::Out(l, r) => kept = Some((l, r)),
                }
            }
            let step = v.step * bend;
            // The filter's coefficients for this segment.
            let velocity = v.velocity;
            let coefs = |env: f32| {
                let fc = (cutoff * 2f32.powf(env_amt * env * velocity)).clamp(20.0, sr * 0.45);
                let g = (PI * fc / sr).tan();
                let a1 = 1.0 / (1.0 + g * (g + k));
                (a1, g * a1, g * g * a1)
            };
            let mut c = coefs(v.env);
            for i in start..end {
                match v.stage {
                    Stage::Attack => {
                        v.env += v.attack_step;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = Stage::Decay;
                        }
                    }
                    Stage::Decay => {
                        v.env = v.sustain + (v.env - v.sustain) * v.decay_coef;
                        if (v.env - v.sustain).abs() < 1e-4 {
                            v.stage = Stage::Sustain;
                        }
                    }
                    Stage::Sustain => v.env = v.sustain,
                    Stage::Release => {
                        v.env *= v.release_coef;
                        if v.env < 1e-4 {
                            v.stage = Stage::Idle;
                        }
                    }
                    Stage::Idle => {}
                }
                if v.stage == Stage::Idle {
                    break;
                }
                if filtering && i % 16 == 0 {
                    c = coefs(v.env);
                }
                let y = match kept {
                    Some((l, r)) => [l[i - start], r[i - start]],
                    None => {
                        let (y, ended) = next_frame(v, s, step);
                        if ended {
                            v.stage = Stage::Idle;
                        }
                        y
                    }
                };
                for ch in 0..2 {
                    let mut x = y[ch];
                    if filtering {
                        let (a1, a2, a3) = c;
                        let (ic1, ic2) = &mut v.svf[ch][0];
                        let v3 = x - *ic2;
                        let v1 = a1 * *ic1 + a2 * v3;
                        let v2 = *ic2 + a2 * *ic1 + a3 * v3;
                        *ic1 = 2.0 * v1 - *ic1;
                        *ic2 = 2.0 * v2 - *ic2;
                        x = match kind {
                            1 => {
                                let (jc1, jc2) = &mut v.svf[ch][1];
                                let w3 = v2 - *jc2;
                                let w1 = a1 * *jc1 + a2 * w3;
                                let w2 = *jc2 + a2 * *jc1 + a3 * w3;
                                *jc1 = 2.0 * w1 - *jc1;
                                *jc2 = 2.0 * w2 - *jc2;
                                w2
                            }
                            2 => v1 * k,
                            3 => x - k * v1 - v2,
                            _ => v2,
                        };
                    }
                    out[ch][i] += x * v.env * v.gain[ch];
                }
            }
            for st in v.svf.iter_mut().flatten() {
                if st.0.abs() < 1e-20 {
                    st.0 = 0.0;
                }
                if st.1.abs() < 1e-20 {
                    st.1 = 0.0;
                }
            }
        }
    }
}

impl PluginProcessor for SamplerProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames.min(self.mix[0].len());
        let watched = self
            .tap
            .as_ref()
            .is_some_and(|t| self.watching.check(t, frames));
        // The samples, if the control side is not swapping them right now.
        let shared = Arc::clone(&self.shared);
        let guard = shared.try_lock();
        let set = guard.as_ref().and_then(|g| {
            if g.generation != self.generation {
                // New samples: what plays from the old ones stops.
                self.generation = g.generation;
                for v in &mut self.voices {
                    v.stage = Stage::Idle;
                }
            }
            g.set.as_deref()
        });
        // Stretchers of voices that stopped go back; the block's priming.
        if let Some(pool) = &mut self.keep {
            let voices = &self.voices;
            pool.begin_block(frames, self.sr, |o, slot| {
                voices[o].stage != Stage::Idle && voices[o].slot == Some(slot)
            });
        }
        // Render both sides into the preallocated mix (taken out and put
        // back: no allocation), split at the events.
        let mut mix = std::mem::take(&mut self.mix);
        {
            let [l, r] = &mut mix;
            l[..frames].fill(0.0);
            r[..frames].fill(0.0);
            let mut sides: [&mut [f32]; 2] = [&mut l[..frames], &mut r[..frames]];
            let mut pos = 0usize;
            if let Some(events) = io.events_in.first() {
                for e in events.iter() {
                    let at = (e.sample_offset as usize).min(frames);
                    if at > pos {
                        self.render(set, &mut sides, pos, at);
                        pos = at;
                    }
                    self.handle(set, e.event);
                    if self.pending > 0 {
                        let due = std::mem::take(&mut self.pending);
                        if let Some(set) = set {
                            for j in 0..due {
                                let (c, k, v) = self.pending_release[j];
                                self.note_on(set, c, k, v, true);
                            }
                        }
                    }
                }
            }
            if frames > pos {
                self.render(set, &mut sides, pos, frames);
            }
        }
        let newest = self
            .voices
            .iter()
            .filter(|v| v.stage != Stage::Idle)
            .max_by_key(|v| v.age)
            .copied();
        let position = newest.and_then(|v| {
            let len = set?.samples.get(v.sample)?.frames as f64;
            (len > 0.0).then(|| (v.pos / len) as f32)
        });
        drop(guard);
        let Some(out) = io.audio_out.first_mut() else {
            self.mix = mix;
            return ProcessStatus::Continue;
        };
        out.clear();
        let channels = out.num_channels();
        if channels >= 2 {
            out.channel_mut(0)[..frames].copy_from_slice(&mix[0][..frames]);
            out.channel_mut(1)[..frames].copy_from_slice(&mix[1][..frames]);
        } else if channels == 1 {
            for (o, (a, b)) in out
                .channel_mut(0)
                .iter_mut()
                .zip(mix[0].iter().zip(&mix[1]))
                .take(frames)
            {
                *o = 0.5 * (a + b);
            }
        }
        self.mix = mix;
        if let Some(tap) = &self.tap {
            let sounding = self
                .voices
                .iter()
                .filter(|v| v.stage != Stage::Idle)
                .count();
            tap.set_value(value::VOICES, sounding as f32);
            let mut held = [0u32; KEY_WORDS];
            for v in &self.voices {
                if matches!(v.stage, Stage::Attack | Stage::Decay | Stage::Sustain) {
                    let k = usize::from(v.key);
                    if let Some(w) = held.get_mut(k / 24) {
                        *w |= 1 << (k % 24);
                    }
                }
            }
            for (i, w) in held.iter().enumerate() {
                tap.set_value(value::KEYS + i, *w as f32);
            }
            if let Some(v) = newest {
                tap.set_value(value::NOTE, f32::from(v.key));
                tap.set_value(
                    value::ZONE,
                    if v.zone == usize::MAX {
                        -1.0
                    } else {
                        v.zone as f32
                    },
                );
                tap.set_value(value::POSITION, position.unwrap_or(0.0));
            }
            let mut peak = 0.0f32;
            for c in 0..2.min(channels) {
                for s in out.channel(c).iter().take(frames) {
                    self.meters[c].add(*s);
                    peak = peak.max(s.abs());
                }
                self.meters[c].publish(&tap.meter_out, c, frames);
            }
            tap.raise_value(value::OUT_PEAK, peak);
            if watched && channels > 0 {
                let r = out.channel(1.min(channels - 1));
                tap.output.push(&out.channel(0)[..frames], &r[..frames]);
            }
        }
        if self.voices.iter().all(|v| v.stage == Stage::Idle) {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; MAX_VOICES];
        self.sustain = [false; 16];
        self.pending = 0;
        if let Some(pool) = &mut self.keep {
            pool.release_all();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{SR, bin_db};
    use crate::devices::samples::{SampleDoc, SampleHost, fixtures};
    use faderframe_audio_graph::AudioBuffer;
    use faderframe_core::ChannelLayout;
    use faderframe_midi::{MidiBuffer, TimedMidiEvent};
    use faderframe_transport::TransportInfo;

    pub(crate) struct Play<P> {
        pub p: P,
        out: Vec<AudioBuffer>,
        events: Vec<MidiBuffer>,
        transport: TransportInfo,
    }

    impl<P: PluginProcessor> Play<P> {
        pub fn new(p: P) -> Self {
            let mut out = vec![AudioBuffer::new(ChannelLayout::Stereo, 256)];
            out[0].set_len(256);
            Self {
                p,
                out,
                events: vec![MidiBuffer::with_capacity(64)],
                transport: TransportInfo::default(),
            }
        }

        pub fn send(&mut self, e: MidiEvent) {
            self.events[0].push(TimedMidiEvent::new(0, e)).unwrap();
        }

        pub fn run(&mut self, seconds: f64) -> (Vec<f32>, Vec<f32>, ProcessStatus) {
            let (mut l, mut r) = (Vec::new(), Vec::new());
            let mut status = ProcessStatus::Continue;
            for _ in 0..(seconds * SR / 256.0).ceil() as usize {
                let ctx = PluginProcessContext {
                    transport: &self.transport,
                    param_events: &[],
                    harmony: &crate::NO_HARMONY,
                };
                let mut io = NodeIo {
                    frames: 256,
                    audio_in: &[],
                    audio_out: &mut self.out,
                    events_in: &self.events,
                    events_out: &mut [],
                };
                status = self.p.process(&ctx, &mut io);
                self.events[0].clear();
                l.extend_from_slice(self.out[0].channel(0));
                r.extend_from_slice(self.out[0].channel(1));
            }
            (l, r, status)
        }
    }

    pub(crate) fn on(key: u8, velocity: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: 0,
            key,
            velocity,
        }
    }

    pub(crate) fn off(key: u8) -> MidiEvent {
        MidiEvent::NoteOff {
            channel: 0,
            key,
            velocity: 0,
        }
    }

    fn sampler(doc: SampleDoc, set: &[(u32, f64)]) -> (Play<SamplerProcessor>, SampleHost) {
        let params = ParamValues::new(parameters());
        for (id, v) in set {
            params.set_by_id(ParameterId(*id), *v).unwrap();
        }
        let mut host = SampleHost::default();
        host.set_doc(doc, None);
        let p = SamplerProcessor::new(
            params,
            None,
            &crate::devices::rig::config(),
            Arc::clone(&host.shared),
        );
        (Play::new(p), host)
    }

    #[test]
    fn a_sample_plays_at_the_pitch_of_the_key_and_loops() {
        let d = fixtures::dir("sampler");
        let mut doc = SampleDoc::default();
        // A at 44.1 kHz, its root A4: an octave up plays 880 Hz.
        doc.set(
            0,
            Some(fixtures::tone(&d.join("a.wav"), 440.0, 44_100, 0.5)),
        );
        let (mut s, _host) = sampler(doc.clone(), &[(id::ROOT, 69.0), (id::VOLUME, 0.0)]);
        s.send(on(81, 127));
        let (l, _, _) = s.run(0.3);
        assert!(bin_db(&l, 880.0) > -12.0, "{}", bin_db(&l, 880.0));
        assert!(bin_db(&l, 440.0) < -60.0);
        // Without a loop it stops at its end (0.25 s at twice the speed)…
        let (l, _, _) = s.run(0.2);
        assert!(l.iter().all(|v| v.abs() < 1e-6));
        // …with one it goes on until released, then sleeps.
        let (mut s, _host) = sampler(
            doc,
            &[
                (id::ROOT, 69.0),
                (id::LOOP, 1.0),
                (id::LOOP_START, 0.2),
                (id::LOOP_END, 0.8),
            ],
        );
        s.send(on(69, 100));
        s.run(1.0);
        let (l, _, _) = s.run(0.2);
        assert!(
            bin_db(&l, 440.0) > -20.0,
            "still looping: {}",
            bin_db(&l, 440.0)
        );
        s.send(off(69));
        let (_, _, status) = s.run(2.0);
        assert_eq!(status, ProcessStatus::Sleep);
    }

    #[test]
    fn the_start_and_end_points_bound_what_plays() {
        // One second: 440 Hz, then 880 Hz.
        let d = fixtures::dir("sampler-region");
        let path = d.join("two.wav");
        let x: Vec<f32> = (0..48_000)
            .map(|i| {
                let f = if i < 24_000 { 440.0 } else { 880.0 };
                (0.5 * (std::f64::consts::TAU * f * i as f64 / 48_000.0).sin()) as f32
            })
            .collect();
        faderframe_audio_files::write_wav(
            &path,
            &[x],
            48_000,
            faderframe_audio_files::WavFormat::Float32,
            false,
        )
        .unwrap();
        let mut doc = SampleDoc::default();
        doc.set(0, Some(path.to_string_lossy().into_owned()));
        let play = |set: &[(u32, f64)]| {
            let mut all = vec![(id::ROOT, 69.0), (id::RELEASE, 5.0), (id::VOLUME, 0.0)];
            all.extend_from_slice(set);
            let (mut s, _host) = sampler(doc.clone(), &all);
            s.send(on(69, 127));
            let (l, _, _) = s.run(1.2);
            let last = l.iter().rposition(|v| v.abs() > 1e-4).unwrap_or(0);
            (l, last as f64 / SR)
        };
        // The first half only.
        let (l, length) = play(&[(id::END, 0.5)]);
        assert!((length - 0.5).abs() < 0.01, "{length}");
        assert!(
            bin_db(&l[..20_000], 440.0) > -12.0,
            "{}",
            bin_db(&l[..20_000], 440.0)
        );
        assert!(bin_db(&l, 880.0) < -40.0, "{}", bin_db(&l, 880.0));
        // The middle.
        let (_, length) = play(&[(id::START, 0.25), (id::END, 0.75)]);
        assert!((length - 0.5).abs() < 0.01, "{length}");
        // Reversed: the region backwards (the 440 half, not the 880 one).
        let (l, length) = play(&[(id::END, 0.5), (id::REVERSE, 1.0)]);
        assert!((length - 0.5).abs() < 0.01, "{length}");
        assert!(
            bin_db(&l[..20_000], 440.0) > -12.0,
            "{}",
            bin_db(&l[..20_000], 440.0)
        );
        assert!(bin_db(&l, 880.0) < -40.0, "{}", bin_db(&l, 880.0));
        // The end does not click: the last milliseconds fade out.
        let (l, length) = play(&[(id::END, 0.3)]);
        let end = (length * SR) as usize;
        let peak =
            |r: std::ops::RangeInclusive<usize>| l[r].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let (before, last) = (peak(end - 400..=end - 300), peak(end - 20..=end));
        assert!(before > 0.4 && last < 0.1, "{before} → {last}");
    }

    #[test]
    fn every_held_key_is_published() {
        let d = fixtures::dir("sampler-keys");
        let mut doc = SampleDoc::default();
        doc.set(
            0,
            Some(fixtures::tone(&d.join("a.wav"), 440.0, 44_100, 2.0)),
        );
        let params = ParamValues::new(parameters());
        params.set_by_id(ParameterId(id::LOOP), 2.0).unwrap();
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        let mut host = SampleHost::default();
        host.set_doc(doc, None);
        let mut s = Play::new(SamplerProcessor::new(
            params,
            Some(Arc::clone(&tap)),
            &crate::devices::rig::config(),
            Arc::clone(&host.shared),
        ));
        let held = |k: u8| key_held(|i| tap.value(i), k);
        // A chord, its notes far apart (in different words of bits).
        for k in [21, 60, 64, 108] {
            s.send(on(k, 100));
        }
        s.run(0.1);
        assert!([21, 60, 64, 108].into_iter().all(held), "all four lit");
        assert!(!held(62) && !held(0) && !held(127));
        // A released key goes dark while it fades out.
        s.send(off(64));
        s.run(0.02);
        assert!(!held(64));
        assert!(held(60) && held(108));
    }

    #[test]
    fn keep_length_moves_the_pitch_not_the_length() {
        let d = fixtures::dir("sampler-keep");
        let mut doc = SampleDoc::default();
        // Half a second of A4 at 44.1 kHz, played an octave up.
        doc.set(
            0,
            Some(fixtures::tone(&d.join("a.wav"), 440.0, 44_100, 0.5)),
        );
        // How long it sounds (above -40 dBFS), and its pitch.
        let play = |mode: f64| {
            let (mut s, _host) = sampler(
                doc.clone(),
                &[(id::ROOT, 69.0), (id::VOLUME, 0.0), (id::PITCH_MODE, mode)],
            );
            s.send(on(81, 127));
            let (l, _, _) = s.run(1.0);
            let sounding = l.iter().rposition(|v| v.abs() > 0.01).unwrap_or(0);
            let head = &l[..(0.2 * SR) as usize];
            (
                sounding as f64 / SR,
                bin_db(head, 880.0),
                bin_db(head, 440.0),
            )
        };
        let (repitch, hi, lo) = play(0.0);
        assert!((repitch - 0.25).abs() < 0.03, "{repitch}");
        assert!(hi > -12.0 && lo < -40.0, "{hi} {lo}");
        let (kept, hi, lo) = play(1.0);
        assert!((kept - 0.5).abs() < 0.05, "the length stays: {kept}");
        assert!(hi > -14.0, "an octave up: {hi}");
        assert!(lo < hi - 20.0, "not the original pitch: {lo} {hi}");
        // It starts on time: the first 20 ms already sound.
        let (mut s, _host) = sampler(
            doc,
            &[(id::ROOT, 69.0), (id::VOLUME, 0.0), (id::PITCH_MODE, 1.0)],
        );
        s.send(on(81, 127));
        let (l, _, _) = s.run(0.02);
        let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.1, "no latency: {peak}");
    }

    #[test]
    fn sfz_layers_round_robins_and_chokes() {
        let d = fixtures::dir("sfz-play");
        for (name, f) in [
            ("soft", 300.0),
            ("loud", 500.0),
            ("rr1", 700.0),
            ("rr2", 900.0),
        ] {
            fixtures::tone(&d.join(format!("{name}.wav")), f, 48_000, 1.0);
        }
        let sfz = "<group>pitch_keycenter=60 key=60 ampeg_release=0.01\n<region>sample=soft.wav hivel=63\n<region>sample=loud.wav lovel=64\n<group>key=62 pitch_keycenter=62 seq_length=2 group=1\n<region>sample=rr1.wav seq_position=1\n<region>sample=rr2.wav seq_position=2\n<group>key=64 pitch_keycenter=64 off_by=1\n<region>sample=soft.wav";
        std::fs::write(d.join("i.sfz"), sfz).unwrap();
        let mut doc = SampleDoc::default();
        doc.set(0, Some(d.join("i.sfz").to_string_lossy().into_owned()));
        let (mut s, host) = sampler(doc, &[(id::VOLUME, 0.0)]);
        assert_eq!(host.doc.files.len(), 1);
        let play = |s: &mut Play<SamplerProcessor>, key: u8, vel: u8| {
            s.send(on(key, vel));
            let (l, _, _) = s.run(0.2);
            s.send(off(key));
            s.run(0.3);
            l
        };
        let soft = play(&mut s, 60, 40);
        assert!(bin_db(&soft, 300.0) > bin_db(&soft, 500.0) + 30.0);
        let loud = play(&mut s, 60, 120);
        assert!(bin_db(&loud, 500.0) > bin_db(&loud, 300.0) + 30.0);
        let first = play(&mut s, 62, 100);
        let second = play(&mut s, 62, 100);
        assert!(bin_db(&first, 700.0) > bin_db(&first, 900.0) + 30.0);
        assert!(bin_db(&second, 900.0) > bin_db(&second, 700.0) + 30.0);
        // Key 64 (group 1's off_by) is cut by a hit on key 62 (group 1).
        s.send(on(64, 100));
        s.run(0.1);
        s.send(on(62, 100));
        let (l, _, _) = s.run(0.3);
        let tail = &l[l.len() / 2..];
        assert!(
            bin_db(tail, 300.0) < -60.0,
            "choked: {}",
            bin_db(tail, 300.0)
        );
    }
}

//! Reverb: early reflections and a 16-line feedback delay network.
//!
//! The input waits out the pre-delay, then feeds
//! * the early reflections — a type's pattern of taps (time, level, side)
//!   scaled by the size, and
//! * the late reverb — through four allpass diffusers per side into 16
//!   delay lines (lengths spread geometrically over the type's range,
//!   scaled by the size, gliding when it moves) mixed every pass by a
//!   16 × 16 Hadamard matrix (orthogonal: the network itself neither adds
//!   nor loses energy). Each line ends in a three band decay filter — a
//!   one-pole split at 250 Hz and at the damping frequency, each band
//!   turned down by exactly what its decay time asks for the line's
//!   length (`10^(−3 L / (RT·rate))`): the bass decays over decay × bass,
//!   the middle over the decay, the highs over a third of it. The lines
//!   are read through slowly modulated taps (each its own rate and phase)
//!   so the tail does not ring at the lines' modes. Left and right are
//!   taken from the lines with orthogonal sign patterns: decorrelated.
//!
//! Freeze closes the input and makes every band lossless (and stops the
//! modulation, whose interpolation would dull the loop); ducking turns the
//! reverb down while the input plays; the wet signal has low and high cuts
//! and a width.

use super::{on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::DelayLine;
use crate::dsp::filter::{BandShape, BandType, Filter, OnePole};
use crate::dsp::smooth::Smoothed;
use crate::dsp::{db, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::f64::consts::{FRAC_1_SQRT_2, TAU};
use std::sync::Arc;

pub mod id {
    pub const TYPE: u32 = 0;
    pub const SIZE: u32 = 1;
    pub const DECAY: u32 = 2;
    pub const PRE_DELAY: u32 = 3;
    pub const DAMPING: u32 = 4;
    pub const BASS: u32 = 5;
    pub const DIFFUSION: u32 = 6;
    pub const MODULATION: u32 = 7;
    pub const MOD_RATE: u32 = 8;
    pub const BALANCE: u32 = 9;
    pub const WIDTH: u32 = 10;
    pub const LOW_CUT: u32 = 11;
    pub const HIGH_CUT: u32 = 12;
    pub const MIX: u32 = 13;
    pub const FREEZE: u32 = 14;
    pub const DUCKING: u32 = 15;
}

/// Published: input, output and wet peaks (linear), the ducking (dB).
pub mod value {
    pub const IN_PEAK: usize = 0;
    pub const OUT_PEAK: usize = 1;
    pub const WET_PEAK: usize = 2;
    pub const DUCK: usize = 3;
}
pub const TAP_VALUES: usize = 4;

pub const TYPES: [&str; 5] = ["Room", "Hall", "Plate", "Chamber", "Ambience"];

/// A type's voicing.
pub struct Voicing {
    /// The delay lines' shortest and longest (ms, at size 1).
    pub lines: (f64, f64),
    /// The early reflections' span (ms) and level.
    pub early: (f64, f64),
    /// The diffusers' lengths (ms).
    pub diffusers: [f64; 4],
    /// The late reverb's level.
    pub late: f64,
}

pub const VOICINGS: [Voicing; 5] = [
    Voicing {
        lines: (7.0, 33.0),
        early: (38.0, 0.9),
        diffusers: [3.1, 2.3, 7.3, 5.1],
        late: 0.9,
    },
    Voicing {
        lines: (27.0, 97.0),
        early: (85.0, 0.6),
        diffusers: [4.7, 3.6, 12.7, 9.3],
        late: 0.65,
    },
    Voicing {
        lines: (9.0, 45.0),
        early: (12.0, 0.0),
        diffusers: [1.9, 3.3, 9.1, 6.7],
        late: 0.8,
    },
    Voicing {
        lines: (14.0, 59.0),
        early: (55.0, 0.8),
        diffusers: [3.7, 2.9, 10.3, 7.9],
        late: 0.75,
    },
    Voicing {
        lines: (3.0, 16.0),
        early: (19.0, 1.0),
        diffusers: [1.3, 1.7, 3.9, 2.9],
        late: 1.0,
    },
];

/// The early reflections at size 1 of a span: (time as a fraction, level,
/// left or right).
const EARLY: [(f64, f64, bool); 12] = [
    (0.043, 0.84, true),
    (0.071, 0.79, false),
    (0.118, -0.72, true),
    (0.162, 0.66, false),
    (0.231, -0.58, false),
    (0.297, 0.55, true),
    (0.364, -0.47, true),
    (0.452, 0.43, false),
    (0.538, -0.37, true),
    (0.649, 0.31, false),
    (0.781, -0.26, true),
    (0.921, 0.21, false),
];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        stepped(id::TYPE, "Type", 4.0, 1.0),
        param(id::SIZE, "Size", 0.0, 1.0, 0.5, Percent),
        param(id::DECAY, "Decay", 0.1, 20.0, 2.0, None),
        param(id::PRE_DELAY, "Pre-Delay", 0.0, 500.0, 12.0, Milliseconds),
        param(id::DAMPING, "Damping", 1_000.0, 20_000.0, 6_000.0, Hertz),
        param(id::BASS, "Bass Decay", 0.5, 2.0, 1.2, None),
        param(id::DIFFUSION, "Diffusion", 0.0, 1.0, 0.75, Percent),
        param(id::MODULATION, "Modulation", 0.0, 1.0, 0.3, Percent),
        param(id::MOD_RATE, "Modulation Rate", 0.05, 3.0, 0.6, Hertz),
        param(id::BALANCE, "Early/Late", 0.0, 1.0, 0.5, Percent),
        param(id::WIDTH, "Width", 0.0, 1.5, 1.0, Percent),
        param(id::LOW_CUT, "Low Cut", 20.0, 1_000.0, 20.0, Hertz),
        param(id::HIGH_CUT, "High Cut", 1_000.0, 20_000.0, 20_000.0, Hertz),
        param(id::MIX, "Mix", 0.0, 1.0, 0.3, Percent),
        stepped(id::FREEZE, "Freeze", 1.0, 0.0),
        param(id::DUCKING, "Ducking", 0.0, 1.0, 0.0, Percent),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::TYPE => pick(&TYPES, v),
        id::FREEZE => on_off(v),
        id::DECAY if v < 1.0 => format!("{:.0} ms", v * 1000.0),
        id::DECAY => format!("{v:.2} s"),
        id::BASS => format!("{v:.2}×"),
        id::DAMPING if v >= 19_500.0 => "Off".into(),
        id::LOW_CUT if v <= 20.5 => "Off".into(),
        id::HIGH_CUT if v >= 19_500.0 => "Off".into(),
        id::BALANCE if (v - 0.5).abs() < 0.01 => "Both".into(),
        id::BALANCE if v < 0.5 => format!("Early {:.0}", (0.5 - v) * 200.0),
        id::BALANCE => format!("Late {:.0}", (v - 0.5) * 200.0),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// How much the size scales a type's lengths.
pub fn size_scale(size: f64) -> f64 {
    0.6 + 0.8 * size.clamp(0.0, 1.0)
}

/// The decay time (s) of the bass, middle and highs.
pub fn decay_times(params: &ParamValues) -> [f64; 3] {
    let rt = get(params, id::DECAY).max(0.05);
    let high = if get(params, id::DAMPING) >= 19_500.0 {
        rt
    } else {
        rt / 3.0
    };
    [rt * get(params, id::BASS), rt, high]
}

const LINES: usize = 16;
const STEP: usize = 32;
/// The bass's crossover (Hz).
const BASS_HZ: f64 = 250.0;

/// The 16 point Hadamard transform, normalised (orthogonal).
#[inline]
fn hadamard(v: &mut [f64; LINES]) {
    let mut h = 1;
    while h < LINES {
        let mut i = 0;
        while i < LINES {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
            i += 2 * h;
        }
        h *= 2;
    }
    for x in v.iter_mut() {
        *x *= 0.25;
    }
}

/// The output sign patterns (two rows of a Hadamard matrix).
const OUT_L: [f64; LINES] = [
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0,
];
const OUT_R: [f64; LINES] = [
    1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0,
];

/// A line's three band decay.
#[derive(Clone, Copy, Default)]
struct Decay {
    low: OnePole,
    high: OnePole,
    gains: [f64; 3],
}

impl Decay {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let low = self.low.low(x);
        let rest = x - low;
        let mid = self.high.low(rest);
        let high = rest - mid;
        self.gains[0] * low + self.gains[1] * mid + self.gains[2] * high
    }
}

/// A Schroeder allpass of a whole number of samples.
struct Allpass {
    line: DelayLine,
    len: usize,
}

impl Allpass {
    #[inline]
    fn process(&mut self, x: f64, g: f64) -> f64 {
        let d = self.line.tap(self.len.max(1) - 1);
        let w = x - g * d;
        self.line.push(flush(w));
        d + g * w
    }
}

pub struct ReverbProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    pre: [DelayLine; 2],
    diffusers: [[Allpass; 4]; 2],
    lines: Vec<DelayLine>,
    /// Each line's length now (samples) and where it glides to.
    length: [f64; LINES],
    target: [f64; LINES],
    decay: [Decay; LINES],
    /// Each line's modulation phase and rate factor.
    phase: [f64; LINES],
    rate: [f64; LINES],
    low_cut: Filter<2>,
    high_cut: Filter<2>,
    freeze: Smoothed,
    mix: Smoothed,
    modulation: Smoothed,
    duck_env: f64,
    duck: f64,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl ReverbProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        // The longest line (Hall at the largest size) with room for the
        // modulation.
        let line_cap = (0.1 * size_scale(1.0) * sr) as usize + 128;
        let ap_cap = (0.014 * size_scale(1.0) * sr) as usize + 8;
        let early_cap = (0.1 * size_scale(1.0) * sr) as usize + 8;
        let pre_cap = (0.5 * sr) as usize + early_cap + 8;
        let mk_ap = || Allpass {
            line: DelayLine::new(ap_cap),
            len: 1,
        };
        let mt = MeterTap::new(sr as f32);
        let mut s = Self {
            watching: Watching::new(sr as f32),
            sr,
            pre: [DelayLine::new(pre_cap), DelayLine::new(pre_cap)],
            diffusers: [
                [mk_ap(), mk_ap(), mk_ap(), mk_ap()],
                [mk_ap(), mk_ap(), mk_ap(), mk_ap()],
            ],
            lines: (0..LINES).map(|_| DelayLine::new(line_cap)).collect(),
            length: [0.0; LINES],
            target: [0.0; LINES],
            decay: [Decay::default(); LINES],
            phase: std::array::from_fn(|i| (i as f64 * 0.618_034).fract()),
            rate: std::array::from_fn(|i| 0.7 + 0.6 * i as f64 / (LINES - 1) as f64),
            low_cut: Filter::default(),
            high_cut: Filter::default(),
            freeze: Smoothed::new(0.0, 20.0, sr),
            mix: Smoothed::new(0.3, 20.0, sr),
            modulation: Smoothed::new(0.0, 50.0, sr),
            duck_env: 0.0,
            duck: 1.0,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        s.control();
        s.length = s.target;
        s.decay_gains();
        s.freeze.snap();
        s.mix.snap();
        s.modulation.snap();
        s
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    fn voicing(&self) -> &'static Voicing {
        &VOICINGS[self.get(id::TYPE).round().clamp(0.0, 4.0) as usize]
    }

    fn control(&mut self) {
        let sr = self.sr;
        let v = self.voicing();
        let scale = size_scale(self.get(id::SIZE));
        let (lo, hi) = (v.lines.0 * scale, v.lines.1 * scale);
        for i in 0..LINES {
            let ms = lo * (hi / lo).powf(i as f64 / (LINES - 1) as f64);
            // Odd whole numbers of samples, kept apart.
            let n = (ms * 0.001 * sr).round() as usize | 1;
            self.target[i] = n as f64 + 2.0 * i as f64;
        }
        for c in 0..2 {
            for (k, ap) in self.diffusers[c].iter_mut().enumerate() {
                // The right side a little longer: decorrelated from the start.
                let ms = v.diffusers[k] * scale * if c == 1 { 1.07 } else { 1.0 };
                ap.len = ((ms * 0.001 * sr).round() as usize).max(1);
            }
        }
        let damping = self.get(id::DAMPING).min(0.45 * sr);
        for d in &mut self.decay {
            d.low.set(BASS_HZ, sr);
            d.high.set(damping, sr);
        }
        let frozen = self.get(id::FREEZE) >= 0.5;
        self.freeze.set(if frozen { 1.0 } else { 0.0 });
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        self.modulation.set(if frozen {
            0.0
        } else {
            self.get(id::MODULATION)
        });
        let low = self.get(id::LOW_CUT);
        if low > 20.5 {
            self.low_cut.set(cut(BandType::LowCut, low), sr);
        } else {
            self.low_cut.clear();
        }
        let high = self.get(id::HIGH_CUT);
        if high < 19_500.0 && high < 0.45 * sr {
            self.high_cut.set(cut(BandType::HighCut, high), sr);
        } else {
            self.high_cut.clear();
        }
    }

    /// The lines' band gains for their lengths now (lossless when frozen).
    fn decay_gains(&mut self) {
        let rts = decay_times(&self.params);
        let frozen = self.freeze.value;
        for (decay, length) in self.decay.iter_mut().zip(&self.length) {
            for (gain, rt) in decay.gains.iter_mut().zip(rts) {
                let g = 10f64.powf(-3.0 * length / (rt * self.sr));
                *gain = g + frozen * (1.0 - g);
            }
        }
    }
}

fn cut(kind: BandType, freq: f64) -> BandShape {
    BandShape {
        kind,
        freq,
        gain: 0.0,
        q: FRAC_1_SQRT_2,
        slope: 12.0,
    }
}

impl PluginProcessor for ReverbProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let Some(channels) = pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let sr = self.sr;
        let glide = 1.0 - (-(STEP as f64) / (0.15 * sr)).exp();
        let duck_attack = 1.0 - (-1.0 / (0.01 * sr)).exp();
        let duck_release = (-1.0 / (0.3 * sr)).exp();
        let mut events = ctx.param_events.iter().peekable();
        let (mut in_peak, mut out_peak, mut wet_peak) = (0.0f64, 0.0f64, 0.0f64);
        let mut at = 0;
        while at < n {
            let end = (at + STEP).min(n);
            let mut moved = at == 0;
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                    moved = true;
                } else {
                    break;
                }
            }
            if moved {
                self.control();
            }
            for i in 0..LINES {
                self.length[i] += (self.target[i] - self.length[i]) * glide;
            }
            self.decay_gains();
            let v = self.voicing();
            let scale = size_scale(self.get(id::SIZE));
            let pre = (self.get(id::PRE_DELAY) * 0.001 * sr).round() as usize;
            let early_span = v.early.0 * 0.001 * scale * sr;
            let balance = self.get(id::BALANCE);
            let early_gain = v.early.1 * (2.0 * (1.0 - balance)).min(1.0) * 0.5;
            let late_gain = v.late * (2.0 * balance).min(1.0);
            let g_ap = 0.75 * self.get(id::DIFFUSION);
            let depth = 0.0015 * sr;
            let step = self.get(id::MOD_RATE) / sr;
            let width = self.get(id::WIDTH);
            let ducking = self.get(id::DUCKING);
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
                let frozen = self.freeze.tick();
                let mix = self.mix.tick();
                let modulation = self.modulation.tick() * depth;
                let live = 1.0 - frozen;
                // Pre-delay, then the early reflections from it.
                let mut early = [0.0; 2];
                let mut fed = [0.0; 2];
                for c in 0..2 {
                    self.pre[c].push(x[c] * live);
                    fed[c] = self.pre[c].tap(pre);
                }
                if early_gain > 0.0 {
                    for (t, g, left) in EARLY {
                        let d = pre + (t * early_span) as usize;
                        let (l, r) = (self.pre[0].tap(d), self.pre[1].tap(d + 7));
                        let (gl, gr) = if left { (1.0, 0.35) } else { (0.35, 1.0) };
                        early[0] += g * gl * l;
                        early[1] += g * gr * r;
                    }
                }
                // Diffused into the network: left into even lines, right
                // into odd ones.
                let mut inject = [0.0; 2];
                for c in 0..2 {
                    let mut s = fed[c];
                    for ap in &mut self.diffusers[c] {
                        s = ap.process(s, g_ap);
                    }
                    inject[c] = s * 0.6;
                }
                let mut y = [0.0; LINES];
                for (k, yk) in y.iter_mut().enumerate() {
                    let m = if modulation > 0.0 {
                        self.phase[k] = (self.phase[k] + step * self.rate[k]).fract();
                        modulation * (1.0 + (TAU * self.phase[k]).sin())
                    } else {
                        0.0
                    };
                    *yk = self.lines[k].read(self.length[k] + m - 1.0);
                }
                let (mut late_l, mut late_r) = (0.0, 0.0);
                let mut f = [0.0; LINES];
                for k in 0..LINES {
                    late_l += OUT_L[k] * y[k];
                    late_r += OUT_R[k] * y[k];
                    f[k] = self.decay[k].process(y[k]);
                }
                hadamard(&mut f);
                for k in 0..LINES {
                    let s = if k % 4 < 2 { 1.0 } else { -1.0 };
                    self.lines[k].push(flush(f[k] + s * inject[k % 2]));
                }
                let late = [late_l * 0.25 * late_gain, late_r * 0.25 * late_gain];
                let mut wet = [0.0; 2];
                for c in 0..2 {
                    let w = early[c] * early_gain + late[c];
                    wet[c] = self.high_cut.process(c, self.low_cut.process(c, w));
                }
                // Width, ducking.
                let mid = 0.5 * (wet[0] + wet[1]);
                let side = 0.5 * (wet[0] - wet[1]) * width;
                let lvl = x[0].abs().max(x[1].abs());
                self.duck_env = if lvl > self.duck_env {
                    self.duck_env + (lvl - self.duck_env) * duck_attack
                } else {
                    self.duck_env * duck_release
                };
                let want = if ducking > 0.0 && frozen < 0.5 {
                    gain(-ducking * 0.6 * (db(self.duck_env) + 40.0).clamp(0.0, 40.0))
                } else {
                    1.0
                };
                self.duck += (want - self.duck) * duck_attack;
                let w = [(mid + side) * self.duck, (mid - side) * self.duck];
                wet_peak = wet_peak.max(w[0].abs()).max(w[1].abs());
                let o = [
                    x[0] * (1.0 - mix) + w[0] * mix,
                    x[1] * (1.0 - mix) + w[1] * mix,
                ];
                out_peak = out_peak.max(o[0].abs()).max(o[1].abs());
                if channels == 1 {
                    out.channel_mut(0)[i] = (0.5 * (o[0] + o[1])) as f32;
                } else {
                    out.channel_mut(0)[i] = o[0] as f32;
                    out.channel_mut(1)[i] = o[1] as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        for d in &mut self.decay {
            d.low.flush();
            d.high.flush();
        }
        self.low_cut.flush();
        self.high_cut.flush();
        self.duck_env = flush(self.duck_env);
        for c in 2..channels {
            out.channel_mut(c).fill(0.0);
        }
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
        self.tap.raise_value(value::WET_PEAK, wet_peak as f32);
        self.tap
            .set_value(value::DUCK, (-db(self.duck.max(1e-6))) as f32);
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.scratch[c][..n].iter().zip(&o[..n]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n);
            self.meters[1][c].publish(&self.tap.meter_out, c, n);
        }
        if watched {
            self.tap
                .input
                .push(&self.scratch[0][..n], &self.scratch[1][..n]);
            let r = out.channel(1.min(channels - 1));
            self.tap.output.push(&out.channel(0)[..n], &r[..n]);
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.reset();
        }
        for c in 0..2 {
            self.pre[c].reset();
            for ap in &mut self.diffusers[c] {
                ap.line.reset();
            }
        }
        for d in &mut self.decay {
            d.low.reset();
            d.high.reset();
        }
        self.low_cut.reset();
        self.high_cut.reset();
        self.duck_env = 0.0;
        self.duck = 1.0;
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, bin_db, silence, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<ReverbProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, ReverbProcessor::new)
    }

    fn click(n: usize) -> (f32, f32) {
        if n == 0 { (1.0, 1.0) } else { (0.0, 0.0) }
    }

    /// The reverberation time of an impulse response (s): Schroeder's
    /// backward integration, the fall from −5 to −25 dB, times three.
    fn rt60(ir: &[f32]) -> f64 {
        let mut energy: Vec<f64> = ir.iter().map(|v| f64::from(*v).powi(2)).collect();
        for i in (0..energy.len() - 1).rev() {
            energy[i] += energy[i + 1];
        }
        let total = energy[0];
        let at = |d: f64| {
            energy
                .iter()
                .position(|e| 10.0 * (e / total).log10() < d)
                .unwrap() as f64
                / SR
        };
        3.0 * (at(-25.0) - at(-5.0))
    }

    #[test]
    fn the_decay_is_as_set() {
        for (kind, rt) in [(1.0, 2.0), (0.0, 0.8), (2.0, 4.0), (3.0, 1.5)] {
            let mut r = rig(&[
                (id::TYPE, kind),
                (id::DECAY, rt),
                (id::BASS, 1.0),
                (id::DAMPING, 20_000.0),
                (id::MIX, 1.0),
                (id::BALANCE, 1.0),
                (id::MODULATION, 0.0),
                (id::PRE_DELAY, 0.0),
            ]);
            let (l, _) = r.run(rt * 1.6 + 0.3, click, silence);
            let measured = rt60(&l);
            assert!(
                (measured / rt - 1.0).abs() < 0.12,
                "{}: {measured:.2} s for {rt} s",
                TYPES[kind as usize]
            );
        }
    }

    #[test]
    fn highs_die_sooner_than_the_bass_and_the_sides_differ() {
        let mut r = rig(&[
            (id::DECAY, 3.0),
            (id::DAMPING, 3_000.0),
            (id::BASS, 1.5),
            (id::MIX, 1.0),
        ]);
        r.run(0.4, tone(150.0, 0.3), silence);
        let (l, _) = r.run(0.05, silence, silence);
        let (low0, high0) = (bin_db(&l, 150.0), 0.0);
        let _ = high0;
        r.run(1.0, silence, silence);
        let (l2, _) = r.run(0.05, silence, silence);
        let low_fall = low0 - bin_db(&l2, 150.0);
        let mut r = rig(&[
            (id::DECAY, 3.0),
            (id::DAMPING, 3_000.0),
            (id::BASS, 1.5),
            (id::MIX, 1.0),
        ]);
        r.run(0.4, tone(8_000.0, 0.3), silence);
        let (h, _) = r.run(0.05, silence, silence);
        let h0 = bin_db(&h, 8_000.0);
        r.run(1.0, silence, silence);
        let (h2, _) = r.run(0.05, silence, silence);
        let high_fall = h0 - bin_db(&h2, 8_000.0);
        // Over 1.05 s: the bass falls 60/4.5 dB a second, the highs 60/1.
        assert!(
            high_fall > 2.5 * low_fall,
            "low −{low_fall:.1}, high −{high_fall:.1}"
        );
        // Left and right are decorrelated.
        let mut r = rig(&[(id::MIX, 1.0), (id::BALANCE, 1.0)]);
        let (l, rr) = r.run(1.0, click, silence);
        let tail = 6_000..l.len();
        let (mut lr, mut ll, mut rr2) = (0.0, 0.0, 0.0);
        for i in tail {
            lr += f64::from(l[i]) * f64::from(rr[i]);
            ll += f64::from(l[i]).powi(2);
            rr2 += f64::from(rr[i]).powi(2);
        }
        let corr = lr / (ll * rr2).sqrt();
        assert!(corr.abs() < 0.3, "{corr:.2}");
    }

    #[test]
    fn freeze_holds_and_pre_delay_waits() {
        let mut r = rig(&[(id::DECAY, 1.0), (id::MIX, 1.0)]);
        r.run(0.3, tone(500.0, 0.3), silence);
        r.set(id::FREEZE, 1.0);
        let (a, _) = r.run(0.5, silence, silence);
        let (b, _) = r.run(2.0, tone(1_234.0, 0.5), silence);
        let level = |x: &[f32]| {
            10.0 * (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).log10()
        };
        let (la, lb) = (level(&a[a.len() / 2..]), level(&b[b.len() / 2..]));
        assert!((la - lb).abs() < 1.5, "{la:.1} → {lb:.1}");
        assert!(bin_db(&b, 1_234.0) < lb - 30.0, "the input stays out");
        // Pre-delay: nothing before it.
        let mut r = rig(&[(id::PRE_DELAY, 100.0), (id::MIX, 1.0)]);
        let (l, _) = r.run(0.3, click, silence);
        assert!(l[..4_800].iter().all(|v| v.abs() < 1e-9));
        assert!(l[4_800..9_600].iter().any(|v| v.abs() > 1e-3));
    }

    #[test]
    fn the_level_is_sensible_and_dry_passes_at_zero_mix() {
        let mut r = rig(&[(id::MIX, 1.0)]);
        let (l, _) = r.run(3.0, click, silence);
        let energy: f64 = l.iter().map(|v| f64::from(*v).powi(2)).sum();
        let e = 10.0 * energy.log10();
        assert!(e > -12.0 && e < 3.0, "{e:.1} dB");
        let mut r = rig(&[(id::MIX, 0.0)]);
        let input = tone(700.0, 0.4);
        let (l, _) = r.run(0.2, &input, silence);
        for (i, v) in l.iter().enumerate().take(4_000) {
            assert!((v - input(i).0).abs() < 1e-6);
        }
    }
}

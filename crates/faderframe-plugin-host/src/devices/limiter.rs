//! Limiter: a lookahead brickwall limiter that keeps the ceiling, sample
//! peaks or true peaks (ITU-R BS.1770, 4× interpolated).
//!
//! Per sample the gain needed to keep the (louder channel's, as linked)
//! peak under the ceiling goes into a sliding window minimum over the
//! lookahead, which is then averaged over an attack window no longer than
//! the lookahead: every value averaged already holds the peak's need, so
//! the gain is down by the time the delayed peak comes out, and it gets
//! there smoothly instead of in a step. The styles set that window
//! (Transparent the whole lookahead, Punchy half, Aggressive a quarter:
//! later, steeper attacks that keep transients; Allround between, Bus
//! slower and always programme dependent, Safe the whole window with a
//! margin so true peaks stay under the ceiling even through the
//! interpolator's error, Modern quick with a programme dependent release)
//! and the release, which in auto mode slows down while the limiting goes
//! on. Channels link in the detection (Stereo Link) and in the release
//! (Release Link: each side's reduction pulled towards the deeper one).
//! What comes out is never over the ceiling (a last clip catches
//! rounding), then trimmed by Output (the output ceiling is the ceiling
//! plus the trim). Input gain drives into the limiter; listening at unity
//! gain takes the gain back off to hear what the limiting does. The
//! output's momentary and short-term loudness (BS.1770) and its true peak
//! are published for the editor.

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::env::DualRelease;
use crate::dsp::loudness::LoudnessMeter;
use crate::dsp::smooth::Smoothed;
use crate::dsp::truepeak::{self, TruePeak};
use crate::dsp::{db, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

pub mod id {
    pub const GAIN: u32 = 0;
    pub const CEILING: u32 = 1;
    pub const RELEASE: u32 = 2;
    pub const LOOKAHEAD: u32 = 3;
    pub const STYLE: u32 = 4;
    pub const TRUE_PEAK: u32 = 5;
    pub const LINK: u32 = 6;
    pub const AUTO_RELEASE: u32 = 7;
    pub const UNITY: u32 = 8;
    /// Trim after the limiter (dB, ≤ 0): the output ceiling is the ceiling
    /// plus this.
    pub const OUTPUT: u32 = 9;
    /// How far each channel's reduction follows the deeper one's.
    pub const RELEASE_LINK: u32 = 10;
}

/// Published values: reduction now and its peak (dB), the input's and the
/// output's peaks (linear; the output's a true peak in true peak mode)
/// since last taken, the output's momentary and short-term loudness
/// (LUFS, −∞ before a window is full).
pub mod value {
    pub const REDUCTION: usize = 0;
    pub const REDUCTION_PEAK: usize = 1;
    pub const IN_PEAK: usize = 2;
    pub const OUT_PEAK: usize = 3;
    pub const LUFS_MOMENTARY: usize = 4;
    pub const LUFS_SHORT: usize = 5;
}
pub const TAP_VALUES: usize = 6;

pub const STYLES: [&str; 7] = [
    "Transparent",
    "Punchy",
    "Aggressive",
    "Allround",
    "Bus",
    "Safe",
    "Modern",
];

/// How a style limits: the share of the lookahead its attack window
/// takes, its release time scale, whether its release is always programme
/// dependent, and the margin (dB) it keeps under the ceiling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub attack_share: f64,
    pub release: f64,
    pub always_auto: bool,
    pub margin: f64,
}

pub fn style(i: usize) -> Style {
    let (attack_share, release, always_auto, margin) = match i {
        1 => (0.5, 0.7, false, 0.0),
        2 => (0.25, 0.4, false, 0.0),
        3 => (0.7, 0.85, false, 0.0),
        4 => (0.6, 1.3, true, 0.0),
        5 => (1.0, 1.5, true, 0.3),
        6 => (0.4, 0.55, true, 0.0),
        _ => (1.0, 1.0, false, 0.0),
    };
    Style {
        attack_share,
        release,
        always_auto,
        margin,
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::GAIN, "Gain", 0.0, 30.0, 0.0, Decibels),
        param(id::CEILING, "Ceiling", -30.0, 0.0, -1.0, Decibels),
        param(id::RELEASE, "Release", 1.0, 1_000.0, 80.0, Milliseconds),
        fixed(param(
            id::LOOKAHEAD,
            "Lookahead",
            0.5,
            10.0,
            4.0,
            Milliseconds,
        )),
        stepped(id::STYLE, "Style", (STYLES.len() - 1) as f64, 0.0),
        fixed(stepped(id::TRUE_PEAK, "True Peak", 1.0, 1.0)),
        param(id::LINK, "Stereo Link", 0.0, 1.0, 1.0, Percent),
        stepped(id::AUTO_RELEASE, "Auto Release", 1.0, 1.0),
        stepped(id::UNITY, "Unity Gain", 1.0, 0.0),
        param(id::OUTPUT, "Output", -30.0, 0.0, 0.0, Decibels),
        param(id::RELEASE_LINK, "Release Link", 0.0, 1.0, 0.0, Percent),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::STYLE => pick(&STYLES, v),
        id::TRUE_PEAK | id::AUTO_RELEASE | id::UNITY => on_off(v),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// The lookahead in samples at `rate`.
fn look(params: &ParamValues, rate: f64) -> usize {
    (get(params, id::LOOKAHEAD) * 0.001 * rate).round().max(1.0) as usize
}

pub fn latency(params: &ParamValues, rate: f64) -> u32 {
    let tp = if get(params, id::TRUE_PEAK) >= 0.5 {
        truepeak::LATENCY
    } else {
        0
    };
    (look(params, rate) + tp) as u32
}

/// A sliding window minimum over the last `len` values (a monotonic queue
/// in a ring, allocated once).
struct WindowMin {
    idx: Vec<u64>,
    val: Vec<f64>,
    head: usize,
    len: usize,
    count: usize,
    t: u64,
    window: u64,
}

impl WindowMin {
    fn new(window: usize) -> Self {
        let cap = window + 2;
        Self {
            idx: vec![0; cap],
            val: vec![0.0; cap],
            head: 0,
            len: 0,
            count: cap,
            t: 0,
            window: window as u64,
        }
    }

    #[inline]
    fn push(&mut self, v: f64) -> f64 {
        let cap = self.count;
        // Drop from the back what the new value makes irrelevant.
        while self.len > 0 {
            let back = (self.head + self.len - 1) % cap;
            if self.val[back] >= v {
                self.len -= 1;
            } else {
                break;
            }
        }
        let at = (self.head + self.len) % cap;
        self.idx[at] = self.t;
        self.val[at] = v;
        self.len += 1;
        // Drop from the front what has left the window.
        while self.len > 0 && self.idx[self.head] + self.window < self.t {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.t += 1;
        self.val[self.head]
    }

    fn reset(&mut self) {
        self.len = 0;
        self.head = 0;
        self.t = 0;
    }
}

/// A running mean over the last `n` values.
struct Mean {
    buf: Vec<f64>,
    pos: usize,
    sum: f64,
    n: usize,
}

impl Mean {
    fn new(capacity: usize) -> Self {
        Self {
            buf: vec![1.0; capacity.max(1)],
            pos: 0,
            sum: capacity.max(1) as f64,
            n: capacity.max(1),
        }
    }

    /// Use the last `n` (≤ capacity) values from now on.
    fn set_len(&mut self, n: usize) {
        let n = n.clamp(1, self.buf.len());
        if n != self.n {
            self.n = n;
            self.sum = (0..n)
                .map(|k| self.buf[(self.pos + self.buf.len() - 1 - k) % self.buf.len()])
                .sum();
        }
    }

    #[inline]
    fn push(&mut self, v: f64) -> f64 {
        let cap = self.buf.len();
        let leaving = self.buf[(self.pos + cap - self.n) % cap];
        self.buf[self.pos] = v;
        self.pos = (self.pos + 1) % cap;
        self.sum += v - leaving;
        self.sum / self.n as f64
    }

    fn reset(&mut self) {
        self.buf.fill(1.0);
        self.sum = self.n as f64;
    }
}

/// A delay of a whole number of samples per channel.
struct Delay2 {
    buf: Vec<[f64; 2]>,
    pos: usize,
}

impl Delay2 {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![[0.0; 2]; len.max(1)],
            pos: 0,
        }
    }

    #[inline]
    fn tick(&mut self, x: [f64; 2]) -> [f64; 2] {
        let y = std::mem::replace(&mut self.buf[self.pos], x);
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }

    fn reset(&mut self) {
        self.buf.fill([0.0; 2]);
    }
}

pub struct LimiterProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    look: usize,
    true_peak: bool,
    tp: [TruePeak; 2],
    /// The input, delayed to line up with the true peak detector.
    tp_align: Delay2,
    /// The output's true peaks (in true peak mode).
    out_tp: [TruePeak; 2],
    loudness: LoudnessMeter,
    audio: Delay2,
    /// Per channel: the window minimum of the needed gain, its mean over
    /// the attack window, the release.
    window: [WindowMin; 2],
    mean: [Mean; 2],
    release: [DualRelease; 2],
    drive: Smoothed,
    trim: Smoothed,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl LimiterProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let look = look(&params, sr);
        let true_peak = get(&params, id::TRUE_PEAK) >= 0.5;
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let delay = look + if true_peak { truepeak::LATENCY } else { 0 };
        let drive = Smoothed::new(gain(get(&params, id::GAIN)), 20.0, sr);
        let trim = Smoothed::new(gain(get(&params, id::OUTPUT)), 20.0, sr);
        Self {
            watching: Watching::new(sr as f32),
            sr,
            look,
            true_peak,
            tp: [TruePeak::new(), TruePeak::new()],
            tp_align: Delay2::new(truepeak::LATENCY),
            out_tp: [TruePeak::new(), TruePeak::new()],
            loudness: LoudnessMeter::new(sr),
            audio: Delay2::new(delay),
            window: [WindowMin::new(look), WindowMin::new(look)],
            mean: [Mean::new(look + 1), Mean::new(look + 1)],
            release: [DualRelease::default(); 2],
            drive,
            trim,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }
}

/// Sub-blocks the controls are updated in.
const STEP: usize = 32;

impl PluginProcessor for LimiterProcessor {
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
        let mut events = ctx.param_events.iter().peekable();
        let (mut gr_peak, mut in_peak, mut out_peak) = (0.0f64, 0.0f64, 0.0f64);
        let mut at = 0;
        while at < n {
            let end = (at + STEP).min(n);
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                } else {
                    break;
                }
            }
            let st = style(self.get(id::STYLE).round().max(0.0) as usize);
            // The ceiling kept (with the style's margin) and the one
            // nothing passes.
            let ceiling = gain(self.get(id::CEILING));
            let kept = gain(self.get(id::CEILING) - st.margin);
            let attack_len = ((self.look as f64 * st.attack_share).round() as usize).max(1) + 1;
            let release_ms = self.get(id::RELEASE) * st.release;
            for c in 0..2 {
                self.mean[c].set_len(attack_len);
                self.release[c].set(0.0001, release_ms, self.sr);
            }
            let auto = st.always_auto || self.get(id::AUTO_RELEASE) >= 0.5;
            let link = self.get(id::LINK).clamp(0.0, 1.0);
            let release_link = self.get(id::RELEASE_LINK).clamp(0.0, 1.0);
            let drive_db = self.get(id::GAIN);
            self.drive.set(gain(drive_db));
            self.trim.set(gain(self.get(id::OUTPUT).min(0.0)));
            let unity = if self.get(id::UNITY) >= 0.5 {
                gain(-drive_db)
            } else {
                1.0
            };
            for i in at..end {
                let d = self.drive.tick();
                let trim = self.trim.tick() * unity;
                let x = [
                    f64::from(self.scratch[0][i]) * d,
                    f64::from(self.scratch[1][i]) * d,
                ];
                in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
                // The peaks the gain must keep under the ceiling.
                let level = if self.true_peak {
                    let aligned = self.tp_align.tick(x);
                    let mut l = [0.0; 2];
                    for c in 0..2 {
                        l[c] = self.tp[c].process(x[c]).max(aligned[c].abs());
                    }
                    l
                } else {
                    [x[0].abs(), x[1].abs()]
                };
                // Each channel limits on its own peak or, as far as they
                // are linked, the louder one's (fully linked: one gain).
                let loudest = level[0].max(level[1]);
                let dry = self.audio.tick(x);
                let mut y = [0.0f64; 2];
                let mut gr = [0.0f64; 2];
                let mut avg = [1.0f64; 2];
                for c in 0..2 {
                    let p = level[c].max(link * loudest);
                    let need = if p > kept { kept / p } else { 1.0 };
                    let held = self.window[c].push(need);
                    avg[c] = self.mean[c].push(held).min(1.0);
                    // Instant down (the window has shaped it), released
                    // gently; never above what the window asks.
                    gr[c] = self.release[c].process((-db(avg[c])).max(0.0), auto);
                }
                // The release link: each side pulled towards the deeper
                // reduction (more reduction keeps the ceiling as well).
                if release_link > 0.0 {
                    let deeper = gr[0].max(gr[1]);
                    for g in &mut gr {
                        *g += release_link * (deeper - *g);
                    }
                }
                for c in 0..2 {
                    let g = gain(-gr[c]).min(avg[c]);
                    gr_peak = gr_peak.max(-db(g));
                    // Rounding and release corners: nothing passes the
                    // ceiling.
                    y[c] = (dry[c] * g).clamp(-ceiling, ceiling) * trim;
                }
                if self.true_peak {
                    for (tp, v) in self.out_tp.iter_mut().zip(y) {
                        out_peak = out_peak.max(tp.process(v));
                    }
                } else {
                    out_peak = out_peak.max(y[0].abs()).max(y[1].abs());
                }
                self.loudness.add(y, channels);
                if channels >= 2 {
                    out.channel_mut(0)[i] = y[0] as f32;
                    out.channel_mut(1)[i] = y[1] as f32;
                } else {
                    out.channel_mut(0)[i] = y[0] as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        self.loudness.flush();
        let gr_now = self.release[0].fast.max(self.release[1].fast).max(0.0);
        self.tap.set_value(value::REDUCTION, gr_now as f32);
        self.tap
            .set_value(value::LUFS_MOMENTARY, self.loudness.momentary() as f32);
        self.tap
            .set_value(value::LUFS_SHORT, self.loudness.short_term() as f32);
        self.tap.raise_value(value::REDUCTION_PEAK, gr_peak as f32);
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
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
        self.tp.iter_mut().for_each(TruePeak::reset);
        self.out_tp.iter_mut().for_each(TruePeak::reset);
        self.loudness.reset();
        self.tp_align.reset();
        self.audio.reset();
        for c in 0..2 {
            self.window[c].reset();
            self.mean[c].reset();
            self.release[c].reset();
        }
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;

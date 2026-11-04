//! Multiband Dynamics: the signal split into up to six bands, each with a
//! dynamics processor of its own — a compressor or an expander, working
//! downward (turning loud parts down, quiet parts further down) or upward
//! (lifting quiet parts, loud parts further up).
//!
//! The split is either minimum phase or linear phase:
//! * Minimum phase: Linkwitz–Riley crossovers (24 dB/oct, two Butterworth
//!   sections each way) in a tree — the first splits off the lowest band,
//!   the next splits what is left — and every band passes the all passes of
//!   the crossovers above it that it did not go through (a band's own
//!   crossover's low and high parts add up to a second-order all pass), so
//!   the bands add up to one all pass whatever their number. No latency.
//! * Linear phase: per band a symmetric FIR whose magnitude is the
//!   Linkwitz–Riley band's (`|LP| = 1 / (1 + (f/fc)⁴)`, `|HP| = 1 − |LP|`, a
//!   band the product along the tree): the bands' magnitudes add up to one
//!   at every frequency, and with the same window on each the FIRs add up
//!   to a delayed impulse — the unprocessed bands give the input back
//!   exactly, a half FIR later. They run on the convolution engine
//!   (`dsp::convolve`, one path per band and channel), new crossovers
//!   designed on a thread of the processor's own and crossfaded in.
//!
//! Each band's detector hears the key's band (the input's, or — External
//! on and a sidechain routed in — the sidechain's, split alike and, in
//! linear phase, delayed by the FIRs' half so it stays in time), as peaks
//! or RMS, stereo linked by Link. The gain computer works in dB with a soft
//! knee: Compress takes `(1 − 1/ratio)` of what is over the threshold,
//! Expand takes `(ratio − 1)` times what is under it, Upward Compress lifts
//! by `(1 − 1/ratio)` of what is under it, Upward Expand by `(ratio − 1)`
//! times what is over it; Range caps every change. Attack moves the gain
//! away from 0 dB, Release back. Lookahead delays the bands (the
//! detectors hear ahead). Bands can be soloed, muted and bypassed; Mix
//! blends with the bands summed unprocessed (in time and phase with the
//! wet signal).

use super::compressor::{INFINITE, reduction};
use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::convolve::{self, Designer, Engine, Kernels, Routing};
use crate::dsp::delay::DelayLine;
use crate::dsp::env::{MeanSquare, coeff};
use crate::dsp::filter::{Crossover, Svf};
use crate::dsp::smooth::Smoothed;
use crate::dsp::{db, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_realtime::{MailboxReceiver, MailboxSender};
use realfft::RealFftPlanner;
use std::sync::Arc;

/// The most bands.
pub const BANDS: usize = 6;

/// Parameter ids (each its position): the globals, then each band's
/// fields at [`id::band`].
pub mod id {
    /// How many bands (1–6).
    pub const BANDS: u32 = 0;
    /// The crossovers between band `k` and `k + 1` (Hz): `CROSSOVER + k`.
    pub const CROSSOVER: u32 = 1;
    /// 0: minimum phase (Linkwitz–Riley), 1: linear phase (latency).
    pub const PHASE: u32 = 6;
    pub const LOOKAHEAD: u32 = 7;
    /// 0: peak, 1: RMS.
    pub const DETECTOR: u32 = 8;
    pub const LINK: u32 = 9;
    /// On: a sidechain routed in keys the bands.
    pub const EXTERNAL: u32 = 10;
    pub const OUTPUT: u32 = 11;
    pub const MIX: u32 = 12;
    /// The first band's first field.
    pub const BAND_BASE: u32 = 13;
    /// Fields per band.
    pub const FIELDS: u32 = 11;

    pub const THRESHOLD: u32 = 0;
    pub const RATIO: u32 = 1;
    pub const KNEE: u32 = 2;
    pub const ATTACK: u32 = 3;
    pub const RELEASE: u32 = 4;
    /// The band's makeup (dB).
    pub const GAIN: u32 = 5;
    /// [`super::MODES`].
    pub const MODE: u32 = 6;
    /// The most the band's level changes (dB).
    pub const RANGE: u32 = 7;
    pub const SOLO: u32 = 8;
    pub const BYPASS: u32 = 9;
    pub const MUTE: u32 = 10;

    /// Band `b`'s `field`.
    pub const fn band(b: usize, field: u32) -> u32 {
        BAND_BASE + FIELDS * b as u32 + field
    }

    /// The band and field of a band parameter.
    pub fn field_of(id: u32) -> Option<(usize, u32)> {
        let rel = id.checked_sub(BAND_BASE)?;
        let b = (rel / FIELDS) as usize;
        (b < super::BANDS).then_some((b, rel % FIELDS))
    }
}

/// Published values: the deepest cut now and its peak since the editor
/// looked (dB, positive), the input and output peaks (linear), and per
/// band its gain change now (dB: negative cut, positive lift) and its
/// detector's level (dB).
pub mod value {
    pub const REDUCTION: usize = 0;
    pub const REDUCTION_PEAK: usize = 1;
    pub const IN_PEAK: usize = 2;
    pub const OUT_PEAK: usize = 3;
    /// Band `b`'s gain change: `GAIN + b`.
    pub const GAIN: usize = 4;
    /// Band `b`'s level: `LEVEL + b`.
    pub const LEVEL: usize = 4 + super::BANDS;
}
pub const TAP_VALUES: usize = 4 + 2 * BANDS;

pub const MODES: [&str; 4] = ["Compress", "Expand", "Up Comp", "Up Exp"];
pub const PHASES: [&str; 2] = ["Minimum", "Linear"];
pub const DETECTORS: [&str; 2] = ["Peak", "RMS"];

/// The crossovers' defaults (Hz).
pub const CROSSOVERS: [f64; BANDS - 1] = [120.0, 600.0, 2_500.0, 6_000.0, 12_000.0];

/// How a band works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Compress,
    Expand,
    UpwardCompress,
    UpwardExpand,
}

impl Mode {
    pub fn of(v: f64) -> Self {
        match v.round() as i32 {
            1 => Mode::Expand,
            2 => Mode::UpwardCompress,
            3 => Mode::UpwardExpand,
            _ => Mode::Compress,
        }
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let mut v = vec![ParameterInfo {
        stepped: true,
        ..param(id::BANDS, "Bands", 1.0, BANDS as f64, 4.0, None)
    }];
    for (k, f) in CROSSOVERS.iter().enumerate() {
        v.push(param(
            id::CROSSOVER + k as u32,
            match k {
                0 => "Crossover 1",
                1 => "Crossover 2",
                2 => "Crossover 3",
                3 => "Crossover 4",
                _ => "Crossover 5",
            },
            20.0,
            20_000.0,
            *f,
            Hertz,
        ));
    }
    v.extend([
        fixed(stepped(id::PHASE, "Phase", 1.0, 0.0)),
        fixed(param(
            id::LOOKAHEAD,
            "Lookahead",
            0.0,
            10.0,
            0.0,
            Milliseconds,
        )),
        stepped(id::DETECTOR, "Detector", 1.0, 0.0),
        param(id::LINK, "Stereo Link", 0.0, 1.0, 1.0, Percent),
        stepped(id::EXTERNAL, "External Sidechain", 1.0, 1.0),
        param(id::OUTPUT, "Output", -24.0, 24.0, 0.0, Decibels),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
    ]);
    const NAMES: [&str; BANDS] = ["Band 1", "Band 2", "Band 3", "Band 4", "Band 5", "Band 6"];
    for (b, band) in NAMES.iter().enumerate() {
        let n = |f: &str| format!("{band} {f}");
        let field = |f: u32, name: &str, min: f64, max: f64, default: f64, unit| ParameterInfo {
            name: n(name),
            ..param(id::band(b, f), name, min, max, default, unit)
        };
        let switch = |f: u32, name: &str, max: f64| ParameterInfo {
            name: n(name),
            ..stepped(id::band(b, f), name, max, 0.0)
        };
        v.extend([
            field(id::THRESHOLD, "Threshold", -60.0, 0.0, -20.0, Decibels),
            field(id::RATIO, "Ratio", 1.0, INFINITE, 2.0, None),
            field(id::KNEE, "Knee", 0.0, 24.0, 6.0, Decibels),
            field(id::ATTACK, "Attack", 0.1, 250.0, 10.0, Milliseconds),
            field(id::RELEASE, "Release", 5.0, 2_500.0, 150.0, Milliseconds),
            field(id::GAIN, "Gain", -24.0, 24.0, 0.0, Decibels),
            switch(id::MODE, "Mode", (MODES.len() - 1) as f64),
            field(id::RANGE, "Range", 0.0, 48.0, 24.0, Decibels),
            switch(id::SOLO, "Solo", 1.0),
            switch(id::BYPASS, "Bypass", 1.0),
            switch(id::MUTE, "Mute", 1.0),
        ]);
    }
    v
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::BANDS => format!("{v:.0}"),
        id::PHASE => pick(&PHASES, v),
        id::DETECTOR => pick(&DETECTORS, v),
        id::EXTERNAL => on_off(v),
        id::LOOKAHEAD if v < 0.005 => "Off".into(),
        other => {
            let (_, f) = id::field_of(other)?;
            match f {
                id::RATIO if v >= INFINITE - 0.5 => "∞ : 1".into(),
                id::RATIO => format!("{v:.1} : 1"),
                id::MODE => pick(&MODES, v),
                id::SOLO | id::BYPASS | id::MUTE => on_off(v),
                id::KNEE if v < 0.05 => "Hard".into(),
                _ => return None,
            }
        }
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// Linear phase chosen.
fn linear(params: &ParamValues) -> bool {
    get(params, id::PHASE) >= 0.5
}

/// The linear-phase FIRs' length at `rate` (taps; odd, symmetric round
/// their middle): about 85 ms.
pub fn fir_taps(rate: f64) -> usize {
    let mut n = 4_096usize;
    while (n as f64) < 4_096.0 * rate / 48_000.0 {
        n *= 2;
    }
    n - 1
}

/// The latency at `rate`: the lookahead, and half the FIRs in linear phase.
pub fn latency(params: &ParamValues, rate: f64) -> u32 {
    let look = (get(params, id::LOOKAHEAD).clamp(0.0, 10.0) * 1e-3 * rate).round() as u32;
    let fir = if linear(params) {
        (fir_taps(rate) / 2) as u32
    } else {
        0
    };
    look + fir
}

/// The bands in use.
pub fn band_count(params: &ParamValues) -> usize {
    (get(params, id::BANDS).round() as usize).clamp(1, BANDS)
}

/// The crossovers in use (Hz), each above the one before it and below
/// Nyquist.
pub fn crossovers(params: &ParamValues, rate: f64) -> [f64; BANDS - 1] {
    let mut f = [0.0; BANDS - 1];
    let mut floor = 10.0;
    let top = 0.45 * rate;
    for (k, v) in f.iter_mut().enumerate() {
        let want = get(params, id::CROSSOVER + k as u32).clamp(20.0, 20_000.0);
        *v = want.max(floor * 1.05).min(top);
        floor = *v;
    }
    f
}

/// A Linkwitz–Riley low part's magnitude at `f` (the high part's is the
/// rest).
fn lr_low(f: f64, fc: f64) -> f64 {
    1.0 / (1.0 + (f / fc).powi(4))
}

/// Band `b`'s magnitude at `f` of `n` bands split at `xs` (they add up to
/// one).
pub fn band_magnitude(b: usize, n: usize, xs: &[f64], f: f64) -> f64 {
    let mut m = 1.0;
    for (k, &fc) in xs.iter().enumerate().take(n.saturating_sub(1)) {
        let low = lr_low(f, fc);
        if k < b {
            m *= 1.0 - low;
        } else if k == b {
            m *= low;
            break;
        }
    }
    m
}

/// The gain change (dB) a band's static curve asks for at `level` (dB).
pub fn curve(mode: Mode, level: f64, threshold: f64, ratio: f64, knee: f64, range: f64) -> f64 {
    let infinite = ratio >= INFINITE - 0.5;
    // The soft excess over and under the threshold.
    let over = reduction(level, threshold, INFINITE, knee);
    let under = reduction(2.0 * threshold - level, threshold, INFINITE, knee);
    let expand = if infinite {
        f64::INFINITY
    } else {
        ratio.max(1.0) - 1.0
    };
    match mode {
        Mode::Compress => -reduction(level, threshold, ratio, knee).min(range),
        Mode::Expand => -(under * expand).min(range),
        Mode::UpwardCompress => {
            let s = if infinite {
                1.0
            } else {
                1.0 - 1.0 / ratio.max(1.0)
            };
            (under * s).min(range)
        }
        Mode::UpwardExpand => (over * expand).min(range),
    }
}

/// A band's settings as read for a sub-block.
#[derive(Clone, Copy, Debug)]
struct BandSet {
    mode: Mode,
    threshold: f64,
    ratio: f64,
    knee: f64,
    range: f64,
    attack: f64,
    release: f64,
    bypass: bool,
}

/// The minimum-phase split of one channel.
#[derive(Clone, Copy, Default)]
struct Split {
    cross: [Crossover; BANDS - 1],
    /// For band `b` the all passes of crossovers `b + 1 …` (`[b][k]`).
    ap: [[Svf; BANDS - 1]; BANDS],
    tuned: [f64; BANDS - 1],
}

impl Split {
    fn tune(&mut self, xs: &[f64; BANDS - 1], rate: f64) {
        for (k, &f) in xs.iter().enumerate() {
            if self.tuned[k] == f {
                continue;
            }
            self.tuned[k] = f;
            self.cross[k].set(f, rate);
            for ap in &mut self.ap {
                ap[k].set(f, std::f64::consts::FRAC_1_SQRT_2, rate);
            }
        }
    }

    /// `x` into `n` bands.
    #[inline]
    fn split(&mut self, x: f64, n: usize, out: &mut [f64; BANDS]) {
        let mut rest = x;
        for (cross, band) in self.cross.iter_mut().zip(out.iter_mut()).take(n - 1) {
            let (low, high) = cross.split(rest);
            *band = low;
            rest = high;
        }
        out[n - 1] = rest;
        // Each band through the all passes of the crossovers after its own.
        for (b, band) in out.iter_mut().enumerate().take(n.saturating_sub(2)) {
            for k in b + 1..n - 1 {
                let v = self.ap[b][k].process(*band);
                *band -= 2.0 * std::f64::consts::SQRT_2 * v.band;
            }
        }
    }

    fn reset(&mut self) {
        for c in &mut self.cross {
            c.reset();
        }
        for s in self.ap.iter_mut().flatten() {
            s.reset();
        }
    }

    fn flush(&mut self) {
        for c in &mut self.cross {
            c.flush();
        }
        for s in self.ap.iter_mut().flatten() {
            s.flush();
        }
    }
}

/// The linear-phase FIRs: per band a symmetric response of
/// [`fir_taps`] taps.
pub fn band_firs(
    n: usize,
    xs: &[f64],
    rate: f64,
    planner: &mut RealFftPlanner<f32>,
) -> Vec<Vec<f32>> {
    let taps = fir_taps(rate);
    let m = 2 * (taps + 1);
    let inverse = planner.plan_fft_inverse(m);
    let half = taps / 2;
    // A Hann window over the taps (the same on every band: they still add
    // up to an impulse).
    let window: Vec<f64> = (0..taps)
        .map(|j| 0.5 - 0.5 * (std::f64::consts::TAU * (j + 1) as f64 / (taps + 1) as f64).cos())
        .collect();
    (0..BANDS)
        .map(|b| {
            if b >= n {
                return vec![0.0; taps];
            }
            let mut spec = inverse.make_input_vec();
            for (k, s) in spec.iter_mut().enumerate() {
                let f = k as f64 * rate / m as f64;
                s.re = band_magnitude(b, n, xs, f) as f32;
            }
            let mut time = inverse.make_output_vec();
            if inverse.process(&mut spec, &mut time).is_err() {
                return vec![0.0; taps];
            }
            (0..taps)
                .map(|j| {
                    let at = (j + m - half) % m;
                    (f64::from(time[at]) / m as f64 * window[j]) as f32
                })
                .collect()
        })
        .collect()
}

/// The linear-phase split's paths: band `b`'s FIR from channel `c` into
/// output `2b + c`.
fn fir_routing() -> Routing {
    let mut paths = Vec::with_capacity(2 * BANDS);
    for b in 0..BANDS {
        for c in 0..2 {
            paths.push((c, 2 * b + c));
        }
    }
    Routing::new(2, 2 * BANDS, paths)
}

fn fir_kernels(n: usize, xs: &[f64], rate: f64, planner: &mut RealFftPlanner<f32>) -> Kernels {
    let firs = band_firs(n, xs, rate, planner);
    let paths: Vec<&[f32]> = firs
        .iter()
        .flat_map(|h| [h.as_slice(), h.as_slice()])
        .collect();
    Kernels::new(&paths, planner)
}

/// The FIRs' design thread: new crossovers (or a new band count) become
/// new kernels for the engine.
struct FirDesign {
    params: ParamValues,
    rate: f64,
    planner: RealFftPlanner<f32>,
    tx: MailboxSender<Kernels>,
    back: rtrb::Consumer<Box<Kernels>>,
    last: (usize, [f64; BANDS - 1]),
}

impl FirDesign {
    fn shape(params: &ParamValues, rate: f64) -> (usize, [f64; BANDS - 1]) {
        (band_count(params), crossovers(params, rate))
    }

    fn poll(&mut self) {
        while let Ok(k) = self.back.pop() {
            drop(k);
        }
        let now = Self::shape(&self.params, self.rate);
        if now == self.last {
            return;
        }
        self.last = now;
        let k = fir_kernels(now.0, &now.1, self.rate, &mut self.planner);
        drop(self.tx.send(Box::new(k)));
    }
}

/// The linear-phase split on the audio thread.
struct Fir {
    engine: Box<Engine>,
    incoming: MailboxReceiver<Kernels>,
    back: rtrb::Producer<Box<Kernels>>,
    held: Option<Box<Kernels>>,
    _designer: Designer,
    /// An external key delayed by the FIRs' half (in time with the bands).
    key_delay: [DelayLine; 2],
    half: usize,
    /// Per output (band and channel), this block's samples.
    out: Vec<Vec<f32>>,
}

impl Fir {
    fn new(params: &ParamValues, rate: f64, block: usize) -> Self {
        let mut planner = RealFftPlanner::new();
        let shape = FirDesign::shape(params, rate);
        let kernels = Box::new(fir_kernels(shape.0, &shape.1, rate, &mut planner));
        let taps = fir_taps(rate);
        let engine = Box::new(Engine::new(fir_routing(), taps, kernels, &mut planner));
        let ((tx, back_rx), (rx, back)) = convolve::channels::<Kernels, Box<Kernels>>();
        let mut design = FirDesign {
            params: params.clone(),
            rate,
            planner,
            tx,
            back: back_rx,
            last: shape,
        };
        let half = taps / 2;
        Self {
            engine,
            incoming: rx,
            back,
            held: None,
            _designer: Designer::spawn("faderframe-crossover", move || design.poll()),
            key_delay: [DelayLine::new(half + 1), DelayLine::new(half + 1)],
            half,
            out: vec![vec![0.0; block]; 2 * BANDS],
        }
    }

    fn take_updates(&mut self) {
        if let Some(h) = self.held.take()
            && let Err(rtrb::PushError::Full(h)) = self.back.push(h)
        {
            self.held = Some(h);
            return;
        }
        if let Some(k) = self.engine.take_retired()
            && let Err(rtrb::PushError::Full(k)) = self.back.push(k)
        {
            self.held = Some(k);
            return;
        }
        if !self.engine.switching()
            && let Some(k) = self.incoming.take()
            && let Err(k) = self.engine.offer(k)
            && let Err(rtrb::PushError::Full(k)) = self.back.push(k)
        {
            self.held = Some(k);
        }
    }
}

const STEP: usize = 32;

pub struct MultibandProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    split: [Split; 2],
    key_split: [Split; 2],
    fir: Option<Fir>,
    /// Lookahead: per band and channel.
    delay: Vec<DelayLine>,
    look: usize,
    rms: [[MeanSquare; 2]; BANDS],
    peak: [[f64; 2]; BANDS],
    peak_fall: f64,
    /// Each band's gain change now (dB) per channel.
    change: [[f64; 2]; BANDS],
    output: Smoothed,
    mix: Smoothed,
    /// Each band's makeup and whether it is heard, smoothed.
    makeup: [Smoothed; BANDS],
    heard: [Smoothed; BANDS],
    meters: [[MeterTap; 2]; 2],
    dry: [Vec<f32>; 2],
}

impl MultibandProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let look = (get(&params, id::LOOKAHEAD).clamp(0.0, 10.0) * 1e-3 * sr).round() as usize;
        let fir = linear(&params).then(|| Fir::new(&params, sr, block));
        let mt = MeterTap::new(sr as f32);
        let mut p = Self {
            watching: Watching::new(sr as f32),
            sr,
            split: [Split::default(); 2],
            key_split: [Split::default(); 2],
            fir,
            delay: (0..2 * BANDS).map(|_| DelayLine::new(look + 1)).collect(),
            look,
            rms: [[MeanSquare::new(10.0, sr); 2]; BANDS],
            peak: [[0.0; 2]; BANDS],
            peak_fall: (-1.0 / (0.010 * sr)).exp(),
            change: [[0.0; 2]; BANDS],
            output: Smoothed::new(1.0, 20.0, sr),
            mix: Smoothed::new(1.0, 20.0, sr),
            makeup: [Smoothed::new(1.0, 20.0, sr); BANDS],
            heard: [Smoothed::new(1.0, 10.0, sr); BANDS],
            meters: [[mt; 2]; 2],
            dry: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        p.control();
        p.output.snap();
        p.mix.snap();
        for s in p.makeup.iter_mut().chain(p.heard.iter_mut()) {
            s.snap();
        }
        p
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    /// Read the controls (every sub-block).
    fn control(&mut self) -> [BandSet; BANDS] {
        let n = band_count(&self.params);
        let xs = crossovers(&self.params, self.sr);
        for s in self.split.iter_mut().chain(self.key_split.iter_mut()) {
            s.tune(&xs, self.sr);
        }
        self.output.set(gain(self.get(id::OUTPUT)));
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        let solo = (0..n).any(|b| self.get(id::band(b, id::SOLO)) >= 0.5);
        let sr = self.sr;
        let params = &self.params;
        let (heard, makeup) = (&mut self.heard, &mut self.makeup);
        std::array::from_fn(|b| {
            let f = |field: u32| get(params, id::band(b, field));
            let audible = if b >= n || f(id::MUTE) >= 0.5 {
                false
            } else {
                !solo || f(id::SOLO) >= 0.5
            };
            heard[b].set(if audible { 1.0 } else { 0.0 });
            makeup[b].set(gain(f(id::GAIN)));
            BandSet {
                mode: Mode::of(f(id::MODE)),
                threshold: f(id::THRESHOLD),
                ratio: f(id::RATIO),
                knee: f(id::KNEE),
                range: f(id::RANGE).max(0.0),
                attack: coeff(f(id::ATTACK), sr),
                release: coeff(f(id::RELEASE), sr),
                bypass: f(id::BYPASS) >= 0.5,
            }
        })
    }
}

impl PluginProcessor for MultibandProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let side = io.audio_in.get(1).filter(|k| k.num_channels() > 0);
        let Some(channels) = pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n_frames = frames.min(self.dry[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.dry[c][..n_frames].copy_from_slice(&src[..n_frames]);
        }
        let external = self.get(id::EXTERNAL) >= 0.5 && side.is_some();
        // Linear phase: the whole block's bands first.
        if let Some(fir) = &mut self.fir {
            fir.take_updates();
            let mut outs: [&mut [f32]; 2 * BANDS] = {
                let mut it = fir.out.iter_mut();
                std::array::from_fn(|_| match it.next() {
                    Some(v) => &mut v[..n_frames],
                    None => &mut [],
                })
            };
            convolve::run(
                &mut fir.engine,
                &[&self.dry[0][..n_frames], &self.dry[1][..n_frames]],
                &mut outs,
            );
        }
        let link = self.get(id::LINK).clamp(0.0, 1.0);
        let rms = self.get(id::DETECTOR) >= 0.5;
        let mut events = ctx.param_events.iter().peekable();
        let (mut in_peak, mut out_peak, mut gr_peak) = (0.0f32, 0.0f32, 0.0f64);
        let mut levels = [-150.0f64; BANDS];
        let mut at = 0;
        while at < n_frames {
            let end = (at + STEP).min(n_frames);
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                } else {
                    break;
                }
            }
            let sets = self.control();
            let n = band_count(&self.params);
            for i in at..end {
                let x = [f64::from(self.dry[0][i]), f64::from(self.dry[1][i])];
                in_peak = in_peak.max(x[0].abs().max(x[1].abs()) as f32);
                // The bands and the key's bands.
                let mut bands = [[0.0f64; BANDS]; 2];
                let mut keys = [[0.0f64; BANDS]; 2];
                match &mut self.fir {
                    Some(fir) => {
                        for (c, channel) in bands.iter_mut().enumerate() {
                            for (b, v) in channel.iter_mut().enumerate() {
                                *v = f64::from(fir.out[2 * b + c][i]);
                            }
                        }
                        if let Some(s) = side.filter(|_| external) {
                            for (c, key) in keys.iter_mut().enumerate() {
                                let k = f64::from(s.channel(c.min(s.num_channels() - 1))[i]);
                                fir.key_delay[c].push(k);
                                let k = fir.key_delay[c].tap(fir.half);
                                self.key_split[c].split(k, n, key);
                            }
                        } else {
                            keys = bands;
                        }
                    }
                    None => {
                        for (c, channel) in bands.iter_mut().enumerate() {
                            self.split[c].split(x[c], n, channel);
                        }
                        if let Some(s) = side.filter(|_| external) {
                            for (c, key) in keys.iter_mut().enumerate() {
                                let k = f64::from(s.channel(c.min(s.num_channels() - 1))[i]);
                                self.key_split[c].split(k, n, key);
                            }
                        } else {
                            keys = bands;
                        }
                    }
                }
                let out_gain = self.output.tick();
                let mix = self.mix.tick();
                let mut wet = [0.0f64; 2];
                let mut dry = [0.0f64; 2];
                for (b, set) in sets.iter().enumerate().take(n) {
                    // The detector, per channel, then linked.
                    let mut lv = [0.0f64; 2];
                    for c in 0..2 {
                        let k = keys[c][b];
                        lv[c] = if rms {
                            10.0 * (2.0 * self.rms[b][c].process(k)).max(1e-24).log10()
                        } else {
                            let pk = &mut self.peak[b][c];
                            let a = k.abs();
                            *pk = if a > *pk { a } else { *pk * self.peak_fall };
                            db(*pk)
                        };
                    }
                    let loudest = lv[0].max(lv[1]);
                    levels[b] = levels[b].max(loudest);
                    let makeup = self.makeup[b].tick();
                    let heard = self.heard[b].tick();
                    for c in 0..2 {
                        let level = link * loudest + (1.0 - link) * lv[c];
                        let target = if set.bypass {
                            0.0
                        } else {
                            curve(
                                set.mode,
                                level,
                                set.threshold,
                                set.ratio,
                                set.knee,
                                set.range,
                            )
                        };
                        let g = &mut self.change[b][c];
                        let k = if target.abs() > g.abs() {
                            set.attack
                        } else {
                            set.release
                        };
                        *g += (target - *g) * k;
                        if *g < 0.0 {
                            gr_peak = gr_peak.max(-*g);
                        }
                        let line = &mut self.delay[2 * b + c];
                        line.push(bands[c][b]);
                        let v = line.tap(self.look);
                        dry[c] += v;
                        let applied = if set.bypass { 1.0 } else { gain(*g) * makeup };
                        wet[c] += v * applied * heard;
                    }
                }
                let mut y = [0.0; 2];
                for c in 0..2 {
                    y[c] = (dry[c] * (1.0 - mix) + wet[c] * mix) * out_gain;
                }
                out_peak = out_peak.max(y[0].abs().max(y[1].abs()) as f32);
                if channels >= 2 {
                    out.channel_mut(0)[i] = y[0] as f32;
                    out.channel_mut(1)[i] = y[1] as f32;
                } else {
                    out.channel_mut(0)[i] = (0.5 * (y[0] + y[1])) as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        for s in self.split.iter_mut().chain(self.key_split.iter_mut()) {
            s.flush();
        }
        for b in 0..BANDS {
            for c in 0..2 {
                self.peak[b][c] = flush(self.peak[b][c]);
                self.rms[b][c].flush();
                self.change[b][c] = flush(self.change[b][c]);
            }
        }
        for c in 2..channels {
            out.channel_mut(c).fill(0.0);
        }
        // Published values.
        let n = band_count(&self.params);
        let mut deepest = 0.0f64;
        for (b, (change, level)) in self.change.iter().zip(levels).enumerate() {
            let g = if b < n {
                0.5 * (change[0] + change[1])
            } else {
                0.0
            };
            deepest = deepest.max(-change[0].min(change[1]).min(0.0));
            self.tap.set_value(value::GAIN + b, g as f32);
            self.tap.set_value(value::LEVEL + b, level as f32);
        }
        self.tap.set_value(value::REDUCTION, deepest as f32);
        self.tap.raise_value(value::REDUCTION_PEAK, gr_peak as f32);
        self.tap.raise_value(value::IN_PEAK, in_peak);
        self.tap.raise_value(value::OUT_PEAK, out_peak);
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.dry[c][..n_frames].iter().zip(&o[..n_frames]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n_frames);
            self.meters[1][c].publish(&self.tap.meter_out, c, n_frames);
        }
        if watched {
            self.tap
                .input
                .push(&self.dry[0][..n_frames], &self.dry[1][..n_frames]);
            let r = out.channel(1.min(channels - 1));
            self.tap
                .output
                .push(&out.channel(0)[..n_frames], &r[..n_frames]);
            if let Some(k) = side {
                let r = k.channel(1.min(k.num_channels() - 1));
                self.tap
                    .sidechain
                    .push(&k.channel(0)[..n_frames], &r[..n_frames]);
            }
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for s in self.split.iter_mut().chain(self.key_split.iter_mut()) {
            s.reset();
        }
        if let Some(fir) = &mut self.fir {
            fir.engine.reset();
            for d in &mut fir.key_delay {
                d.reset();
            }
        }
        for d in &mut self.delay {
            d.reset();
        }
        for b in 0..BANDS {
            for c in 0..2 {
                self.rms[b][c].reset();
                self.peak[b][c] = 0.0;
                self.change[b][c] = 0.0;
            }
        }
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;

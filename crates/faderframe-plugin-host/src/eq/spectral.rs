//! Spectral dynamics: a dynamic band that acts on each frequency of its
//! region on its own, only where that frequency stands over the threshold,
//! instead of moving the whole band.
//!
//! The signal runs through a short-time Fourier transform (square-root
//! Hann windows, 75 % overlap, so that unity gains rebuild the signal
//! exactly) of [`frame`] samples, which is also the stage's latency. For
//! each spectral band, per frame:
//!
//! * the trigger's power per frequency (the input, or the sidechain; both
//!   channels, one side, mid or side as the band is placed), optionally
//!   tilted by 3 dB/oct round 1 kHz (so a pink mix triggers evenly), is
//!   smoothed across frequency — widely at a low density, down to single
//!   bins at a high one — and over time with the band's attack and release;
//! * the threshold is the band's, or (auto) a little over the region's own
//!   level smoothed over an octave and a second;
//! * each frequency moves by [`super::dynamic_gain`] of its own excess,
//!   weighted by how much the frequency belongs to the band (the band's
//!   shape, normalised), on top of the band's static gain (also applied
//!   here: spectral bands are linear phase).
//!
//! The gains of all spectral bands on each part of the signal are summed in
//! dB and applied to the spectra; mid and side bands work on the mid and
//! side spectra.

use super::design::{AnalogBand, BandShape, BandType};
use super::{
    BANDS, BandParams, Placement, SPECTRAL_POINTS, dynamic_gain, global, spectral_point_hz, value,
};
use crate::ParamValues;
use crate::tap::AnalysisTap;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Frame sizes of the resolutions (the highest two as high: spectral
/// dynamics would smear too long beyond it).
pub const FRAMES: [usize; 5] = [1024, 2048, 4096, 4096, 4096];

/// The frame size at a resolution.
pub fn frame(quality: usize) -> usize {
    FRAMES[quality.min(FRAMES.len() - 1)]
}

/// The stage's latency at a resolution: the frame and the hop its work is
/// spread over.
pub fn latency(quality: usize) -> usize {
    let n = frame(quality);
    n + n / 4
}

/// Whether any band in use is spectral (the stage exists; switching one off
/// for a moment keeps it, so the latency holds).
pub fn wanted(params: &ParamValues) -> bool {
    (0..BANDS).any(|b| {
        let p = BandParams::read(params, b);
        p.used && p.is_spectral()
    })
}

/// The automatic threshold sits this far over the region's level (dB).
const AUTO_MARGIN: f64 = 3.0;
/// The automatic threshold's time constant (seconds).
const AUTO_TIME: f64 = 1.0;
/// The automatic attack and release (ms).
const AUTO_ATTACK: f64 = 4.0;
const AUTO_RELEASE: f64 = 80.0;
/// Level floor (dB).
const FLOOR: f64 = -140.0;

type C64 = Complex<f64>;

/// The smoothing coefficient per bin for a width of `octaves` (forward and
/// backward one-pole passes along the bins).
fn widths(octaves: f64, bins: usize, out: &mut [f64]) {
    let span = 2f64.powf(octaves) - 1.0;
    for (k, c) in out.iter_mut().enumerate().take(bins) {
        let tau = 0.5 * k as f64 * span;
        *c = if tau > 0.05 { (-1.0 / tau).exp() } else { 0.0 };
    }
}

/// Smooth `src` along the bins into `dst` with per-bin coefficients.
fn smooth(src: &[f64], coeff: &[f64], dst: &mut [f64]) {
    let n = src.len().min(dst.len()).min(coeff.len());
    if n == 0 {
        return;
    }
    let mut y = src[0];
    for k in 0..n {
        let a = coeff[k];
        y = (1.0 - a) * src[k] + a * y;
        dst[k] = y;
    }
    let mut z = dst[n - 1];
    for k in (0..n).rev() {
        let a = coeff[k];
        z = (1.0 - a) * dst[k] + a * z;
        dst[k] = z;
    }
}

/// One spectral band's curves and state.
struct Band {
    /// The shape and density its curves were made for.
    made_for: Option<(BandShape, f64)>,
    weight: Vec<f64>,
    static_db: Vec<f64>,
    density: Vec<f64>,
    env: Vec<f64>,
    reference: Vec<f64>,
    live: bool,
    /// A remake in parts: the analog shapes it evaluates, the weight's
    /// peak so far, and whether only the density changed (nothing to
    /// evaluate).
    analog: Option<AnalogBand>,
    unit: Option<AnalogBand>,
    peak: f64,
    density_only: bool,
}

impl Band {
    fn new(bins: usize) -> Self {
        Self {
            made_for: None,
            weight: vec![0.0; bins],
            static_db: vec![0.0; bins],
            density: vec![0.0; bins],
            env: vec![FLOOR; bins],
            reference: vec![-60.0; bins],
            live: false,
            analog: None,
            unit: None,
            peak: 0.0,
            density_only: false,
        }
    }
}

/// Parts a band's curves are remade in (bins split evenly).
const REMAKE_PARTS: usize = 8;
/// Steps of one frame at most: four forward transforms, per band its
/// remake and its dynamics, the gains, two inverse transforms.
const MAX_STEPS: usize = 4 + BANDS * (REMAKE_PARTS + 1) + 3;

/// One step of a frame's work.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// The forward transform of an input (left, right, key left, key
    /// right).
    Forward(usize),
    /// A part of a band's curves.
    Remake { band: usize, part: usize },
    /// A band's movement per frequency, into the gains.
    Band(usize),
    /// The gains onto the spectra.
    Gains,
    /// The inverse transform of a channel, overlapped into the output.
    Inverse(usize),
}

/// The spectral stage. A frame's work is spread over the hop after it:
/// at a hop's end the windowed input is copied and the frame planned in
/// steps (transforms, a band's curves in parts, a band's dynamics, the
/// gains); the steps run as the next hop's samples arrive, evenly, and
/// the frame's output plays from the hop after — one hop more latency
/// than the frame itself, no burst of every transform on one sample.
pub struct Spectral {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    sr: f64,
    n: usize,
    hop: usize,
    bins: usize,
    window: Vec<f64>,
    /// The last `n` samples of the input (left, right) and the sidechain
    /// (left, right); `pos` is the oldest.
    rings: [Vec<f64>; 4],
    pos: usize,
    /// The output playing, and the one the frame under way makes.
    out: [Vec<f64>; 2],
    next: [Vec<f64>; 2],
    out_pos: usize,
    acc: [Vec<f64>; 2],
    /// The frame's windowed inputs (copied at the hop's end).
    frames: [Vec<f64>; 4],
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    scratch_f: Vec<C64>,
    scratch_i: Vec<C64>,
    time: Vec<f64>,
    main: [Vec<C64>; 2],
    key: [Vec<C64>; 2],
    bands: Box<[Band]>,
    /// Gains (dB) on both channels, left, right, mid, side.
    gains: [Vec<f64>; 5],
    touched: [bool; 5],
    power: Vec<f64>,
    smoothed: Vec<f64>,
    wide: Vec<f64>,
    moved: Vec<f64>,
    freq: Vec<f64>,
    tilt: Vec<f64>,
    wide_coeff: Vec<f64>,
    /// Power to a sine's peak level.
    norm: f64,
    /// The frame's settings (read at the hop's end).
    read: [BandParams; BANDS],
    scale: f64,
    interact: bool,
    /// The frame's steps, how many there are and how many are done.
    plan: [Step; MAX_STEPS],
    steps: usize,
    done: usize,
}

impl Spectral {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, sr: f64, n: usize) -> Self {
        let n = n.next_power_of_two().max(256);
        let bins = n / 2 + 1;
        let mut planner = RealFftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(n);
        let inverse = planner.plan_fft_inverse(n);
        let freq: Vec<f64> = (0..bins)
            .map(|k| (k as f64 * sr / n as f64).max(0.25 * sr / n as f64))
            .collect();
        let tilt = freq.iter().map(|f| f / 1000.0).collect();
        let mut wide_coeff = vec![0.0; bins];
        widths(1.0, bins, &mut wide_coeff);
        let read = [BandParams::read(&params, 0); BANDS];
        Self {
            params,
            tap,
            sr,
            n,
            hop: n / 4,
            bins,
            window: (0..n)
                .map(|i| (std::f64::consts::PI * i as f64 / n as f64).sin())
                .collect(),
            rings: std::array::from_fn(|_| vec![0.0; n]),
            pos: 0,
            out: [vec![0.0; n / 4], vec![0.0; n / 4]],
            next: [vec![0.0; n / 4], vec![0.0; n / 4]],
            out_pos: 0,
            acc: [vec![0.0; n], vec![0.0; n]],
            frames: std::array::from_fn(|_| vec![0.0; n]),
            scratch_f: forward.make_scratch_vec(),
            scratch_i: inverse.make_scratch_vec(),
            forward,
            inverse,
            time: vec![0.0; n],
            main: [
                vec![C64::new(0.0, 0.0); bins],
                vec![C64::new(0.0, 0.0); bins],
            ],
            key: [
                vec![C64::new(0.0, 0.0); bins],
                vec![C64::new(0.0, 0.0); bins],
            ],
            bands: (0..BANDS).map(|_| Band::new(bins)).collect(),
            gains: std::array::from_fn(|_| vec![0.0; bins]),
            touched: [false; 5],
            power: vec![0.0; bins],
            smoothed: vec![0.0; bins],
            wide: vec![0.0; bins],
            moved: vec![0.0; bins],
            freq,
            tilt,
            wide_coeff,
            norm: (std::f64::consts::PI / n as f64).powi(2),
            read,
            scale: 1.0,
            interact: false,
            plan: [Step::Gains; MAX_STEPS],
            steps: 0,
            done: 0,
        }
    }

    /// The stage's latency (samples): the frame and the hop its work is
    /// spread over.
    pub fn latency(&self) -> usize {
        self.n + self.hop
    }

    /// One frame in (the input and the sidechain), one out, `latency()`
    /// later.
    #[inline]
    pub fn process(&mut self, l: f64, r: f64, side: [f64; 2]) -> (f64, f64) {
        let p = self.pos;
        self.rings[0][p] = l;
        self.rings[1][p] = r;
        self.rings[2][p] = side[0];
        self.rings[3][p] = side[1];
        self.pos = if p + 1 == self.n { 0 } else { p + 1 };
        let y = (self.out[0][self.out_pos], self.out[1][self.out_pos]);
        self.out_pos += 1;
        // The frame under way: its steps due by this sample.
        let due = self.steps * self.out_pos / self.hop;
        while self.done < due {
            let step = self.plan[self.done];
            self.done += 1;
            self.run(step);
        }
        if self.out_pos == self.hop {
            self.out_pos = 0;
            self.hop_end();
        }
        y
    }

    /// A hop's end: the finished frame's output plays next; the next frame
    /// is copied and planned.
    fn hop_end(&mut self) {
        while self.done < self.steps {
            let step = self.plan[self.done];
            self.done += 1;
            self.run(step);
        }
        std::mem::swap(&mut self.out, &mut self.next);
        // The settings for this frame.
        self.scale = f64::from(self.params.get(global::GAIN_SCALE));
        self.interact = self.params.get(global::GAIN_Q) >= 0.5;
        for (b, slot) in self.read.iter_mut().enumerate() {
            *slot = BandParams::read(&self.params, b);
        }
        let live = |p: &BandParams| p.enabled && p.is_spectral();
        let keyed = self
            .read
            .iter()
            .any(|p| live(p) && p.dynamics_on() && p.keyed_externally());
        // The windowed input as it is now.
        let (n, pos) = (self.n, self.pos);
        for f in 0..if keyed { 4 } else { 2 } {
            let (ring, frame) = (&self.rings[f], &mut self.frames[f]);
            for (i, (x, w)) in frame.iter_mut().zip(&self.window).enumerate() {
                let mut j = pos + i;
                if j >= n {
                    j -= n;
                }
                *x = ring[j] * w;
            }
        }
        for g in &mut self.gains {
            g.fill(0.0);
        }
        self.touched = [false; 5];
        // The steps.
        let mut steps = 0;
        let mut push = |plan: &mut [Step; MAX_STEPS], s: Step| {
            if steps < MAX_STEPS {
                plan[steps] = s;
                steps += 1;
            }
        };
        for f in 0..if keyed { 4 } else { 2 } {
            push(&mut self.plan, Step::Forward(f));
        }
        for b in 0..BANDS {
            let p = self.read[b];
            if !live(&p) {
                if self.bands[b].live {
                    self.bands[b].live = false;
                    self.publish_rest(b);
                }
                continue;
            }
            let shape = p.shape(self.scale, self.interact);
            if self.bands[b].made_for != Some((shape, p.density)) {
                for part in 0..REMAKE_PARTS {
                    push(&mut self.plan, Step::Remake { band: b, part });
                }
            }
            push(&mut self.plan, Step::Band(b));
        }
        push(&mut self.plan, Step::Gains);
        push(&mut self.plan, Step::Inverse(0));
        push(&mut self.plan, Step::Inverse(1));
        self.steps = steps;
        self.done = 0;
    }

    fn run(&mut self, step: Step) {
        match step {
            Step::Forward(f) => {
                self.time.copy_from_slice(&self.frames[f]);
                let dst = if f < 2 {
                    &mut self.main[f]
                } else {
                    &mut self.key[f - 2]
                };
                if self
                    .forward
                    .process_with_scratch(&mut self.time, dst, &mut self.scratch_f)
                    .is_err()
                {
                    dst.fill(C64::new(0.0, 0.0));
                }
            }
            Step::Remake { band, part } => self.remake(band, part),
            Step::Band(b) => self.band(b),
            Step::Gains => self.gains(),
            Step::Inverse(c) => self.inverse(c),
        }
    }

    /// A part of a band's weight and static curves (for its shape now).
    fn remake(&mut self, b: usize, part: usize) {
        let p = self.read[b];
        let shape = p.shape(self.scale, self.interact);
        let bins = self.bins;
        let band = &mut self.bands[b];
        if part == 0 {
            let same_shape = band.made_for.is_some_and(|(s, _)| s == shape);
            band.made_for = Some((shape, p.density));
            // From a full octave at 0 % down to about a fiftieth at 100 %.
            let octaves = 2f64.powf(-p.density.clamp(0.0, 1.0) * 5.6);
            widths(octaves, bins, &mut band.density);
            band.density_only = same_shape;
            if same_shape {
                return;
            }
            band.analog = Some(AnalogBand::new(&shape));
            // The band's region: its shape at +12 dB, normalised.
            band.unit = (shape.kind != BandType::FlatTilt).then(|| {
                AnalogBand::new(&BandShape {
                    gain: 12.0,
                    ..shape
                })
            });
            band.peak = 0.0;
        }
        if band.density_only {
            return;
        }
        let (from, to) = (bins * part / REMAKE_PARTS, bins * (part + 1) / REMAKE_PARTS);
        if let Some(analog) = &band.analog {
            for (s, f) in band.static_db[from..to]
                .iter_mut()
                .zip(&self.freq[from..to])
            {
                *s = analog.db(*f);
            }
        }
        match &band.unit {
            None => band.weight[from..to].fill(1.0),
            Some(unit) => {
                for (w, f) in band.weight[from..to].iter_mut().zip(&self.freq[from..to]) {
                    *w = unit.db(*f).abs();
                    band.peak = band.peak.max(*w);
                }
                if part + 1 == REMAKE_PARTS && band.peak > 0.0 {
                    let peak = band.peak;
                    for w in &mut band.weight {
                        *w /= peak;
                    }
                }
            }
        }
    }

    /// A band's movement per frequency, added into the gains.
    fn band(&mut self, b: usize) {
        let p = self.read[b];
        let scale = self.scale;
        let hop_s = self.hop as f64 / self.sr;
        let band = &mut self.bands[b];
        if !band.live {
            band.live = true;
            band.env.fill(FLOOR);
            band.reference.fill(-60.0);
        }
        let target = p.placement.index();
        self.touched[target] = true;
        if p.dynamics_on() {
            // The trigger's power per frequency.
            let src = if p.keyed_externally() {
                &self.key
            } else {
                &self.main
            };
            for (((power, a), c), tilt) in self
                .power
                .iter_mut()
                .zip(&src[0])
                .zip(&src[1])
                .zip(&self.tilt)
            {
                let (a, c) = (*a, *c);
                let pw = match p.placement {
                    Placement::Stereo => a.norm_sqr().max(c.norm_sqr()),
                    Placement::Left => a.norm_sqr(),
                    Placement::Right => c.norm_sqr(),
                    Placement::Mid => ((a + c) * 0.5).norm_sqr(),
                    Placement::Side => ((a - c) * 0.5).norm_sqr(),
                };
                *power = if p.spectral_tilt { pw * tilt } else { pw };
            }
            smooth(&self.power, &band.density, &mut self.smoothed);
            let auto = p.auto_threshold();
            if auto {
                smooth(&self.power, &self.wide_coeff, &mut self.wide);
            }
            let (fa, fr) = p.time_factors();
            let ka = 1.0 - (-hop_s / (AUTO_ATTACK * fa * 0.001)).exp();
            let kr = 1.0 - (-hop_s / (AUTO_RELEASE * fr * 0.001)).exp();
            let k_auto = 1.0 - (-hop_s / AUTO_TIME).exp();
            let range = p.range * scale;
            let (mut extreme, mut loudest, mut thr_sum, mut weight_sum) = (0.0f64, FLOOR, 0.0, 0.0);
            for k in 0..self.bins {
                let level = 10.0 * (self.smoothed[k] * self.norm + 1e-30).log10();
                let e = &mut band.env[k];
                *e += (level - *e) * if level > *e { ka } else { kr };
                let threshold = if auto {
                    let wide = 10.0 * (self.wide[k] * self.norm + 1e-30).log10();
                    let r = &mut band.reference[k];
                    if wide > -100.0 {
                        *r += (wide - *r) * k_auto;
                    }
                    *r + AUTO_MARGIN
                } else {
                    p.threshold
                };
                let w = band.weight[k];
                let moved = dynamic_gain(range, threshold, *e) * w;
                self.moved[k] = moved;
                if moved.abs() > extreme.abs() {
                    extreme = moved;
                }
                if w > 0.5 {
                    loudest = loudest.max(*e);
                    thr_sum += threshold * w;
                    weight_sum += w;
                }
            }
            self.tap.set_value(value::DYN + b, extreme as f32);
            self.tap.set_value(value::KEY + b, loudest as f32);
            if weight_sum > 0.0 {
                self.tap
                    .set_value(value::THRESHOLD + b, (thr_sum / weight_sum) as f32);
            }
        } else {
            self.moved.fill(0.0);
            self.tap.set_value(value::DYN + b, 0.0);
        }
        let band = &self.bands[b];
        for ((g, s), m) in self.gains[target]
            .iter_mut()
            .zip(&band.static_db)
            .zip(&self.moved)
        {
            *g += s + m;
        }
        self.publish(b);
    }

    /// Publish a band's movement per frequency for the editor.
    fn publish(&self, b: usize) {
        let ratio = 3_000.0f64.powf(0.5 / (SPECTRAL_POINTS - 1) as f64);
        let per_bin = self.n as f64 / self.sr;
        for i in 0..SPECTRAL_POINTS {
            let f = spectral_point_hz(i);
            let v = if f >= 0.5 * self.sr {
                0.0
            } else {
                let lo = ((f / ratio) * per_bin).floor().max(0.0) as usize;
                let hi = (((f * ratio) * per_bin).ceil() as usize).min(self.bins - 1);
                let near = ((f * per_bin).round() as usize).min(self.bins - 1);
                let mut v = self.moved[near];
                for m in &self.moved[lo.min(hi)..=hi] {
                    if m.abs() > v.abs() {
                        v = *m;
                    }
                }
                v
            };
            self.tap
                .set_value(value::SPECTRAL + b * SPECTRAL_POINTS + i, v as f32);
        }
    }

    fn publish_rest(&self, b: usize) {
        for i in 0..SPECTRAL_POINTS {
            self.tap
                .set_value(value::SPECTRAL + b * SPECTRAL_POINTS + i, 0.0);
        }
        self.tap.set_value(value::DYN + b, 0.0);
    }

    /// Apply the gains to the spectra.
    fn gains(&mut self) {
        let db = |v: f64| 10f64.powf(v / 20.0);
        let touched = self.touched;
        for c in 0..2 {
            let side = 1 + c;
            if touched[0] || touched[side] {
                for k in 0..self.bins {
                    let g = self.gains[0][k] + self.gains[side][k];
                    if g != 0.0 {
                        self.main[c][k] *= db(g);
                    }
                }
            }
        }
        if touched[3] || touched[4] {
            for k in 0..self.bins {
                let (a, b) = (self.main[0][k], self.main[1][k]);
                let m = (a + b) * 0.5 * db(self.gains[3][k]);
                let s = (a - b) * 0.5 * db(self.gains[4][k]);
                self.main[0][k] = m + s;
                self.main[1][k] = m - s;
            }
        }
    }

    /// Transform a channel back and overlap-add it: the next hop's output.
    fn inverse(&mut self, c: usize) {
        // Square-root Hann twice at 75 % overlap sums to 2.
        let norm = 1.0 / (2.0 * self.n as f64);
        let (n, hop) = (self.n, self.hop);
        self.main[c][0].im = 0.0;
        self.main[c][self.bins - 1].im = 0.0;
        if self
            .inverse
            .process_with_scratch(&mut self.main[c], &mut self.time, &mut self.scratch_i)
            .is_err()
        {
            self.time.fill(0.0);
        }
        let acc = &mut self.acc[c];
        for ((a, t), w) in acc.iter_mut().zip(&self.time).zip(&self.window) {
            *a += t * w * norm;
        }
        self.next[c].copy_from_slice(&acc[..hop]);
        acc.copy_within(hop.., 0);
        acc[n - hop..].fill(0.0);
    }

    pub fn reset(&mut self) {
        for r in &mut self.rings {
            r.fill(0.0);
        }
        for c in 0..2 {
            self.out[c].fill(0.0);
            self.next[c].fill(0.0);
            self.acc[c].fill(0.0);
        }
        self.out_pos = 0;
        self.steps = 0;
        self.done = 0;
        // A band's envelopes start again when it next runs (as when it
        // comes on): a reset clears nothing there.
        for b in self.bands.iter_mut() {
            b.live = false;
        }
    }
}

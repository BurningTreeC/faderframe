//! Binaural monitoring: a surround bed (or a stereo or mono master) heard
//! on headphones as if each of its speakers stood around the listener.
//!
//! Each speaker's feed is convolved with the two impulse responses measured
//! from its direction at the ears of a dummy head (SADIE II, the Neumann
//! KU100, University of York, Apache-2.0; `data/LICENSE-SADIE.txt`): fifteen
//! directions, the ITU-R BS.2051 angles FaderFrame's formats use, baked by
//! `scripts/binaural_hrirs.py` at 44.1, 48 and 96 kHz (other rates
//! resampled from the nearest). The convolution is uniformly partitioned
//! in the frequency domain (blocks of 64 frames, 128 at high rates): one
//! forward transform per speaker, products accumulated per ear, one
//! inverse transform per ear — a block of latency. The LFE has no
//! direction: low-passed at 120 Hz it reaches both ears at +10 dB (its
//! level in a speaker system). [`Room`]: Near is the dry measurement; Mid
//! and Far add a small room (a feedback delay network, decorrelated left
//! and right) about 8 and 3 dB under the direct sound.
//!
//! Nothing allocates once a [`Renderer`] is made: it can run on the audio
//! thread.

#![forbid(unsafe_code)]

use faderframe_core::ChannelLayout;
use faderframe_core::surround::speakers_of;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

static DATA: &[u8] = include_bytes!("../data/sadie-d1.ffhr");

/// The baked directions, in the file's order (azimuth positive to the
/// left, elevation).
pub const DIRECTIONS: [(f32, f32); 15] = [
    (0.0, 0.0),
    (30.0, 0.0),
    (-30.0, 0.0),
    (90.0, 0.0),
    (-90.0, 0.0),
    (110.0, 0.0),
    (-110.0, 0.0),
    (135.0, 0.0),
    (-135.0, 0.0),
    (45.0, 30.0),
    (-45.0, 30.0),
    (135.0, 30.0),
    (-135.0, 30.0),
    (90.0, 45.0),
    (-90.0, 45.0),
];

#[derive(Debug, thiserror::Error)]
pub enum BinauralError {
    #[error("the HRIR data is damaged")]
    Data,
}

/// The impulse responses of one direction.
#[derive(Clone, Debug)]
pub struct Pair {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// A set of directions at one sample rate.
#[derive(Clone, Debug)]
pub struct Hrirs {
    pub rate: u32,
    pub taps: usize,
    pub pairs: Vec<Pair>,
}

fn read_u32(b: &[u8], at: &mut usize) -> Option<u32> {
    let v = u32::from_le_bytes(b.get(*at..*at + 4)?.try_into().ok()?);
    *at += 4;
    Some(v)
}

fn read_f32s(b: &[u8], at: &mut usize, n: usize) -> Option<Vec<f32>> {
    let bytes = b.get(*at..*at + 4 * n)?;
    *at += 4 * n;
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

/// Every baked set.
fn baked() -> Result<Vec<Hrirs>, BinauralError> {
    let b = DATA;
    if b.get(..4) != Some(b"FFHR") {
        return Err(BinauralError::Data);
    }
    let mut at = 4;
    let _version = read_u32(b, &mut at).ok_or(BinauralError::Data)?;
    let sets = read_u32(b, &mut at).ok_or(BinauralError::Data)?;
    let mut out = Vec::new();
    for _ in 0..sets {
        let rate = read_u32(b, &mut at).ok_or(BinauralError::Data)?;
        let taps = read_u32(b, &mut at).ok_or(BinauralError::Data)? as usize;
        let n = read_u32(b, &mut at).ok_or(BinauralError::Data)? as usize;
        let mut pairs = Vec::with_capacity(n);
        for _ in 0..n {
            let _dir = read_f32s(b, &mut at, 2).ok_or(BinauralError::Data)?;
            let left = read_f32s(b, &mut at, taps).ok_or(BinauralError::Data)?;
            let right = read_f32s(b, &mut at, taps).ok_or(BinauralError::Data)?;
            pairs.push(Pair { left, right });
        }
        out.push(Hrirs { rate, taps, pairs });
    }
    Ok(out)
}

/// `x` resampled by `ratio` (windowed sinc, 16 taps each side; below the
/// lower rate's Nyquist).
fn resample(x: &[f32], ratio: f64) -> Vec<f32> {
    let n = ((x.len() as f64) * ratio).ceil() as usize;
    let cutoff = ratio.min(1.0);
    let half = 16.0;
    (0..n)
        .map(|i| {
            let t = i as f64 / ratio;
            let lo = (t - half).floor().max(0.0) as usize;
            let hi = ((t + half).ceil() as usize).min(x.len().saturating_sub(1));
            let mut acc = 0.0f64;
            for (j, &v) in x.iter().enumerate().take(hi + 1).skip(lo) {
                let d = t - j as f64;
                let s = if d.abs() < 1e-9 {
                    1.0
                } else {
                    let a = std::f64::consts::PI * d * cutoff;
                    a.sin() / a
                };
                let w = 0.5 + 0.5 * (std::f64::consts::PI * d / half).cos();
                acc += f64::from(v) * s * cutoff * if d.abs() <= half { w } else { 0.0 };
            }
            acc as f32
        })
        .collect()
}

/// The set for `rate`: baked, or resampled from the nearest baked rate.
pub fn hrirs(rate: u32) -> Result<Hrirs, BinauralError> {
    let sets = baked()?;
    if let Some(s) = sets.iter().find(|s| s.rate == rate) {
        return Ok(s.clone());
    }
    // From the same family where there is one (88.2/176.4 from 44.1 is
    // not baked: from 96 kHz), else the closest above.
    let source = sets
        .iter()
        .filter(|s| s.rate >= rate.min(96_000))
        .min_by_key(|s| s.rate)
        .or_else(|| sets.iter().max_by_key(|s| s.rate))
        .ok_or(BinauralError::Data)?;
    let ratio = f64::from(rate) / f64::from(source.rate);
    let pairs: Vec<Pair> = source
        .pairs
        .iter()
        .map(|p| Pair {
            left: resample(&p.left, ratio),
            right: resample(&p.right, ratio),
        })
        .collect();
    let taps = pairs.first().map_or(0, |p| p.left.len());
    Ok(Hrirs { rate, taps, pairs })
}

/// The baked direction a speaker of `layout` (by its label) stands at;
/// `None`: the LFE (or discrete channels).
pub fn direction(layout: ChannelLayout, channel: usize) -> Option<usize> {
    let quad = layout == ChannelLayout::Surround(faderframe_core::SurroundFormat::Quad);
    let s = speakers_of(layout)?.get(channel)?;
    if s.lfe {
        return None;
    }
    Some(match s.label {
        "C" | "M" => 0,
        "L" => 1,
        "R" => 2,
        "Lss" => 3,
        "Rss" => 4,
        "Ls" if quad => 7,
        "Rs" if quad => 8,
        "Ls" => 5,
        "Rs" => 6,
        "Lrs" => 7,
        "Rrs" => 8,
        "Ltf" => 9,
        "Rtf" => 10,
        "Ltr" => 11,
        "Rtr" => 12,
        "Ltm" => 13,
        "Rtm" => 14,
        _ => return None,
    })
}

/// How the headphone render places the mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Room {
    /// The dry measurement: close, anechoic.
    Near,
    /// A little room around it.
    #[default]
    Mid,
    /// More room, further away.
    Far,
}

impl Room {
    pub const ALL: [Room; 3] = [Room::Near, Room::Mid, Room::Far];

    pub fn name(self) -> &'static str {
        match self {
            Room::Near => "Near",
            Room::Mid => "Mid",
            Room::Far => "Far",
        }
    }

    /// Its stable id (preferences, scripts).
    pub fn id(self) -> &'static str {
        match self {
            Room::Near => "near",
            Room::Mid => "mid",
            Room::Far => "far",
        }
    }

    pub fn from_id(id: &str) -> Option<Room> {
        Room::ALL.into_iter().find(|r| r.id() == id)
    }

    /// The room's level against the direct sound and its decay (s).
    fn settings(self) -> Option<(f32, f32, f32)> {
        match self {
            Room::Near => None,
            // (wet gain, RT60, pre-delay s)
            Room::Mid => Some((0.5, 0.32, 0.008)),
            Room::Far => Some((0.85, 0.5, 0.016)),
        }
    }
}

/// A small room: an eight-line feedback delay network (Hadamard mixing,
/// damped), fed with the mix in mono, two decorrelated outputs.
struct Fdn {
    lines: [Vec<f32>; 8],
    pos: [usize; 8],
    len: [usize; 8],
    gain: [f32; 8],
    damp: [f32; 8],
    lp: f32,
    pre: Vec<f32>,
    pre_pos: usize,
    wet: f32,
}

const FDN_MS: [f32; 8] = [11.3, 13.7, 15.1, 17.9, 19.3, 23.1, 25.7, 29.3];

impl Fdn {
    fn new(rate: f32, wet: f32, rt60: f32, predelay: f32) -> Self {
        let len = FDN_MS.map(|ms| ((ms * 0.001 * rate) as usize).max(1));
        let gain = len.map(|l| 10f32.powf(-3.0 * l as f32 / (rt60 * rate)));
        Self {
            lines: len.map(|l| vec![0.0; l]),
            pos: [0; 8],
            len,
            gain,
            damp: [0.0; 8],
            // A one-pole at about 6 kHz in the loop.
            lp: 1.0 - (-std::f32::consts::TAU * 6_000.0 / rate).exp(),
            pre: vec![0.0; ((predelay * rate) as usize).max(1)],
            pre_pos: 0,
            wet,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        let x = {
            let d = self.pre[self.pre_pos];
            self.pre[self.pre_pos] = x;
            self.pre_pos = (self.pre_pos + 1) % self.pre.len();
            d
        };
        let mut out = [0.0f32; 8];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.lines[i][self.pos[i]];
        }
        // Hadamard 8 × 8, scaled to stay lossless.
        let mut h = out;
        let mut step = 1;
        while step < 8 {
            for i in (0..8).step_by(step * 2) {
                for j in i..i + step {
                    let (a, b) = (h[j], h[j + step]);
                    h[j] = a + b;
                    h[j + step] = a - b;
                }
            }
            step *= 2;
        }
        let norm = 1.0 / 8f32.sqrt();
        for (i, &hi) in h.iter().enumerate() {
            let fb = hi * norm * self.gain[i];
            self.damp[i] += (fb - self.damp[i]) * self.lp;
            self.lines[i][self.pos[i]] = self.damp[i] + x * 0.35;
            self.pos[i] = (self.pos[i] + 1) % self.len[i];
        }
        let l = out[0] - out[2] + out[4] - out[6] + out[1] * 0.5;
        let r = out[1] - out[3] + out[5] - out[7] + out[2] * 0.5;
        (l * self.wet, r * self.wet)
    }

    fn flush(&mut self) {
        for d in &mut self.damp {
            if d.abs() < 1e-20 {
                *d = 0.0;
            }
        }
    }
}

/// Renders the channels of a layout to two ears.
pub struct Renderer {
    block: usize,
    parts: usize,
    /// Per channel: the direction's filters per ear and partition (`None`:
    /// the LFE).
    filters: Vec<Option<[Vec<Vec<Complex32>>; 2]>>,
    /// Per channel: the last `parts` input spectra (a ring).
    fdl: Vec<Vec<Vec<Complex32>>>,
    ring: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    frame: Vec<f32>,
    spec: Vec<Complex32>,
    acc: [Vec<Complex32>; 2],
    time: Vec<f32>,
    scratch_f: Vec<Complex32>,
    scratch_i: Vec<Complex32>,
    /// Per channel: the previous block and the one being filled.
    prev: Vec<Vec<f32>>,
    fill_in: Vec<Vec<f32>>,
    fill: usize,
    out: [Vec<f32>; 2],
    lfe_gain: f32,
    lfe: [[f32; 4]; 1],
    lfe_coef: [f32; 5],
    lfe_buf: Vec<f32>,
    room: Option<Fdn>,
    mono_buf: Vec<f32>,
}

impl Renderer {
    /// For `layout` at `rate` with `room`.
    pub fn new(layout: ChannelLayout, rate: u32, room: Room) -> Result<Self, BinauralError> {
        let set = hrirs(rate)?;
        let block = if rate > 50_000 { 128 } else { 64 };
        let parts = set.taps.div_ceil(block).max(1);
        let n = 2 * block;
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let channels = layout.channel_count();
        let spectrum = |taps: &[f32], p: usize| -> Vec<Complex32> {
            let mut t = vec![0.0f32; n];
            for (i, v) in taps.iter().skip(p * block).take(block).enumerate() {
                t[i] = *v;
            }
            let mut s = fft.make_output_vec();
            let _ = fft.process(&mut t, &mut s);
            s
        };
        let filters = (0..channels)
            .map(|c| {
                direction(layout, c)
                    .and_then(|d| set.pairs.get(d))
                    .map(|pair| {
                        [
                            (0..parts).map(|p| spectrum(&pair.left, p)).collect(),
                            (0..parts).map(|p| spectrum(&pair.right, p)).collect(),
                        ]
                    })
            })
            .collect();
        let lfe_coef = lowpass(120.0, rate as f32);
        Ok(Self {
            block,
            parts,
            filters,
            fdl: vec![vec![vec![Complex32::default(); block + 1]; parts]; channels],
            ring: 0,
            frame: vec![0.0; n],
            spec: vec![Complex32::default(); block + 1],
            acc: [
                vec![Complex32::default(); block + 1],
                vec![Complex32::default(); block + 1],
            ],
            time: vec![0.0; n],
            scratch_f: fft.make_scratch_vec(),
            scratch_i: ifft.make_scratch_vec(),
            fft,
            ifft,
            prev: vec![vec![0.0; block]; channels],
            fill_in: vec![vec![0.0; block]; channels],
            fill: 0,
            out: [vec![0.0; block], vec![0.0; block]],
            lfe_gain: 10f32.powf(10.0 / 20.0),
            lfe: [[0.0; 4]],
            lfe_coef,
            lfe_buf: vec![0.0; block],
            room: room
                .settings()
                .map(|(wet, rt, pre)| Fdn::new(rate as f32, wet, rt, pre)),
            mono_buf: vec![0.0; block],
        })
    }

    /// Frames of delay the render adds (one block).
    pub fn latency(&self) -> usize {
        self.block
    }

    /// Render `frames` frames of `input` (one slice per channel, at least
    /// `frames` long) into `left` and `right`.
    pub fn process(
        &mut self,
        input: &[&[f32]],
        left: &mut [f32],
        right: &mut [f32],
        frames: usize,
    ) {
        for i in 0..frames {
            for (c, buf) in self.fill_in.iter_mut().enumerate() {
                buf[self.fill] = input.get(c).and_then(|s| s.get(i)).copied().unwrap_or(0.0);
            }
            left[i] = self.out[0][self.fill];
            right[i] = self.out[1][self.fill];
            self.fill += 1;
            if self.fill == self.block {
                self.fill = 0;
                self.run_block();
            }
        }
    }

    fn run_block(&mut self) {
        let b = self.block;
        let n = 2 * b;
        let parts = self.parts;
        self.ring = (self.ring + parts - 1) % parts;
        for a in &mut self.acc {
            a.iter_mut().for_each(|v| *v = Complex32::default());
        }
        self.lfe_buf.iter_mut().for_each(|v| *v = 0.0);
        self.mono_buf.iter_mut().for_each(|v| *v = 0.0);
        for c in 0..self.fill_in.len() {
            match &self.filters[c] {
                None => {
                    // The LFE: low-passed, to both ears.
                    for (i, &x) in self.fill_in[c].iter().enumerate() {
                        self.lfe_buf[i] += biquad(&mut self.lfe[0], &self.lfe_coef, x);
                    }
                }
                Some(f) => {
                    // Overlap-save: the previous block then this one.
                    self.frame[..b].copy_from_slice(&self.prev[c]);
                    self.frame[b..n].copy_from_slice(&self.fill_in[c]);
                    for (m, &x) in self.mono_buf.iter_mut().zip(&self.fill_in[c]) {
                        *m += x;
                    }
                    let _ = self.fft.process_with_scratch(
                        &mut self.frame,
                        &mut self.spec,
                        &mut self.scratch_f,
                    );
                    self.fdl[c][self.ring].copy_from_slice(&self.spec);
                    for (ear, acc) in self.acc.iter_mut().enumerate() {
                        for (p, h) in f[ear].iter().enumerate() {
                            let x = &self.fdl[c][(self.ring + p) % parts];
                            for ((a, &xv), &hv) in acc.iter_mut().zip(x).zip(h) {
                                *a += xv * hv;
                            }
                        }
                    }
                }
            }
            std::mem::swap(&mut self.prev[c], &mut self.fill_in[c]);
        }
        let scale = 1.0 / n as f32;
        for ear in 0..2 {
            // The inverse transform needs real DC and Nyquist bins.
            self.acc[ear][0].im = 0.0;
            self.acc[ear][b].im = 0.0;
            let _ = self.ifft.process_with_scratch(
                &mut self.acc[ear],
                &mut self.time,
                &mut self.scratch_i,
            );
            for i in 0..b {
                self.out[ear][i] = self.time[b + i] * scale + self.lfe_buf[i] * self.lfe_gain;
            }
        }
        if let Some(room) = self.room.as_mut() {
            for i in 0..b {
                let (l, r) = room.process(self.mono_buf[i]);
                self.out[0][i] += l;
                self.out[1][i] += r;
            }
            room.flush();
        }
    }
}

/// A second-order Butterworth low pass: (b0, b1, b2, a1, a2).
fn lowpass(freq: f32, rate: f32) -> [f32; 5] {
    let w = std::f32::consts::TAU * freq / rate;
    let (s, c) = w.sin_cos();
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let alpha = s / (2.0 * q);
    let a0 = 1.0 + alpha;
    let b1 = (1.0 - c) / a0;
    [b1 / 2.0, b1, b1 / 2.0, -2.0 * c / a0, (1.0 - alpha) / a0]
}

#[inline]
fn biquad(z: &mut [f32; 4], k: &[f32; 5], x: f32) -> f32 {
    let y = k[0] * x + k[1] * z[0] + k[2] * z[1] - k[3] * z[2] - k[4] * z[3];
    z[1] = z[0];
    z[0] = x;
    z[3] = z[2];
    z[2] = if y.abs() < 1e-20 { 0.0 } else { y };
    y
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::SurroundFormat;

    fn level(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// The first sample over a tenth of the peak.
    fn onset(x: &[f32]) -> usize {
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        x.iter().position(|v| v.abs() > peak * 0.1).unwrap_or(0)
    }

    #[test]
    fn the_data_holds_every_direction_at_three_rates() {
        let sets = baked().unwrap();
        let rates: Vec<u32> = sets.iter().map(|s| s.rate).collect();
        assert_eq!(rates, [44_100, 48_000, 96_000]);
        for s in &sets {
            assert_eq!(s.pairs.len(), DIRECTIONS.len());
        }
        // 88.2 kHz from 96 kHz.
        let r = hrirs(88_200).unwrap();
        assert_eq!(r.rate, 88_200);
        assert!((r.taps as f64 - 384.0 * 88.2 / 96.0).abs() < 2.0);
    }

    #[test]
    fn every_speaker_has_a_direction_but_the_lfe() {
        for f in SurroundFormat::ALL {
            let layout = ChannelLayout::Surround(f);
            for (c, s) in f.speakers().iter().enumerate() {
                assert_eq!(
                    direction(layout, c).is_none(),
                    s.lfe,
                    "{} {}",
                    f.name(),
                    s.label
                );
            }
        }
        assert_eq!(direction(ChannelLayout::Stereo, 0), Some(1));
        assert_eq!(direction(ChannelLayout::Mono, 0), Some(0));
    }

    /// An impulse on a stereo master's left speaker: the left ear first
    /// and louder; on the centre both alike.
    #[test]
    fn a_left_speaker_reaches_the_left_ear_first_and_louder() {
        let mut r = Renderer::new(ChannelLayout::Stereo, 48_000, Room::Near).unwrap();
        let n = 2048;
        let mut x = vec![0.0f32; n];
        x[100] = 1.0;
        let silent = vec![0.0f32; n];
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        r.process(&[&x, &silent], &mut l, &mut rr, n);
        assert!(
            onset(&l) + 5 < onset(&rr),
            "ITD: {} {}",
            onset(&l),
            onset(&rr)
        );
        assert!(
            level(&l) > 1.5 * level(&rr),
            "ILD: {} {}",
            level(&l),
            level(&rr)
        );
        // Delayed by one block plus the trimmed onset margin.
        assert!(onset(&l) >= 100 + r.latency());
        let mut c = Renderer::new(ChannelLayout::Mono, 48_000, Room::Near).unwrap();
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        c.process(&[&x], &mut l, &mut rr, n);
        let (a, b) = (level(&l), level(&rr));
        assert!((a - b).abs() < 0.2 * a.max(b), "centre: {a} {b}");
    }

    /// Block by block or all at once: the same output.
    #[test]
    fn the_split_of_the_calls_does_not_matter() {
        let layout = ChannelLayout::Surround(SurroundFormat::S714);
        let n = 3000;
        let input: Vec<Vec<f32>> = (0..12)
            .map(|c| {
                (0..n)
                    .map(|i| ((i * (c + 3)) % 97) as f32 / 97.0 - 0.5)
                    .collect()
            })
            .collect();
        let refs: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
        let mut a = Renderer::new(layout, 48_000, Room::Mid).unwrap();
        let (mut al, mut ar) = (vec![0.0; n], vec![0.0; n]);
        a.process(&refs, &mut al, &mut ar, n);
        let mut b = Renderer::new(layout, 48_000, Room::Mid).unwrap();
        let (mut bl, mut br) = (vec![0.0; n], vec![0.0; n]);
        let mut at = 0;
        for step in [1usize, 63, 64, 65, 200, 1000].iter().cycle() {
            if at >= n {
                break;
            }
            let m = (*step).min(n - at);
            let part: Vec<&[f32]> = input.iter().map(|c| &c[at..at + m]).collect();
            b.process(&part, &mut bl[at..at + m], &mut br[at..at + m], m);
            at += m;
        }
        for i in 0..n {
            assert!(
                (al[i] - bl[i]).abs() < 1e-5 && (ar[i] - br[i]).abs() < 1e-5,
                "{i}"
            );
        }
    }

    /// The room adds a tail after the direct sound (Mid), Near has none.
    #[test]
    fn mid_and_far_add_a_room() {
        let tail = |room: Room| {
            let mut r = Renderer::new(ChannelLayout::Mono, 48_000, room).unwrap();
            let n = 24_000;
            let mut x = vec![0.0f32; n];
            x[0] = 1.0;
            let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
            r.process(&[&x], &mut l, &mut rr, n);
            level(&l[4_800..])
        };
        let energy = |room: Room| {
            let mut r = Renderer::new(ChannelLayout::Mono, 48_000, room).unwrap();
            let n = 48_000;
            let mut x = vec![0.0f32; n];
            x[0] = 1.0;
            let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
            r.process(&[&x], &mut l, &mut rr, n);
            let e = |s: &[f32]| s.iter().map(|v| v * v).sum::<f32>();
            // Direct: the first 5 ms after the block; the room: after 20 ms.
            let d = e(&l[..64 + 240]) + e(&rr[..64 + 240]);
            let w = e(&l[960..]) + e(&rr[960..]);
            10.0 * (w / d).log10()
        };
        // About 8 dB under the direct sound in Mid, 3 dB in Far.
        let (mid_db, far_db) = (energy(Room::Mid), energy(Room::Far));
        assert!((-11.0..-5.0).contains(&mid_db), "Mid {mid_db:.1} dB");
        assert!((-6.0..0.0).contains(&far_db), "Far {far_db:.1} dB");
        assert!(tail(Room::Near) < 1e-6);
        let (mid, far) = (tail(Room::Mid), tail(Room::Far));
        assert!(mid > 0.0 && far > mid, "{mid} {far}");
    }
}

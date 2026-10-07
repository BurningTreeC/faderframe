//! Binaural monitoring: a surround bed (or a stereo or mono master) heard
//! on headphones as if each of its speakers stood around the listener.
//!
//! Each speaker's feed is convolved with the two impulse responses measured
//! from its direction at the ears of a [`Head`]: built in are twenty heads
//! of the SADIE II database (University of York, Apache-2.0;
//! `data/LICENSE-SADIE.txt`) — the KU100 and KEMAR dummy heads and eighteen
//! listeners — and any SOFA file can be read ([`Head::from_sofa`]). Fifteen
//! directions, the ITU-R BS.2051 angles FaderFrame's formats use, baked by
//! `scripts/binaural_hrirs.py` at 44.1, 48 and 96 kHz (other rates
//! resampled from the nearest). The convolution is uniformly partitioned
//! in the frequency domain (blocks of 64 frames, 128 at high rates): one
//! forward transform per speaker, products accumulated per ear, one
//! inverse transform per ear — a block of latency. The LFE has no
//! direction: low-passed at 120 Hz it reaches both ears at +10 dB (its
//! level in a speaker system). [`Room`]: Near is the dry measurement; Mid
//! and Far add a small room (a feedback delay network, decorrelated left
//! and right) about 8 and 3 dB under the direct sound. A [`Correction`]
//! evens out the headphones (a parametric or graphic EQ as EqualizerAPO
//! and AutoEq write them, or an impulse response), in the same block.
//!
//! Nothing allocates once a [`Renderer`] is made: it can run on the audio
//! thread.

#![forbid(unsafe_code)]

mod correction;
mod head;
mod render;
mod sofa;

pub use correction::{Correction, FilterKind};
pub use head::{BUILTIN_HEADS, BuiltinHead, Head};
pub use render::{Renderer, Room};

use faderframe_core::ChannelLayout;
use faderframe_core::surround::speakers_of;

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
    #[error("no built-in head '{0}'")]
    NoHead(String),
    #[error("{0}")]
    Sofa(String),
    #[error("{0}")]
    Correction(String),
}

/// The impulse responses of one direction.
#[derive(Clone, Debug, PartialEq)]
pub struct Pair {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// A set of directions at one sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Hrirs {
    pub rate: u32,
    pub taps: usize,
    pub pairs: Vec<Pair>,
}

/// `x` resampled by `ratio` (windowed sinc, 16 taps each side; below the
/// lower rate's Nyquist).
pub fn resample(x: &[f32], ratio: f64) -> Vec<f32> {
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

/// The magnitude of `x` at `freq` (a single-bin DFT).
pub(crate) fn magnitude_at(x: &[f32], freq: f64, rate: f64) -> f64 {
    let w = std::f64::consts::TAU * freq / rate;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, &v) in x.iter().enumerate() {
        let (s, c) = (w * n as f64).sin_cos();
        re += f64::from(v) * c;
        im -= f64::from(v) * s;
    }
    re.hypot(im)
}

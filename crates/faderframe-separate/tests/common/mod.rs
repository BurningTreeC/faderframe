//! What the tests share with `scripts/separation_reference.py`: its test
//! signal and the reference files' layout. Change them together.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::HashMap;

/// Bass, kick, hats, a voice through two formants and a pad, a little
/// apart in the stereo (`signal` in the script).
pub fn signal(n: usize, rate: f64) -> [Vec<f32>; 2] {
    let tau = std::f64::consts::TAU;
    let mut state: u32 = 12345;
    let (mut left, mut right) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = f64::from(state) / 4_294_967_296.0 * 2.0 - 1.0;
        let t = i as f64 / rate;
        let bass = 0.22 * (tau * 55.0 * t).sin() + 0.08 * (tau * 110.0 * t).sin();
        let tk = t.rem_euclid(0.5);
        let kick =
            0.5 * (tau * (50.0 * tk + 2.4 * (1.0 - (-tk / 0.03).exp()))).sin() * (-tk / 0.12).exp();
        let th = (t - 0.25).rem_euclid(0.5);
        let hat = 0.12 * noise * (-th / 0.025).exp();
        let f0 = if t < 1.5 { 220.0 } else { 261.63 };
        let tn = t.rem_euclid(1.5);
        let env = (tn / 0.05).min(1.0) * (-tn / 1.2).exp();
        let mut voice = 0.0;
        for h in 1..16 {
            let f = h as f64 * f0;
            let g = (-((f - 700.0) / 150.0).powi(2)).exp()
                + 0.6 * (-((f - 1200.0) / 200.0).powi(2)).exp()
                + 0.05;
            voice += g * (tau * f * t).sin() / h as f64;
        }
        voice *= 0.3 * env;
        let pad = 0.07 * ((tau * 329.63 * t).sin() + (tau * 493.88 * t).sin());
        left.push((bass + kick + voice + 0.8 * hat + 0.3 * pad) as f32);
        right.push((bass + kick + voice + 0.4 * hat + 0.9 * pad) as f32);
    }
    [left, right]
}

#[derive(serde::Deserialize)]
pub struct Spectrogram {
    /// frames, bins, channels, sources
    pub shape: Vec<usize>,
    /// source, frame, channel, bin, value
    pub samples: Vec<(usize, usize, usize, usize, f64)>,
    /// Per source, per frame: the sum over bins and channels.
    pub frame_sums: Vec<Vec<f64>>,
}

#[derive(serde::Deserialize)]
pub struct Stems {
    pub length: usize,
    /// source, channel, sample, value
    pub samples: Vec<(usize, usize, usize, f64)>,
    pub block: usize,
    /// Per source, per channel, per block: the RMS.
    pub block_rms: Vec<Vec<Vec<f64>>>,
}

#[derive(serde::Deserialize)]
pub struct Reference {
    pub frames: usize,
    pub rate: u32,
    pub sources: Vec<String>,
    pub input_sample: Vec<f64>,
    pub spectrogram: Spectrogram,
    /// By EM iterations.
    pub stems: HashMap<String, Stems>,
}

pub fn reference(text: &str) -> Reference {
    serde_json::from_str(text).unwrap()
}

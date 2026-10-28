//! Synthetic signals and profiles for the tests.

use crate::config::AnalysisConfig;
use crate::profile::{ProfileRequest, TrackProfile, analyse};
use std::f64::consts::TAU;

pub const SR: u32 = 48_000;

/// A tone at `hz` of amplitude `amp` sounding from `a` to `b` seconds of a
/// `len`-second signal (10 ms fades).
pub fn tone(hz: f64, amp: f64, a: f64, b: f64, len: f64) -> Vec<f32> {
    let rate = f64::from(SR);
    let fade = 0.01 * rate;
    (0..(len * rate) as usize)
        .map(|i| {
            let t = i as f64 / rate;
            if t < a || t >= b {
                return 0.0;
            }
            let g = ((t - a) * rate / fade).min((b - t) * rate / fade).min(1.0);
            (amp * g * (TAU * hz * t).sin()) as f32
        })
        .collect()
}

pub fn mix(parts: &[&[f32]]) -> Vec<f32> {
    let n = parts.iter().map(|p| p.len()).max().unwrap_or(0);
    (0..n)
        .map(|i| parts.iter().map(|p| p.get(i).copied().unwrap_or(0.0)).sum())
        .collect()
}

pub fn profile(channels: &[&[f32]]) -> TrackProfile {
    profile_with_spans(channels, &[])
}

pub fn profile_with_spans(channels: &[&[f32]], spans: &[(usize, usize)]) -> TrackProfile {
    let config = AnalysisConfig::for_rate(SR);
    analyse(&ProfileRequest {
        channels,
        rate: SR,
        config: &config,
        spans,
        cancel: None,
    })
    .unwrap_or_else(|e| panic!("{e}"))
}

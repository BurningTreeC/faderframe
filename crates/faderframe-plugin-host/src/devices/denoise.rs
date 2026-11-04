//! De-noise: steady broadband noise (hiss, hum's bed, air conditioning, a
//! preamp's floor) taken out of the spectrum, from a profile learned off a
//! stretch of noise alone or followed adaptively.
//!
//! The DSP is [`faderframe_restore::denoise`] on a streaming STFT: frames
//! of about 11 ms (Low Latency), 43 ms (Balanced) or 85 ms with three hops
//! of lookahead (Quality: a gain opens three hops before an onset, so
//! attacks are not dulled). The mode changes the latency, so it is not
//! automatable and the instance restarts the processor.
//!
//! Learn: while it is on, the processor averages the input's spectrum (the
//! channels together) into a profile, plays through it so the result is
//! heard at once, and publishes it ([`value::NOISE`]); the editor's Learn
//! button, pressed again, writes it into the profile parameters (64 bands,
//! [`id::profile`], not automatable, kept by factory presets) as one undo
//! step and switches the source to Learned. Without a profile Learned works
//! as Adaptive.

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_restore::denoise::{Denoiser, NoiseTracker, Settings};
use faderframe_restore::profile::{self, FLOOR_DB, Learner};
/// The profile's bands (for editors drawing it).
pub use faderframe_restore::profile::{BANDS, band_centre, band_edges};
use faderframe_restore::stft::{C64, Streaming};
use std::sync::Arc;

pub mod id {
    pub const THRESHOLD: u32 = 0;
    pub const REDUCTION: u32 = 1;
    pub const SMOOTHING: u32 = 2;
    pub const SPECTRAL: u32 = 3;
    pub const TILT: u32 = 4;
    pub const TRANSIENTS: u32 = 5;
    pub const LOW: u32 = 6;
    pub const HIGH: u32 = 7;
    pub const SOURCE: u32 = 8;
    pub const MODE: u32 = 9;
    pub const LEARN: u32 = 10;
    pub const LISTEN: u32 = 11;
    pub const LINK: u32 = 12;
    /// Whether a profile was learned (not automatable).
    pub const PROFILE_SET: u32 = 13;
    /// The profile's first band; band `b` is `profile(b)`.
    pub const PROFILE: u32 = 14;

    /// The profile's band `b` (dB, per sample).
    pub const fn profile(b: usize) -> u32 {
        PROFILE + b as u32
    }
}

/// Published: the reduction now (dB, positive: the energy taken from the
/// frame), seconds learned (while learning), whether the noise is the
/// tracker's, the noise used per band (dB, [`BANDS`] values from `NOISE`)
/// and the gain per band now (dB, from `GAIN`).
pub mod value {
    pub const REDUCTION: usize = 0;
    pub const LEARNED: usize = 1;
    pub const ADAPTIVE: usize = 2;
    pub const NOISE: usize = 3;
    pub const GAIN: usize = NOISE + super::BANDS;
}
pub const TAP_VALUES: usize = value::GAIN + BANDS;

pub const SOURCES: [&str; 2] = ["Adaptive", "Learned"];
pub const MODES: [&str; 3] = ["Low Latency", "Balanced", "Quality"];
pub const LISTENS: [&str; 2] = ["Output", "Noise"];

/// Frame lengths of the modes (seconds) and their lookahead (hops).
const FRAME_SECONDS: [f64; 3] = [512.0 / 48_000.0, 2048.0 / 48_000.0, 4096.0 / 48_000.0];
const LOOKAHEAD: [usize; 3] = [0, 0, 3];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let mut v = vec![
        param(id::THRESHOLD, "Threshold", -10.0, 20.0, 3.0, Decibels),
        param(id::REDUCTION, "Reduction", 0.0, 40.0, 12.0, Decibels),
        param(id::SMOOTHING, "Smoothing", 0.0, 1.0, 0.5, Percent),
        param(id::SPECTRAL, "Spectral Smoothing", 0.0, 1.0, 0.3, Percent),
        param(id::TILT, "Tilt", -12.0, 12.0, 0.0, Decibels),
        param(id::TRANSIENTS, "Transients", 0.0, 1.0, 0.3, Percent),
        param(id::LOW, "Low Limit", 20.0, 20_000.0, 20.0, Hertz),
        param(id::HIGH, "High Limit", 200.0, 24_000.0, 24_000.0, Hertz),
        stepped(id::SOURCE, "Source", 1.0, 0.0),
        fixed(stepped(id::MODE, "Mode", 2.0, 1.0)),
        fixed(stepped(id::LEARN, "Learn", 1.0, 0.0)),
        stepped(id::LISTEN, "Listen", 1.0, 0.0),
        stepped(id::LINK, "Stereo Link", 1.0, 1.0),
        fixed(stepped(id::PROFILE_SET, "Profile Learned", 1.0, 0.0)),
    ];
    for b in 0..BANDS {
        v.push(fixed(param(
            id::profile(b),
            &format!("Profile {}", b + 1),
            FLOOR_DB,
            0.0,
            FLOOR_DB,
            Decibels,
        )));
    }
    v
}

/// Parameters a factory preset leaves as they are: the learned profile is
/// the noise of this recording, not a setting (and whether it is used, and
/// a learning under way, go with it).
pub fn kept(pid: ParameterId) -> bool {
    matches!(pid.0, id::SOURCE | id::LEARN | id::PROFILE_SET)
        || (id::PROFILE..id::PROFILE + BANDS as u32).contains(&pid.0)
}

/// Parameters lists of settings leave out (the profile's bands and flag).
pub fn hidden(pid: ParameterId) -> bool {
    pid.0 == id::PROFILE_SET || (id::PROFILE..id::PROFILE + BANDS as u32).contains(&pid.0)
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::SOURCE => pick(&SOURCES, v),
        id::MODE => pick(&MODES, v),
        id::LISTEN => pick(&LISTENS, v),
        id::LEARN | id::LINK | id::PROFILE_SET => on_off(v),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

fn mode_of(params: &ParamValues) -> usize {
    (get(params, id::MODE).round().max(0.0) as usize).min(2)
}

/// The frame length of a mode at `rate`.
pub fn frame(mode: usize, rate: f64) -> usize {
    faderframe_restore::frame_for(FRAME_SECONDS[mode.min(2)], rate)
}

pub fn latency(params: &ParamValues, rate: f64) -> u32 {
    let mode = mode_of(params);
    let n = frame(mode, rate);
    (n + LOOKAHEAD[mode] * n / 4) as u32
}

/// The parameter values that keep what the processor learned (`None`
/// while nothing was learned): Learn off, the source Learned, the profile.
pub fn learned_profile(tap: &AnalysisTap) -> Option<Vec<(ParameterId, f64)>> {
    if tap.value(value::LEARNED) <= 0.05 {
        return None;
    }
    let mut v = vec![
        (ParameterId(id::LEARN), 0.0),
        (ParameterId(id::SOURCE), 1.0),
        (ParameterId(id::PROFILE_SET), 1.0),
    ];
    for b in 0..BANDS {
        let db = f64::from(tap.value(value::NOISE + b)).clamp(FLOOR_DB, 0.0);
        v.push((ParameterId(id::profile(b)), db));
    }
    Some(v)
}

/// The settings of the parameters now.
fn settings(params: &ParamValues) -> Settings {
    Settings {
        threshold_db: get(params, id::THRESHOLD),
        reduction_db: get(params, id::REDUCTION),
        smoothing: get(params, id::SMOOTHING),
        freq_smoothing: get(params, id::SPECTRAL),
        tilt_db: get(params, id::TILT),
        transients: get(params, id::TRANSIENTS),
        low_hz: get(params, id::LOW),
        high_hz: get(params, id::HIGH),
    }
}

/// Parameter events are taken this often (samples).
const STEP: usize = 16;

pub struct DenoiseProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    stream: Streaming,
    den: [Denoiser; 2],
    tracker: [NoiseTracker; 2],
    learner: Learner,
    learning: bool,
    power: [Vec<f64>; 2],
    noise: [Vec<f64>; 2],
    gains: [Vec<f64>; 2],
    /// The learned profile per bin, and the band values it was made from.
    profile_bins: Vec<f64>,
    profile_from: [f32; BANDS],
    learned_bins: Vec<f64>,
    /// Each bin's profile band (for publishing per band).
    band_of: Vec<u8>,
    /// Quality mode: the spectra and gains of the frames ahead.
    ahead: usize,
    held: Vec<Vec<C64>>,
    held_gains: Vec<Vec<f64>>,
    held_at: usize,
    held_filled: usize,
    band_scratch: [f64; BANDS],
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
    frame_reduction: f64,
}

impl DenoiseProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let mode = mode_of(&params);
        let n = frame(mode, sr);
        let stream = Streaming::new(n, 2);
        let (bins, hop) = (stream.stft.bins, stream.stft.hop);
        let bin_hz = sr / stream.stft.n as f64;
        let block = config.max_block_size.max(1) as usize;
        let ahead = LOOKAHEAD[mode];
        let band_of = (0..bins)
            .map(|k| {
                let f = k as f64 * bin_hz;
                let b = ((f.max(profile::LOW_HZ) / profile::LOW_HZ).ln()
                    / (profile::HIGH_HZ / profile::LOW_HZ).ln()
                    * BANDS as f64)
                    .floor() as usize;
                b.min(BANDS - 1) as u8
            })
            .collect();
        let mt = MeterTap::new(sr as f32);
        Self {
            watching: Watching::new(sr as f32),
            sr,
            den: [Denoiser::new(bins, hop, sr), Denoiser::new(bins, hop, sr)],
            tracker: [
                NoiseTracker::new(bins, hop, sr),
                NoiseTracker::new(bins, hop, sr),
            ],
            learner: Learner::new(bins, bin_hz),
            learning: false,
            power: [vec![0.0; bins], vec![0.0; bins]],
            noise: [vec![0.0; bins], vec![0.0; bins]],
            gains: [vec![1.0; bins], vec![1.0; bins]],
            profile_bins: vec![0.0; bins],
            profile_from: [f32::NAN; BANDS],
            learned_bins: vec![0.0; bins],
            band_of,
            ahead,
            held: (0..2 * (ahead + 1))
                .map(|_| vec![C64::new(0.0, 0.0); bins])
                .collect(),
            held_gains: (0..2 * (ahead + 1)).map(|_| vec![1.0; bins]).collect(),
            held_at: 0,
            held_filled: 0,
            band_scratch: [0.0; BANDS],
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            frame_reduction: 0.0,
            stream,
            params,
            tap,
        }
    }

    /// The learned profile per bin (remade when its parameters move).
    fn refresh_profile(&mut self) {
        let mut changed = false;
        for b in 0..BANDS {
            let v = self.params.get(id::profile(b) as usize);
            if v.to_bits() != self.profile_from[b].to_bits() {
                self.profile_from[b] = v;
                changed = true;
            }
        }
        if changed {
            for (s, v) in self.band_scratch.iter_mut().zip(&self.profile_from) {
                *s = f64::from(*v);
            }
            let bin_hz = self.sr / self.stream.stft.n as f64;
            profile::to_bins(&self.band_scratch, bin_hz, &mut self.profile_bins);
        }
    }

    /// A frame is ready in the stream: its gains, applied.
    fn frame(&mut self, channels: usize) {
        let s = settings(&self.params);
        let link = get(&self.params, id::LINK) >= 0.5 || channels == 1;
        let learn = get(&self.params, id::LEARN) >= 0.5;
        let learned =
            get(&self.params, id::SOURCE) >= 0.5 && get(&self.params, id::PROFILE_SET) >= 0.5;
        let noise_only = get(&self.params, id::LISTEN) >= 0.5;
        if learn && !self.learning {
            self.learner.clear();
        }
        self.learning = learn;
        let scale = self.stream.stft.power_scale();
        let bins = self.stream.stft.bins;
        for c in 0..channels {
            for (p, x) in self.power[c].iter_mut().zip(&self.stream.spectra[c]) {
                *p = x.norm_sqr() / scale;
            }
        }
        // The channels' mean (linked gains, learning, the noise per band).
        if channels == 2 {
            let (a, b) = self.power.split_at_mut(1);
            if link {
                for (x, y) in a[0].iter_mut().zip(b[0].iter()) {
                    *x = 0.5 * (*x + *y);
                }
            }
        }
        if learn {
            if link || channels == 1 {
                self.learner.add(&self.power[0]);
            } else {
                for k in 0..bins {
                    self.learned_bins[k] = 0.5 * (self.power[0][k] + self.power[1][k]);
                }
                self.learner.add(&self.learned_bins);
            }
            self.learner.bins(&mut self.learned_bins);
        }
        self.refresh_profile();
        let lanes = if link { 1 } else { channels };
        let mut adaptive = false;
        for c in 0..lanes {
            self.tracker[c].update(&self.power[c]);
            let noise: &[f64] = if learn && self.learner.frames() >= 4 {
                &self.learned_bins
            } else if learned {
                &self.profile_bins
            } else {
                adaptive = true;
                self.tracker[c].noise()
            };
            self.noise[c].copy_from_slice(noise);
            self.den[c].gains(&self.power[c], &self.noise[c], &s, &mut self.gains[c]);
        }
        if link && channels == 2 {
            let (a, b) = self.gains.split_at_mut(1);
            b[0].copy_from_slice(&a[0]);
        }
        // Published: the noise and gains per band, the reduction.
        let mut sum = [0.0f64; BANDS];
        let mut gsum = [0.0f64; BANDS];
        let mut count = [0u32; BANDS];
        let (mut before, mut after) = (0.0, 0.0);
        for k in 1..bins {
            let b = usize::from(self.band_of[k]);
            sum[b] += self.noise[0][k];
            gsum[b] += self.gains[0][k];
            count[b] += 1;
            before += self.power[0][k];
            after += self.power[0][k] * self.gains[0][k] * self.gains[0][k];
        }
        if learn {
            let mut bands = [0.0; BANDS];
            self.learner.bands(&mut bands);
            for (b, v) in bands.iter().enumerate() {
                self.tap.set_value(value::NOISE + b, *v as f32);
            }
        } else {
            let mut last = (FLOOR_DB, 0.0);
            for b in 0..BANDS {
                if count[b] > 0 {
                    let n = f64::from(count[b]);
                    last = (
                        (10.0 * (sum[b] / n).max(1e-30).log10()).max(FLOOR_DB),
                        20.0 * (gsum[b] / n).max(1e-6).log10(),
                    );
                }
                self.tap.set_value(value::NOISE + b, last.0 as f32);
                self.tap.set_value(value::GAIN + b, last.1 as f32);
            }
        }
        if learn {
            for b in 0..BANDS {
                let g = if count[b] > 0 {
                    20.0 * (gsum[b] / f64::from(count[b])).max(1e-6).log10()
                } else {
                    0.0
                };
                self.tap.set_value(value::GAIN + b, g as f32);
            }
        }
        let frames = self.learner.frames() as f64;
        let learned_s = if learn {
            frames * self.stream.stft.hop as f64 / self.sr
        } else {
            0.0
        };
        self.tap.set_value(value::LEARNED, learned_s as f32);
        self.tap
            .set_value(value::ADAPTIVE, if adaptive { 1.0 } else { 0.0 });
        self.frame_reduction = if before > 1e-20 {
            (10.0 * (before / after.max(1e-30)).log10()).max(0.0)
        } else {
            0.0
        };
        // Apply (through the lookahead in Quality).
        if self.ahead == 0 {
            for c in 0..channels {
                for (x, &g) in self.stream.spectra[c].iter_mut().zip(&self.gains[c]) {
                    *x *= if noise_only { 1.0 - g } else { g };
                }
            }
        } else {
            let slots = self.ahead + 1;
            for c in 0..channels {
                // This frame in; the one `ahead` frames back out, with the
                // most open gain of it and the frames after.
                let at = c * slots + self.held_at;
                std::mem::swap(&mut self.held[at], &mut self.stream.spectra[c]);
                self.held_gains[at].copy_from_slice(&self.gains[c]);
                let oldest = c * slots + (self.held_at + 1) % slots;
                std::mem::swap(&mut self.held[oldest], &mut self.stream.spectra[c]);
                let stale = self.held_filled < self.ahead;
                for k in 0..bins {
                    let mut g = 0.0f64;
                    for j in 0..slots {
                        g = g.max(self.held_gains[c * slots + j][k]);
                    }
                    let x = &mut self.stream.spectra[c][k];
                    if stale {
                        *x = C64::new(0.0, 0.0);
                    } else {
                        *x *= if noise_only { 1.0 - g } else { g };
                    }
                }
            }
            self.held_at = (self.held_at + 1) % slots;
            self.held_filled = (self.held_filled + 1).min(self.ahead);
        }
        self.stream.finish();
    }
}

impl PluginProcessor for DenoiseProcessor {
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
        let lanes = channels.min(2);
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let mut events = ctx.param_events.iter().peekable();
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
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                let mut y = [0.0; 2];
                if self.stream.push(&x[..lanes], &mut y[..lanes]) {
                    self.frame(lanes);
                }
                for (c, v) in y.iter().enumerate().take(lanes) {
                    out.channel_mut(c)[i] = *v as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        self.tap
            .set_value(value::REDUCTION, self.frame_reduction as f32);
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
        // The rings are forgotten, not cleared; the trackers and gains start
        // again with the next frame.
        self.stream.reset();
        for d in &mut self.den {
            d.reset();
        }
        for t in &mut self.tracker {
            t.reset();
        }
        self.learner.clear();
        self.held_filled = 0;
        self.frame_reduction = 0.0;
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, bin_db, silence, white};

    fn rig(set: &[(u32, f64)]) -> Rig<DenoiseProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, DenoiseProcessor::new)
    }

    /// A melody (a voice-like tone and its harmonics, a new note every
    /// quarter second, short rests) with hiss of `hiss` RMS; the melody
    /// from `from` seconds (`steady`: one held note instead).
    fn hissy(hiss: f64, from: f64, steady: bool) -> impl Fn(usize) -> (f32, f32) {
        move |n| {
            let t = n as f64 / SR;
            let note = ((t - from).max(0.0) / 0.25) as usize;
            let f = if steady {
                220.0
            } else {
                [220.0, 247.0, 277.0, 330.0, 294.0][note % 5]
            };
            let rest = !steady && (t - from).rem_euclid(0.25) > 0.21;
            let tone = if t >= from && !rest {
                (1..6)
                    .map(|h| 0.15 / h as f64 * (std::f64::consts::TAU * f * h as f64 * t).sin())
                    .sum::<f64>()
            } else {
                0.0
            };
            (
                (tone + hiss * white(n, 1)) as f32,
                (tone + hiss * white(n, 2)) as f32,
            )
        }
    }

    /// The SNR (dB) of `out` against the clean tone, delay compensated,
    /// over the second half.
    fn snr(out: &[f32], start: usize, from: f64, steady: bool, latency: usize) -> f64 {
        let clean = hissy(0.0, from, steady);
        let half = out.len() / 2;
        let (mut sig, mut err) = (0.0, 0.0);
        for (i, v) in out.iter().enumerate().skip(half) {
            let c = f64::from(clean(start + i - latency).0);
            sig += c * c;
            err += (f64::from(*v) - c).powi(2);
        }
        10.0 * (sig / err.max(1e-30)).log10()
    }

    #[test]
    fn adaptive_mode_raises_the_snr_of_a_hissy_tone() {
        let mut r = rig(&[(id::REDUCTION, 20.0)]);
        let lat = latency(&r.params, SR) as usize;
        let (l, _) = r.run(4.0, hissy(0.02, 0.0, false), silence);
        let before = {
            let clean = hissy(0.0, 0.0, false);
            let noisy = hissy(0.02, 0.0, false);
            let (mut sig, mut err) = (0.0, 0.0);
            for i in l.len() / 2..l.len() {
                let c = f64::from(clean(i).0);
                sig += c * c;
                err += (f64::from(noisy(i).0) - c).powi(2);
            }
            10.0 * (sig / err).log10()
        };
        let after = snr(&l, 0, 0.0, false, lat);

        assert!(after > before + 8.0, "SNR {before:.1} → {after:.1} dB");
    }

    #[test]
    fn a_learned_profile_takes_the_hiss_and_keeps_the_tone() {
        // Learn on the hiss alone (the first second), then the tone comes.
        let mut r = rig(&[(id::LEARN, 1.0), (id::REDUCTION, 24.0)]);
        r.run(1.0, hissy(0.02, 10.0, true), silence);
        let learned = learned_profile(&r.tap).unwrap();
        // The hiss is white at a variance of 4e-4: −34 dB per sample.
        for (pid, v) in &learned[3..] {
            let b = (pid.0 - id::PROFILE) as usize;
            if (8..60).contains(&b) {
                assert!((v + 34.0).abs() < 2.0, "band {b}: {v:.1} dB");
            }
        }
        for (pid, v) in learned {
            r.params.set_by_id(pid, v).unwrap();
        }
        let lat = latency(&r.params, SR) as usize;
        // A held note: a profile, unlike tracking, knows it is not noise.
        let start = r.at;
        let (l, _) = r.run(4.0, hissy(0.02, 1.0, true), silence);
        let after = snr(&l, start, 1.0, true, lat);
        assert!(after > 18.0, "SNR {after:.1} dB");
        // The tone's fundamental keeps its level.
        let f = bin_db(&l, 220.0) - 20.0 * 0.15f64.log10();
        assert!(f.abs() < 1.0, "fundamental {f:.2} dB");
        // (The tone is most of the energy: what is taken is a little.)
        assert!(r.tap.value(value::REDUCTION) > 0.05);
    }

    #[test]
    fn listening_to_the_noise_plays_what_is_taken() {
        // Learned on the hiss alone, then the held note comes.
        let mut r = rig(&[(id::LISTEN, 1.0), (id::REDUCTION, 40.0), (id::LEARN, 1.0)]);
        r.run(1.0, hissy(0.02, 10.0, true), silence);
        for (pid, v) in learned_profile(&r.tap).unwrap() {
            r.params.set_by_id(pid, v).unwrap();
        }
        let (l, _) = r.run(3.0, hissy(0.02, 0.0, true), silence);
        // The tone is (mostly) not in what is taken.
        assert!(
            bin_db(&l, 220.0) < 20.0 * 0.15f64.log10() - 15.0,
            "{}",
            bin_db(&l, 220.0)
        );
        let rms = (l[l.len() / 2..]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            / (l.len() / 2) as f64)
            .sqrt();
        assert!(rms > 0.01, "the hiss: {rms}");
    }

    #[test]
    fn every_mode_reports_its_latency_and_delays_by_it() {
        for mode in 0..3 {
            let mut r = rig(&[
                (id::MODE, mode as f64),
                (id::REDUCTION, 0.0),
                (id::THRESHOLD, -10.0),
            ]);
            let lat = latency(&r.params, SR) as usize;
            // An impulse at 0.2 s comes out `lat` later (reduction 0: the
            // gains are one).
            let at = (0.2 * SR) as usize;
            let (l, _) = r.run(
                0.5,
                move |n| if n == at { (0.5, 0.5) } else { (0.0, 0.0) },
                silence,
            );
            let peak = l
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0;
            assert_eq!(peak, at + lat, "mode {mode}");
        }
    }

    #[test]
    fn presets_leave_the_profile_alone() {
        assert!(kept(ParameterId(id::profile(10))));
        assert!(kept(ParameterId(id::PROFILE_SET)));
        assert!(!kept(ParameterId(id::REDUCTION)));
        let p = parameters();
        assert_eq!(p.len(), id::PROFILE as usize + BANDS);
        for (i, info) in p.iter().enumerate() {
            assert_eq!(info.id.0 as usize, i, "{}", info.name);
        }
    }
}

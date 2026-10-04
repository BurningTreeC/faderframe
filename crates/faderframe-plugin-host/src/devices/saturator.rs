//! Saturator: drive into one of six curves, oversampled.
//!
//! The signal goes through a low cut, the drive, the curve (run at up to
//! 8× through the halfband oversampler, whose latency the dry signal is
//! held back by), a DC blocker (the bias makes DC), the type's colour
//! (tape: a head bump and its high frequency loss), a tilt round 1 kHz and
//! a high cut; auto gain sets the level back by what the curve does to a
//! −12 dBFS sine (static: the dynamics stay as the curve made them).
//!
//! Every curve has a slope of one at zero, so a quiet signal passes at its
//! level and only the drive decides how hard it is pushed:
//! * Soft — `tanh`, round, odd harmonics.
//! * Tape — an arctangent: a softer knee, more third harmonic early.
//! * Tube — asymmetric (the positive half rounds off slowly, the negative
//!   like `tanh`): even harmonics, and more with bias.
//! * Transistor — a hard knee, `x / (1 + x⁴)^¼`.
//! * Fold — `sin`: past the top the wave folds back.
//! * Clip — hard clipping.

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::filter::{BandShape, BandType, DcBlock, Filter, OnePole};
use crate::dsp::gain;
use crate::dsp::oversample::Oversampler;
use crate::dsp::smooth::Smoothed;
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, TAU};
use std::sync::Arc;

pub mod id {
    pub const DRIVE: u32 = 0;
    pub const TYPE: u32 = 1;
    pub const BIAS: u32 = 2;
    pub const TONE: u32 = 3;
    pub const MIX: u32 = 4;
    pub const OUTPUT: u32 = 5;
    pub const AUTO_GAIN: u32 = 6;
    pub const OVERSAMPLING: u32 = 7;
    pub const LOW_CUT: u32 = 8;
    pub const HIGH_CUT: u32 = 9;
}

/// Published: input and output peaks (linear), the driven level's peak
/// (where on the curve the signal goes) and the auto gain (dB).
pub mod value {
    pub const IN_PEAK: usize = 0;
    pub const OUT_PEAK: usize = 1;
    pub const DRIVEN: usize = 2;
    pub const AUTO: usize = 3;
}
pub const TAP_VALUES: usize = 4;

pub const TYPES: [&str; 6] = ["Soft", "Tape", "Tube", "Transistor", "Fold", "Clip"];
pub const FACTORS: [&str; 4] = ["1×", "2×", "4×", "8×"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::DRIVE, "Drive", 0.0, 36.0, 6.0, Decibels),
        stepped(id::TYPE, "Type", 5.0, 0.0),
        param(id::BIAS, "Bias", -1.0, 1.0, 0.0, None),
        param(id::TONE, "Tone", -1.0, 1.0, 0.0, None),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        param(id::OUTPUT, "Output", -24.0, 12.0, 0.0, Decibels),
        stepped(id::AUTO_GAIN, "Auto Gain", 1.0, 1.0),
        fixed(stepped(id::OVERSAMPLING, "Oversampling", 3.0, 2.0)),
        param(id::LOW_CUT, "Low Cut", 10.0, 1_000.0, 10.0, Hertz),
        param(id::HIGH_CUT, "High Cut", 1_000.0, 30_000.0, 30_000.0, Hertz),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::TYPE => pick(&TYPES, v),
        id::OVERSAMPLING => pick(&FACTORS, v),
        id::AUTO_GAIN => on_off(v),
        id::BIAS | id::TONE if v.abs() < 0.005 => "0".into(),
        id::BIAS => format!("{v:+.2}"),
        id::TONE if v < 0.0 => format!("Dark {:.0}", -v * 100.0),
        id::TONE => format!("Bright {:.0}", v * 100.0),
        id::LOW_CUT if v <= 10.5 => "Off".into(),
        id::HIGH_CUT if v >= 29_500.0 => "Off".into(),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

fn factor(params: &ParamValues) -> usize {
    1 << (get(params, id::OVERSAMPLING).round().clamp(0.0, 3.0) as usize)
}

pub fn latency(params: &ParamValues, _rate: f64) -> u32 {
    Oversampler::new(factor(params)).latency()
}

/// A curve (by type index) at `x`.
#[inline]
pub fn curve(kind: usize, x: f64) -> f64 {
    match kind {
        1 => (FRAC_PI_2 * x).atan() / FRAC_PI_2,
        2 => {
            if x >= 0.0 {
                1.0 - (-x).exp()
            } else {
                x.tanh()
            }
        }
        3 => x / (1.0 + x.powi(4)).powf(0.25),
        4 => x.sin(),
        5 => x.clamp(-1.0, 1.0),
        _ => x.tanh(),
    }
}

/// The whole static shaping: driven `x`, bias `b` (silence stays silent).
#[inline]
pub fn shape(kind: usize, x: f64, b: f64) -> f64 {
    curve(kind, x + b) - curve(kind, b)
}

/// The gain that brings a −12 dBFS sine back to its level after the drive
/// and the curve (DC taken off, as the blocker does).
pub fn auto_gain(kind: usize, drive: f64, b: f64) -> f64 {
    const N: usize = 64;
    let a = 0.25;
    let mut y = [0.0; N];
    for (k, v) in y.iter_mut().enumerate() {
        *v = shape(kind, drive * a * (TAU * k as f64 / N as f64).sin(), b);
    }
    let mean = y.iter().sum::<f64>() / N as f64;
    let rms = (y.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / N as f64).sqrt();
    (a * FRAC_1_SQRT_2 / rms.max(1e-9)).clamp(0.01, 10.0)
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

const STEP: usize = 32;

pub struct SaturatorProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    over: [Oversampler; 2],
    low_cut: Filter<2>,
    high_cut: Filter<2>,
    tilt: Filter<2>,
    bump: Filter<2>,
    head: [OnePole; 2],
    dc: [DcBlock; 2],
    drive: Smoothed,
    makeup: Smoothed,
    mix: Smoothed,
    /// The dry signal, held back by the oversampler's latency.
    dry: [Vec<f64>; 2],
    pos: usize,
    /// What the auto gain was worked out for.
    auto_for: (usize, f64, f64),
    auto: f64,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl SaturatorProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let f = factor(&params);
        let lat = Oversampler::new(f).latency() as usize;
        let mt = MeterTap::new(sr as f32);
        let mut s = Self {
            watching: Watching::new(sr as f32),
            sr,
            over: [Oversampler::new(f), Oversampler::new(f)],
            low_cut: Filter::default(),
            high_cut: Filter::default(),
            tilt: Filter::default(),
            bump: Filter::default(),
            head: [OnePole::new(15_000.0, sr); 2],
            dc: [DcBlock::new(sr); 2],
            drive: Smoothed::new(1.0, 20.0, sr),
            makeup: Smoothed::new(1.0, 20.0, sr),
            mix: Smoothed::new(1.0, 20.0, sr),
            dry: [vec![0.0; lat.max(1)], vec![0.0; lat.max(1)]],
            pos: 0,
            auto_for: (usize::MAX, 0.0, 0.0),
            auto: 1.0,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        s.control();
        s.drive.snap();
        s.makeup.snap();
        s.mix.snap();
        s
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    fn kind(&self) -> usize {
        self.get(id::TYPE).round().clamp(0.0, 5.0) as usize
    }

    /// Follow the parameters (every step).
    fn control(&mut self) {
        let sr = self.sr;
        let kind = self.kind();
        let drive = gain(self.get(id::DRIVE));
        let bias = 0.5 * self.get(id::BIAS);
        if self.auto_for != (kind, drive, bias) {
            self.auto_for = (kind, drive, bias);
            self.auto = auto_gain(kind, drive, bias);
        }
        let auto = if self.get(id::AUTO_GAIN) >= 0.5 {
            self.auto
        } else {
            1.0
        };
        self.drive.set(drive);
        self.makeup.set(auto * gain(self.get(id::OUTPUT)));
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        let low = self.get(id::LOW_CUT);
        if low > 10.5 {
            self.low_cut.set(cut(BandType::LowCut, low), sr);
        } else {
            self.low_cut.clear();
        }
        let high = self.get(id::HIGH_CUT);
        if high < 29_500.0 && high < 0.45 * sr {
            self.high_cut.set(cut(BandType::HighCut, high), sr);
        } else {
            self.high_cut.clear();
        }
        let tone = self.get(id::TONE);
        if tone.abs() > 0.005 {
            self.tilt.set(
                BandShape {
                    kind: BandType::TiltShelf,
                    freq: 1_000.0,
                    gain: 12.0 * tone,
                    q: FRAC_1_SQRT_2,
                    slope: 6.0,
                },
                sr,
            );
        } else {
            self.tilt.clear();
        }
        if kind == 1 {
            self.bump.set(
                BandShape {
                    kind: BandType::Bell,
                    freq: 90.0,
                    gain: 1.2,
                    q: 0.9,
                    slope: 12.0,
                },
                sr,
            );
        } else {
            self.bump.clear();
        }
    }
}

impl PluginProcessor for SaturatorProcessor {
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
        let used = channels.min(2);
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let lat = self.over[0].latency() as usize;
        let mut events = ctx.param_events.iter().peekable();
        let (mut in_peak, mut out_peak, mut driven) = (0.0f64, 0.0f64, 0.0f64);
        let mut at = 0;
        while at < n {
            let end = (at + STEP).min(n);
            let mut moved = false;
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                    moved = true;
                } else {
                    break;
                }
            }
            if moved || at == 0 {
                self.control();
            }
            let kind = self.kind();
            let bias = 0.5 * self.get(id::BIAS);
            for i in at..end {
                let d = self.drive.tick();
                let makeup = self.makeup.tick();
                let mix = self.mix.tick();
                let mut y = [0.0; 2];
                for (c, yc) in y.iter_mut().enumerate().take(used) {
                    let x = f64::from(self.scratch[c][i]);
                    in_peak = in_peak.max(x.abs());
                    let pre = self.low_cut.process(c, x) * d;
                    driven = driven.max(pre.abs());
                    let mut w = self.over[c].process(pre, &mut |v| shape(kind, v, bias));
                    w = self.dc[c].process(w);
                    if kind == 1 {
                        w = self.head[c].low(self.bump.process(c, w));
                    }
                    w = self.high_cut.process(c, self.tilt.process(c, w)) * makeup;
                    let dry = if lat > 0 {
                        let p = self.pos;
                        let v = self.dry[c][p];
                        self.dry[c][p] = x;
                        v
                    } else {
                        x
                    };
                    *yc = dry + mix * (w - dry);
                    out_peak = out_peak.max(yc.abs());
                }
                if lat > 0 {
                    self.pos = (self.pos + 1) % lat;
                }
                for (c, v) in y.iter().enumerate().take(used) {
                    out.channel_mut(c)[i] = *v as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        for c in 0..2 {
            self.head[c].flush();
        }
        self.low_cut.flush();
        self.high_cut.flush();
        self.tilt.flush();
        self.bump.flush();
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
        self.tap.raise_value(value::DRIVEN, driven as f32);
        let auto = if self.get(id::AUTO_GAIN) >= 0.5 {
            self.auto
        } else {
            1.0
        };
        self.tap
            .set_value(value::AUTO, (20.0 * auto.log10()) as f32);
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
        for c in 0..2 {
            self.over[c].reset();
            self.dc[c].reset();
            self.head[c].reset();
            self.dry[c].fill(0.0);
        }
        self.low_cut.reset();
        self.high_cut.reset();
        self.tilt.reset();
        self.bump.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, bin_db, level, silence, thd, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<SaturatorProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, SaturatorProcessor::new)
    }

    /// The level of the `k`-th harmonic of `f` relative to the fundamental.
    fn harmonic(x: &[f32], f: f64, k: f64) -> f64 {
        bin_db(x, f * k) - bin_db(x, f)
    }

    #[test]
    fn every_curve_is_unity_for_small_signals_and_bounded() {
        for kind in 0..TYPES.len() {
            let s = shape(kind, 1e-4, 0.0) / 1e-4;
            assert!((s - 1.0).abs() < 1e-3, "{kind}: slope {s}");
            for x in [-40.0, -3.0, 3.0, 40.0] {
                assert!(shape(kind, x, 0.0).abs() <= 1.0 + 1e-9, "{kind} at {x}");
            }
            assert_eq!(shape(kind, 0.0, 0.3), 0.0);
        }
    }

    #[test]
    fn auto_gain_keeps_the_level_and_drive_adds_harmonics() {
        for (kind, name) in TYPES.iter().enumerate() {
            let mut r = rig(&[(id::TYPE, kind as f64), (id::DRIVE, 18.0)]);
            let (l, _) = r.run(0.5, tone(500.0, 0.25), silence);
            let lv = level(&l, 500.0);
            // The fundamental stays round its level (the harmonics take
            // some of the energy, hard curves the most).
            assert!(lv > -15.5 && lv < -11.0, "{name}: {lv:.2}");
            assert!(thd(&l, 500.0) > 0.02, "{name}: clean?");
        }
        let mut r = rig(&[(id::DRIVE, 0.0)]);
        let (l, _) = r.run(0.5, tone(500.0, 0.01), silence);
        // tanh's third harmonic of a −40 dB sine is a²/12: −102 dB.
        let h3 = harmonic(&l, 500.0, 3.0);
        assert!(h3 < -95.0, "quiet stays clean: {h3:.1}");
    }

    #[test]
    fn symmetric_curves_make_odd_harmonics_and_bias_makes_even_ones() {
        let mut r = rig(&[(id::DRIVE, 18.0)]);
        let (l, _) = r.run(0.5, tone(500.0, 0.25), silence);
        assert!(
            harmonic(&l, 500.0, 2.0) < -80.0,
            "{}",
            harmonic(&l, 500.0, 2.0)
        );
        assert!(harmonic(&l, 500.0, 3.0) > -30.0);
        let mut r = rig(&[(id::DRIVE, 18.0), (id::BIAS, 0.6)]);
        let (l, _) = r.run(0.5, tone(500.0, 0.25), silence);
        assert!(
            harmonic(&l, 500.0, 2.0) > -30.0,
            "{}",
            harmonic(&l, 500.0, 2.0)
        );
        // No DC comes out.
        let tail = &l[l.len() / 2..];
        let dc = tail.iter().map(|v| f64::from(*v)).sum::<f64>() / tail.len() as f64;
        assert!(dc.abs() < 1e-3, "{dc}");
        // A tube is asymmetric of its own.
        let mut r = rig(&[(id::TYPE, 2.0), (id::DRIVE, 18.0)]);
        let (l, _) = r.run(0.5, tone(500.0, 0.25), silence);
        assert!(harmonic(&l, 500.0, 2.0) > -40.0);
    }

    #[test]
    fn oversampling_keeps_aliases_down_and_the_dry_signal_lines_up() {
        // A hard clip of 7 kHz: its 5th harmonic (35 kHz) folds to 13 kHz
        // at 48 kHz without oversampling.
        let alias = |os: f64| {
            let mut r = rig(&[
                (id::TYPE, 5.0),
                (id::DRIVE, 24.0),
                (id::OVERSAMPLING, os),
                (id::AUTO_GAIN, 0.0),
            ]);
            let (l, _) = r.run(0.5, tone(7_000.0, 0.25), silence);
            bin_db(&l, 13_000.0) - bin_db(&l, 7_000.0)
        };
        let (plain, eight) = (alias(0.0), alias(3.0));
        assert!(eight < plain - 30.0, "1×: {plain:.1}, 8×: {eight:.1}");
        // Half wet at 4×: the dry part is delayed as the wet one, so
        // nothing cancels and the latency is reported.
        let mut r = rig(&[(id::OVERSAMPLING, 2.0), (id::MIX, 0.5), (id::DRIVE, 0.0)]);
        assert_eq!(latency(&r.params, SR), 72);
        let (l, _) = r.run(0.3, tone(1_000.0, 0.01), silence);
        assert!((level(&l, 1_000.0) - 20.0 * 0.01f64.log10()).abs() < 0.1);
    }
}

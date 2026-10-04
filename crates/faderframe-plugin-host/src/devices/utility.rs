//! Utility (the built-in "Gain", grown up): gain, balance, width, mono
//! bass, polarity per side, channel choice, a DC filter and mute.
//!
//! Everything but mono bass and the DC filter is one 2 × 2 matrix from the
//! input's left and right to the output's: the channel choice (stereo,
//! left or right on both, swapped, mid, side), polarity, width (the side
//! scaled against the mid), balance and gain. Moving any of them ramps the
//! matrix across the block, so nothing clicks; with everything at rest it
//! is the identity times the gain (the old Gain exactly). Mono bass splits
//! mid and side at the frequency (Linkwitz–Riley, 24 dB/oct) and keeps only
//! the side's highs; the mid goes through the same crossover's all pass,
//! so above the split both stay in phase. Turning it on or off crossfades.
//! Mono tracks take the gain, polarity (left), DC filter and mute; tracks
//! of more than two channels take them on every channel and the stereo
//! controls on the first two.

use super::{on_off, param, pick, stepped};
use crate::dsp::filter::{DcBlock, Svf};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2};
use std::sync::Arc;

pub mod id {
    /// The old Gain's only parameter.
    pub const GAIN: u32 = 0;
    pub const PAN: u32 = 1;
    pub const WIDTH: u32 = 2;
    pub const MONO_BASS: u32 = 3;
    pub const BASS_FREQ: u32 = 4;
    pub const INVERT_L: u32 = 5;
    pub const INVERT_R: u32 = 6;
    pub const CHANNELS: u32 = 7;
    pub const DC: u32 = 8;
    pub const MUTE: u32 = 9;
}

/// Published: input and output peaks (linear).
pub mod value {
    pub const IN_PEAK: usize = 0;
    pub const OUT_PEAK: usize = 1;
}
pub const TAP_VALUES: usize = 2;

pub const CHANNEL_MODES: [&str; 6] = ["Stereo", "Left", "Right", "Swap", "Mid", "Side"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::GAIN, "Gain", -60.0, 24.0, 0.0, Decibels),
        param(id::PAN, "Balance", -1.0, 1.0, 0.0, None),
        param(id::WIDTH, "Width", 0.0, 2.0, 1.0, Percent),
        stepped(id::MONO_BASS, "Mono Bass", 1.0, 0.0),
        param(
            id::BASS_FREQ,
            "Mono Bass Frequency",
            40.0,
            400.0,
            120.0,
            Hertz,
        ),
        stepped(id::INVERT_L, "Invert Left", 1.0, 0.0),
        stepped(id::INVERT_R, "Invert Right", 1.0, 0.0),
        stepped(id::CHANNELS, "Channels", 5.0, 0.0),
        stepped(id::DC, "DC Filter", 1.0, 0.0),
        stepped(id::MUTE, "Mute", 1.0, 0.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::CHANNELS => pick(&CHANNEL_MODES, v),
        id::MONO_BASS | id::INVERT_L | id::INVERT_R | id::DC | id::MUTE => on_off(v),
        id::PAN if v.abs() < 0.005 => "C".into(),
        id::PAN if v < 0.0 => format!("L {:.0}", -v * 100.0),
        id::PAN => format!("R {:.0}", v * 100.0),
        id::GAIN if v <= -59.95 => "−∞ dB".into(),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// The matrix (out L from in L, R; out R from in L, R) for the parameters.
pub fn matrix(params: &ParamValues) -> [f64; 4] {
    let gain = if get(params, id::MUTE) >= 0.5 || get(params, id::GAIN) <= -59.95 {
        0.0
    } else {
        10f64.powf(get(params, id::GAIN) / 20.0)
    };
    // Which input feeds each side.
    let pick: [f64; 4] = match get(params, id::CHANNELS).round() as i64 {
        1 => [1.0, 0.0, 1.0, 0.0],
        2 => [0.0, 1.0, 0.0, 1.0],
        3 => [0.0, 1.0, 1.0, 0.0],
        4 => [0.5, 0.5, 0.5, 0.5],
        5 => [0.5, -0.5, 0.5, -0.5],
        _ => [1.0, 0.0, 0.0, 1.0],
    };
    let (pl, pr) = (
        if get(params, id::INVERT_L) >= 0.5 {
            -1.0
        } else {
            1.0
        },
        if get(params, id::INVERT_R) >= 0.5 {
            -1.0
        } else {
            1.0
        },
    );
    let w = get(params, id::WIDTH).clamp(0.0, 2.0);
    let width = [
        (1.0 + w) / 2.0,
        (1.0 - w) / 2.0,
        (1.0 - w) / 2.0,
        (1.0 + w) / 2.0,
    ];
    let p = get(params, id::PAN).clamp(-1.0, 1.0);
    let (bl, br) = (
        if p > 0.0 { (p * FRAC_PI_2).cos() } else { 1.0 },
        if p < 0.0 { (-p * FRAC_PI_2).cos() } else { 1.0 },
    );
    let mul = |a: [f64; 4], b: [f64; 4]| {
        [
            a[0] * b[0] + a[1] * b[2],
            a[0] * b[1] + a[1] * b[3],
            a[2] * b[0] + a[3] * b[2],
            a[2] * b[1] + a[3] * b[3],
        ]
    };
    let polarity = [pl, 0.0, 0.0, pr];
    let balance = [bl * gain, 0.0, 0.0, br * gain];
    mul(balance, mul(width, mul(polarity, pick)))
}

/// A Linkwitz–Riley crossover (two Butterworth sections each way).
#[derive(Clone, Copy, Default)]
struct Crossover {
    low: [Svf; 2],
    high: [Svf; 2],
}

impl Crossover {
    fn set(&mut self, freq: f64, rate: f64) {
        for s in self.low.iter_mut().chain(&mut self.high) {
            s.set(freq, FRAC_1_SQRT_2, rate);
        }
    }

    /// The low and high bands (they sum to an all pass).
    #[inline]
    fn split(&mut self, x: f64) -> (f64, f64) {
        let l = self.low[0].process(x).low;
        let l = self.low[1].process(l).low;
        let h = self.high[0].process(x).high;
        let h = self.high[1].process(h).high;
        (l, h)
    }

    fn reset(&mut self) {
        for s in self.low.iter_mut().chain(&mut self.high) {
            s.reset();
        }
    }

    fn flush(&mut self) {
        for s in self.low.iter_mut().chain(&mut self.high) {
            s.flush();
        }
    }
}

pub struct UtilityProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    /// The matrix in use at the end of the last block.
    current: [f64; 4],
    mid: Crossover,
    side: Crossover,
    /// How much of the mono bass path is heard (0…1) and its step.
    bass: f64,
    bass_step: f64,
    dc: Vec<DcBlock>,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl UtilityProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let bass = if get(&params, id::MONO_BASS) >= 0.5 {
            1.0
        } else {
            0.0
        };
        let mut mid = Crossover::default();
        mid.set(get(&params, id::BASS_FREQ), sr);
        Self {
            watching: Watching::new(sr as f32),
            sr,
            current: matrix(&params),
            mid,
            side: mid,
            bass,
            bass_step: 1.0 / (0.01 * sr),
            // Enough for any layout a track has.
            dc: vec![DcBlock::new(sr); 16],
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }
}

impl PluginProcessor for UtilityProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return ProcessStatus::Continue;
        };
        out.copy_from(input);
        let channels = out.num_channels();
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let in_peak = self.scratch[0][..n]
            .iter()
            .chain(&self.scratch[1][..n])
            .fold(0.0f32, |m, v| m.max(v.abs()));
        if get(&self.params, id::DC) >= 0.5 {
            for c in 0..channels.min(self.dc.len()) {
                let dc = &mut self.dc[c];
                for s in out.channel_mut(c).iter_mut() {
                    *s = dc.process(f64::from(*s)) as f32;
                }
            }
        }
        let target = matrix(&self.params);
        let steps = frames.max(1) as f64;
        let step = [
            (target[0] - self.current[0]) / steps,
            (target[1] - self.current[1]) / steps,
            (target[2] - self.current[2]) / steps,
            (target[3] - self.current[3]) / steps,
        ];
        if channels >= 2 {
            let want_bass = get(&self.params, id::MONO_BASS) >= 0.5;
            let freq = get(&self.params, id::BASS_FREQ);
            self.mid.set(freq, self.sr);
            self.side.set(freq, self.sr);
            if want_bass && self.bass == 0.0 {
                self.mid.reset();
                self.side.reset();
            }
            let mut m = self.current;
            let (l, r) = out.channel_pair_mut(0, 1);
            for i in 0..frames.min(l.len()) {
                for k in 0..4 {
                    m[k] += step[k];
                }
                let (mut x, mut y) = (f64::from(l[i]), f64::from(r[i]));
                if self.bass > 0.0 || want_bass {
                    self.bass = if want_bass {
                        (self.bass + self.bass_step).min(1.0)
                    } else {
                        (self.bass - self.bass_step).max(0.0)
                    };
                    let mid = 0.5 * (x + y);
                    let side = 0.5 * (x - y);
                    let (ml, mh) = self.mid.split(mid);
                    let (_, sh) = self.side.split(side);
                    let (bx, by) = (ml + mh + sh, ml + mh - sh);
                    x += self.bass * (bx - x);
                    y += self.bass * (by - y);
                }
                l[i] = (m[0] * x + m[1] * y) as f32;
                r[i] = (m[2] * x + m[3] * y) as f32;
            }
            self.mid.flush();
            self.side.flush();
            // Channels past the first two take the gain.
            let (g0, gs) = (
                self.current[0].abs().max(self.current[3].abs()),
                (target[0].abs().max(target[3].abs())),
            );
            for c in 2..channels {
                let mut g = g0;
                let s = (gs - g0) / steps;
                for v in out.channel_mut(c) {
                    g += s;
                    *v = (f64::from(*v) * g) as f32;
                }
            }
        } else {
            // Mono: the gain and the left polarity (row 0, column 0 with a
            // stereo choice; the mid of the choices that mix).
            let (mut g, s) = (
                self.current[0] + self.current[1],
                (target[0] + target[1] - self.current[0] - self.current[1]) / steps,
            );
            for v in out.channel_mut(0) {
                g += s;
                *v = (f64::from(*v) * g) as f32;
            }
        }
        self.current = target;
        let mut out_peak = 0.0f32;
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.scratch[c][..n].iter().zip(&o[..n]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
                out_peak = out_peak.max(y.abs());
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n);
            self.meters[1][c].publish(&self.tap.meter_out, c, n);
        }
        self.tap.raise_value(value::IN_PEAK, in_peak);
        self.tap.raise_value(value::OUT_PEAK, out_peak);
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
        self.mid.reset();
        self.side.reset();
        self.dc.iter_mut().for_each(DcBlock::reset);
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, bin_db, level, silence, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<UtilityProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, UtilityProcessor::new)
    }

    fn stereo(fl: f64, al: f64, fr: f64, ar: f64) -> impl Fn(usize) -> (f32, f32) {
        move |n| {
            let t = n as f64 / SR;
            (
                (al * (std::f64::consts::TAU * fl * t).sin()) as f32,
                (ar * (std::f64::consts::TAU * fr * t).sin()) as f32,
            )
        }
    }

    #[test]
    fn at_rest_it_is_the_old_gain() {
        let mut r = rig(&[(id::GAIN, -6.0)]);
        let input = stereo(300.0, 0.5, 700.0, 0.25);
        let (l, rr) = r.run(0.1, &input, silence);
        let g = 10f64.powf(-6.0 / 20.0);
        for i in 0..l.len() {
            let (a, b) = input(i);
            assert!((f64::from(l[i]) - f64::from(a) * g).abs() < 1e-6);
            assert!((f64::from(rr[i]) - f64::from(b) * g).abs() < 1e-6);
        }
    }

    #[test]
    fn channels_width_balance_and_polarity() {
        let input = stereo(300.0, 0.5, 700.0, 0.25);
        // Swapped.
        let (l, rr) = rig(&[(id::CHANNELS, 3.0)]).run(0.2, &input, silence);
        assert!((bin_db(&l, 700.0) - 20.0 * 0.25f64.log10()).abs() < 0.1);
        assert!((bin_db(&rr, 300.0) - 20.0 * 0.5f64.log10()).abs() < 0.1);
        // Width 0: both carry the mid.
        let (l, rr) = rig(&[(id::WIDTH, 0.0)]).run(0.2, &input, silence);
        assert!(
            l.iter()
                .zip(&rr)
                .skip(500)
                .all(|(a, b)| (a - b).abs() < 1e-6)
        );
        // Balance hard right: the left is gone, the right untouched.
        let (l, rr) = rig(&[(id::PAN, 1.0)]).run(0.2, &input, silence);
        assert!(level(&l[4_000..], 300.0) < -100.0);
        assert!((bin_db(&rr, 700.0) - 20.0 * 0.25f64.log10()).abs() < 0.1);
        // Polarity: the left inverted.
        let (l, _) = rig(&[(id::INVERT_L, 1.0)]).run(0.2, &input, silence);
        for (i, v) in l.iter().enumerate().skip(500).take(100) {
            assert!((f64::from(*v) + f64::from(input(i).0)).abs() < 1e-6);
        }
        // Mute.
        let (l, _) = rig(&[(id::MUTE, 1.0)]).run(0.2, &input, silence);
        assert!(l.iter().skip(500).all(|v| *v == 0.0));
    }

    #[test]
    fn mono_bass_takes_the_side_out_below_the_frequency_only() {
        // A low tone only on the left (half mid, half side) and a high one
        // only on the right.
        let input = stereo(40.0, 0.5, 2_000.0, 0.5);
        let (l, rr) =
            rig(&[(id::MONO_BASS, 1.0), (id::BASS_FREQ, 150.0)]).run(1.0, &input, silence);
        // The low tone is now in the middle: half on each side.
        let low = 20.0 * 0.25f64.log10();
        assert!((bin_db(&l, 40.0) - low).abs() < 0.5, "{}", bin_db(&l, 40.0));
        assert!((bin_db(&rr, 40.0) - low).abs() < 0.5);
        // The high one stays on the right.
        assert!((bin_db(&rr, 2_000.0) - 20.0 * 0.5f64.log10()).abs() < 0.2);
        assert!(bin_db(&l, 2_000.0) < -40.0, "{}", bin_db(&l, 2_000.0));
        let _ = tone(1.0, 0.0);
    }
}

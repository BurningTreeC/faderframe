//! 76 Compressor: a FET limiting amplifier in the manner of the classic
//! 1176 (written from how that circuit behaves, not from any other code).
//!
//! * The Input drives a fixed threshold (it is the amount of compression);
//!   the Output is the make-up.
//! * Feedback gain control: the reduction follows how far the gain cell's
//!   output is over the threshold, times ratio − 1 — at rest that is exactly
//!   threshold + (input − threshold) / ratio — with a knee that is soft at
//!   4:1 and hard at 20:1, as a FET's is.
//! * Attack 800 µs (1) to 20 µs (7) and release 1.1 s (1) to 50 ms (7) —
//!   7 is fastest, as on the panel; the release depends on the programme (a
//!   slow stage under the fast one after long reduction). The attack's Off
//!   position (fully anticlockwise) disconnects the compression: the
//!   signal still goes through the amplifier and its transformer.
//! * The ratio buttons (4, 8, 12, 20) go in in any combination, as the
//!   hardware's do when pressed together ([`network`]): their taps mix, so
//!   the ratio lands between theirs, and the bias shifts with each extra
//!   button. All four in: the ratio between 12:1 and 20:1, the attack lags
//!   (transients get through), the release quickens, distortion rises and
//!   past a point louder input comes out quieter. None in: no compression.
//! * The meter buttons: GR, +4, +8 — and Off, which is the unit's power
//!   switch (nothing comes through; with the Mix, the dry part is left).
//! * Colour: the FET's even harmonics grow with the reduction, the class-A
//!   output amplifier rounds peaks, and its transformer adds weight under
//!   100 Hz and saturates there first — at 2× oversampling, the dry path of
//!   the Mix delayed to match.
//! * Stereo link (one reduction for both sides), a sidechain high-pass so
//!   the lows do not pump everything.

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::DelayLine;
use crate::dsp::env::MeanSquare;
use crate::dsp::filter::{DcBlock, OnePole};
use crate::dsp::oversample::Oversampler;
use crate::dsp::smooth::Smoothed;
use crate::dsp::{db, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

pub mod id {
    pub const INPUT: u32 = 0;
    pub const OUTPUT: u32 = 1;
    /// 0 (Off: no compression, the line amplifier alone), 1 (slowest) …
    /// 7 (fastest).
    pub const ATTACK: u32 = 2;
    pub const RELEASE: u32 = 3;
    /// The ratio buttons pressed: bit 0 = 4, 1 = 8, 2 = 12, 3 = 20 (any
    /// combination, as on the hardware; 15 = all four).
    pub const RATIO: u32 = 4;
    pub const MIX: u32 = 5;
    pub const LINK: u32 = 6;
    /// The meter buttons: GR, +4, +8, Off — which is the unit's power
    /// switch (the others switch it on).
    pub const METER: u32 = 7;
    /// Sidechain high-pass: off, 60, 120, 240 Hz.
    pub const SC_HPF: u32 = 8;
}

/// Published values.
pub mod value {
    /// The reduction now (dB, ≥ 0; the most of both sides).
    pub const GR: usize = 0;
    /// The output's and the input's mean square over the last block.
    pub const OUT_MS: usize = 1;
    pub const IN_MS: usize = 2;
}
pub const TAP_VALUES: usize = 3;

/// The ratio buttons' ratios, bit by bit.
pub const BUTTONS: [u32; 4] = [4, 8, 12, 20];
pub const ALL_BUTTONS: u8 = 0b1111;
pub const METERS: [&str; 4] = ["GR", "+4", "+8", "Off"];
pub const SC_FILTERS: [&str; 4] = ["Off", "60 Hz", "120 Hz", "240 Hz"];
/// The meter position that switches the unit off.
pub const POWER_OFF: usize = 3;

/// Where the gain cell starts to work at 4:1 (dBFS, the cell's output).
const THRESHOLD: f64 = -18.0;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::INPUT, "Input", -20.0, 40.0, 0.0, Decibels),
        param(id::OUTPUT, "Output", -30.0, 24.0, 0.0, Decibels),
        param(id::ATTACK, "Attack", 0.0, 7.0, 3.0, None),
        param(id::RELEASE, "Release", 1.0, 7.0, 4.0, None),
        stepped(id::RATIO, "Ratio Buttons", 15.0, 1.0),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        stepped(id::LINK, "Stereo Link", 1.0, 1.0),
        fixed(stepped(id::METER, "Meter", 3.0, 0.0)),
        stepped(id::SC_HPF, "Sidechain High-Pass", 3.0, 0.0),
    ]
}

/// The buttons a Ratio value has in.
pub fn buttons(v: f64) -> u8 {
    (v.round().clamp(0.0, 15.0) as u8) & ALL_BUTTONS
}

/// "4:1", "8 + 20", "All buttons", "None".
pub fn buttons_name(mask: u8) -> String {
    match mask & ALL_BUTTONS {
        0 => "None (no compression)".into(),
        ALL_BUTTONS => "All buttons".into(),
        m if m.count_ones() == 1 => format!("{}:1", BUTTONS[m.trailing_zeros() as usize]),
        m => (0..4)
            .filter(|b| m >> b & 1 == 1)
            .map(|b| BUTTONS[b].to_string())
            .collect::<Vec<_>>()
            .join(" + "),
    }
}

/// Is the attack knob at Off (the compression disconnected)?
pub fn attack_off(knob: f64) -> bool {
    knob < 0.5
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::ATTACK if attack_off(v) => "Off".into(),
        id::ATTACK => format!("{:.1} ({:.0} µs)", v.max(1.0), attack_ms(v) * 1000.0),
        id::RELEASE => format!("{:.1} ({:.0} ms)", v, release_ms(v)),
        id::RATIO => buttons_name(buttons(v)),
        id::METER if v.round() as usize == POWER_OFF => "Off (power)".into(),
        id::METER => pick(&METERS, v),
        id::SC_HPF => pick(&SC_FILTERS, v),
        id::LINK => on_off(v),
        _ => return None,
    })
}

/// The attack's time constant (ms) at a knob position 1…7.
pub fn attack_ms(knob: f64) -> f64 {
    let t = ((knob - 1.0) / 6.0).clamp(0.0, 1.0);
    // 800 µs → 20 µs, evenly in log.
    0.8 * (0.02f64 / 0.8).powf(t)
}

/// The release's time constant (ms) at a knob position 1…7.
pub fn release_ms(knob: f64) -> f64 {
    let t = ((knob - 1.0) / 6.0).clamp(0.0, 1.0);
    1100.0 * (50.0f64 / 1100.0).powf(t)
}

/// The oversampling factor of the gain cell and its colour.
const OVERSAMPLING: usize = 2;

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    Oversampler::new(OVERSAMPLING).latency()
}

/// How the pressed buttons make the gain control behave: the ratio, the
/// knee (dB), the threshold's offset, the reduction's share that follows
/// the input's overshoot too (all four in: more than one — past a point
/// louder input comes out quieter), the FET's colour, the attack's and
/// the release's scaling, and how far the bias has moved (0 with one
/// button in, 1 with all four).
#[derive(Clone, Copy, Debug)]
pub struct Mode {
    pub ratio: f64,
    pub knee: f64,
    pub threshold: f64,
    pub forward: f64,
    pub colour: f64,
    pub attack: f64,
    pub release: f64,
    pub bias: f64,
}

/// The most the cell reduces (dB).
const MAX_GR: f64 = 40.0;

/// Each button's place in the ratio network: its loop gain (ratio − 1),
/// how strongly it pulls on the sidechain's tap, its threshold (higher
/// ratios set it higher, as the hardware's do) and its knee.
const NETWORK: [(f64, f64, f64, f64); 4] = [
    (3.0, 1.0, 0.0, 8.0),
    (7.0, 1.5, 1.5, 5.0),
    (11.0, 2.5, 3.0, 3.5),
    (19.0, 3.5, 4.5, 2.0),
];

/// The gain control for the buttons in `mask` (None: no button in, no
/// compression). Buttons pressed together are taps of the ratio network
/// tied together: the sidechain hears their mix (weighted by how hard each
/// pulls), so the ratio lands between theirs — all four between 12:1 and
/// 20:1, as the hardware's manual has it — and the bias network shifts with
/// every extra button: the threshold drops, the attack lags (transients
/// get through first), the release quickens, the FET distorts more and
/// past three buttons louder input comes out quieter.
pub fn network(mask: u8) -> Option<Mode> {
    let mask = mask & ALL_BUTTONS;
    if mask == 0 {
        return None;
    }
    let (mut g, mut k, mut th, mut knee) = (0.0, 0.0, 0.0, 0.0);
    for (b, (gain, pull, threshold, soft)) in NETWORK.iter().enumerate() {
        if mask >> b & 1 == 1 {
            g += pull;
            k += pull * gain;
            th += pull * threshold;
            knee += pull * soft;
        }
    }
    let bias = f64::from(mask.count_ones() - 1) / 3.0;
    Some(Mode {
        ratio: 1.0 + k / g,
        knee: knee / g * (1.0 - 0.3 * bias),
        threshold: th / g - 4.0 * bias,
        forward: 1.15 * bias * bias.sqrt(),
        colour: 1.0 + 2.0 * bias,
        attack: 1.0 + 0.6 * bias,
        release: 1.0 - 0.25 * bias,
        bias,
    })
}

fn mode(mask: u8) -> Mode {
    network(mask).unwrap_or(Mode {
        ratio: 1.0,
        knee: 1.0,
        threshold: 0.0,
        forward: 0.0,
        colour: 1.0,
        attack: 1.0,
        release: 1.0,
        bias: 0.0,
    })
}

/// The threshold (dBFS at the cell's output) the buttons set.
pub fn threshold(mask: u8) -> f64 {
    THRESHOLD + mode(mask).threshold
}

/// What part of an overshoot (dB over the threshold) a knee of `knee`
/// counts (the overshoot above it, a quadratic blend across it).
pub fn soft_over(over: f64, knee: f64) -> f64 {
    let w = knee.max(1e-6) / 2.0;
    if over <= -w {
        0.0
    } else if over >= w {
        over
    } else {
        (over + w) * (over + w) / (4.0 * w)
    }
}

/// The static curve: the output level (dB) for an input level `input`
/// (dB, after the Input gain) with the ratio buttons `mask` in.
pub fn static_curve(input: f64, mask: u8) -> f64 {
    if mask & ALL_BUTTONS == 0 {
        return input;
    }
    // The feedback at rest: out = in − gr(out, in); out + gr rises with
    // out, so the root is found by halving.
    let m = mode(mask);
    let t = THRESHOLD + m.threshold;
    let gr = |out: f64| want(&m, out - t, input - t);
    let (mut lo, mut hi) = (input - MAX_GR - 1.0, input);
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if mid + gr(mid) > input {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The reduction a mode wants for the output's and the input's overshoot
/// (dB over the threshold).
fn want(m: &Mode, out_over: f64, in_over: f64) -> f64 {
    let fb = (m.ratio - 1.0) * soft_over(out_over, m.knee);
    let ff = m.forward * soft_over(in_over, m.knee);
    (fb + ff).min(MAX_GR)
}

/// The slope of [`soft_over`] at `over`.
fn soft_slope(over: f64, knee: f64) -> f64 {
    let w = knee.max(1e-6) / 2.0;
    if over <= -w {
        0.0
    } else if over >= w {
        1.0
    } else {
        (over + w) / (2.0 * w)
    }
}

/// One sample of the feedback loop, solved for the new reduction `x`
/// without a sample's delay: `x = r + k·(want(L − x − t, L − t) − r)` for a
/// detected level `level` (dB) and the reduction so far `r`, `k` the
/// attack's coefficient when the loop asks for more, the release's when
/// less. The left side less the right rises with `x` (concave), so Newton
/// from `r` lands on it in a few steps. A loop as fast as the panel's 20 µs
/// stays stable at any ratio this way, where an explicit step would ring.
fn step(m: &Mode, level: f64, t: f64, r: f64, attack: f64, release: f64) -> f64 {
    let k = if want(m, level - r - t, level - t) > r {
        attack
    } else {
        release
    };
    let f = |x: f64| x - r - k * (want(m, level - x - t, level - t) - r);
    let mut x = r;
    for _ in 0..6 {
        let d = 1.0 + k * (m.ratio - 1.0) * soft_slope(level - x - t, m.knee);
        let next = x - f(x) / d;
        if (next - x).abs() < 1e-9 {
            x = next;
            break;
        }
        x = next;
    }
    x.clamp(0.0, MAX_GR)
}

pub struct Fet76Processor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    over: [Oversampler; 2],
    /// The reduction per side (dB ≥ 0) and its slow companion, which holds
    /// part of it after long reduction (the release that depends on the
    /// programme).
    gr: [f64; 2],
    slow: [f64; 2],
    slow_up: f64,
    /// The detector (a rectifier and a short hold) per side.
    peak: [f64; 2],
    peak_fall: f64,
    sc_hp: [OnePole; 2],
    dc: [DcBlock; 2],
    /// The output transformer's low band (it saturates there first).
    iron: [OnePole; 2],
    dry: [DelayLine; 2],
    delay: usize,
    input: Smoothed,
    output: Smoothed,
    mix: Smoothed,
    /// The unit's power (Meter Off switches it off).
    power: Smoothed,
    ms_in: MeanSquare,
    ms_out: MeanSquare,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl Fet76Processor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let inner = sr * OVERSAMPLING as f64;
        let over = [
            Oversampler::new(OVERSAMPLING),
            Oversampler::new(OVERSAMPLING),
        ];
        let delay = over[0].latency() as usize;
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let mut p = Self {
            watching: Watching::new(sr as f32),
            sr,
            over,
            gr: [0.0; 2],
            slow: [0.0; 2],
            slow_up: crate::dsp::env::coeff(1000.0, sr),
            peak: [0.0; 2],
            peak_fall: (-1.0 / (0.005 * sr)).exp(),
            sc_hp: [OnePole::new(60.0, sr); 2],
            dc: [DcBlock::new(inner); 2],
            iron: [OnePole::new(90.0, inner); 2],
            dry: [DelayLine::new(delay + 1), DelayLine::new(delay + 1)],
            delay,
            input: Smoothed::new(1.0, 20.0, sr),
            output: Smoothed::new(1.0, 20.0, sr),
            mix: Smoothed::new(1.0, 20.0, sr),
            power: Smoothed::new(1.0, 30.0, sr),
            ms_in: MeanSquare::new(50.0, sr),
            ms_out: MeanSquare::new(50.0, sr),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        p.controls();
        p.input.snap();
        p.output.snap();
        p.mix.snap();
        p.power.snap();
        p
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    /// Read the knobs: the mode, whether it compresses at all (a ratio
    /// button in, the attack not at Off), the attack's and the release's
    /// coefficients, the slow stage's fall and the sidechain filter's corner
    /// (0: off).
    fn controls(&mut self) -> (Mode, bool, f64, f64, f64, f64) {
        let mask = buttons(self.get(id::RATIO));
        let m = mode(mask);
        let compressing = mask != 0 && !attack_off(self.get(id::ATTACK));
        let sr = self.sr;
        let release = release_ms(self.get(id::RELEASE)) * m.release;
        let attack = crate::dsp::env::coeff(attack_ms(self.get(id::ATTACK)) * m.attack, sr);
        let slow_fall = crate::dsp::env::coeff(release * 3.0, sr);
        let release = crate::dsp::env::coeff(release, sr);
        let hp = [0.0, 60.0, 120.0, 240.0][self.get(id::SC_HPF).round().clamp(0.0, 3.0) as usize];
        if hp > 0.0 {
            for f in &mut self.sc_hp {
                f.set(hp, sr);
            }
        }
        self.input.set(gain(self.get(id::INPUT)));
        self.output.set(gain(self.get(id::OUTPUT)));
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        // Meter Off is the power switch: nothing comes through the unit.
        let on = self.get(id::METER).round() as usize != POWER_OFF;
        self.power.set(if on { 1.0 } else { 0.0 });
        (m, compressing, attack, release, slow_fall, hp)
    }
}

impl PluginProcessor for Fet76Processor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let Some(channels) = pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n = io.frames.min(self.scratch[0].len());
        let watched = self.watching.check(&self.tap, n);
        let sides = channels.min(2);
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let (m, compressing, attack, release, slow_fall, hp) = self.controls();
        let link = self.get(id::LINK) >= 0.5 && sides == 2;
        let t = THRESHOLD + m.threshold;
        let fet = 0.006 * m.colour;
        let mut most = 0.0f64;
        for i in 0..n {
            let g_in = self.input.tick();
            let g_out = self.output.tick();
            let mix = self.mix.tick();
            let power = self.power.tick();
            // The detectors (after the Input, through the sidechain's
            // high-pass).
            let mut level = [-120.0f64; 2];
            for (c, level) in level.iter_mut().enumerate().take(sides) {
                let s = f64::from(self.scratch[c][i]) * g_in;
                let d = if hp > 0.0 { self.sc_hp[c].high(s) } else { s };
                self.peak[c] = flush(d.abs().max(self.peak[c] * self.peak_fall));
                // No ratio button in, or the attack at Off: the sidechain
                // hears nothing and the reduction lets go.
                *level = if compressing {
                    db(self.peak[c].max(1e-7))
                } else {
                    -120.0
                };
            }
            // The loop: one reduction for both sides when linked.
            let shared = if link {
                Some(step(
                    &m,
                    level[0].max(level[1]),
                    t,
                    self.gr[0].max(self.gr[1]),
                    attack,
                    release,
                ))
            } else {
                None
            };
            for (c, &level) in level.iter().enumerate().take(sides) {
                let r = shared.unwrap_or_else(|| step(&m, level, t, self.gr[c], attack, release));
                // The slow stage climbs during long reduction and holds a
                // third of what it reached, falling three times slower.
                let k = if r > self.slow[c] {
                    self.slow_up
                } else {
                    slow_fall
                };
                self.slow[c] = flush(self.slow[c] + (r - self.slow[c]) * k);
                let r = r.max(0.3 * self.slow[c]);
                self.gr[c] = r;
                most = most.max(r);
                let x = f64::from(self.scratch[c][i]);
                self.dry[c].push(x);
                let g = gain(-r);
                // The FET's even harmonics: they grow with the reduction
                // (its bias) and with the signal across it (the cell's
                // input), to a share of it.
                let amount = fet * (r / 20.0).min(2.0);
                let (dc, iron) = (&mut self.dc[c], &mut self.iron[c]);
                let mut cell = |u: f64| -> f64 {
                    let s = u * g_in;
                    let y = dc.process(g * (s + amount * s * s.tanh()));
                    // The class-A amplifier and its transformer: rounded
                    // peaks, the lows first.
                    let low = iron.low(y);
                    let z = (low * 1.4).tanh() / 1.4 + (y - low);
                    (z * 0.6).tanh() / 0.6
                };
                let wet = self.over[c].process(x, &mut cell);
                let dry = self.dry[c].tap(self.delay);
                let y = (dry * (1.0 - mix) + wet * mix * power) * g_out;
                out.channel_mut(c)[i] = y as f32;
                self.ms_in.process(x);
                self.ms_out.process(y);
            }
        }
        for o in &mut self.iron {
            o.flush();
        }
        self.ms_in.flush();
        self.ms_out.flush();
        self.tap.set_value(value::GR, most as f32);
        self.tap.set_value(value::OUT_MS, self.ms_out.value as f32);
        self.tap.set_value(value::IN_MS, self.ms_in.value as f32);
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.scratch[c][..n].iter().zip(&o[..n]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n);
            self.meters[1][c].publish(&self.tap.meter_out, c, n);
        }
        // The editor's scope.
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
        for o in &mut self.over {
            o.reset();
        }
        self.gr = [0.0; 2];
        self.slow = [0.0; 2];
        self.peak = [0.0; 2];
        for f in &mut self.sc_hp {
            f.reset();
        }
        for f in &mut self.iron {
            f.reset();
        }
        self.ms_in.reset();
        self.ms_out.reset();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, level, peak_db, silence, thd, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<Fet76Processor> {
        Rig::with(parameters(), TAP_VALUES, set, Fet76Processor::new)
    }

    fn amp(db: f64) -> f64 {
        10f64.powf(db / 20.0)
    }

    /// A 1 kHz tone `over` dB over the threshold: the output (dB) once
    /// settled.
    fn settled(set: &[(u32, f64)], over: f64) -> f64 {
        let mut r = rig(set);
        let (l, _) = r.run(1.0, tone(1000.0, amp(THRESHOLD + over)), silence);
        level(&l[l.len() / 2..], 1000.0)
    }

    const FOUR: f64 = 1.0;
    const EIGHT: f64 = 2.0;
    const TWENTY: f64 = 8.0;
    const ALL: f64 = 15.0;

    #[test]
    fn played_it_follows_the_static_curve() {
        let flat = [(id::INPUT, 0.0), (id::OUTPUT, 0.0)];
        for (mask, over) in [(FOUR, 12.0), (EIGHT, 12.0), (TWENTY, 18.0), (9.0, 14.0)] {
            let set = [flat[0], flat[1], (id::RATIO, mask)];
            let got = settled(&set, over);
            let want = static_curve(THRESHOLD + over, mask as u8);
            assert!(
                (got - want).abs() < 1.0,
                "{}: {got:.2} dB, the curve says {want:.2}",
                buttons_name(mask as u8)
            );
        }
        // Under the threshold it is (nearly) a wire.
        let quiet = settled(&flat, -20.0);
        assert!((quiet - (THRESHOLD - 20.0)).abs() < 0.2, "{quiet}");
    }

    #[test]
    fn each_button_is_its_ratio_and_higher_ones_set_the_threshold_higher() {
        let mut last = f64::NEG_INFINITY;
        for (b, ratio) in BUTTONS.iter().enumerate() {
            let mask = 1u8 << b;
            let t = threshold(mask);
            assert!(t > last, "{ratio}:1's threshold {t} over the last {last}");
            last = t;
            // Well over the threshold: a ratio-th of the overshoot.
            let over = static_curve(t + 30.0, mask) - t;
            let want = 30.0 / f64::from(*ratio);
            assert!(
                (over - want).abs() < 0.35,
                "{ratio}:1: {over:.2} for {want:.2}"
            );
        }
    }

    #[test]
    fn buttons_pressed_together_mix_their_taps() {
        // Two buttons: a ratio between theirs, the bias a third of the way.
        for (a, b) in [(0, 3), (1, 2), (2, 3), (0, 1)] {
            let m = network((1 << a) | (1 << b)).unwrap();
            let (lo, hi) = (f64::from(BUTTONS[a]), f64::from(BUTTONS[b]));
            assert!(m.ratio > lo && m.ratio < hi, "{lo}+{hi}: {}", m.ratio);
            assert!((m.bias - 1.0 / 3.0).abs() < 1e-9);
        }
        // All four: between 12:1 and 20:1 (the hardware's manual), the bias
        // all the way.
        let all = network(ALL_BUTTONS).unwrap();
        assert!(all.ratio > 12.0 && all.ratio < 20.0, "{}", all.ratio);
        assert_eq!(all.bias, 1.0);
        // Every combination has its name and differs from its neighbours.
        let names: std::collections::HashSet<String> = (0..16).map(buttons_name).collect();
        assert_eq!(names.len(), 16);
        assert_eq!(buttons_name(9), "4 + 20");
        assert_eq!(buttons_name(4), "12:1");
        // None in: no compression at all.
        assert!(network(0).is_none());
        assert_eq!(static_curve(THRESHOLD + 30.0, 0), THRESHOLD + 30.0);
    }

    #[test]
    fn all_buttons_in_turns_louder_into_quieter() {
        let set = |input| [(id::INPUT, input), (id::OUTPUT, 0.0), (id::RATIO, ALL)];
        let a = settled(&set(12.0), 0.0);
        let b = settled(&set(30.0), 0.0);
        assert!(b < a, "+12 dB in: {a:.2} dB out, +30 dB in: {b:.2} dB out");
        // Three buttons lean that way, two do not yet.
        let slope =
            |mask: u8| static_curve(THRESHOLD + 30.0, mask) - static_curve(THRESHOLD + 20.0, mask);
        assert!(slope(ALL_BUTTONS) < 0.0);
        assert!(slope(0b0111) < slope(0b0011) && slope(0b0011) > 0.0);
        // And it grinds: more harmonics than at 4:1 for the same reduction.
        let grind = |mask: f64, input: f64| {
            let mut r = rig(&[(id::INPUT, input), (id::RATIO, mask)]);
            let (l, _) = r.run(1.0, tone(200.0, amp(THRESHOLD)), silence);
            thd(&l[l.len() / 2..], 200.0)
        };
        // (The rig reads about 0.3 % on a clean sine.)
        let (all, four) = (grind(ALL, 24.0), grind(FOUR, 24.0));
        assert!(all > 1.5 * four, "all buttons {all:.2} %, 4:1 {four:.2} %");
    }

    #[test]
    fn attack_off_or_no_button_leaves_the_line_amplifier() {
        let loud = 20.0;
        for set in [
            vec![(id::ATTACK, 0.0)],
            vec![(id::RATIO, 0.0)],
            vec![(id::RATIO, ALL), (id::ATTACK, 0.2)],
        ] {
            let mut all = vec![(id::INPUT, loud), (id::OUTPUT, 0.0)];
            all.extend(set.iter().copied());
            let mut r = rig(&all);
            let (l, _) = r.run(0.5, tone(1000.0, amp(-30.0)), silence);
            assert_eq!(r.tap.value(value::GR), 0.0, "{set:?}");
            // The level goes through (the colour stays, a little).
            let out = level(&l[l.len() / 2..], 1000.0);
            assert!((out - (-30.0 + loud)).abs() < 0.5, "{set:?}: {out:.2} dB");
        }
        assert_eq!(format(ParameterId(id::ATTACK), 0.0).unwrap(), "Off");
    }

    #[test]
    fn meter_off_switches_the_unit_off() {
        let mut r = rig(&[(id::METER, POWER_OFF as f64), (id::OUTPUT, 0.0)]);
        let (l, _) = r.run(0.3, tone(1000.0, 0.5), silence);
        assert!(peak_db(&l[l.len() / 2..]) < -100.0, "silent when off");
        // Back on (GR): it plays again.
        r.set(id::METER, 0.0);
        let (l, _) = r.run(0.3, tone(1000.0, 0.5), silence);
        assert!(peak_db(&l[l.len() / 2..]) > -30.0);
        // Parallel: only the dry part is left with the unit off.
        let mut r = rig(&[
            (id::METER, POWER_OFF as f64),
            (id::MIX, 0.5),
            (id::OUTPUT, 0.0),
        ]);
        let (l, _) = r.run(0.3, tone(1000.0, 0.5), silence);
        let dry = level(&l[l.len() / 2..], 1000.0);
        assert!((dry - 20.0 * 0.25f64.log10()).abs() < 0.2, "{dry:.2} dB");
    }

    #[test]
    fn the_release_runs_from_slow_to_fast() {
        let after = |release: f64| {
            let mut r = rig(&[(id::INPUT, 20.0), (id::RELEASE, release)]);
            r.run(0.5, tone(1000.0, 0.25), silence);
            let before = f64::from(r.tap.value(value::GR));
            r.run(0.15, silence, silence);
            (before, f64::from(r.tap.value(value::GR)))
        };
        let (b7, a7) = after(7.0);
        let (b1, a1) = after(1.0);
        assert!(b7 > 10.0 && b1 > 10.0, "{b7} {b1}");
        // 150 ms after: 7 (50 ms) has let go, 1 (1.1 s) holds most of it.
        assert!(a7 < 0.15 * b7, "release 7: {a7:.2} of {b7:.2} dB left");
        assert!(a1 > 0.6 * b1, "release 1: {a1:.2} of {b1:.2} dB left");
    }

    #[test]
    fn the_fastest_loop_does_not_ring() {
        // Attack and release 7, all buttons in, a loud low tone: the
        // reduction rides the waveform (as the hardware's does) but stays
        // bounded and the output finite.
        let mut r = rig(&[
            (id::INPUT, 40.0),
            (id::ATTACK, 7.0),
            (id::RELEASE, 7.0),
            (id::RATIO, ALL),
            (id::OUTPUT, 0.0),
        ]);
        let (l, rr) = r.run(1.0, tone(60.0, 0.5), silence);
        assert!(l.iter().chain(&rr).all(|v| v.is_finite()));
        let p = peak_db(&l[l.len() / 2..]);
        assert!(p < 0.0 && p > -40.0, "{p}");
        for mask in 1..=ALL_BUTTONS {
            let name = buttons_name(mask);
            for level in [-30.0, 0.0, 20.0, 40.0] {
                let mut x = 0.0;
                for _ in 0..200 {
                    x = step(&mode(mask), THRESHOLD + level, threshold(mask), x, 1.0, 1.0);
                }
                // A step that takes the whole way lands on the curve.
                let o = THRESHOLD + level - x;
                assert!(
                    (o - static_curve(THRESHOLD + level, mask)).abs() < 0.05 || x >= MAX_GR - 1e-9,
                    "{name} at {level}: {o}"
                );
            }
        }
    }

    #[test]
    fn linked_sides_share_the_reduction() {
        let loud_left = |n: usize| {
            let v = (std::f64::consts::TAU * 1000.0 * n as f64 / SR).sin();
            ((0.5 * v) as f32, (0.01 * v) as f32)
        };
        let right = |link: f64| {
            let mut r = rig(&[(id::INPUT, 10.0), (id::OUTPUT, 0.0), (id::LINK, link)]);
            let (_, rr) = r.run(0.5, loud_left, silence);
            level(&rr[rr.len() / 2..], 1000.0)
        };
        let quiet = 20.0 * 0.01f64.log10() + 10.0;
        let free = right(0.0);
        let linked = right(1.0);
        assert!((free - quiet).abs() < 0.3, "unlinked: {free:.2} dB");
        assert!(linked < quiet - 6.0, "linked: {linked:.2} dB");
    }

    #[test]
    fn mix_at_zero_is_the_dry_signal_in_time() {
        let mut r = rig(&[(id::MIX, 0.0), (id::INPUT, 30.0), (id::OUTPUT, -6.0)]);
        let input = tone(440.0, 0.5);
        let (l, _) = r.run(0.2, &input, silence);
        let d = latency(&r.params, SR) as usize;
        let g = amp(-6.0);
        for (i, y) in l.iter().enumerate().skip(d + 1) {
            let want = f64::from(input(i - d).0) * g;
            assert!((f64::from(*y) - want).abs() < 1e-5, "{i}: {y} vs {want}");
        }
    }

    #[test]
    fn below_the_knee_nothing_happens() {
        for mask in 1..=ALL_BUTTONS {
            let quiet = threshold(mask) - 20.0;
            assert!((static_curve(quiet, mask) - quiet).abs() < 1e-6, "{mask}");
        }
    }

    #[test]
    fn seven_is_fastest() {
        assert!((attack_ms(7.0) - 0.02).abs() < 1e-9 && (attack_ms(1.0) - 0.8).abs() < 1e-9);
        assert!((release_ms(7.0) - 50.0).abs() < 1e-9 && (release_ms(1.0) - 1100.0).abs() < 1e-6);
    }
}

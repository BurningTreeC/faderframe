//! Measurements shared by the compressors' tests: steady tones, envelopes,
//! the step that a compressor's attack and release are read off, and the
//! plucked note every pedal's level is set by.

use crate::dsp::time::Simulation;
use crate::voice::{GUITAR_VOLTS, NOMINAL_DBFS};

pub const SR: f64 = 48_000.0;

/// RMS in dBV.
pub fn dbv(x: &[f64]) -> f64 {
    let mean = x.iter().map(|v| v * v).sum::<f64>() / x.len().max(1) as f64;
    10.0 * mean.max(1e-30).log10()
}

/// RMS in dBV of what moves, the mean taken off: a node inside a circuit
/// sits at its bias.
pub fn ac_dbv(x: &[f64]) -> f64 {
    let mean = x.iter().sum::<f64>() / x.len().max(1) as f64;
    let centred: Vec<f64> = x.iter().map(|v| v - mean).collect();
    dbv(&centred)
}

/// A sine of `level` dBV RMS at `hz` for `seconds`, through `sim`: the RMS of
/// what moves in its last half second, in dBV.
pub fn steady(sim: &mut Simulation, level: f64, hz: f64, seconds: f64) -> f64 {
    let amplitude = 10f64.powf(level / 20.0) * std::f64::consts::SQRT_2;
    let n = (seconds * SR) as usize;
    let tail = (0.5 * SR) as usize;
    let mut out = Vec::with_capacity(tail);
    for k in 0..n {
        let y = sim.process(amplitude * (std::f64::consts::TAU * hz * k as f64 / SR).sin());
        if k >= n - tail {
            out.push(y);
        }
    }
    ac_dbv(&out)
}

/// The largest magnitude in each window of `window` samples.
pub fn peaks(y: &[f64], window: usize) -> Vec<f64> {
    y.chunks(window)
        .map(|c| c.iter().fold(0.0f64, |m, v| m.max(v.abs())))
        .collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// What a compressor does across a step, as its envelope shows it.
#[derive(Debug)]
pub struct Step {
    /// The settled output peak before the step up, after it, and after the
    /// step back down.
    pub before: f64,
    pub loud: f64,
    pub after: f64,
    /// The highest output peak in the first 50 ms after the step up.
    pub overshoot: f64,
    /// How long the excess over the settled level takes to fall to 1/e of
    /// what it was, in seconds.
    pub attack: f64,
    /// And until the output is within a decibel of where it settles.
    pub settled: f64,
    /// How long, after the step down, the output takes to recover 63 % of
    /// the decibels it fell by, in seconds.
    pub release: f64,
    /// And until it is within a decibel of where it settles.
    pub recovered: f64,
}

/// A 1 kHz sine of `quiet` volts peak for 2 s, `loud` for 2 s, `quiet` again
/// for 3 s (the shape of the DAFx-11 paper's Figure 5), envelope in 1 ms
/// windows.
pub fn step(sim: &mut Simulation, quiet: f64, loud: f64) -> Step {
    let ms = (SR / 1000.0) as usize;
    let n = 7 * SR as usize;
    let mut y = Vec::with_capacity(n);
    for k in 0..n {
        let t = k as f64 / SR;
        let a = if (2.0..4.0).contains(&t) { loud } else { quiet };
        y.push(sim.process(a * (std::f64::consts::TAU * 1000.0 * t).sin()));
    }
    let env = peaks(&y, ms);
    let at = |s: f64| (s * 1000.0) as usize;
    let before = median(env[at(1.5)..at(2.0)].to_vec());
    let loud = median(env[at(3.5)..at(4.0)].to_vec());
    let after = median(env[at(6.5)..at(7.0)].to_vec());
    let overshoot = env[at(2.0)..at(2.05)].iter().fold(0.0f64, |m, &v| m.max(v));
    let excess = overshoot - loud;
    let attack = env[at(2.0)..at(3.0)]
        .iter()
        .enumerate()
        .skip_while(|&(_, &v)| v < overshoot)
        .find(|&(_, &v)| v - loud <= excess / std::f64::consts::E)
        .map_or(1.0, |(i, _)| i as f64 / 1000.0);
    let within = |slice: &[f64], level: f64| {
        let db = |v: f64| (20.0 * (v / level).log10()).abs();
        slice
            .iter()
            .rposition(|&v| db(v) > 1.0)
            .map_or(0.0, |i| (i + 1) as f64 / 1000.0)
    };
    let settled = within(&env[at(2.0)..at(3.0)], loud);
    let floor = env[at(4.0)..at(4.1)]
        .iter()
        .fold(f64::MAX, |m, &v| m.min(v));
    let fell = 20.0 * (after / floor).log10();
    let release = env[at(4.0)..]
        .iter()
        .enumerate()
        .skip_while(|&(_, &v)| v > floor)
        .find(|&(_, &v)| 20.0 * (after / v).log10() <= fell / std::f64::consts::E)
        .map_or(3.0, |(i, _)| i as f64 / 1000.0);
    let recovered = within(&env[at(4.0)..at(6.5)], after);
    Step {
        before,
        loud,
        after,
        overshoot,
        attack,
        settled,
        release,
        recovered,
    }
}

/// The plucked note the plug-in's tests play (a 110 Hz string with two
/// harmonics, plucked three times a second), in volts at the pedal slot's
/// input: the plug-in's 0.2 of full scale at `GUITAR_VOLTS` for a nominal
/// signal.
pub fn pluck(n: usize) -> f64 {
    let t = n as f64 / SR;
    let since = (t * 3.0).fract() / 3.0;
    let env = (-since * 7.0).exp();
    let volts = GUITAR_VOLTS / 10f64.powf(NOMINAL_DBFS / 20.0);
    volts
        * 0.2
        * env
        * ((std::f64::consts::TAU * 110.0 * t).sin()
            + 0.4 * (std::f64::consts::TAU * 220.0 * t).sin()
            + 0.2 * (std::f64::consts::TAU * 330.0 * t).sin())
}

/// What `sim` does to two seconds of the pluck: output less input, RMS over
/// the second second, in dB.
pub fn pluck_gain(sim: &mut Simulation) -> f64 {
    let n = 2 * SR as usize;
    let (mut x, mut y) = (Vec::new(), Vec::new());
    for k in 0..n {
        let v = pluck(k);
        let out = sim.process(v);
        if k >= n / 2 {
            x.push(v);
            y.push(out);
        }
    }
    dbv(&y) - dbv(&x)
}

/// A string plucked once and left to ring: `volts` peak at 196 Hz, falling
/// by 1/e every `tau` seconds. The output's fall from 0.2 s to 1.2 s against
/// the input's, both in dB.
pub fn sustain(sim: &mut Simulation, volts: f64, tau: f64) -> (f64, f64) {
    let n = (1.3 * SR) as usize;
    let mut y = Vec::with_capacity(n);
    let mut x = Vec::with_capacity(n);
    for k in 0..n {
        let t = k as f64 / SR;
        let v = volts * (-t / tau).exp() * (std::f64::consts::TAU * 196.0 * t).sin();
        x.push(v);
        y.push(sim.process(v));
    }
    let window = |s: &[f64], at: f64| {
        let start = (at * SR) as usize;
        dbv(&s[start..start + (0.1 * SR) as usize])
    };
    (
        window(&y, 0.15) - window(&y, 1.15),
        window(&x, 0.15) - window(&x, 1.15),
    )
}

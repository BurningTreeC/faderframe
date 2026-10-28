//! The 2A Leveler's circuits against the factory drawing's test points and
//! the unit's specifications (UREI C-10953 rev C; the Teletronix 3-66
//! manual: gain 40 ±1 dB, response ±0.1 dB 30 Hz–15 kHz, distortion under
//! 0.35 % at +10 dBm and 0.75 % at +16 dBm out).
#![allow(clippy::unwrap_used)]

use faderframe_circuit::circuits::leveler::{
    self as lev, CELL, R37_FLAT, R37_OHMS, R37_SLOT, R7_OHMS, R7_SLOT, SWITCH_OHMS,
};
use faderframe_circuit::dsp::measure::{run, Tone};
use faderframe_circuit::dsp::netlist::{Circuit, Taper};
use faderframe_circuit::dsp::time::Simulation;

const RATE: f64 = 96_000.0;

fn dbm(volts_rms: f64) -> f64 {
    20.0 * (volts_rms / 0.774_6).log10()
}

/// A pot's wiper for a knob at `t` (audio taper): its share of the node
/// above it.
fn share(t: f64) -> f64 {
    Taper::Audio.fraction(t)
}

fn node(c: &Circuit, name: &str) -> usize {
    c.unknown_named(name).unwrap()
}

/// The three circuits as the device runs them: the input's node `c` into
/// the amplifier, its node `b` into the sidechain.
struct Leveler {
    input: Simulation,
    amp: Simulation,
    side: Simulation,
    nodes: Vec<(&'static str, usize, usize)>,
    b: usize,
    c: usize,
    terminal: usize,
    gain: f64,
    peak: f64,
}

const AMP_NODES: [&str; 11] = [
    "b350", "b275", "b205", "p1", "k1", "p2", "k2", "f", "p3", "out", "k4",
];
const SIDE_NODES: [&str; 7] = ["b350", "b275", "p5", "k5", "p6", "s6", "k6"];

impl Leveler {
    fn new(gain: f64, peak: f64, limit: bool, cell: f64) -> Self {
        let ic = lev::input(600.0).unwrap();
        let (b, c, terminal) = (node(&ic, "b"), node(&ic, "c"), node(&ic, "in"));
        let ac = lev::amplifier(600.0).unwrap();
        let sc = lev::sidechain().unwrap();
        let mut nodes = Vec::new();
        for n in AMP_NODES {
            nodes.push((n, 1, node(&ac, n)));
        }
        for n in SIDE_NODES {
            nodes.push((n, 2, node(&sc, n)));
        }
        nodes.push(("panel", 2, node(&sc, "panel")));
        let mut input = Simulation::new(ic, RATE);
        input.set_value(R7_SLOT, if limit { R7_OHMS } else { SWITCH_OHMS });
        input.set_realtime_value(CELL, cell);
        input.reset();
        let mut amp = Simulation::new(ac, RATE);
        amp.reset();
        let mut side = Simulation::new(sc, RATE);
        side.set_value(R37_SLOT, R37_FLAT);
        side.reset();
        for s in [&mut input, &mut amp, &mut side] {
            s.set_early_acceptance(true);
        }
        Self {
            input,
            amp,
            side,
            nodes,
            b,
            c,
            terminal,
            gain: share(gain),
            peak: share(peak),
        }
    }

    /// One sample of the source's EMF: the line out.
    fn process(&mut self, x: f64) -> f64 {
        self.input.process(x);
        let line = self.amp.process(self.gain * self.input.voltage_at(self.c));
        self.side.process(self.peak * self.input.voltage_at(self.b));
        line
    }

    fn v(&self, name: &str) -> f64 {
        let (_, which, at) = *self.nodes.iter().find(|(n, _, _)| *n == name).unwrap();
        if which == 1 {
            self.amp.voltage_at(at)
        } else {
            self.side.voltage_at(at)
        }
    }

    fn sv(&self, name: &str) -> f64 {
        let (_, _, at) = *self
            .nodes
            .iter()
            .find(|(n, w, _)| *n == name && *w == 2)
            .unwrap();
        self.side.voltage_at(at)
    }
}

/// The source's EMF (peak) that puts `dbm` on a 600 ohm input's terminals.
fn emf(dbm_in: f64) -> f64 {
    2.0 * 0.774_6 * 10f64.powf(dbm_in / 20.0) * std::f64::consts::SQRT_2
}

/// A 1 kHz (or `hz`) tone of `dbm_in` through a unit at full Gain, no
/// reduction: (dBm at the terminals, dBm out, THD %).
fn tone(gain: f64, hz: f64, dbm_in: f64) -> (f64, f64, f64) {
    let mut u = Leveler::new(gain, 0.0, false, 1e7);
    let t = Tone::near(RATE, 19_200, hz, emf(dbm_in));
    let mut at_terminal = Vec::new();
    let m = run(t, 48_000, |x| {
        let y = u.process(x);
        at_terminal.push(u.input.voltage_at(u.terminal));
        y
    });
    let tail = &at_terminal[at_terminal.len() - 19_200..];
    let rms = (tail.iter().map(|v| v * v).sum::<f64>() / tail.len() as f64).sqrt();
    (
        dbm(rms),
        dbm(m.fundamental().magnitude() / std::f64::consts::SQRT_2),
        m.thd_percent(),
    )
}

/// The drawing's test points (volts) against the circuits' operating points:
/// within 20 % (the drawing's own figures disagree by about that much — its
/// supply budget, the valves' spread). Node F is the exception: the drawing
/// prints 27 V, but its own output node (100 V), R21 100 k, R22 33 k to
/// ground and R11 68 k into a 1.2 V cathode put it at about 19 V.
#[test]
fn the_operating_points_are_the_drawings() {
    let u = Leveler::new(0.5, 0.5, false, 1e7);
    for (n, want) in [
        ("b275", 275.0),
        ("b205", 205.0),
        ("p1", 115.0),
        ("k1", 1.2),
        ("p2", 120.0),
        ("k2", 1.1),
        ("f", 19.0),
        ("p3", 220.0),
        ("out", 100.0),
        ("k4", 4.0),
    ] {
        let got = u.v(n);
        assert!(
            (got / want - 1.0).abs() < 0.2,
            "amplifier {n}: {got:.2} V, drawn {want}"
        );
    }
    for (n, want) in [
        ("b275", 275.0),
        ("p5", 105.0),
        ("k5", 0.75),
        ("p6", 135.0),
        ("s6", 100.0),
        ("k6", 7.0),
    ] {
        let got = u.sv(n);
        assert!(
            (got / want - 1.0).abs() < 0.2,
            "sidechain {n}: {got:.2} V, drawn {want}"
        );
    }
}

/// "Gain: 40 DB ±1 DB": full Gain, no reduction, in a 600 ohm system.
#[test]
fn the_gain_is_forty_db() {
    let (input, out, _) = tone(1.0, 1000.0, -30.0);
    let gain = out - input;
    assert!((gain - 40.0).abs() < 1.5, "{gain:.2} dB");
}

/// "±0.1 DB 30 cycles to 15 KC": the circuit's estimated iron holds it to
/// a quarter of a decibel.
#[test]
fn the_response_is_flat_to_fifteen_kilohertz() {
    let (_, reference, _) = tone(1.0, 1000.0, -40.0);
    for hz in [30.0, 100.0, 5_000.0, 10_000.0, 15_000.0] {
        let (_, out, _) = tone(1.0, hz, -40.0);
        assert!(
            (out - reference).abs() < 0.25,
            "{hz} Hz: {:+.2} dB",
            out - reference
        );
    }
}

/// "Less than 0.35% total harmonics at +10 DBM, and less than 0.75% total
/// harmonics at +16 DBM": met, and the valves are there (second harmonic
/// first, as a single-ended amplifier's is).
#[test]
fn the_distortion_meets_the_specification() {
    let (input, out, _) = tone(1.0, 1000.0, -30.0);
    let gain = out - input;
    for (level, most, least) in [(10.0, 0.35, 0.05), (16.0, 0.75, 0.15)] {
        let (_, out, thd) = tone(1.0, 1000.0, level - gain);
        assert!((out - level).abs() < 1.0, "{out:.2} dBm for {level}");
        assert!(thd < most && thd > least, "+{level} dBm: {thd:.3} %");
    }
}

/// In Compress the sidechain hears the cell's node (feedback); in Limit the
/// junction above R7, which cannot fall below R7's share of the secondary
/// (part feed-forward).
#[test]
fn limit_takes_the_sidechain_above_r7() {
    let ratio = |limit: bool, cell: f64| {
        let mut u = Leveler::new(0.5, 0.5, limit, cell);
        let (mut b, mut c) = (0.0f64, 0.0f64);
        for i in 0..48_000 {
            u.process(emf(-10.0) * (std::f64::consts::TAU * 1000.0 * i as f64 / RATE).sin());
            if i > 24_000 {
                b = b.max(u.input.voltage_at(u.b).abs());
                c = c.max(u.input.voltage_at(u.c).abs());
            }
        }
        (b, c)
    };
    let (b, c) = ratio(false, 1_000.0);
    assert!((b / c - 1.0).abs() < 0.01, "Compress: b {b:.4}, c {c:.4}");
    let (b, c) = ratio(true, 1_000.0);
    assert!(b > 3.0 * c, "Limit: b {b:.4}, c {c:.4}");
    // With the cell dark the two modes hear the same.
    let (b, c) = ratio(true, 1e7);
    assert!(
        (b / c - 1.0).abs() < 0.05,
        "Limit, dark: b {b:.4}, c {c:.4}"
    );
}

/// The panel's voltage for a sine of `hz` and `volts` at node `b`, R37 at
/// `r37` ohms, Peak Reduction at half.
fn panel(hz: f64, r37: f64, volts: f64) -> f64 {
    let c = lev::sidechain().unwrap();
    let at = node(&c, "panel");
    let mut s = Simulation::new(c, RATE);
    s.set_value(R37_SLOT, r37);
    s.reset();
    let volts = volts * share(0.5);
    let mut most = 0.0f64;
    for i in 0..48_000 {
        s.process(volts * (std::f64::consts::TAU * hz * i as f64 / RATE).sin());
        if i > 24_000 {
            most = most.max(s.voltage_at(at).abs());
        }
    }
    most
}

/// R37 turned up takes the lows down ahead of V4 and leaves the highs (C12
/// across it): the 3-66 manual's "greater [levelling] on the high
/// frequencies".
#[test]
fn r37_lifts_the_sidechains_highs() {
    let tilt = |r37: f64| 20.0 * (panel(12_000.0, r37, 0.05) / panel(300.0, r37, 0.05)).log10();
    let (flat, lifted) = (tilt(R37_FLAT), tilt(R37_OHMS));
    assert!(
        lifted > flat + 4.0,
        "flat {flat:+.2} dB, R37 up {lifted:+.2} dB"
    );
}

/// What the circuits measure (run by hand:
/// `cargo test -p faderframe-circuit --release --test leveler report -- --ignored --nocapture`).
#[test]
#[ignore]
fn report() {
    let u = Leveler::new(0.5, 0.5, false, 1e7);
    println!("operating points (drawing: 275, 205; V1 115/1.2, 120/1.1; f 27; V2 220/100/4):");
    for n in AMP_NODES {
        println!("  amplifier {n:6} {:8.2} V", u.v(n));
    }
    println!("  (drawing: V3 105/0.75; V4 135/100/7)");
    for n in SIDE_NODES {
        println!("  sidechain {n:6} {:8.2} V", u.sv(n));
    }
    let (input, out, thd) = tone(1.0, 1000.0, -30.0);
    println!(
        "full gain: {input:.2} dBm in → {out:.2} dBm out = {:.2} dB, THD {thd:.3} %",
        out - input
    );
    for hz in [
        20.0, 30.0, 50.0, 100.0, 1000.0, 5000.0, 10_000.0, 15_000.0, 20_000.0,
    ] {
        let (_, out, _) = tone(1.0, hz, -40.0);
        println!("  {hz:6.0} Hz: {out:+.2} dBm");
    }
    let gain = out - input;
    for level in [4.0, 10.0, 16.0, 20.0] {
        let (_, out, thd) = tone(1.0, 1000.0, level - gain);
        println!("  {out:+.2} dBm out: THD {thd:.3} %");
    }
    for hz in [100.0, 300.0, 1000.0, 3000.0, 6000.0, 12_000.0, 15_000.0] {
        println!(
            "  sidechain {hz:6.0} Hz: panel {:7.2} V peak flat, {:7.2} V with R37 up (50 mV in)",
            panel(hz, R37_FLAT, 0.05),
            panel(hz, R37_OHMS, 0.05)
        );
    }
}

/// What each circuit costs a second of audio (release; run by hand).
#[test]
#[ignore]
fn cost() {
    let mut acc = 0.0;
    for rate in [96_000.0, 48_000.0] {
        let ic = lev::input(600.0).unwrap();
        let c = node(&ic, "c");
        let b = node(&ic, "b");
        let mut input = Simulation::new(ic, rate);
        let mut amp = Simulation::new(lev::amplifier(600.0).unwrap(), rate);
        let mut side = Simulation::new(lev::sidechain().unwrap(), rate);
        let wiper = share(0.6);
        for s in [&mut input, &mut amp, &mut side] {
            s.set_early_acceptance(true);
            s.reset();
        }
        let n = rate as usize;
        let (mut ti, mut ta, mut ts) = (0.0, 0.0, 0.0);
        for i in 0..n {
            let x = emf(4.0) * (std::f64::consts::TAU * 220.0 * i as f64 / rate).sin();
            let t = std::time::Instant::now();
            input.set_realtime_value(CELL, 30_000.0 + 10_000.0 * (i as f64 * 1e-3).sin());
            input.process(x);
            let t1 = std::time::Instant::now();
            acc += amp.process(wiper * input.voltage_at(c));
            let t2 = std::time::Instant::now();
            acc += side.process(wiper * input.voltage_at(b));
            let t3 = std::time::Instant::now();
            ti += (t1 - t).as_secs_f64();
            ta += (t2 - t1).as_secs_f64();
            ts += (t3 - t2).as_secs_f64();
        }
        println!(
            "at {rate}: input {:.1}, amplifier {:.1}, sidechain {:.1} ms per second",
            ti * 1e3,
            ta * 1e3,
            ts * 1e3
        );
    }
    println!("({acc:.1})");
}

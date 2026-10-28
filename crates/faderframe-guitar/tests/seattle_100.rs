//! The Seattle 100 (Soldano SLO-100) against its 1996 drawings
//! (`circuits::seattle100`): the supply nodes sheet 1 prints, the power
//! stage's idle, the tone stack, Crunch and Bright, the channel switching,
//! and the loop's presence and depth.

use faderframe_guitar::circuits::{power, seattle100};
use faderframe_guitar::dsp::ac;
use faderframe_guitar::dsp::device::Pentode;
use faderframe_guitar::dsp::measure::{self, Tone};
use faderframe_guitar::dsp::netlist::{Circuit, PentodeSpec, GROUND};
use faderframe_guitar::dsp::time::Simulation;
use seattle100::Channel;

fn near(got: f64, chart: f64, within: f64) -> bool {
    (got / chart - 1.0).abs() <= within
}

fn at(c: &Circuit, n: &str) -> usize {
    c.unknown_named(n).unwrap_or_else(|| panic!("no node {n}"))
}

fn settled(c: Circuit) -> Simulation {
    let mut s = Simulation::new(c, 48_000.0);
    s.find_operating_point();
    s
}

/// The power stage at idle: the plates' and screens' nodes on sheet 1's 497 V
/// and 495 V, the 6L6GCs at `power::SEATTLE_BIAS`'s 37 mA, and the inverter
/// drawing from B2 what the preamplifier's chain charges it with
/// (`seattle100::INVERTER_IDLE`).
#[test]
fn the_power_stage_sits_on_the_drawing() {
    let spec = &power::PowerSpec::SEATTLE_6L6;
    let c = power::build(spec, 10_000.0).unwrap();
    let nodes = ["ht", "scr", "pi_b", "pi_k", "tail", "pi_p1", "pi_p2"];
    let idx: Vec<usize> = nodes.iter().map(|n| at(&c, n)).collect();
    let s = settled(c);
    let v: Vec<f64> = idx.iter().map(|&i| s.voltage_at(i)).collect();
    for (n, x) in nodes.iter().zip(&v) {
        println!("{n:<6} {x:8.2} V");
    }
    let valve = Pentode::new(0, 1, GROUND, 2, 1.0, PentodeSpec::T6L6GC);
    let (ia, is) = valve.currents(v[0], spec.bias, v[1]);
    println!(
        "a 6L6GC idles at {:.1} mA plate, {:.1} mA screen, {:.1} W",
        ia * 1e3,
        is * 1e3,
        ia * v[0]
    );
    let draw = (spec.pi_supply - v[5]) / spec.pi_plate_driven
        + (spec.pi_supply - v[6]) / spec.pi_plate_other;
    println!("the inverter draws {:.2} mA", draw * 1e3);
    assert!(near(v[0], 497.0, 0.01), "plates' node {}", v[0]);
    assert!(near(v[1], 495.0, 0.01), "screens' node {}", v[1]);
    assert!(near(ia * 1e3, 37.0, 0.05), "{ia}");
    assert!(near(spec.pi_supply / draw, seattle100::INVERTER_IDLE, 0.03));
}

/// The preamplifier's supply nodes as sheet 1 prints them: B2 378 V, B4
/// 359 V, B3 350 V. B3 comes out some 13 V high: the stages it feeds (V2a,
/// and V2b idling near cut-off on its 39 k) draw 1.2 mA here, where the
/// drawing's 28 V across R62's 15 k asks for 1.9 mA -- more than those two
/// valves can draw on those cathode resistors. The drawing's reading, not the
/// circuit, is the odd one out.
#[test]
fn the_preamp_sits_on_the_drawing() {
    let c = seattle100::build_channel(Channel::Overdrive, 10_000.0, 2e6).unwrap();
    let nodes = [
        ("b2", 378.0, 0.02),
        ("b4", 359.0, 0.02),
        ("b3", 350.0, 0.05),
    ];
    let idx: Vec<usize> = nodes.iter().map(|(n, ..)| at(&c, n)).collect();
    let probes = [
        "v1b_k", "v2a_k", "v2b_k", "v1a_k", "v3b_p", "v3a_k", "v4b_p", "cf",
    ];
    let pidx: Vec<usize> = probes.iter().map(|n| at(&c, n)).collect();
    let s = settled(c);
    for ((n, want, within), i) in nodes.iter().zip(idx) {
        let v = s.voltage_at(i);
        println!("{n:<6} {v:8.2} V (drawing {want})");
        assert!(near(v, *want, *within), "{n}: {v} against {want}");
    }
    let mut probed = Vec::new();
    for (n, i) in probes.iter().zip(pidx) {
        let v = s.voltage_at(i);
        println!("{n:<6} {v:8.2} V");
        probed.push(v);
    }
    // The cold clipper: 39 k unbypassed holds V2b near cut-off, a tenth of a
    // milliamp, where V2a on its bypassed 1.8 k runs at a milliamp.
    let (v2a, v2b) = (probed[1] / 1_800.0, probed[2] / 39_000.0);
    println!("V2a {:.2} mA, V2b {:.3} mA", v2a * 1e3, v2b * 1e3);
    assert!(v2b < 0.2e-3 && v2a > 0.8e-3, "{v2a} {v2b}");
}

fn gain_db(mut sim: Simulation, hz: f64, volts: f64) -> f64 {
    sim.find_operating_point();
    let t = Tone::near(96_000.0, 16_384, hz, volts);
    measure::run(t, 24_000, |x| sim.process(x)).gain_db()
}

/// The small-signal gain from J1 to `node`, with `set` (control, position)
/// and `values` (slot, value) applied.
fn preamp(ch: Channel, node: &str, set: &[(usize, f64)], values: &[(usize, f64)], hz: f64) -> f64 {
    let c = seattle100::tap_channel(ch, 10_000.0, 2e6, node).unwrap();
    let mut s = Simulation::new(c, 96_000.0);
    for &(k, v) in set {
        s.set_control(k, v);
    }
    for &(k, v) in values {
        s.set_value(k, v);
    }
    gain_db(s, hz, 1e-4)
}

/// The tone stack's response: the JCM800's but for R30's 47 k, its scoop at
/// the middle of the band, each control moving its own part of it.
#[test]
fn the_tone_stack_moves_its_bands() {
    use seattle100::{BASS, MIDDLE, TREBLE};
    let stack = seattle100::tone_stack(1_000.0, 1e6).unwrap();
    let db = |set: &[(usize, f64)], hz: f64| {
        let mut controls = [0.5; 7];
        for &(k, v) in set {
            controls[k] = v;
        }
        ac::solve(&stack, &controls, hz).db()
    };
    let flat: Vec<f64> = [100.0, 650.0, 3_000.0]
        .iter()
        .map(|&f| db(&[], f))
        .collect();
    println!(
        "noon: 100 Hz {:+.1}, 650 Hz {:+.1}, 3 kHz {:+.1} dB",
        flat[0], flat[1], flat[2]
    );
    assert!(
        flat[1] < flat[0] - 3.0 && flat[1] < flat[2] - 3.0,
        "{flat:?}"
    );
    let swing = |k: usize, hz: f64| db(&[(k, 1.0)], hz) - db(&[(k, 0.0)], hz);
    let (mid, treble, bass) = (
        swing(MIDDLE, 650.0),
        swing(TREBLE, 3_000.0),
        swing(BASS, 100.0),
    );
    println!(
        "middle {mid:+.1} dB at 650 Hz, treble {treble:+.1} at 3 kHz, bass {bass:+.1} at 100 Hz"
    );
    assert!(mid > 8.0 && treble > 8.0 && bass > 8.0);
    // Each mostly its own band.
    assert!(swing(TREBLE, 100.0) < treble / 2.0);
    assert!(swing(BASS, 3_000.0) < bass / 2.0);
}

/// S1: Crunch hands V1a about half the signal, Clean about 4 %: the
/// dividers' arithmetic (470 k into 500 k; 940 k into 39 k // 500 k) is
/// 22.9 dB, at a frequency where C5 and C8 do nothing.
#[test]
fn crunch_and_clean_are_the_dividers() {
    use seattle100::{CRUNCH, CRUNCH_SLOTS, NORMAL_VOLUME};
    let set = [(NORMAL_VOLUME, 1.0)];
    let off = [(seattle100::BRIGHT_SLOT, seattle100::BRIGHT_OFF_FARADS)];
    let with = |throw: [f64; 2]| {
        let values = [
            off[0],
            (CRUNCH_SLOTS[0], throw[0]),
            (CRUNCH_SLOTS[1], throw[1]),
        ];
        preamp(Channel::Normal, "v1a_g", &set, &values, 200.0)
    };
    let (clean, crunch) = (with(CRUNCH[0]), with(CRUNCH[1]));
    println!("to V1a's grid at 200 Hz: clean {clean:+.1} dB, crunch {crunch:+.1} dB");
    assert!((crunch - clean - 22.9).abs() < 1.0, "{}", crunch - clean);
}

/// S2: C5 across R10 lifts the top of what reaches V1a.
#[test]
fn bright_lifts_the_top() {
    use seattle100::{BRIGHT_FARADS, BRIGHT_OFF_FARADS, BRIGHT_SLOT, CRUNCH, CRUNCH_SLOTS};
    let tilt = |throw: [f64; 2], bright: f64| {
        let values = [
            (BRIGHT_SLOT, bright),
            (CRUNCH_SLOTS[0], throw[0]),
            (CRUNCH_SLOTS[1], throw[1]),
        ];
        let set = [(seattle100::NORMAL_VOLUME, 0.7)];
        preamp(Channel::Normal, "v1a_g", &set, &values, 5_000.0)
            - preamp(Channel::Normal, "v1a_g", &set, &values, 200.0)
    };
    let clean = tilt(CRUNCH[0], BRIGHT_FARADS) - tilt(CRUNCH[0], BRIGHT_OFF_FARADS);
    let crunch = tilt(CRUNCH[1], BRIGHT_FARADS) - tilt(CRUNCH[1], BRIGHT_OFF_FARADS);
    println!("bright adds {clean:+.1} dB at 5 kHz in clean, {crunch:+.1} dB in crunch");
    // Shorting R10 at the top: in Clean the series arm halves (940 k to
    // 470 k against 36 k), in Crunch it goes (470 k against 500 k) -- about
    // 5.7 dB either way.
    assert!((clean - 5.7).abs() < 1.0 && (crunch - 5.7).abs() < 1.0);
}

/// The photo-resistors: the overdrive channel reaches V3b only in its own
/// mode (VR1 shunts V2b's grid in the other, VR2 lifts its plate off M), and
/// each mode's master is the one that reaches the inverter.
#[test]
fn the_photo_resistors_switch_the_channels() {
    use seattle100::{NORMAL_MASTER, OD_MASTER, OD_VOLUME};
    let od = |ch: Channel, node: &str, set: &[(usize, f64)]| preamp(ch, node, set, &[], 1_000.0);
    let shunted = od(Channel::Overdrive, "v2b_g", &[]) - od(Channel::Normal, "v2b_g", &[]);
    println!("V2b's grid: {shunted:+.1} dB more in overdrive mode");
    assert!(shunted > 50.0);
    // A master turned to nothing silences its own mode and not the other.
    let quiet = [(OD_MASTER, 0.0)];
    let od_q = od(Channel::Overdrive, "q", &quiet) - od(Channel::Overdrive, "q", &[]);
    let n_q = od(Channel::Normal, "q", &quiet) - od(Channel::Normal, "q", &[]);
    println!("OD master down: {od_q:+.1} dB in overdrive mode, {n_q:+.1} dB in normal");
    assert!(od_q < -40.0 && n_q.abs() < 0.5);
    let quiet = [(NORMAL_MASTER, 0.0)];
    let n_q = od(Channel::Normal, "q", &quiet) - od(Channel::Normal, "q", &[]);
    assert!(n_q < -40.0, "{n_q}");
    // The overdrive channel's own volume reaches M only in its own mode.
    let down = [(OD_VOLUME, 0.0)];
    let in_od = od(Channel::Overdrive, "m", &[]) - od(Channel::Overdrive, "m", &down);
    let in_normal = od(Channel::Normal, "m", &[]) - od(Channel::Normal, "m", &down);
    println!("OD volume at M: {in_od:+.1} dB in overdrive mode, {in_normal:+.1} dB in normal");
    assert!(in_od > 20.0 && in_normal.abs() < 1.0);
}

/// The loop: presence up takes the top out of the feedback and so puts it
/// back in the output; depth up takes the bottom out, the same way.
#[test]
fn presence_and_depth_work_on_the_loop() {
    let spec = &power::PowerSpec::SEATTLE_6L6;
    let g = |presence: f64, depth: f64, hz: f64| {
        let c = power::build(spec, 10_000.0).unwrap();
        let mut s = Simulation::new(c, 96_000.0);
        s.set_control(power::PRESENCE, presence);
        s.set_control(power::RESONANCE, depth);
        gain_db(s, hz, 0.05)
    };
    let presence =
        (g(1.0, 0.0, 5_000.0) - g(1.0, 0.0, 500.0)) - (g(0.0, 0.0, 5_000.0) - g(0.0, 0.0, 500.0));
    let depth =
        |hz: f64| (g(0.5, 1.0, hz) - g(0.5, 1.0, 500.0)) - (g(0.5, 0.0, hz) - g(0.5, 0.0, 500.0));
    let (d50, d80, d150) = (depth(50.0), depth(80.0), depth(150.0));
    println!(
        "presence lifts 5 kHz by {presence:+.1} dB; depth lifts 50 Hz by {d50:+.1}, 80 Hz by {d80:+.1}, 150 Hz by {d150:+.1} dB"
    );
    assert!(presence > 3.0, "{presence}");
    // Not most at the very bottom: the 0.1 uF in series has already taken
    // the loop's lowest octave away with the rheostat out.
    assert!(d50 > 0.5 && d80 > 1.5 && d150 > 1.5, "{d50} {d80} {d150}");
    assert_eq!(spec.presence_name(), Some("PRESENCE"));
}

/// Printed: a 6L6GC's idle on the drawing's plate and screen voltages
/// across the bias, for `power::SEATTLE_BIAS`.
#[test]
#[ignore]
fn print_the_bias() {
    let valve = Pentode::new(0, 1, GROUND, 2, 1.0, PentodeSpec::T6L6GC);
    for tenth in (400..=500).step_by(5) {
        let bias = -(tenth as f64) / 10.0;
        let (ia, _) = valve.currents(497.0, bias, 495.0);
        println!("{bias:6.1} V: {:5.1} mA, {:4.1} W", ia * 1e3, ia * 497.0);
    }
}

/// Printed: how hard the power stage is driven across the master, with a
/// guitar's chord at the calibration level into each channel: the peak volts
/// at the 4 ohm tap (100 W is 28.3 V) and at the inverter's grid.
#[test]
#[ignore]
fn print_the_power_drive() {
    use faderframe_guitar::voice::{self, Amplifier, Diode, Gain, Level};
    for gain in [Gain::Seattle100, Gain::Seattle100Normal] {
        let Some(Level::Circuit(master)) = gain.level_control() else {
            panic!("{gain:?} has its master in its preamplifier")
        };
        for m in [0.1, 0.2, 0.3, 0.4, 0.5, 0.7, 1.0] {
            let mut pre = Simulation::new(
                voice::build_voice(gain, Diode::Silicon, Amplifier::Valve).unwrap(),
                96_000.0,
            );
            pre.set_control(gain.drive_control(), 0.5);
            pre.set_control(master, m);
            let c = power::build(&power::PowerSpec::SEATTLE_6L6, 10_000.0).unwrap();
            let grid = at(&c, "pi_a");
            let mut pw = Simulation::new(c, 96_000.0);
            let (mut peak, mut sum, mut lo, mut hi) = (0.0f64, 0.0, f64::MAX, f64::MIN);
            for k in 0..48_000 {
                let t = k as f64 / 96_000.0;
                let x = 0.122
                    * (0.5 * (std::f64::consts::TAU * 82.4 * t).sin()
                        + 0.3 * (std::f64::consts::TAU * 123.5 * t).sin()
                        + 0.2 * (std::f64::consts::TAU * 246.9 * t).sin());
                let y = pw.process(pre.process(x));
                if k > 24_000 {
                    peak = peak.max(y.abs());
                    sum += y * y;
                    lo = lo.min(pw.voltage_at(grid));
                    hi = hi.max(pw.voltage_at(grid));
                }
            }
            let watts = sum / 24_000.0 / 4.0;
            println!(
                "{gain:?} master {m}: {peak:5.1} V peak, {watts:5.1} W at the tap, grid swing {:5.2} V",
                hi - lo
            );
        }
    }
}

/// Printed: the small-signal gain to a few nodes across the channels.
#[test]
#[ignore]
fn print_the_preamp() {
    for ch in [Channel::Overdrive, Channel::Normal] {
        for node in ["v1b_p", "v2a_p", "v2b_p", "m", "v3b_p", "cf", "t_out", "q"] {
            println!(
                "{ch:?} {node:<6} 1 kHz {:+.1} dB",
                preamp(ch, node, &[], &[], 1_000.0)
            );
        }
    }
}

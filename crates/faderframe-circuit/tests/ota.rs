//! The CA3080 against its data sheet (Intersil FN475.5, April 2001), as a
//! device and inside the solver.
//!
//! Figures are the sheet's at 25 degrees C on +-15 V: the electrical table and
//! Figures 13 (the propagation-delay circuit, whose 56 k sets 500 uA), 16
//! (input bias current), 17 (peak output current), 18 (peak output voltage),
//! 19 (amplifier supply current), 21 (transconductance) and 27 (amplifier bias
//! voltage).

use faderframe_circuit::dsp::device::{Device, Ota, Stamper};
use faderframe_circuit::dsp::netlist::{Netlist, OtaSpec};
use faderframe_circuit::dsp::time::Simulation;

const VT: f64 = 0.025_852;
const SPEC: OtaSpec = OtaSpec::CA3080;

/// Terminals 0..6: non-inverting, inverting, output, bias, V+, V-.
fn part() -> Ota {
    Ota::new(0, 1, 2, 3, 4, 5, SPEC)
}

/// The bias pin voltage above V- that passes `amps`.
fn bias_for(amps: f64) -> f64 {
    SPEC.bias_emission * VT * (amps / SPEC.bias_saturation + 1.0).ln()
}

/// Comfortably inside the output's swing either way.
const ROOM: f64 = 10.0;

#[test]
fn the_bias_pin_sits_where_figure_27_puts_it() {
    // 490 mV at 0.1 uA, about 610 at 10 uA, 710 typical at 500 uA (the
    // table), 730 at 1 mA; read off the plot to +-10 mV.
    for (amps, volts, within) in [
        (0.1e-6, 0.490, 0.012),
        (10e-6, 0.610, 0.012),
        (500e-6, 0.710, 0.005),
        (1e-3, 0.730, 0.012),
    ] {
        let v = bias_for(amps);
        assert!(
            (v - volts).abs() < within,
            "{amps} A: {v:.3} V against {volts} V"
        );
        // And the device's own current there is that current.
        let c = part().currents(v, 0.0, ROOM, ROOM);
        // (Less the teraohm of `GMIN` every junction carries.)
        let diode = c.bias - 1e-12 * v;
        assert!(((diode - amps) / amps).abs() < 1e-9, "{amps}: {diode}");
    }
}

#[test]
fn the_transconductance_follows_the_bias_current_as_figure_21() {
    // gm = I_ABC / 2 VT: 19.2 uS per uA, three decades straight.
    let ota = part();
    let step = 1e-6;
    for amps in [1e-6, 10e-6, 100e-6, 500e-6, 1e-3] {
        let v = bias_for(amps);
        let up = ota.currents(v, step, ROOM, ROOM).output();
        let down = ota.currents(v, -step, ROOM, ROOM).output();
        let gm = (up - down) / (2.0 * step);
        let expected = amps / (2.0 * VT);
        assert!(((gm - expected) / expected).abs() < 1e-4, "{amps}: {gm}");
        assert!((Ota::transconductance(amps) - expected).abs() < 1e-15);
    }
    // The table: 9600 uS typical at 500 uA, 6700 to 13000 across parts.
    let gm = Ota::transconductance(500e-6);
    assert!((gm - 9.6e-3).abs() < 0.1e-3, "{gm}");
}

#[test]
fn the_transfer_is_the_pairs_tanh_and_peaks_at_the_bias_current() {
    let ota = part();
    let amps = 500e-6;
    let v = bias_for(amps);
    for mv in [
        -200.0, -100.0, -52.0, -26.0, -10.0, 0.0, 10.0, 26.0, 52.0, 100.0, 200.0,
    ] {
        let vd = mv * 1e-3;
        let out = ota.currents(v, vd, ROOM, ROOM).output();
        let expected = amps * (vd / (2.0 * VT)).tanh();
        assert!((out - expected).abs() < 1e-9 * amps.max(1.0), "{mv} mV");
    }
    // Figure 17: the peak output current is the bias current, either way.
    for sign in [1.0, -1.0] {
        let out = ota.currents(v, sign * 0.4, ROOM, ROOM).output();
        assert!((out.abs() - amps).abs() < 1e-3 * amps, "{out}");
        assert_eq!(out.signum(), sign);
    }
}

#[test]
fn the_inputs_draw_the_sheets_bias_current_and_resistance() {
    let ota = part();
    // Table: 2 uA typical at 500 uA (5 uA maximum), and Figure 16's line,
    // a 250th of the bias current, at the other decades.
    for amps in [1e-6, 10e-6, 100e-6, 500e-6] {
        let c = ota.currents(bias_for(amps), 0.0, ROOM, ROOM);
        let each = 0.5 * (c.plus + c.minus);
        assert!(
            ((each - amps / 250.0) / each).abs() < 0.01,
            "{amps}: {each}"
        );
    }
    let c = ota.currents(bias_for(500e-6), 0.0, ROOM, ROOM);
    assert!((c.plus - 2e-6).abs() < 0.05e-6, "{}", c.plus);
    // The differential input resistance: 26 k typical, 10 k minimum.
    let v = bias_for(500e-6);
    let step = 1e-6;
    let into = |vd| ota.currents(v, vd, ROOM, ROOM).plus;
    let r = 2.0 * step / (into(step) - into(-step));
    assert!((r - 26e3).abs() < 1e3, "{r} ohm");
}

#[test]
fn the_supply_carries_twice_the_bias_current() {
    // Table: 1 mA (0.8-1.2) amplifier supply current at 500 uA; Figure 19
    // the same line at every decade.
    let ota = part();
    for amps in [1e-6, 100e-6, 500e-6] {
        let c = ota.currents(bias_for(amps), 0.0, ROOM, ROOM);
        let from_v_plus = c.supply + c.source;
        assert!(((from_v_plus - 2.0 * amps) / amps).abs() < 1e-9, "{amps}");
        // And nothing is lost: what comes in at V+, the bias pin and the
        // inputs leaves at V- and the output.
        let into = c.bias + c.plus + c.minus + c.supply + c.source;
        let out = c.bias + c.plus + c.minus + c.supply + c.sink + c.output();
        assert!((into - out).abs() < 1e-18);
    }
}

#[test]
fn the_output_gives_its_current_up_at_the_supply_pins() {
    let ota = part();
    let v = bias_for(500e-6);
    // Sourcing: all of it a volt inside the headroom, none at the edge, and
    // backwards beyond it; the same for sinking at the bottom.
    let at = |top: f64, bottom: f64, vd: f64| ota.currents(v, vd, top, bottom).output();
    let full = at(ROOM, ROOM, 0.3);
    assert!((at(SPEC.top_headroom + 1.0, ROOM, 0.3) / full - 1.0).abs() < 1e-4);
    assert!(at(SPEC.top_headroom, ROOM, 0.3).abs() < 1e-3 * full);
    assert!(at(SPEC.top_headroom - 0.1, ROOM, 0.3) < 0.0);
    let full = at(ROOM, ROOM, -0.3);
    assert!((at(ROOM, SPEC.bottom_headroom + 1.0, -0.3) / full - 1.0).abs() < 1e-4);
    assert!(at(ROOM, SPEC.bottom_headroom, -0.3).abs() < 1e-3 * full.abs());
    assert!(at(ROOM, SPEC.bottom_headroom - 0.1, -0.3) > 0.0);
}

/// Every node's current as the stamp's linear model has it: `M v - rhs`.
fn linear_model(ota: &mut Ota, at: &[f64; 6], probe: &[f64; 6]) -> [f64; 6] {
    let mut matrix = [0.0; 36];
    let mut rhs = [0.0; 6];
    let mut s = Stamper {
        matrix: &mut matrix,
        rhs: &mut rhs,
        n: 6,
        map: None,
        mapping_failed: false,
        limiting: false,
        junction_held: false,
    };
    ota.stamp(&mut s, at);
    assert!(!s.junction_held);
    let mut out = [0.0; 6];
    for (r, o) in out.iter_mut().enumerate() {
        *o = (0..6).map(|c| matrix[r * 6 + c] * probe[c]).sum::<f64>() - rhs[r];
    }
    out
}

/// Every node's current leaving it into the part, from the curves.
fn exact(ota: &Ota, v: &[f64; 6]) -> [f64; 6] {
    let c = ota.currents(v[3] - v[5], v[0] - v[1], v[4] - v[2], v[2] - v[5]);
    [
        c.plus,
        c.minus,
        c.sink - c.source,
        c.bias,
        c.supply + c.source,
        -(c.bias + c.plus + c.minus + c.supply + c.sink),
    ]
}

#[test]
fn the_stamp_is_the_tangent_of_the_currents() {
    // Points on the pair's linear stretch (where the slope is the tangent),
    // in and out of the output's compliance at either pin.
    let points = [
        [4.60, 4.59, 6.0, 0.62, 9.0, 0.0],
        [4.60, 4.62, 2.0, 0.66, 9.0, 0.0],
        [4.60, 4.59, 7.80, 0.66, 9.0, 0.0],
        [4.60, 4.61, 0.50, 0.64, 9.0, 0.0],
        [0.01, -0.02, 13.9, -14.30, 15.0, -15.0],
    ];
    for at in points {
        // Settle the limiter where the point is, so the stamp is at it.
        let mut ota = part();
        for _ in 0..40 {
            linear_model_unchecked(&mut ota, &at);
        }
        let here = linear_model(&mut ota, &at, &at);
        let truth = exact(&ota, &at);
        for k in 0..6 {
            assert!(
                (here[k] - truth[k]).abs() < 1e-12,
                "{at:?} node {k}: {} against {}",
                here[k],
                truth[k]
            );
        }
        // A small step away: the linear model and the curves agree to
        // second order.
        for (k, dv) in [
            (0, 1e-5),
            (1, -1e-5),
            (2, 1e-4),
            (3, 1e-5),
            (4, 1e-4),
            (5, -1e-4),
        ] {
            let mut probe = at;
            probe[k] += dv;
            let model = linear_model(&mut ota, &at, &probe);
            let truth = exact(&ota, &probe);
            for n in 0..6 {
                let scale = truth[n].abs().max(1e-9);
                assert!(
                    (model[n] - truth[n]).abs() < 1e-3 * scale,
                    "{at:?} moving {k}: node {n} {} against {}",
                    model[n],
                    truth[n]
                );
            }
        }
    }
}

fn linear_model_unchecked(ota: &mut Ota, at: &[f64; 6]) {
    let mut matrix = [0.0; 36];
    let mut rhs = [0.0; 6];
    let mut s = Stamper {
        matrix: &mut matrix,
        rhs: &mut rhs,
        n: 6,
        map: None,
        mapping_failed: false,
        limiting: false,
        junction_held: false,
    };
    ota.stamp(&mut s, at);
}

/// Figure 13's circuit on +-15 V: 56 k from V+ to the bias pin (the sheet's
/// 500 uA), the inverting input grounded, the non-inverting one driven through
/// 51 ohm, and an output load of `load`.
fn figure_13(drive: f64, load: f64) -> (Simulation, [usize; 5]) {
    let mut net = Netlist::new("CA3080 figure 13");
    net.supply("vp", 1.0, 15.0)
        .supply("vn", 1.0, -15.0)
        .resistor("vp", "abc", 56_000.0)
        .input_at("in", 51.0, drive)
        .resistor("in", "gnd", 1e6)
        .ota("in", "gnd", "out", "abc", "vp", "vn", SPEC)
        .resistor("out", "gnd", load);
    let circuit = net.build("out").unwrap();
    let nodes = [
        circuit.unknown_named("abc").unwrap(),
        circuit.unknown_named("in").unwrap(),
        circuit.unknown_named("out").unwrap(),
        circuit.unknown_named("vp").unwrap(),
        circuit.unknown_named("vn").unwrap(),
    ];
    let mut sim = Simulation::new(circuit, 48_000.0);
    sim.watch_structure();
    assert!(sim.find_operating_point());
    (sim, nodes)
}

#[test]
fn figure_13_sets_half_a_milliamp_and_the_output_follows_the_tanh() {
    for drive in [-0.1, -0.05, -0.02, 0.0, 0.02, 0.05, 0.1] {
        let (mut sim, [abc, input, out, vp, vn]) = figure_13(drive, 1_000.0);
        // Run a little, so the solve has been through the sample path too.
        for _ in 0..64 {
            sim.process(0.0);
        }
        let pin = sim.voltage_at(abc);
        // The supplies sag by the part's own current through their ohm.
        let (high, low) = (sim.voltage_at(vp), sim.voltage_at(vn));
        let i_abc = (high - pin) / 56_000.0;
        // (15 + 15 - 0.71) / 56 k: the sheet's 500 uA, as it rounds it.
        assert!((i_abc - 523e-6).abs() < 2e-6, "{i_abc}");
        assert!((pin - low - bias_for(i_abc)).abs() < 1e-6);
        let vd = sim.voltage_at(input);
        let i_out = sim.voltage_at(out) / 1_000.0;
        let expected = i_abc * (vd / (2.0 * VT)).tanh();
        assert!(
            (i_out - expected).abs() < 1e-3 * i_abc,
            "{drive} V: {i_out} A against {expected} A"
        );
        assert_eq!(sim.violations(), 0, "stamped outside its footprint");
    }
}

#[test]
fn figure_18_the_unloaded_output_stops_short_of_each_supply() {
    // No load to speak of: the output runs to the swing limits, 13.5 to
    // 14.1 V up and -14.4 to -14.5 V down.
    let (mut sim, [_, _, out, _, _]) = figure_13(0.2, 1e9);
    for _ in 0..64 {
        sim.process(0.0);
    }
    let high = sim.voltage_at(out);
    assert!((13.4..14.1).contains(&high), "{high} V");
    let (mut sim, [_, _, out, _, _]) = figure_13(-0.2, 1e9);
    for _ in 0..64 {
        sim.process(0.0);
    }
    let low = sim.voltage_at(out);
    assert!((-14.55..-14.3).contains(&low), "{low} V");
    assert_eq!(sim.violations(), 0);
}

#[test]
fn a_gated_ota_passes_nothing() {
    // The bias pin open (tied to V- through a resistor): the part is off,
    // and a volt at the input brings out nothing but leakage.
    let mut net = Netlist::new("CA3080 gated");
    net.supply("vp", 1.0, 15.0)
        .supply("vn", 1.0, -15.0)
        .resistor("abc", "vn", 1e6)
        .input("in", 51.0)
        .resistor("in", "gnd", 1e6)
        .ota("in", "gnd", "out", "abc", "vp", "vn", SPEC)
        .resistor("out", "gnd", 10_000.0);
    let mut sim = Simulation::new(net.build("out").unwrap(), 48_000.0);
    assert!(sim.find_operating_point());
    let mut most = 0.0f64;
    for k in 0..480 {
        let x = (std::f64::consts::TAU * 1000.0 * k as f64 / 48_000.0).sin();
        most = most.max(sim.process(x).abs());
    }
    assert!(most < 1e-9, "{most} V");
}

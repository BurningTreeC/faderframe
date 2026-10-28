//! The Red Squeeze against what is published about the pedal it is drawn
//! from: a built one's bias voltages, the DAFx-11 paper's measurements of a real
//! Dyna Comp (its Figure 6 static curves and Figure 5 step), and MXR's own
//! figures for the M102 (maximum compression 36 dB, attack 5 ms, release 1 s).

use super::*;
use crate::circuits::dynamics::{pluck_gain, steady, step, sustain, SR};
use crate::dsp::time::Simulation;

fn sim(values: &Values, at: &str, sensitivity: f64, output: f64) -> (Simulation, Circuit) {
    let circuit = tap(values, 10_000.0, 470_000.0, at).unwrap();
    let mut s = Simulation::new(circuit.clone(), SR);
    s.set_control(SENSITIVITY, sensitivity);
    s.set_control(OUTPUT, output);
    s.set_control(TRIM, TRIM_REST);
    assert!(s.find_operating_point());
    (s, circuit)
}

/// Mad Bean's Ambulator, a Dyna Comp on 2N3904s, measured with its knobs at
/// half on a 9.44 V supply (9.26 V at the circuit, past its polarity diode):
/// its build notes' table of every pin. The supply scaled to match.
#[test]
fn the_operating_point_is_a_real_ones() {
    let circuit = tap(&DYNA, 10_000.0, 470_000.0, OUTPUT_NODE).unwrap();
    let mut s = Simulation::new(circuit.clone(), SR);
    s.set_supply_scale(9.26 / 9.0);
    s.set_control(SENSITIVITY, 0.5);
    s.set_control(OUTPUT, 0.5);
    s.set_control(TRIM, TRIM_REST);
    assert!(s.find_operating_point());
    let at = |name: &str| s.voltage_at(circuit.unknown_named(name).unwrap());
    for (name, part, measured, within) in [
        ("inv", "IC1 pin 2", 4.55, 0.15),
        ("noninv", "IC1 pin 3", 4.55, 0.15),
        ("abc", "IC1 pin 5", 0.66, 0.03),
        ("ota", "IC1 pin 6", 2.42, 0.1),
        ("q1b", "Q1 base", 1.79, 0.1),
        ("q1e", "Q1 emitter", 1.27, 0.1),
        ("q2c", "Q2 collector", 7.44, 0.15),
        ("q2e", "Q2 emitter", 1.83, 0.1),
        (ENVELOPE, "Q3, Q4 collectors", 9.11, 0.15),
        ("q5e", "Q5 emitter", 8.64, 0.2),
        ("q3b", "Q3 base", 0.016, 0.03),
        ("q4b", "Q4 base", 0.007, 0.03),
    ] {
        let v = at(name);
        assert!(
            (v - measured).abs() < within,
            "{part}: {v:.3} V, measured {measured} V"
        );
    }
    // With the trimmer centred the two inputs sit together.
    assert!((at("inv") - at("noninv")).abs() < 1e-3);
}

/// Output against input, 440 Hz, Output fully up: the DAFx-11 paper's
/// Figure 6, measured on a real Dyna Comp at both ends of Sensitivity.
#[test]
fn the_static_curve_is_the_measured_pedals() {
    let curve = |sensitivity: f64| -> Vec<(f64, f64)> {
        [-60.0, -50.0, -40.0, -30.0, -20.0, -10.0]
            .into_iter()
            .map(|level| {
                let (mut s, _) = sim(&DYNA, OUTPUT_NODE, sensitivity, 1.0);
                (level, steady(&mut s, level, 440.0, 2.5))
            })
            .collect()
    };
    let most = curve(1.0);
    let least = curve(0.0);
    eprintln!("Sensitivity full: {most:.1?}\nSensitivity none: {least:.1?}");
    // Full: about 32 dB of gain on a quiet note in the paper; MXR gives the
    // M102 36 dB of maximum compression.
    let gain = most[0].1 - most[0].0;
    assert!((30.0..38.0).contains(&gain), "{gain:.1} dB");
    // Above the knee (-50 dBV in the paper) the output stops where the
    // rectifiers conduct, -11 dBV in the paper: an infinite ratio, the
    // output within two decibels across 30 dB of input.
    let held: Vec<f64> = most[2..].iter().map(|p| p.1).collect();
    let (lo, hi) = held
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    assert!(hi - lo < 2.0, "{held:.1?}");
    assert!((-14.0..-10.0).contains(&hi), "ceiling {hi:.1} dBV");
    // None: unity's neighbourhood, linear to well above a guitar's quiet
    // notes (one decibel per decibel from -50 to -30 dBV), and the same
    // ceiling once it does compress.
    let slope = (least[3].1 - least[1].1) / 20.0;
    assert!((slope - 1.0).abs() < 0.05, "slope {slope:.2}");
    assert!(
        (least[5].1 - hi).abs() < 1.5,
        "{:.1} against {hi:.1}",
        least[5].1
    );
    assert!(least[4].1 - least[4].0 < least[3].1 - least[3].0 - 2.0);
}

/// A 10 mV to 100 mV step and back, Sensitivity and Output fully up: the
/// DAFx-11 paper's Figure 5, and MXR's attack and release.
#[test]
fn it_squashes_a_step_and_recovers_in_a_second() {
    let (mut s, _) = sim(&DYNA, OUTPUT_NODE, 1.0, 1.0);
    let r = step(&mut s, 0.01, 0.1);
    eprintln!("{r:#?}");
    // The paper: the same level either side of the step (0.32 V peak),
    // a spike at the step about twice that, gone in a few milliseconds.
    assert!((r.loud / r.before - 1.0).abs() < 0.15, "{r:?}");
    assert!(r.overshoot > 1.5 * r.loud, "no squash: {r:?}");
    // MXR: attack 5 ms (here the overshoot's time constant) and release
    // 1 s (here the gain back within a decibel). The paper's envelope has
    // recovered two thirds of its decibels about a third of a second after
    // the step down, and all but a decibel of them after about a second.
    assert!((0.002..0.010).contains(&r.attack), "attack {:.4} s", r.attack);
    assert!(r.settled < 0.025, "settled {:.4} s", r.settled);
    assert!(
        (0.2..0.5).contains(&r.release),
        "release {:.3} s",
        r.release
    );
    assert!(
        (0.6..1.6).contains(&r.recovered),
        "recovered {:.3} s",
        r.recovered
    );
    assert!((r.after / r.before - 1.0).abs() < 0.1);
}

/// A string left ringing: the input falls 25 dB in a second, the output a
/// fraction of that -- the sustain the pedal is bought for.
#[test]
fn a_ringing_string_sustains() {
    for (sensitivity, most) in [(0.5, 0.45), (1.0, 0.3)] {
        let (mut s, _) = sim(&DYNA, OUTPUT_NODE, sensitivity, 1.0);
        let (output, input) = sustain(&mut s, 0.3, 0.35);
        eprintln!("Sensitivity {sensitivity}: in fell {input:.1} dB, out {output:.1} dB");
        assert!(
            output < most * input && output > 0.0,
            "{output:.1} of {input:.1} dB"
        );
    }
}

/// Nothing in, nothing out -- at the most gain the pedal has, which is
/// where a solver's residue would be heard if there were any.
#[test]
fn it_is_silent_with_nothing_to_compress() {
    let (mut s, _) = sim(&DYNA, OUTPUT_NODE, 1.0, 1.0);
    let mut most = 0.0f64;
    for k in 0..SR as usize {
        let y = s.process(0.0);
        if k > 4800 {
            most = most.max(y.abs());
        }
    }
    assert!(most < 1e-6, "{most} V");
}

/// The level knob's middle is unity at a guitar's level with Sensitivity at
/// noon, as every pedal's on the line is.
#[test]
fn the_output_rest_is_unity_through_the_pedal() {
    let (mut s, _) = sim(&DYNA, OUTPUT_NODE, 0.5, OUTPUT_REST);
    let gain = pluck_gain(&mut s);
    assert!(gain.abs() < 0.5, "{gain:.2} dB at OUTPUT_REST");
}

/// Where OUTPUT_REST comes from. `--ignored --nocapture`.
#[test]
#[ignore]
fn calibrate_the_output_rest() {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..14 {
        let mid: f64 = 0.5 * (lo + hi);
        let (mut s, _) = sim(&DYNA, OUTPUT_NODE, 0.5, mid);
        if pluck_gain(&mut s) > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    eprintln!("OUTPUT_REST = {:.3}", 0.5 * (lo + hi));
}

/// The Ross: the same compressor, a little more of it. Its decoupled bias
/// feed takes away the Dyna's feedback from the bias line into Q1's base
/// (3 dB of gain at full Sensitivity), and its 100 k Level leaves a
/// decibel more at the top of its travel -- both places the Dyna's own knobs
/// reach. The ratio, the attack, the squash and the release are the same.
#[test]
fn the_ross_is_the_dyna_within_its_own_knobs() {
    let curve = |values: &Values, sensitivity: f64| -> Vec<f64> {
        [-70.0, -60.0, -40.0, -20.0]
            .into_iter()
            .map(|level| {
                let (mut s, _) = sim(values, OUTPUT_NODE, sensitivity, 1.0);
                steady(&mut s, level, 440.0, 2.5)
            })
            .collect()
    };
    let (dyna, ross) = (curve(&DYNA, 1.0), curve(&ROSS, 1.0));
    eprintln!("Dyna {dyna:.1?}\nRoss {ross:.1?}");
    // More gain below the knee, by what the Dyna's feedback costs it ...
    let more = ross[0] - dyna[0];
    assert!((2.5..4.5).contains(&more), "{more:.1} dB");
    // ... the same ceiling, less what the larger pot leaves ...
    let ceiling = ross[3] - dyna[3];
    assert!((0.5..1.5).contains(&ceiling), "{ceiling:.1} dB");
    // ... and the same flat top: the ratio is the rectifiers', in both.
    assert!(((ross[3] - ross[2]) - (dyna[3] - dyna[2])).abs() < 0.3);
    // And the same dynamics.
    let (mut a, _) = sim(&DYNA, OUTPUT_NODE, 1.0, 1.0);
    let (mut b, _) = sim(&ROSS, OUTPUT_NODE, 1.0, 1.0);
    let (a, b) = (step(&mut a, 0.01, 0.1), step(&mut b, 0.01, 0.1));
    eprintln!("{a:?}\n{b:?}");
    assert!((a.attack - b.attack).abs() <= 0.002, "{a:?} {b:?}");
    assert!((a.settled - b.settled).abs() <= 0.004, "{a:?} {b:?}");
    assert!((a.release / b.release - 1.0).abs() < 0.1, "{a:?} {b:?}");
    assert!(
        (a.recovered / b.recovered - 1.0).abs() < 0.15,
        "{a:?} {b:?}"
    );
    let squash = |r: &crate::circuits::dynamics::Step| r.overshoot / r.loud;
    assert!((squash(&a) / squash(&b) - 1.0).abs() < 0.2, "{a:?} {b:?}");
}

/// Every write the OTA and the transistors make is inside what they
/// declared, through the loudest passage the pedal will see.
#[test]
fn the_devices_keep_to_their_footprints() {
    let (mut s, _) = sim(&DYNA, OUTPUT_NODE, 1.0, 1.0);
    s.watch_structure();
    for k in 0..(0.5 * SR) as usize {
        let t = k as f64 / SR;
        s.process(1.5 * (std::f64::consts::TAU * 82.4 * t).sin() * (-3.0 * t).exp());
    }
    assert_eq!(s.violations(), 0);
}

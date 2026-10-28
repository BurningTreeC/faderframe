//! The Juicer against its drawing and its bias procedure. No published
//! measurement of an Orange Squeezer's curves or times was found; its times
//! are held to the parts that set them (R11 and C7 for the attack, R12 and C7
//! for the release), its levels to GGG's measured bias voltages.

use super::*;
use crate::circuits::dynamics::{pluck_gain, steady, step, sustain, SR};
use crate::dsp::time::Simulation;

fn sim(at: &str, bias: f64, volume: f64) -> (Simulation, Circuit) {
    let circuit = tap(10_000.0, 470_000.0, at).unwrap();
    let mut s = Simulation::new(circuit.clone(), SR);
    s.set_control(BIAS, bias);
    s.set_control(VOLUME, volume);
    assert!(s.find_operating_point());
    (s, circuit)
}

#[test]
fn the_bias_procedure_is_the_knobs_noon() {
    let (s, c) = sim(OUTPUT, 0.5, 1.0);
    let at = |name: &str| s.voltage_at(c.unknown_named(name).unwrap());
    // Q1: the square law with its gate on the far end of R5.
    let i = (at("q1s") - at(BIAS_POINT)) / 2_400.0;
    let spec = JfetSpec::J2N5457;
    let law = spec.idss * (1.0 - i * 2_400.0 / -spec.pinch_off).powi(2);
    assert!((i / law - 1.0).abs() < 0.01, "{i} A against {law} A");
    // Q2 at its pinch-off with nothing to squeeze: the procedure's point.
    let vgs = at("g") - at(BIAS_POINT);
    assert!((vgs - spec.pinch_off).abs() < 0.02, "Vgs {vgs:.3} V");
    // The op-amp sits where R4 and R8 put it: 4.92 V of 9 (GGG: 4.7 to
    // 4.8 V of 8.8).
    assert!((at("o1") - 4.92).abs() < 0.03, "{}", at("o1"));
    assert!(at(DETECTOR).abs() < 1e-3 && at("o").abs() < 1e-3);
}

/// GGG: "counter-clockwise until the sound dies ... turning too far will
/// lead to distortion". The knob keeps to the part between: turned down it
/// squeezes hardest and still plays, turned up the op-amp clips.
#[test]
fn turned_down_it_squeezes_hardest_turned_up_it_distorts() {
    let level = |bias: f64, input: f64| {
        let (mut s, _) = sim(OUTPUT, bias, 1.0);
        steady(&mut s, input, 440.0, 2.0)
    };
    let noon = level(0.5, -30.0);
    let down = level(0.0, -30.0);
    assert!(
        (15.0..22.0).contains(&(noon - down)),
        "{noon:.1} against {down:.1} dBV"
    );
    // Fully up Q2 is held off: a hot note is the op-amp's 27 dB, against
    // its swing.
    let (mut s, c) = sim(OUTPUT, 1.0, 1.0);
    let o1 = c.unknown_named("o1").unwrap();
    let mut top = 0.0f64;
    for k in 0..SR as usize {
        let t = k as f64 / SR;
        s.process(0.2 * (std::f64::consts::TAU * 440.0 * t).sin());
        top = top.max((s.voltage_at(o1) - 4.5).abs());
    }
    assert!(top > 2.95, "the op-amp reached {top:.2} V of its 3");
    assert!(level(1.0, -10.0) - level(0.5, -10.0) > 10.0);
}

/// Output against input at noon: a quiet note gets most of the op-amp's
/// gain, and from there the output rises gently -- a soft knee and a low
/// ratio, never a ceiling.
#[test]
fn it_squeezes_gently() {
    let curve: Vec<(f64, f64)> = [-60.0, -40.0, -25.0, -10.0]
        .into_iter()
        .map(|level| {
            let (mut s, _) = sim(OUTPUT, 0.5, 1.0);
            (level, steady(&mut s, level, 440.0, 2.5))
        })
        .collect();
    eprintln!("{curve:.1?}");
    let quiet = curve[0].1 - curve[0].0;
    assert!((17.0..25.0).contains(&quiet), "{quiet:.1} dB");
    let slope = (curve[3].1 - curve[1].1) / 30.0;
    assert!((0.25..0.6).contains(&slope), "slope {slope:.2} dB/dB");
    let upper = (curve[3].1 - curve[2].1) / 15.0;
    assert!(upper > 0.15, "a ceiling: {upper:.2} dB/dB");
}

#[test]
fn its_times_are_c7s() {
    let (mut s, _) = sim(OUTPUT, 0.5, 1.0);
    let r = step(&mut s, 0.01, 0.1);
    eprintln!("{r:#?}");
    assert!(r.overshoot > 1.5 * r.loud, "{r:?}");
    // R11 1.5 k charging C7 4.7 uF through the diode: 7 ms.
    assert!(
        (0.003..0.015).contains(&r.attack),
        "attack {:.4} s",
        r.attack
    );
    // R12 100 k discharging it: 0.47 s.
    assert!(
        (0.2..0.7).contains(&r.release),
        "release {:.3} s",
        r.release
    );
    assert!(r.recovered < 1.5, "recovered {:.3} s", r.recovered);
    // The ratio across the step: 20 dB in, a third of it out.
    let out = 20.0 * (r.loud / r.before).log10();
    assert!((5.0..12.0).contains(&out), "{out:.1} dB");
}

#[test]
fn a_ringing_string_sustains() {
    let (mut s, _) = sim(OUTPUT, 0.5, 1.0);
    let (output, input) = sustain(&mut s, 0.3, 0.35);
    eprintln!("in fell {input:.1} dB, out {output:.1} dB");
    assert!(
        output < 0.6 * input && output > 0.0,
        "{output:.1} of {input:.1} dB"
    );
}

#[test]
fn it_is_silent_with_nothing_to_squeeze() {
    let (mut s, _) = sim(OUTPUT, 0.5, 1.0);
    let mut most = 0.0f64;
    for k in 0..SR as usize {
        let y = s.process(0.0);
        if k > 4800 {
            most = most.max(y.abs());
        }
    }
    assert!(most < 1e-6, "{most} V");
}

#[test]
fn the_volume_rest_is_unity_through_the_pedal() {
    let (mut s, _) = sim(OUTPUT, 0.5, VOLUME_REST);
    let gain = pluck_gain(&mut s);
    assert!(gain.abs() < 0.5, "{gain:.2} dB at VOLUME_REST");
}

/// Where VOLUME_REST comes from. `--ignored --nocapture`.
#[test]
#[ignore]
fn calibrate_the_volume_rest() {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..14 {
        let mid: f64 = 0.5 * (lo + hi);
        let (mut s, _) = sim(OUTPUT, 0.5, mid);
        if pluck_gain(&mut s) > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    eprintln!("VOLUME_REST = {:.3}", 0.5 * (lo + hi));
}

#[test]
fn the_devices_keep_to_their_footprints() {
    let (mut s, _) = sim(OUTPUT, 0.8, 1.0);
    s.watch_structure();
    for k in 0..(0.5 * SR) as usize {
        let t = k as f64 / SR;
        s.process(1.5 * (std::f64::consts::TAU * 82.4 * t).sin() * (-3.0 * t).exp());
    }
    assert_eq!(s.violations(), 0);
}

/// The knob moves the squeeze evenly: a quiet note rises by similar steps
/// from the bottom to 0.7 (by 4.5 dB an eighth below noon, by 7.5 dB a
/// tenth above), never back.
#[test]
fn the_bias_knob_spreads_the_squeeze_evenly() {
    let level = |bias: f64| {
        let (mut s, _) = sim(OUTPUT, bias, 1.0);
        steady(&mut s, -30.0, 440.0, 1.5)
    };
    let points = [0.0, 0.125, 0.25, 0.375, 0.5, 0.6, 0.7];
    let levels: Vec<f64> = points.iter().map(|&k| level(k)).collect();
    for (w, k) in levels.windows(2).zip(points.windows(2)) {
        let step = w[1] - w[0];
        let expected = if k[1] <= 0.5 { 4.5 } else { 7.5 };
        assert!(
            (step - expected).abs() < 2.0,
            "{:.3} to {:.3}: {step:.1} dB ({levels:?})",
            k[0],
            k[1]
        );
    }
}

#![allow(clippy::unwrap_used)]

use super::*;
use crate::ProcessStatus;
use crate::devices::poly::rig::{Play, off, on, rms, spectrum};
use faderframe_core::ParameterId;

fn play(set: &[(u32, f64)]) -> Play<FmProcessor> {
    let params = ParamValues::new(parameters());
    for (pid, v) in set {
        params.set_by_id(ParameterId(*pid), *v).unwrap();
    }
    let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
    Play::new(FmProcessor::new(
        params,
        Some(tap),
        &crate::devices::rig::config(),
    ))
}

fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).map(|(a, b)| a + b).collect()
}

/// The level (dB, relative to the loudest bin) near `f`.
fn level_at(x: &[f32], f: f64) -> f64 {
    let (mags, bin) = spectrum(x);
    let top = mags.iter().fold(0.0f64, |m, v| m.max(*v));
    let k = (f / bin).round() as usize;
    let near = mags[k.saturating_sub(3)..=(k + 3).min(mags.len() - 1)]
        .iter()
        .fold(0.0f64, |m, v| m.max(*v));
    20.0 * (near / top).max(1e-12).log10()
}

#[test]
fn ids_are_positions() {
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i, "{}", p.name);
    }
    assert_eq!(parameters().len() as u32, id::out(5) + 1);
    assert_eq!(TARGETS.len(), target::MOD_INDEX + 1);
    assert_eq!(
        format(ParameterId(id::ALGORITHM), 31.0).as_deref(),
        Some("32")
    );
    assert_eq!(
        format(ParameterId(id::ALGORITHM), FREE_MATRIX as f64).as_deref(),
        Some("Matrix")
    );
    assert_eq!(
        format(ParameterId(id::op(2, id::RATIO)), 0.0).as_deref(),
        Some("½")
    );
    assert!((ratio(0.0, 0.0) - 0.5).abs() < 1e-12);
    assert!((ratio(3.0, 0.5) - 4.5).abs() < 1e-12);
    assert!(per_voice(ParameterId(id::op(0, id::LEVEL))));
    assert!(!per_voice(ParameterId(id::ALGORITHM)));
    assert!((step_gain(99.0) - 1.0).abs() < 1e-6);
    assert!((step_gain(59.0) - 10f32.powf(-1.5)).abs() < 1e-3);
    assert_eq!(step_gain(0.0), 0.0);
}

/// Ratios (coarse, fine) that keep every carrier and first sideband apart.
const RATIOS: [(f64, f64); 6] = [
    (1.0, 0.0),
    (1.0, 0.37),
    (2.0, 0.055),
    (2.0, 0.465),
    (3.0, 0.237),
    (4.0, 0.1325),
];

fn op_hz(k: usize) -> f64 {
    110.0 * ratio(RATIOS[k].0, RATIOS[k].1)
}

/// Every algorithm: its carriers are heard and its modulators are not;
/// each modulator moves exactly the carriers it is drawn into (a first
/// sideband at their sum), at a small index so higher sidebands stay
/// down.
#[test]
fn every_algorithm_routes_its_operators_as_drawn() {
    let render = |alg: usize, levels: [f64; 6]| {
        let mut set = vec![(id::ALGORITHM, alg as f64), (id::KEY_SYNC, 1.0)];
        for (k, (ratio, level)) in RATIOS.iter().zip(levels).enumerate() {
            set.extend([
                (id::op(k, id::RATIO), ratio.0),
                (id::op(k, id::FINE), ratio.1),
                (id::op(k, id::LEVEL), level),
                (id::op(k, id::OP_DECAY), 30_000.0),
                (id::op(k, id::BREAK), 99.0),
                (id::op(k, id::OP_SUSTAIN), 99.0),
                (id::op(k, id::VELOCITY), 0.0),
            ]);
        }
        let mut p = play(&set);
        p.send(on(45));
        let (l, r) = p.run(0.5);
        mono(&l, &r)
    };
    for (n, a) in ALGORITHMS.iter().enumerate() {
        let carriers_only: [f64; 6] =
            std::array::from_fn(|k| if a.is_carrier(k) { 99.0 } else { 0.0 });
        let x = render(n, carriers_only);
        for k in 0..6 {
            let lvl = level_at(&x, op_hz(k));
            if a.is_carrier(k) {
                assert!(
                    lvl > -12.0,
                    "algorithm {}: carrier {} at {lvl:.1}",
                    n + 1,
                    k + 1
                );
            } else {
                assert!(
                    lvl < -60.0,
                    "algorithm {}: modulator {} heard at {lvl:.1}",
                    n + 1,
                    k + 1
                );
            }
        }
        for m in (0..6).filter(|k| !a.is_carrier(*k)) {
            let mut levels = carriers_only;
            levels[m] = 59.0;
            let x = render(n, levels);
            for c in (0..6).filter(|k| a.is_carrier(*k)) {
                let side = level_at(&x, op_hz(c) + op_hz(m));
                if a.modulates(m, c) {
                    assert!(
                        side > -35.0,
                        "algorithm {}: {} → {} only {side:.1}",
                        n + 1,
                        m + 1,
                        c + 1
                    );
                } else {
                    assert!(
                        side < -45.0,
                        "algorithm {}: {} reaches {} ({side:.1})",
                        n + 1,
                        m + 1,
                        c + 1
                    );
                }
            }
        }
    }
}

/// A modulator of a modulator is heard through it (algorithm 1's stack:
/// 6 → 5 → 4 → 3).
#[test]
fn a_stack_carries_its_top_down() {
    let render = |top: f64| {
        let mut set = vec![(id::ALGORITHM, 0.0)];
        for (k, ratio) in RATIOS.iter().enumerate() {
            set.extend([
                (id::op(k, id::RATIO), ratio.0),
                (id::op(k, id::FINE), ratio.1),
                (id::op(k, id::OP_SUSTAIN), 99.0),
                (id::op(k, id::BREAK), 99.0),
            ]);
        }
        set.extend([
            (id::op(0, id::LEVEL), 0.0),
            (id::op(2, id::LEVEL), 99.0),
            (id::op(3, id::LEVEL), 85.0),
            (id::op(4, id::LEVEL), 85.0),
            (id::op(5, id::LEVEL), top),
        ]);
        let mut p = play(&set);
        p.send(on(45));
        let (l, r) = p.run(0.5);
        mono(&l, &r)
    };
    let (a, _) = spectrum(&render(0.0));
    let (b, _) = spectrum(&render(80.0));
    let diff: f64 = a.iter().zip(&b).map(|(x, y)| (x - y).powi(2)).sum();
    let total: f64 = a.iter().map(|x| x * x).sum();
    assert!(
        diff / total > 0.01,
        "operator 6 changes the sound through the stack"
    );
}

#[test]
fn feedback_and_the_matrix_add_harmonics() {
    let second = |set: &[(u32, f64)]| {
        let mut p = play(set);
        p.send(on(57));
        let (l, r) = p.run(0.5);
        level_at(&mono(&l, &r), 440.0)
    };
    // Algorithm 32: six carriers, the feedback on operator 6.
    let plain = second(&[
        (id::ALGORITHM, 31.0),
        (id::op(0, id::LEVEL), 0.0),
        (id::op(5, id::LEVEL), 99.0),
    ]);
    let fed = second(&[
        (id::ALGORITHM, 31.0),
        (id::op(0, id::LEVEL), 0.0),
        (id::op(5, id::LEVEL), 99.0),
        (id::FEEDBACK, 1.0),
    ]);
    assert!(plain < -60.0 && fed > -20.0, "{plain:.1} → {fed:.1}");
    // The free matrix: operator 2 into operator 1, heard only through 1.
    let matrixed = |depth: f64| {
        let mut p = play(&[
            (id::ALGORITHM, FREE_MATRIX as f64),
            (id::op(1, id::LEVEL), 99.0),
            (id::op(1, id::RATIO), 2.0),
            (id::matrix(1, 0), depth),
        ]);
        p.send(on(57));
        let (l, r) = p.run(0.5);
        mono(&l, &r)
    };
    let x = matrixed(0.0);
    assert!(level_at(&x, 440.0) < -60.0, "operator 2 is not heard");
    let x = matrixed(0.3);
    assert!(level_at(&x, 660.0) > -30.0, "a sideband at 220 + 440");
    // Its diagonal is operator 1's own feedback.
    let mut p = play(&[(id::ALGORITHM, FREE_MATRIX as f64), (id::matrix(0, 0), 1.0)]);
    p.send(on(57));
    let (l, r) = p.run(0.5);
    assert!(level_at(&mono(&l, &r), 440.0) > -20.0);
}

#[test]
fn velocity_and_keyboard_scale_the_levels() {
    let loud = |set: &[(u32, f64)], key: u8, velocity: u8| {
        let mut p = play(set);
        p.send(MidiEvent::NoteOn {
            channel: 0,
            key,
            velocity,
        });
        let (l, r) = p.run(0.3);
        rms(&mono(&l, &r))
    };
    let sens = [(id::op(0, id::VELOCITY), 1.0)];
    let ratio_db = 20.0 * (loud(&sens, 60, 40) / loud(&sens, 60, 127)).log10();
    assert!(ratio_db < -12.0, "velocity takes {ratio_db:.1} dB");
    let scaled = [
        (id::op(0, id::VELOCITY), 0.0),
        (id::op(0, id::KEY_RIGHT), -1.0),
        (id::op(0, id::KEY_BREAK), 60.0),
    ];
    let up = 20.0 * (loud(&scaled, 72, 100) / loud(&scaled, 60, 100)).log10();
    assert!((up + 24.0).abs() < 2.0, "an octave up {up:.1} dB");
}

#[test]
fn a_voice_ends_when_its_carriers_have_released() {
    let mut p = play(&[(id::op(0, id::OP_RELEASE), 100.0)]);
    p.send(on(60));
    p.run(0.2);
    assert_eq!(p.p.chassis.sounding(), 1);
    p.send(off(60));
    p.run(0.3);
    assert_eq!(
        p.p.chassis.sounding(),
        0,
        "done although the voice envelope is long"
    );
    let (l, _) = p.run(0.05);
    assert!(rms(&l) < 1e-6);
}

#[test]
fn the_pitch_envelope_and_transpose_move_the_pitch() {
    let mut p = play(&[(id::TRANSPOSE, 12.0)]);
    p.send(on(57));
    let (l, r) = p.run(0.5);
    assert!(level_at(&mono(&l, &r), 440.0) > -3.0, "an octave up");
    let mut p = play(&[(id::PITCH_START, 12.0), (id::PITCH_TIME, 2_000.0)]);
    p.send(on(57));
    let (l, r) = p.run(0.05);
    let early = crate::devices::poly::rig::peak_hz(&mono(&l, &r));
    assert!(early > 300.0, "starts high: {early}");
}

#[test]
fn a_dx7_voice_plays() {
    let mut d = [0u8; 155];
    for block in 0..6 {
        let o = &mut d[block * 21..block * 21 + 21];
        o[0..4].copy_from_slice(&[95, 40, 30, 60]);
        o[4..8].copy_from_slice(&[99, 85, 0, 0]);
        o[8] = 39;
        o[16] = match block {
            5 => 99,
            4 => 82,
            _ => 0,
        };
        o[18] = 1;
        o[20] = 7;
    }
    d[130..134].copy_from_slice(&[50, 50, 50, 50]);
    d[134] = 0;
    d[144] = 24;
    d[145..155].copy_from_slice(b"TEST BELL ");
    let voices = dx7::parse(&dx7::single_voice_dump(&d)).unwrap();
    let mut p = play(&[]);
    for (pid, v) in &voices[0].values {
        p.p.params.set_by_id(*pid, *v).unwrap();
    }
    p.send(on(60));
    let (l, r) = p.run(0.5);
    let x = mono(&l, &r);
    assert!(rms(&x) > 1e-3, "it sounds");
    assert!(level_at(&x, 523.25) > -10.0, "at its pitch");
    p.send(off(60));
    let mut status = ProcessStatus::Continue;
    for _ in 0..40 {
        p.run(0.25);
        if p.p.chassis.sounding() == 0 {
            status = ProcessStatus::Sleep;
            break;
        }
    }
    assert_eq!(status, ProcessStatus::Sleep, "the release ends it");
}

/// The cost of a voice (by hand: `cargo test -p faderframe-plugin-host
/// --release fm_cost -- --ignored --nocapture`).
#[test]
#[ignore]
fn fm_cost() {
    for (name, alg, filter) in [
        ("algorithm 1", 0.0, 0.0),
        ("algorithm 32", 31.0, 0.0),
        ("matrix, all routes", FREE_MATRIX as f64, 0.0),
        ("algorithm 5, filter", 4.0, 2.0),
    ] {
        let mut set = vec![
            (id::ALGORITHM, alg),
            (id::FILTER_TYPE, filter),
            (id::POLYPHONY, 32.0),
        ];
        for k in 0..6 {
            set.push((id::op(k, id::LEVEL), 80.0));
            for t in 0..6 {
                set.push((id::matrix(k, t), 0.2));
            }
        }
        let mut p = play(&set);
        for k in 0..16u8 {
            p.send(on(40 + 2 * k));
        }
        p.run(0.2);
        let seconds = 4.0;
        let t = std::time::Instant::now();
        p.run(seconds);
        let used = t.elapsed().as_secs_f64() / seconds;
        println!(
            "{name:24} {:6.3} % of a core per voice",
            100.0 * used / 16.0
        );
    }
}

use faderframe_midi::MidiEvent;

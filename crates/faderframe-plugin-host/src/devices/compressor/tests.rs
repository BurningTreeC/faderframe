#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{BLOCK, Rig, SR, level, peak_db, silence, thd, tone};

/// A compressor keyed by its input (the rig always connects a sidechain,
/// silent unless a test feeds it).
fn rig(set: &[(u32, f64)]) -> Rig<CompressorProcessor> {
    let mut all = vec![(id::EXTERNAL, 0.0)];
    all.extend_from_slice(set);
    Rig::with(parameters(), TAP_VALUES, &all, CompressorProcessor::new)
}

#[test]
fn the_static_curve_is_threshold_ratio_and_knee() {
    assert_eq!(reduction(-30.0, -20.0, 4.0, 0.0), 0.0);
    assert!((reduction(-10.0, -20.0, 4.0, 0.0) - 7.5).abs() < 1e-9);
    assert!((reduction(0.0, -20.0, INFINITE, 0.0) - 20.0).abs() < 1e-9);
    // The knee starts below the threshold and joins the line above it.
    let k = reduction(-20.0, -20.0, 4.0, 12.0);
    assert!(k > 0.0 && k < 2.0);
    assert!((reduction(-10.0, -20.0, 4.0, 12.0) - 7.5).abs() < 1e-9);
    // A steady tone 10 dB over: 4:1 leaves 2.5 dB of it.
    for style in 0..5 {
        let mut r = rig(&[
            (id::STYLE, f64::from(style)),
            (id::KNEE, 0.0),
            (id::ATTACK, 1.0),
            (id::RELEASE, 50.0),
        ]);
        let (l, _) = r.run(1.0, tone(1_000.0, gain(-10.0)), silence);
        let got = level(&l, 1_000.0);
        // Feedback styles (opto, vintage) too: their curve is steepened
        // to make up for hearing their own output.
        let want = -17.5;
        let tolerance = 0.6;
        assert!((got - want).abs() < tolerance, "style {style}: {got:.2}");
        assert!(r.tap.value(value::REDUCTION) > 1.0);
    }
}

#[test]
fn attack_and_release_take_their_time() {
    // A step from quiet to 20 dB over: after the attack time most of the
    // reduction is there, after many attack times all of it.
    let mut r = rig(&[
        (id::STYLE, 1.0),
        (id::KNEE, 0.0),
        (id::RATIO, INFINITE),
        (id::ATTACK, 20.0),
        (id::RELEASE, 200.0),
    ]);
    r.run(0.2, tone(1_000.0, gain(-40.0)), silence);
    let (l, _) = r.run(0.3, tone(1_000.0, 1.0), silence);
    let at = |ms: f64| {
        let i = (ms * 0.001 * SR) as usize;
        peak_db(&l[i..i + 48])
    };
    assert!(at(2.0) > -5.0, "barely touched at first: {}", at(2.0));
    assert!(at(250.0) < -18.0, "fully in later: {}", at(250.0));
    // Release: back down to quiet, the gain comes back slowly.
    let (l, _) = r.run(1.5, tone(1_000.0, gain(-40.0)), silence);
    let early = peak_db(&l[960..1_008]);
    let late = peak_db(&l[l.len() - 480..]);
    assert!(early < -45.0 && (late + 40.0).abs() < 0.5, "{early} {late}");
}

#[test]
fn the_sidechain_keys_and_can_be_heard() {
    let mut r = rig(&[
        (id::THRESHOLD, -30.0),
        (id::RATIO, 10.0),
        (id::STYLE, 1.0),
        (id::EXTERNAL, 1.0),
    ]);
    // Quiet input, loud key: ducked.
    let (l, _) = r.run(0.5, tone(500.0, gain(-36.0)), tone(80.0, 1.0));
    assert!(level(&l, 500.0) < -50.0, "{}", level(&l, 500.0));
    // Off: the key is ignored.
    r.set(id::EXTERNAL, 0.0);
    let (l, _) = r.run(0.5, tone(500.0, gain(-36.0)), tone(80.0, 1.0));
    assert!((level(&l, 500.0) + 36.0).abs() < 0.3);
    // The sidechain high cut removes a low key; listening plays the
    // filtered key.
    r.set(id::EXTERNAL, 1.0);
    r.set(id::SC_LOW, 1_000.0);
    let (l, _) = r.run(0.5, tone(500.0, gain(-36.0)), tone(80.0, 1.0));
    assert!(level(&l, 500.0) > -38.0, "{}", level(&l, 500.0));
    r.set(id::LISTEN, 1.0);
    r.set(id::SC_LOW, 10.0);
    let (l, _) = r.run(0.3, silence, tone(200.0, 0.5));
    assert!((level(&l, 200.0) - 20.0 * 0.5f64.log10()).abs() < 0.5);
}

#[test]
fn lookahead_catches_the_attack_and_mix_brings_back_the_dry() {
    let ms = 5.0;
    let mut r = rig(&[
        (id::LOOKAHEAD, ms),
        (id::STYLE, 1.0),
        (id::RATIO, INFINITE),
        (id::KNEE, 0.0),
        (id::ATTACK, 1.0),
    ]);
    let lat = latency(&r.params, SR) as usize;
    assert_eq!(lat, 240);
    // An impulse-like burst: delayed by the lookahead, and already turned
    // down when it comes out.
    r.run(0.1, silence, silence);
    let (l, _) = r.run(
        0.1,
        |n| {
            if (6_000..6_048).contains(&n) {
                (1.0, 1.0)
            } else {
                (0.0, 0.0)
            }
        },
        silence,
    );
    let first = l.iter().position(|v| v.abs() > 1e-3).unwrap();
    assert!(first >= lat - 1, "comes out late: {first}");
    assert!(peak_db(&l) < -6.0, "{}", peak_db(&l));
    // All dry: the input, delayed by the lookahead.
    let mut r = rig(&[(id::LOOKAHEAD, ms), (id::MIX, 0.0)]);
    let (l, _) = r.run(0.2, tone(1_000.0, 1.0), silence);
    let input = tone(1_000.0, 1.0);
    for (i, v) in l.iter().enumerate().take(2_100).skip(2_000) {
        assert!((v - input(i - lat).0).abs() < 1e-5);
    }
}

#[test]
fn range_caps_the_reduction_and_auto_makeup_makes_up() {
    let mut r = rig(&[
        (id::RANGE, 6.0),
        (id::RATIO, INFINITE),
        (id::KNEE, 0.0),
        (id::STYLE, 1.0),
    ]);
    let (l, _) = r.run(0.5, tone(1_000.0, 1.0), silence);
    assert!(
        (level(&l, 1_000.0) + 6.0).abs() < 0.3,
        "{}",
        level(&l, 1_000.0)
    );
    let mut r = rig(&[(id::AUTO_MAKEUP, 1.0)]);
    let (l, _) = r.run(0.5, tone(1_000.0, gain(-30.0)), silence);
    assert!(
        level(&l, 1_000.0) > -30.0 + 3.0,
        "louder below the threshold"
    );
}

#[test]
fn colour_comes_with_the_style() {
    let clean = {
        let mut r = rig(&[(id::THRESHOLD, -20.0), (id::STYLE, 0.0)]);
        let (l, _) = r.run(1.0, tone(100.0, 0.5), silence);
        thd(&l[l.len() - 24_000..], 100.0)
    };
    let vintage = {
        let mut r = rig(&[(id::THRESHOLD, -20.0), (id::STYLE, 3.0), (id::COLOR, 1.0)]);
        let (l, _) = r.run(1.0, tone(100.0, 0.5), silence);
        thd(&l[l.len() - 24_000..], 100.0)
    };
    assert!(clean < 0.5, "clean: {clean:.3} %");
    assert!(
        vintage > clean + 0.3,
        "vintage: {vintage:.3} % vs {clean:.3}"
    );
    let _ = BLOCK;
}

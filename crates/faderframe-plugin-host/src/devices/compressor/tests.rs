#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{BLOCK, Rig, SR, bin_db, level, peak_db, silence, thd, tone};

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
    for style in 0..STYLES.len() as u8 {
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
fn feedback_styles_do_not_ratchet_reduction_on_repeated_attacks() {
    for style in [2, 3] {
        let mut r = rig(&[
            (id::STYLE, f64::from(style)),
            (id::THRESHOLD, -24.0),
            (id::RATIO, 4.0),
            (id::KNEE, 0.0),
            (id::ATTACK, 0.01),
            (id::RELEASE, 300.0),
        ]);
        for _ in 0..12 {
            r.run(0.1, tone(1_000.0, gain(-6.0)), silence);
            let reduction = r.tap.value(value::REDUCTION_PEAK);
            // 18 dB over threshold at 4:1 needs 13.5 dB of reduction.
            assert!(reduction <= 14.5, "style {style}: {reduction:.2} dB");
            r.run(0.05, tone(1_000.0, gain(-36.0)), silence);
        }
    }
}

#[test]
fn feedback_styles_keep_the_selected_ratio_with_an_external_key() {
    for style in [2, 3] {
        let mut r = rig(&[
            (id::STYLE, f64::from(style)),
            (id::EXTERNAL, 1.0),
            (id::THRESHOLD, -24.0),
            (id::RATIO, 4.0),
            (id::KNEE, 0.0),
            (id::ATTACK, 1.0),
            (id::RELEASE, 50.0),
        ]);
        let (l, _) = r.run(1.0, tone(1_000.0, gain(-30.0)), tone(1_000.0, gain(-6.0)));
        let got = level(&l, 1_000.0);
        assert!((got + 43.5).abs() < 0.6, "style {style}: {got:.2} dB");
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

/// Drum-like bursts: sharp attacks every 125 ms, decaying.
fn bursts() -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let t = (n % 6_000) as f64 / SR;
        let env = (-t / 0.03).exp();
        let x = (env * (std::f64::consts::TAU * 180.0 * t).sin()
            + 0.4 * env * (std::f64::consts::TAU * 3_100.0 * t).sin()) as f32;
        (x, x * 0.8)
    }
}

#[test]
fn every_parameter_is_at_its_index_and_the_later_ones_are_neutral() {
    let infos = parameters();
    for (i, info) in infos.iter().enumerate() {
        assert_eq!(info.id.0 as usize, i, "{}", info.name);
    }
    // What came after the first release does nothing by default: a saved
    // project keeps its sound.
    for pid in [
        id::HOLD,
        id::CHANNELS,
        id::UPWARD,
        id::OVERSAMPLE,
        id::DELTA,
    ] {
        assert_eq!(
            infos[pid as usize].default, 0.0,
            "{}",
            infos[pid as usize].name
        );
    }
    let r = rig(&[]);
    assert_eq!(latency(&r.params, SR), 0);
    assert_eq!(format(ParameterId(id::HOLD), 0.0).as_deref(), Some("Off"));
    assert_eq!(format(ParameterId(id::UPWARD), 0.0).as_deref(), Some("Off"));
    assert_eq!(
        format(ParameterId(id::CHANNELS), 1.0).as_deref(),
        Some("M/S")
    );
    assert_eq!(
        format(ParameterId(id::OVERSAMPLE), 2.0).as_deref(),
        Some("4×")
    );
    assert_eq!(
        format(ParameterId(id::STYLE), 6.0).as_deref(),
        Some("Pumping")
    );
}

#[test]
fn the_styles_sound_different_from_each_other() {
    let outs: Vec<Vec<f32>> = (0..STYLES.len())
        .map(|style| {
            let mut r = rig(&[
                (id::STYLE, f64::from(style as u8)),
                (id::THRESHOLD, -24.0),
                (id::RATIO, 4.0),
                (id::COLOR, 0.5),
            ]);
            r.run(1.0, bursts(), silence).0
        })
        .collect();
    for (i, a) in outs.iter().enumerate() {
        for (j, b) in outs.iter().enumerate().skip(i + 1) {
            let diff = a
                .iter()
                .zip(b)
                .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
            assert!(
                diff > 1e-3,
                "{} and {} sound the same",
                STYLES[i],
                STYLES[j]
            );
        }
    }
    // Every one of them compresses.
    let input = {
        let b = bursts();
        peak_db(&(0..6_000).map(|n| b(n).0).collect::<Vec<_>>())
    };
    for (style, o) in STYLES.iter().zip(&outs) {
        let peak = peak_db(&o[o.len() - 6_000..]);
        assert!(peak < input - 3.0, "{style}: {peak} vs {input}");
    }
}

#[test]
fn hold_keeps_the_reduction_down_before_it_releases() {
    // Loud, then quiet: without a hold the gain is back within a few
    // release times; with a hold of 200 ms it is still down after 100 ms.
    let run = |hold: f64| {
        let mut r = rig(&[
            (id::STYLE, 1.0),
            (id::KNEE, 0.0),
            (id::RATIO, INFINITE),
            (id::ATTACK, 0.1),
            (id::RELEASE, 10.0),
            (id::HOLD, hold),
        ]);
        r.run(0.3, tone(1_000.0, 1.0), silence);
        let (l, _) = r.run(0.3, tone(1_000.0, gain(-40.0)), silence);
        let at = |ms: f64| {
            let i = (ms * 0.001 * SR) as usize;
            peak_db(&l[i..i + 96])
        };
        (at(100.0), at(280.0))
    };
    let (held_100, held_280) = run(200.0);
    let (free_100, _) = run(0.0);
    assert!(held_100 < -55.0, "still held: {held_100}");
    assert!(
        (held_280 + 40.0).abs() < 1.0,
        "let go after the hold: {held_280}"
    );
    assert!((free_100 + 40.0).abs() < 1.0, "no hold: {free_100}");
}

#[test]
fn mid_side_compresses_the_middle_and_leaves_the_sides() {
    // A loud mono tone with a quiet wide one: in M/S (unlinked) only the
    // middle comes down; in L/R each channel hears the loud tone and
    // takes the wide one down with it.
    let signal = |n: usize| {
        let t = n as f64 / SR;
        let m = gain(-6.0) * (std::f64::consts::TAU * 1_000.0 * t).sin();
        let s = gain(-40.0) * (std::f64::consts::TAU * 3_000.0 * t).sin();
        ((m + s) as f32, (m - s) as f32)
    };
    let side_level = |mode: f64| {
        let mut r = rig(&[
            (id::CHANNELS, mode),
            (id::LINK, 0.0),
            (id::THRESHOLD, -20.0),
            (id::RATIO, INFINITE),
            (id::KNEE, 0.0),
            (id::ATTACK, 1.0),
            (id::RELEASE, 50.0),
        ]);
        let (l, rr) = r.run(1.0, signal, silence);
        let mid: Vec<f32> = l.iter().zip(&rr).map(|(a, b)| 0.5 * (a + b)).collect();
        let side: Vec<f32> = l.iter().zip(&rr).map(|(a, b)| 0.5 * (a - b)).collect();
        (level(&mid, 1_000.0), bin_db(&side, 3_000.0))
    };
    let (mid_ms, side_ms) = side_level(1.0);
    let (mid_lr, side_lr) = side_level(0.0);
    assert!((mid_ms + 20.0).abs() < 0.5, "mid in M/S: {mid_ms}");
    assert!(
        (side_ms + 40.0).abs() < 0.5,
        "side in M/S untouched: {side_ms}"
    );
    assert!((mid_lr + 20.0).abs() < 0.5, "mid in L/R: {mid_lr}");
    assert!(
        side_lr < -50.0,
        "side in L/R comes down with the middle: {side_lr}"
    );
}

#[test]
fn upward_compression_lifts_what_is_under_the_threshold() {
    // 4:1 below −20 dB, at most 12 dB: a −40 dB tone would get 15 dB and
    // gets 12; a −10 dB tone is compressed as ever.
    let mut r = rig(&[
        (id::STYLE, 1.0),
        (id::THRESHOLD, -20.0),
        (id::RATIO, 4.0),
        (id::KNEE, 0.0),
        (id::UPWARD, 12.0),
        (id::ATTACK, 1.0),
        (id::RELEASE, 20.0),
    ]);
    let (l, _) = r.run(1.0, tone(1_000.0, gain(-40.0)), silence);
    assert!(
        (level(&l, 1_000.0) + 28.0).abs() < 0.3,
        "{}",
        level(&l, 1_000.0)
    );
    assert!(r.tap.value(value::GAIN) > 11.0);
    let (l, _) = r.run(1.0, tone(1_000.0, gain(-10.0)), silence);
    assert!(
        (level(&l, 1_000.0) + 17.5).abs() < 0.3,
        "{}",
        level(&l, 1_000.0)
    );
    // The static curve the editor draws says the same.
    assert!((curve(-40.0, -20.0, 4.0, 0.0, f64::INFINITY, 12.0) - 12.0).abs() < 1e-9);
    assert!((curve(-10.0, -20.0, 4.0, 0.0, f64::INFINITY, 12.0) + 7.5).abs() < 1e-9);
    // A −30 dB tone sits 10 under: 7.5 dB of lift.
    assert!((lift(-30.0, -20.0, 4.0, 0.0, 12.0) - 7.5).abs() < 1e-9);
    assert_eq!(lift(-30.0, -20.0, 4.0, 0.0, 0.0), 0.0);
}

#[test]
fn oversampling_keeps_the_colour_from_aliasing_and_counts_as_latency() {
    // A 15 kHz tone saturated: its second harmonic at 30 kHz folds down
    // to 18 kHz, the third at 45 kHz to 3 kHz. Oversampled, the folded
    // products are gone.
    let alias = |os: f64| {
        let mut r = rig(&[
            (id::STYLE, 3.0),
            (id::THRESHOLD, -30.0),
            (id::COLOR, 1.0),
            (id::OVERSAMPLE, os),
        ]);
        let (l, _) = r.run(1.0, tone(15_000.0, 0.5), silence);
        (
            bin_db(&l, 3_000.0),
            bin_db(&l, 15_000.0),
            latency(&r.params, SR),
        )
    };
    let (folded_off, tone_off, lat_off) = alias(0.0);
    let (folded_4x, tone_4x, lat_4x) = alias(2.0);
    assert_eq!(lat_off, 0);
    assert_eq!(lat_4x, 72);
    assert!((tone_off - tone_4x).abs() < 1.0, "{tone_off} {tone_4x}");
    assert!(
        folded_4x < folded_off - 20.0,
        "folded: {folded_off:.1} dB without, {folded_4x:.1} dB with oversampling"
    );
    // The dry path is delayed to match: all dry, the input comes out as
    // it went in, the latency later.
    let mut r = rig(&[(id::OVERSAMPLE, 1.0), (id::MIX, 0.0)]);
    let lat = latency(&r.params, SR) as usize;
    assert_eq!(lat, 64);
    let input = tone(1_000.0, 0.5);
    let (l, _) = r.run(0.2, &input, silence);
    for (i, v) in l.iter().enumerate().take(4_100).skip(4_000) {
        assert!((v - input(i - lat).0).abs() < 1e-5);
    }
    // And without colour the oversampler's filters are transparent.
    let mut r = rig(&[(id::OVERSAMPLE, 1.0), (id::THRESHOLD, 0.0)]);
    let (l, _) = r.run(0.2, &input, silence);
    for (i, v) in l.iter().enumerate().take(4_100).skip(4_000) {
        assert!((v - input(i - lat).0).abs() < 2e-3, "{i}: {v}");
    }
}

#[test]
fn delta_plays_only_what_the_compressor_takes_away() {
    let mut r = rig(&[
        (id::DELTA, 1.0),
        (id::THRESHOLD, -20.0),
        (id::RATIO, 4.0),
        (id::KNEE, 0.0),
        (id::STYLE, 1.0),
    ]);
    // Nothing to compress: nothing to hear.
    let (l, _) = r.run(0.5, tone(1_000.0, gain(-40.0)), silence);
    assert!(peak_db(&l[l.len() - 4_800..]) < -90.0, "{}", peak_db(&l));
    // 10 dB over at 4:1: the 7.5 dB taken is a tone 20 dB · log(1 −
    // 10^(−7.5/20)) under the input.
    let (l, _) = r.run(1.0, tone(1_000.0, gain(-10.0)), silence);
    let want = -10.0 + 20.0 * (1.0 - gain(-7.5)).log10();
    assert!(
        (level(&l, 1_000.0) - want).abs() < 0.5,
        "{} vs {want}",
        level(&l, 1_000.0)
    );
}

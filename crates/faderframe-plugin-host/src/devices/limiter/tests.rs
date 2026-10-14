#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{Rig, SR, level, peak_db, silence, tone};
use crate::dsp::Noise;

fn rig(set: &[(u32, f64)]) -> Rig<LimiterProcessor> {
    Rig::with(parameters(), TAP_VALUES, set, LimiterProcessor::new)
}

/// Loud drum-like noise: bursts of noise with sharp attacks.
fn bursts() -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let mut noise = Noise::new((n as u32).wrapping_mul(2_654_435_761).wrapping_add(1));
        let v = noise.tick();
        let env = (-((n % 6_000) as f64) / 800.0).exp();
        let x = (v * env * 0.9) as f32;
        (x, -x * 0.7)
    }
}

#[test]
fn nothing_passes_the_ceiling() {
    for style in 0..STYLES.len() as u8 {
        for tp in [0.0, 1.0] {
            let mut r = rig(&[
                (id::GAIN, 18.0),
                (id::CEILING, -1.0),
                (id::STYLE, f64::from(style)),
                (id::TRUE_PEAK, tp),
            ]);
            let (l, rr) = r.run(2.0, bursts(), silence);
            let peak = peak_db(&l).max(peak_db(&rr));
            assert!(peak <= -1.0 + 1e-4, "style {style} tp {tp}: {peak:.4} dB");
            // Not squashed to nothing either.
            assert!(peak > -2.0, "{peak}");
            if tp >= 0.5 {
                // The true peak too (within the interpolator's accuracy).
                let mut t = crate::dsp::truepeak::TruePeak::new();
                let tpk = l
                    .iter()
                    .map(|v| t.process(f64::from(*v)))
                    .fold(0.0, f64::max);
                assert!(db(tpk) < -0.8, "style {style}: true peak {:.3}", db(tpk));
            }
        }
    }
}

#[test]
fn the_latency_is_the_lookahead_and_quiet_audio_passes_untouched() {
    let r = rig(&[(id::LOOKAHEAD, 5.0), (id::TRUE_PEAK, 0.0)]);
    assert_eq!(latency(&r.params, SR), 240);
    let r = rig(&[(id::LOOKAHEAD, 5.0), (id::TRUE_PEAK, 1.0)]);
    assert_eq!(latency(&r.params, SR), 246);
    let mut r = rig(&[(id::LOOKAHEAD, 5.0), (id::TRUE_PEAK, 0.0)]);
    let input = tone(1_000.0, 0.5);
    let (l, _) = r.run(0.2, &input, silence);
    for (i, v) in l.iter().enumerate().take(1_100).skip(1_000) {
        assert!((v - input(i - 240).0).abs() < 1e-6);
    }
    assert_eq!(r.tap.value(value::REDUCTION), 0.0);
}

#[test]
fn a_peak_is_caught_and_released() {
    let mut r = rig(&[
        (id::CEILING, -6.0),
        (id::RELEASE, 50.0),
        (id::AUTO_RELEASE, 0.0),
        (id::TRUE_PEAK, 0.0),
    ]);
    // A 0 dB tone over the −6 dB ceiling, then quiet.
    let (l, _) = r.run(0.5, tone(500.0, 1.0), silence);
    assert!((level(&l, 500.0) + 6.0).abs() < 0.3, "{}", level(&l, 500.0));
    let gr = f64::from(r.tap.value(value::REDUCTION));
    assert!((gr - 6.0).abs() < 0.5, "{gr}");
    let (l, _) = r.run(1.0, tone(500.0, 0.1), silence);
    assert!(
        (level(&l, 500.0) + 20.0).abs() < 0.2,
        "released: {}",
        level(&l, 500.0)
    );
}

#[test]
fn unity_gain_listens_at_the_input_level() {
    let mut r = rig(&[(id::GAIN, 12.0), (id::UNITY, 1.0), (id::TRUE_PEAK, 0.0)]);
    let (l, _) = r.run(0.5, tone(500.0, 0.05), silence);
    assert!((level(&l, 500.0) - 20.0 * 0.05f64.log10()).abs() < 0.2);
}

/// A tone at a quarter of the rate, 45° along: every sample sits 3 dB
/// under its true peaks.
fn between_samples(amp: f64) -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let x = (amp * (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin())
            as f32;
        (x, x)
    }
}

fn true_peak_db(x: &[f32]) -> f64 {
    let mut t = crate::dsp::truepeak::TruePeak::new();
    db(x.iter()
        .map(|v| t.process(f64::from(*v)))
        .fold(0.0, f64::max))
}

#[test]
fn every_parameter_is_at_its_index_and_the_later_ones_are_neutral() {
    let infos = parameters();
    for (i, info) in infos.iter().enumerate() {
        assert_eq!(info.id.0 as usize, i, "{}", info.name);
    }
    assert_eq!(infos[id::OUTPUT as usize].default, 0.0);
    assert_eq!(infos[id::RELEASE_LINK as usize].default, 0.0);
    assert_eq!(style(0), style(99));
    assert_eq!(format(ParameterId(id::STYLE), 5.0).as_deref(), Some("Safe"));
}

#[test]
fn the_styles_sound_different_from_each_other() {
    let outs: Vec<Vec<f32>> = (0..STYLES.len())
        .map(|style| {
            let mut r = rig(&[
                (id::GAIN, 12.0),
                (id::CEILING, -1.0),
                (id::STYLE, f64::from(style as u8)),
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
}

#[test]
fn safe_keeps_true_peaks_under_the_ceiling_on_intersample_peaks() {
    // Driven hard into a −1 dB ceiling by a tone peaking between samples:
    // Transparent may let the true peak over by the interpolator's error;
    // Safe keeps its margin.
    let tp = |style: f64| {
        let mut r = rig(&[
            (id::GAIN, 18.0),
            (id::CEILING, -1.0),
            (id::STYLE, style),
            (id::TRUE_PEAK, 1.0),
        ]);
        let (l, _) = r.run(1.0, between_samples(0.5), silence);
        let out = true_peak_db(&l[l.len() - 24_000..]);
        (
            out,
            peak_db(&l[l.len() - 24_000..]),
            f64::from(r.tap.take_value(value::OUT_PEAK)),
        )
    };
    let (safe, safe_samples, published) = tp(5.0);
    assert!(safe <= -1.0 + 0.02, "safe: {safe:.3} dBTP");
    assert!(safe > -1.6, "not over-limited: {safe:.3}");
    // The published output peak is the true peak (over the sample peak).
    assert!(
        db(published) > safe_samples + 1.0,
        "{} vs {safe_samples}",
        db(published)
    );
    let (transparent, _, _) = tp(0.0);
    assert!(
        transparent <= -1.0 + 0.3,
        "transparent: {transparent:.3} dBTP"
    );
    // Bursts of noise too.
    let mut r = rig(&[
        (id::GAIN, 18.0),
        (id::CEILING, -1.0),
        (id::STYLE, 5.0),
        (id::TRUE_PEAK, 1.0),
    ]);
    let (l, rr) = r.run(2.0, bursts(), silence);
    assert!(true_peak_db(&l).max(true_peak_db(&rr)) <= -1.0 + 0.02);
}

#[test]
fn output_trims_after_the_limiter() {
    let mut r = rig(&[
        (id::CEILING, -1.0),
        (id::OUTPUT, -6.0),
        (id::TRUE_PEAK, 0.0),
        (id::AUTO_RELEASE, 0.0),
    ]);
    let (l, _) = r.run(1.0, tone(500.0, 1.0), silence);
    // Limited to −1, then 6 dB down: −7 dB, and nothing over −7.
    assert!((level(&l, 500.0) + 7.0).abs() < 0.3, "{}", level(&l, 500.0));
    assert!(peak_db(&l) <= -7.0 + 0.05);
    assert!((db(f64::from(r.tap.take_value(value::OUT_PEAK))) + 7.0).abs() < 0.3);
}

#[test]
fn the_release_link_pulls_the_quiet_side_down_with_the_loud_one() {
    let right = |release_link: f64| {
        let mut r = rig(&[
            (id::CEILING, -6.0),
            (id::LINK, 0.0),
            (id::RELEASE_LINK, release_link),
            (id::TRUE_PEAK, 0.0),
            (id::AUTO_RELEASE, 0.0),
        ]);
        let signal = |n: usize| {
            let t = n as f64 / SR;
            let x = (std::f64::consts::TAU * 500.0 * t).sin();
            (x as f32, (0.1 * x) as f32)
        };
        let (_, rr) = r.run(1.0, signal, silence);
        level(&rr, 500.0)
    };
    // Unlinked: the quiet side is under the ceiling and untouched.
    assert!((right(0.0) + 20.0).abs() < 0.3, "{}", right(0.0));
    // Linked: it takes the loud side's 6 dB of reduction.
    assert!((right(1.0) + 26.0).abs() < 0.5, "{}", right(1.0));
    assert!((right(0.5) + 23.0).abs() < 0.7, "{}", right(0.5));
}

#[test]
fn loudness_is_measured_on_the_output() {
    let mut r = rig(&[(id::CEILING, 0.0), (id::TRUE_PEAK, 0.0)]);
    // −20 dBFS on both channels: −20 LUFS (K-weighting is flat at 1 kHz).
    r.run(3.5, tone(997.0, 0.1), silence);
    let short = f64::from(r.tap.value(value::LUFS_SHORT));
    let momentary = f64::from(r.tap.value(value::LUFS_MOMENTARY));
    assert!((short + 20.0).abs() < 0.2, "{short}");
    assert!((momentary + 20.0).abs() < 0.2, "{momentary}");
    // The output trim moves it; the short-term window still remembers
    // the louder second.
    r.set(id::OUTPUT, -10.0);
    r.run(1.0, tone(997.0, 0.1), silence);
    let momentary = f64::from(r.tap.value(value::LUFS_MOMENTARY));
    assert!((momentary + 30.0).abs() < 0.2, "{momentary}");
    assert!(f64::from(r.tap.value(value::LUFS_SHORT)) > momentary);
}

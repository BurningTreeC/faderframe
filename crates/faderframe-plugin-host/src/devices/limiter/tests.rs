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
    for style in 0..3 {
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

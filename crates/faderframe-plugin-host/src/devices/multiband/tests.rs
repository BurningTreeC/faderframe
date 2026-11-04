#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{Rig, SR, bin_db, level, silence, tone};

fn rig(set: &[(u32, f64)]) -> Rig<MultibandProcessor> {
    // The rig always feeds a sidechain: keyed by the input unless asked.
    let mut all = vec![(id::EXTERNAL, 0.0)];
    all.extend_from_slice(set);
    Rig::with(parameters(), TAP_VALUES, &all, MultibandProcessor::new)
}

/// Every band bypassed: what the split alone does.
fn bypassed(n: usize) -> Vec<(u32, f64)> {
    let mut v = vec![(id::BANDS, n as f64)];
    for b in 0..BANDS {
        v.push((id::band(b, id::BYPASS), 1.0));
    }
    v
}

fn two_tones(a: (f64, f64), b: (f64, f64)) -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let t = n as f64 / SR;
        let v = a.1 * (std::f64::consts::TAU * a.0 * t).sin()
            + b.1 * (std::f64::consts::TAU * b.0 * t).sin();
        (v as f32, v as f32)
    }
}

#[test]
fn the_minimum_phase_bands_add_up_to_an_all_pass() {
    for n in 1..=BANDS {
        for f in [40.0, 110.0, 130.0, 600.0, 2_500.0, 7_000.0, 15_000.0] {
            let mut r = rig(&bypassed(n));
            let (l, _) = r.run(0.5, tone(f, 0.5), silence);
            let got = level(&l, f);
            let want = 20.0 * 0.5f64.log10();
            assert!(
                (got - want).abs() < 0.05,
                "{n} bands at {f} Hz: {got:.3} dB"
            );
        }
    }
}

#[test]
fn linear_phase_gives_the_input_back_half_an_fir_later() {
    let mut r = rig(&[(id::PHASE, 1.0), (id::BANDS, 6.0)]);
    let lat = latency(&r.params, SR) as usize;
    assert_eq!(lat, fir_taps(SR) / 2);
    // Every band untouched (the default bands only compress past −20 dB).
    let x = |n: usize| {
        let v = crate::devices::rig::white(n, 9) * 0.01;
        (v as f32, v as f32)
    };
    let (l, _) = r.run(1.0, x, silence);
    for (n, v) in l.iter().enumerate().skip(lat) {
        let want = x(n - lat).0;
        assert!((v - want).abs() < 2e-5, "at {n}: {v} vs {want}");
    }
}

#[test]
fn a_loud_band_is_compressed_and_the_others_left_alone() {
    // A loud 80 Hz under a quiet 3 kHz; the low band at a −30 dB
    // threshold, 4:1, hard knee.
    let set = [
        (id::BANDS, 3.0),
        (id::CROSSOVER, 300.0),
        (id::CROSSOVER + 1, 1_500.0),
        (id::band(0, id::THRESHOLD), -30.0),
        (id::band(0, id::RATIO), 4.0),
        (id::band(0, id::KNEE), 0.0),
        (id::band(0, id::ATTACK), 1.0),
        (id::band(0, id::RELEASE), 50.0),
        (id::band(2, id::THRESHOLD), -10.0),
        (id::DETECTOR, 1.0),
    ];
    let mut r = rig(&set);
    let x = two_tones((80.0, 0.5), (3_000.0, 0.05));
    let (l, _) = r.run(1.5, x, silence);
    let low = bin_db(&l, 80.0);
    let high = bin_db(&l, 3_000.0);
    // The detector reads the low tone at its amplitude, −6 dB: 24 dB over,
    // 3/4 of it taken.
    let amp = 20.0 * 0.5f64.log10();
    let want = amp - 0.75 * (amp + 30.0);
    assert!((low - want).abs() < 1.5, "low {low:.1} dB, want {want:.1}");
    assert!(
        (high - 20.0 * 0.05f64.log10()).abs() < 0.3,
        "high {high:.1} dB"
    );
    let gr = r.tap.value(value::GAIN);
    assert!(gr < -12.0, "band 1 published {gr}");
    assert!(r.tap.value(value::REDUCTION) > 12.0);
    assert!(r.tap.value(value::GAIN + 2).abs() < 0.1);
}

#[test]
fn the_static_curves_compress_expand_and_lift() {
    use Mode::*;
    // 10 dB over a −20 dB threshold at 2:1, hard knee.
    assert!((curve(Compress, -10.0, -20.0, 2.0, 0.0, 48.0) + 5.0).abs() < 1e-9);
    assert_eq!(curve(Compress, -30.0, -20.0, 2.0, 0.0, 48.0), 0.0);
    // 10 dB under: an expander at 3:1 takes 20 more, capped by the range.
    assert!((curve(Expand, -30.0, -20.0, 3.0, 0.0, 48.0) + 20.0).abs() < 1e-9);
    assert!((curve(Expand, -30.0, -20.0, 3.0, 0.0, 12.0) + 12.0).abs() < 1e-9);
    assert!(curve(Expand, -80.0, -20.0, INFINITE, 0.0, 30.0) == -30.0);
    // Upward compression lifts what is under by half at 2:1.
    assert!((curve(UpwardCompress, -40.0, -20.0, 2.0, 0.0, 48.0) - 10.0).abs() < 1e-9);
    assert_eq!(curve(UpwardCompress, -10.0, -20.0, 2.0, 0.0, 48.0), 0.0);
    // Upward expansion lifts what is over.
    assert!((curve(UpwardExpand, -14.0, -20.0, 1.5, 0.0, 48.0) - 3.0).abs() < 1e-9);
    // A soft knee eases in.
    let at = curve(Compress, -20.0, -20.0, 4.0, 6.0, 48.0);
    assert!(at < 0.0 && at > -1.0);
}

#[test]
fn upward_compression_lifts_a_quiet_band() {
    let set = [
        (id::BANDS, 2.0),
        (id::CROSSOVER, 1_000.0),
        (id::band(1, id::MODE), 2.0),
        (id::band(1, id::THRESHOLD), -20.0),
        (id::band(1, id::RATIO), 2.0),
        (id::band(1, id::KNEE), 0.0),
        (id::band(1, id::RANGE), 12.0),
        (id::band(1, id::ATTACK), 1.0),
        (id::band(1, id::RELEASE), 20.0),
        (id::DETECTOR, 1.0),
    ];
    let mut r = rig(&set);
    // A 5 kHz tone at −40 dB: 20 dB under, half of it lifted (10 dB),
    // within the 12 dB range.
    let amp = 10f64.powf(-40.0 / 20.0);
    let (l, _) = r.run(1.0, tone(5_000.0, amp), silence);
    let got = level(&l, 5_000.0) - 20.0 * amp.log10();
    assert!((got - 10.0).abs() < 1.0, "lifted by {got:.1} dB");
    assert!(r.tap.value(value::GAIN + 1) > 9.0);
}

#[test]
fn solo_mute_and_bypass_choose_what_is_heard() {
    let x = two_tones((100.0, 0.3), (5_000.0, 0.3));
    let base = [
        (id::BANDS, 2.0),
        (id::CROSSOVER, 1_000.0),
        (id::band(0, id::BYPASS), 1.0),
        (id::band(1, id::BYPASS), 1.0),
    ];
    let run = |extra: &[(u32, f64)]| {
        let mut set = base.to_vec();
        set.extend_from_slice(extra);
        let mut r = rig(&set);
        let (l, _) = r.run(0.5, &x, silence);
        (bin_db(&l, 100.0), bin_db(&l, 5_000.0))
    };
    let (lo, hi) = run(&[(id::band(1, id::SOLO), 1.0)]);
    assert!(lo < -60.0 && hi > -12.0, "{lo} {hi}");
    let (lo, hi) = run(&[(id::band(1, id::MUTE), 1.0)]);
    assert!(lo > -12.0 && hi < -60.0, "{lo} {hi}");
    // A bypassed band ignores its settings (makeup too).
    let (_, hi) = run(&[(id::band(1, id::GAIN), 12.0)]);
    assert!((hi - 20.0 * 0.3f64.log10()).abs() < 0.2);
}

#[test]
fn an_external_key_ducks_its_band() {
    let set = [
        (id::BANDS, 2.0),
        (id::CROSSOVER, 1_000.0),
        (id::EXTERNAL, 1.0),
        (id::band(0, id::THRESHOLD), -40.0),
        (id::band(0, id::RATIO), INFINITE),
        (id::band(0, id::RANGE), 12.0),
        (id::band(0, id::ATTACK), 1.0),
        (id::band(1, id::BYPASS), 1.0),
    ];
    let mut r = Rig::with(parameters(), TAP_VALUES, &set, MultibandProcessor::new);
    let (l, _) = r.run(1.0, tone(200.0, 0.01), tone(150.0, 0.5));
    // The quiet input alone would be left; the loud key takes its range.
    let got = level(&l, 200.0) - 20.0 * 0.01f64.log10();
    assert!((got + 12.0).abs() < 0.5, "{got:.1} dB");
}

#[test]
fn lookahead_delays_by_its_latency() {
    let mut r = rig(&[(id::LOOKAHEAD, 5.0), (id::BANDS, 1.0)]);
    let lat = latency(&r.params, SR) as usize;
    assert_eq!(lat, 240);
    let (l, _) = r.run(
        0.1,
        |n| if n == 10 { (0.5, 0.5) } else { (0.0, 0.0) },
        silence,
    );
    let at = l.iter().position(|v| v.abs() > 1e-3).unwrap();
    assert_eq!(at, 10 + lat);
    assert_eq!(latency(&r.params, SR) as usize + fir_taps(SR) / 2, {
        r.set(id::PHASE, 1.0);
        latency(&r.params, SR) as usize
    });
}

#[test]
fn band_magnitudes_add_up_to_one() {
    let xs = CROSSOVERS;
    for n in 1..=BANDS {
        for f in [10.0, 100.0, 1_000.0, 5_000.0, 19_000.0] {
            let sum: f64 = (0..n).map(|b| band_magnitude(b, n, &xs, f)).sum();
            assert!((sum - 1.0).abs() < 1e-12);
        }
    }
    assert_eq!(id::field_of(id::band(3, id::MODE)), Some((3, id::MODE)));
    assert_eq!(id::field_of(id::MIX), None);
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i, "{}", p.name);
    }
    assert_eq!(
        parameters().len(),
        (id::BAND_BASE + id::FIELDS * BANDS as u32) as usize
    );
    assert_eq!(
        format(ParameterId(id::band(0, id::RATIO)), INFINITE).unwrap(),
        "∞ : 1"
    );
}

#[test]
fn a_reset_forgets_the_bands() {
    let mut r = rig(&[(id::PHASE, 1.0)]);
    r.run(0.3, tone(300.0, 0.5), silence);
    r.p.reset();
    let (l, _) = r.run(0.5, silence, silence);
    assert!(l.iter().all(|v| *v == 0.0));
}

/// Linear phase: crossovers moved while playing are designed on the
/// processor's thread and taken in (crossfaded).
#[test]
fn linear_phase_crossovers_move_while_playing() {
    let mut r = rig(&[
        (id::PHASE, 1.0),
        (id::BANDS, 2.0),
        (id::CROSSOVER, 1_000.0),
        (id::band(0, id::BYPASS), 1.0),
        (id::band(1, id::MUTE), 1.0),
    ]);
    // 3 kHz is in the muted upper band.
    let (l, _) = r.run(0.5, tone(3_000.0, 0.5), silence);
    assert!(level(&l, 3_000.0) < -40.0, "{}", level(&l, 3_000.0));
    // Up to 6 kHz: now in the lower band, heard.
    r.set(id::CROSSOVER, 6_000.0);
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (l, _) = r.run(0.05, tone(3_000.0, 0.5), silence);
        if level(&l, 3_000.0) > -7.0 {
            return;
        }
    }
    panic!("the crossover never moved");
}

use faderframe_circuit::preamp::{Preamp, MODELS};
use std::time::Instant;

#[test]
fn zero_gain_silences_a_running_circuit_and_reopens_after_reset() {
    for model in 0..MODELS {
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            let mut p = Preamp::new(model, rate, 1.0, 0.0).unwrap();
            let frames = (rate * 0.3) as usize;
            let signal =
                |i: usize| 0.125 * (i as f64 * std::f64::consts::TAU * 1000.0 / rate).sin();
            for i in 0..frames {
                p.process(signal(i));
            }
            p.set_controls(0.0, 0.0);
            for i in 0..frames {
                let y = p.process(signal(i));
                if i > (rate * 0.25) as usize {
                    assert_eq!(y, 0.0, "model {model} at {rate} Hz: closed Gain leaks");
                }
            }
            p.reset();
            for i in 0..256 {
                assert_eq!(p.process(signal(i)), 0.0);
            }
            p.set_controls(0.5, 0.0);
            let mut energy = 0.0;
            for i in 0..frames {
                let y = p.process(signal(i));
                if i >= frames / 2 {
                    energy += y * y;
                }
            }
            let rms = (energy / (frames / 2) as f64).sqrt();
            assert!(
                (0.06..0.12).contains(&rms),
                "model {model} at {rate} Hz: reopened RMS {rms}"
            );
        }
    }
}

#[test]
fn all_models_pass_audio_and_master_is_an_output_trim() {
    for model in 0..MODELS {
        let mut unity = Preamp::new(model, 48_000.0, 0.5, 0.0).unwrap();
        let mut quiet = Preamp::new(model, 48_000.0, 0.5, -6.0).unwrap();
        let scale = 10f64.powf(-6.0 / 20.0);
        let mut energy = 0.0;
        let mut peak = 0.0f64;
        for n in 0..24_000 {
            let x = 0.125 * (n as f64 * std::f64::consts::TAU * 1_000.0 / 48_000.0).sin();
            let a = unity.process(x);
            let b = quiet.process(x);
            assert!(a.is_finite() && b.is_finite());
            assert!(
                (b - a * scale).abs() < 1e-7,
                "model {model}: master changes circuit behaviour"
            );
            if n > 12_000 {
                energy += a * a;
                peak = peak.max(a.abs());
            }
        }
        let rms = (energy / 11_999.0).sqrt();
        eprintln!("model {model}: RMS {rms:.6}, peak {peak:.6}");
        assert!(rms > 0.02 && rms < 0.3, "model {model}: calibration {rms}");
    }
}

#[test]
fn rates_gain_extremes_and_reset_remain_finite() {
    for model in 0..MODELS {
        for rate in [44_100.0, 96_000.0] {
            let mut p = Preamp::new(model, rate, 0.0, 0.0).unwrap();
            for gain in [0.0, 1.0, 0.5] {
                p.set_controls(gain, -12.0);
                for n in 0..2048 {
                    let x = 0.5 * (n as f64 * 0.13).sin();
                    let y = p.process(x);
                    assert!(
                        y.is_finite() && y.abs() < 100.0,
                        "model {model}, gain {gain}: {y}"
                    );
                }
                p.reset();
                for _ in 0..128 {
                    assert!(p.process(0.0).is_finite());
                }
            }
        }
    }
}

#[test]
fn tube610_level_closes_without_boosting_high_frequency_leakage() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for frequency in [100.0, 1_000.0, 10_000.0] {
            let mut p = Preamp::new(3, rate, 0.5, 0.0).unwrap();
            let frames = (rate * 0.25) as usize;
            let mut n = 0;
            let mut previous = f64::INFINITY;
            for gain in [0.5, 0.01, 0.001, 0.0005, 0.0] {
                p.set_controls(gain, 0.0);
                let mut energy = 0.0;
                for i in 0..2 * frames {
                    let x = 0.125 * (n as f64 * std::f64::consts::TAU * frequency / rate).sin();
                    n += 1;
                    let y = p.process(x);
                    assert!(
                        y.abs() < 0.3,
                        "gain change burst at {rate} Hz, gain {gain}: {y}"
                    );
                    if i >= frames {
                        energy += y * y;
                    }
                }
                let rms = (energy / frames as f64).sqrt();
                assert!(rms <= previous * 1.01, "{rate} Hz / {frequency} Hz: closing to {gain} increased RMS {previous} -> {rms}");
                if gain == 0.0 {
                    assert!(rms < 0.001, "closed pot must attenuate: {rms}");
                }
                previous = rms;
            }
        }
    }
}

/// Actual circuit cost; run explicitly with --ignored --nocapture.
#[test]
#[ignore]
fn preamp_throughput() {
    for model in 0..MODELS {
        let mut p = Preamp::new(model, 48_000.0, 0.75, 0.0).unwrap();
        let input: Vec<f64> = (0..48_000)
            .map(|n| 0.125 * (n as f64 * 0.131).sin())
            .collect();
        let start = Instant::now();
        for x in input {
            std::hint::black_box(p.process(x));
        }
        eprintln!(
            "model {model}: {:.1} ms / mono audio second",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Cost by gain and level: per 128-frame block of one channel at 48 kHz
/// (the budget is 2667 us), and Newton passes per solve. A test signal with
/// harmonics and transients (a decaying chord, retriggered). Run with
/// --ignored --nocapture in release mode.
#[test]
#[ignore]
fn preamp_cost_by_gain() {
    const SR: f64 = 48_000.0;
    let signal = |level_db: f64| -> Vec<f64> {
        let amp = 10f64.powf(level_db / 20.0);
        (0..96_000)
            .map(|n| {
                let t = n as f64 / SR;
                let env = (-(t % 0.25) * 12.0).exp();
                let x = [110.0, 220.0 * 1.26, 330.0, 880.0]
                    .iter()
                    .map(|f| (std::f64::consts::TAU * f * t).sin())
                    .sum::<f64>()
                    / 4.0;
                amp * env * x
            })
            .collect()
    };
    let models: Vec<usize> = std::env::var("MODELS")
        .ok()
        .map(|m| m.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| (0..MODELS).collect());
    for model in models {
        for gain in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for level in [-30.0, -18.0, -6.0] {
                let mut p = Preamp::new(model, SR, gain, 0.0).unwrap();
                let input = signal(level);
                let before = p.solver_statistics();
                let mut worst = 0f64;
                let mut total = 0f64;
                for block in input.chunks(128) {
                    let start = Instant::now();
                    for &x in block {
                        std::hint::black_box(p.process(x));
                    }
                    let us = start.elapsed().as_secs_f64() * 1e6;
                    worst = worst.max(us);
                    total += us;
                }
                let after = p.solver_statistics();
                let solves = (after.0 - before.0).max(1);
                eprintln!(
                    "model {model} gain {gain:.2} level {level:>4} dB: mean {:>7.1} us, worst {:>7.1} us / 128 frames, {:.2} passes/solve, {} unsettled",
                    total / (input.len() / 128) as f64,
                    worst,
                    (after.1 - before.1) as f64 / solves as f64,
                    after.2 - before.2,
                );
            }
        }
    }
}

/// Gain is the circuit's gain: the level rises with it on every model, by
/// what [`level_change_db`] says (the circuit's calibration and the British
/// 73's input divider), and the middle of the travel is the calibrated
/// level.
#[test]
fn gain_moves_the_level_as_the_circuit_does() {
    use faderframe_circuit::preamp::{level_change_db, REFERENCE};
    let rate = 48_000.0;
    let level = |model: usize, gain: f64| {
        let mut p = Preamp::new(model, rate, gain, 0.0).unwrap();
        let n = 24_000;
        let mut e = 0.0;
        for i in 0..2 * n {
            let x = 0.125 * (i as f64 * std::f64::consts::TAU * 1_000.0 / rate).sin();
            let y = p.process(x);
            if i >= n {
                e += y * y;
            }
        }
        10.0 * (e / n as f64).log10()
    };
    for model in 0..MODELS {
        let middle = level(model, REFERENCE);
        assert!((-23.0..-19.0).contains(&middle), "model {model}: {middle}");
        let (low, high) = (level(model, 0.1), level(model, 0.9));
        assert!(
            high - low > 9.0,
            "model {model}: the Gain moves the level only {:.1} dB",
            high - low
        );
        for gain in [0.1, 0.3, 0.7] {
            let measured = level(model, gain) - middle;
            let said = level_change_db(model, gain);
            assert!(
                (measured - said).abs() < 2.0,
                "model {model} at {gain}: {measured:.1} dB, said {said:.1}"
            );
        }
    }
    assert_eq!(level_change_db(2, REFERENCE), 0.0);
    // The British 73 reaches below its circuit's range by its divider.
    assert!(level(0, 0.05) < level(0, REFERENCE) - 30.0);
    // 0 % is silence on every model, reached gradually: from a fifth of
    // the travel down the level falls step by step, never at once.
    for model in 0..MODELS {
        assert!(
            level(model, 0.0) < -120.0,
            "model {model}: {}",
            level(model, 0.0)
        );
        let mut last = level(model, 0.2);
        for step in (1..10).rev() {
            let now = level(model, 0.02 * f64::from(step));
            assert!(now < last + 0.5, "model {model}: rises toward 0 %");
            assert!(
                last - now < 15.0,
                "model {model}: falls {} dB at once",
                last - now
            );
            last = now;
        }
    }
}

/// The British 73's output block, driven from the card's trim as on the
/// drawing: its top is open (not the 4 dB down at 20 kHz a 470 k feed
/// made); into a bridging input it is a little brighter, as loud at 1 kHz,
/// and runs out later.
#[test]
fn the_british_73_drives_its_line_like_the_card() {
    use faderframe_circuit::dsp::measure::{run, Tone};
    let rate = 48_000.0;
    let at = |bridging: bool, gain: f64, hz: f64, dbfs: f64| {
        let mut p = Preamp::with_line(0, rate, gain, 0.0, bridging).unwrap();
        let tone = Tone::near(rate, 9600, hz, 10f64.powf(dbfs / 20.0));
        run(tone, 28_800, |x| p.process(x))
    };
    for bridging in [false, true] {
        let reference = at(bridging, 0.5, 1000.0, -40.0).gain_db();
        let top = at(bridging, 0.5, 20_000.0, -40.0).gain_db() - reference;
        let ten = at(bridging, 0.5, 10_000.0, -40.0).gain_db() - reference;
        assert!(
            ten.abs() < 0.6,
            "bridging {bridging}: {ten:+.2} dB at 10 kHz"
        );
        assert!(
            (-1.0..2.5).contains(&top),
            "bridging {bridging}: {top:+.2} dB at 20 kHz"
        );
    }
    // The same level either way.
    let (a, b) = (at(false, 0.5, 1000.0, -30.0), at(true, 0.5, 1000.0, -30.0));
    assert!((a.gain_db() - b.gain_db()).abs() < 0.15);
    // Brighter bridging.
    let top = |bridging| at(bridging, 0.5, 20_000.0, -40.0).gain_db();
    assert!(top(true) > top(false) + 0.3);
    // Driven hard at full gain, the terminated line runs out first.
    let hot = |bridging| at(bridging, 1.0, 1000.0, -12.0).thd_percent();
    assert!(
        hot(false) > 1.5 * hot(true),
        "{} vs {}",
        hot(false),
        hot(true)
    );
}

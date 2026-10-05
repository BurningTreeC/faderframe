use faderframe_circuit::preamp::{Preamp, MODELS};
use std::time::Instant;

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

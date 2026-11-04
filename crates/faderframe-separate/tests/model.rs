//! A tiny X-UMX with random weights (`tests/data/tiny-xumx.pth`, saved by
//! `scripts/separation_reference.py` as asteroid saves models) gives what
//! the reference (asteroid's model, norbert, scipy) gives for the test
//! signal (`reference-tiny.json`): the network's estimates and the stems
//! after zero, one and two EM iterations.
#![allow(clippy::unwrap_used)]

mod common;

use faderframe_separate::torch::Checkpoint;
use faderframe_separate::xumx::XUmx;
use faderframe_separate::{Options, SeparateError, Separator, stft};
use std::path::PathBuf;

fn tiny() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/tiny-xumx.pth")
}

fn reference() -> common::Reference {
    common::reference(include_str!("reference-tiny.json"))
}

#[test]
fn the_checkpoint_reads_as_torch_wrote_it() {
    let s = Separator::load(&tiny()).unwrap();
    let c = s.config();
    assert_eq!(s.sources(), ["bass", "drums", "vocals", "other"]);
    assert_eq!((c.n_fft, c.hop, c.hidden, c.layers), (256, 64, 8, 3));
    assert_eq!((c.max_bin, c.channels, c.sample_rate), (64, 2, 44_100));
}

#[test]
fn the_signal_is_the_scripts() {
    let r = reference();
    let x = common::signal(r.frames, f64::from(r.rate));
    let n = r.frames;
    for ((i, c), want) in [(0, 0), (n / 3, 1), (n - 1, 0)].iter().zip(&r.input_sample) {
        assert!((f64::from(x[*c][*i]) - want).abs() < 1e-6, "{i}");
    }
}

#[test]
fn the_network_estimates_as_the_reference() {
    let r = reference();
    let model = XUmx::load(&Checkpoint::open(&tiny()).unwrap()).unwrap();
    let cfg = &model.config;
    let x = common::signal(r.frames, f64::from(r.rate));
    let w = stft::hann(cfg.n_fft);
    let mag: Vec<Vec<f32>> = x
        .iter()
        .map(|c| stft::forward(c, cfg.n_fft, cfg.hop, &w).magnitude())
        .collect();
    let frames = stft::frames(r.frames, cfg.hop);
    let shape = &r.spectrogram.shape;
    assert_eq!((frames, cfg.bins()), (shape[0], shape[1]));
    let est = model.estimate(&mag, frames, 3, &mut || true).unwrap();
    let bins = cfg.bins();
    let peak = r
        .spectrogram
        .samples
        .iter()
        .fold(0.0f64, |m, s| m.max(s.4.abs()));
    let mut worst = 0.0f64;
    for &(s, f, c, b, want) in &r.spectrogram.samples {
        let got = f64::from(est[s][(f * 2 + c) * bins + b]);
        worst = worst.max((got - want).abs() / (want.abs() + 1e-3 * peak));
    }
    // f32 through three LSTM layers: rounding, no more.
    assert!(worst < 1e-3, "estimates off by {worst} (relative)");
    for (s, sums) in r.spectrogram.frame_sums.iter().enumerate() {
        for (f, want) in sums.iter().enumerate() {
            let got: f64 = est[s][f * 2 * bins..(f + 1) * 2 * bins]
                .iter()
                .map(|v| f64::from(*v))
                .sum();
            assert!(
                (got - want).abs() <= 1e-4 * want.abs() + 1e-3,
                "source {s} frame {f}: {got} vs {want}"
            );
        }
    }
}

#[test]
fn the_stems_are_the_references() {
    let r = reference();
    let sep = Separator::load(&tiny()).unwrap();
    let x = common::signal(r.frames, f64::from(r.rate));
    for iterations in [0usize, 1, 2] {
        let want = &r.stems[&iterations.to_string()];
        let opts = Options {
            iterations,
            threads: 3,
        };
        let stems = sep.separate(&x[0], &x[1], &opts, &mut |_| true).unwrap();
        assert_eq!(stems.len(), 4);
        assert_eq!(stems[0].channels[0].len(), want.length);
        let peak = want.samples.iter().fold(0.0f64, |m, s| m.max(s.3.abs()));
        let mut worst = 0.0f64;
        for &(s, c, i, v) in &want.samples {
            let got = f64::from(stems[s].channels[c][i]);
            worst = worst.max((got - v).abs() / peak);
        }
        assert!(
            worst < 1e-4,
            "{iterations} iterations: samples off by {worst} of the peak"
        );
        let block = want.block;
        for (s, chans) in want.block_rms.iter().enumerate() {
            for (c, blocks) in chans.iter().enumerate() {
                for (k, rms) in blocks.iter().enumerate() {
                    let x = &stems[s].channels[c][k * block..(k + 1) * block];
                    let got = (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / block as f64)
                        .sqrt();
                    assert!(
                        (got - rms).abs() <= 1e-4 * rms + 1e-6,
                        "{iterations} iterations, source {s} channel {c} block {k}: {got} vs {rms}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_stems_sum_back_to_the_mixture() {
    let sep = Separator::load(&tiny()).unwrap();
    let x = common::signal(30_000, 44_100.0);
    let stems = sep
        .separate(&x[0], &x[1], &Options::default(), &mut |_| true)
        .unwrap();
    assert_sums_back(&stems, &x);
}

/// The stems add up to the mixture but for what the filter's
/// regularisation keeps back where every estimate is (nearly) silent:
/// less than −60 dB of it.
fn assert_sums_back(stems: &[faderframe_separate::Stem], x: &[Vec<f32>; 2]) {
    for (c, mix) in x.iter().enumerate() {
        let (mut err, mut all) = (0.0f64, 0.0f64);
        for (i, v) in mix.iter().enumerate() {
            let sum: f32 = stems.iter().map(|s| s.channels[c][i]).sum();
            err += f64::from(sum - v).powi(2);
            all += f64::from(*v).powi(2);
        }
        let ratio = (err / all).sqrt();
        assert!(ratio < 1e-3, "channel {c}: {ratio}");
    }
}

/// Past a segment's length the recording goes in crossfaded segments; the
/// stems still sum back, and progress runs from 0 to 1.
#[test]
fn a_long_recording_goes_in_segments() {
    let sep = Separator::load(&tiny()).unwrap();
    let n = 44_100 * 40 + 123;
    let x = common::signal(n, 44_100.0);
    let mut seen = Vec::new();
    let stems = sep
        .separate(&x[0], &x[1], &Options::default(), &mut |f| {
            seen.push(f);
            true
        })
        .unwrap();
    assert_eq!(stems[0].channels[1].len(), n);
    assert!(seen.windows(2).all(|w| w[1] >= w[0]), "{seen:?}");
    assert_eq!(seen.last().copied(), Some(1.0));
    assert_sums_back(&stems, &x);
}

#[test]
fn a_separation_stops_when_asked() {
    let sep = Separator::load(&tiny()).unwrap();
    let x = common::signal(20_000, 44_100.0);
    let mut calls = 0;
    let r = sep.separate(&x[0], &x[1], &Options::default(), &mut |_| {
        calls += 1;
        calls < 3
    });
    assert!(matches!(r, Err(SeparateError::Cancelled)), "{r:?}");
}

#[test]
fn files_that_are_not_checkpoints_are_refused() {
    let dir = std::env::temp_dir().join(format!("ff-separate-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("model.pth");
    std::fs::write(&p, b"not a zip archive at all").unwrap();
    assert!(Separator::load(&p).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

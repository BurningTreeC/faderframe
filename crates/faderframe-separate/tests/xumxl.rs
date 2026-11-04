//! The published X-UMXL gives what the reference gives for the test
//! signal (`reference-xumxl.json`, from `scripts/separation_reference.py
//! --checkpoint`). Opt-in: the weights are not in the repository.
//!
//!     FADERFRAME_TEST_XUMXL=<pretrained_xumxl.pth> \
//!     cargo test -p faderframe-separate --test xumxl -- --ignored --nocapture
#![allow(clippy::unwrap_used)]

mod common;

use faderframe_separate::torch::Checkpoint;
use faderframe_separate::xumx::XUmx;
use faderframe_separate::{Options, Separator, models, stft};
use std::path::PathBuf;
use std::time::Instant;

fn checkpoint() -> PathBuf {
    PathBuf::from(std::env::var("FADERFRAME_TEST_XUMXL").unwrap())
}

#[test]
#[ignore]
fn the_published_checkpoint_is_the_catalogues() {
    models::verify(&checkpoint(), &models::X_UMXL).unwrap();
}

#[test]
#[ignore]
fn x_umxl_separates_as_the_reference() {
    let r = common::reference(include_str!("reference-xumxl.json"));
    let t0 = Instant::now();
    let model = XUmx::load(&Checkpoint::open(&checkpoint()).unwrap()).unwrap();
    eprintln!("loaded in {:?}", t0.elapsed());
    let cfg = model.config.clone();
    assert_eq!(cfg.sources, r.sources);
    assert_eq!(
        (cfg.n_fft, cfg.hop, cfg.hidden, cfg.max_bin),
        (4096, 1024, 512, 1487)
    );
    let x = common::signal(r.frames, f64::from(r.rate));
    let w = stft::hann(cfg.n_fft);
    let mag: Vec<Vec<f32>> = x
        .iter()
        .map(|c| stft::forward(c, cfg.n_fft, cfg.hop, &w).magnitude())
        .collect();
    let frames = stft::frames(r.frames, cfg.hop);
    let threads = Options::default().threads;
    let est = model.estimate(&mag, frames, threads, &mut || true).unwrap();
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
    eprintln!("estimates: worst relative difference {worst:.2e}");
    assert!(worst < 1e-3, "estimates off by {worst}");
    drop(model);

    let sep = Separator::load(&checkpoint()).unwrap();
    let t1 = Instant::now();
    let stems = sep
        .separate(&x[0], &x[1], &Options::default(), &mut |_| true)
        .unwrap();
    eprintln!(
        "{:.1} s separated in {:?} on {threads} threads",
        r.frames as f64 / f64::from(r.rate),
        t1.elapsed()
    );
    let want = &r.stems["1"];
    let peak = want.samples.iter().fold(0.0f64, |m, s| m.max(s.3.abs()));
    let mut worst = 0.0f64;
    for &(s, c, i, v) in &want.samples {
        worst = worst.max((f64::from(stems[s].channels[c][i]) - v).abs() / peak);
    }
    eprintln!("stems: worst difference {worst:.2e} of the peak");
    assert!(worst < 1e-3, "stems off by {worst} of the peak");
    for (s, chans) in want.block_rms.iter().enumerate() {
        for (c, blocks) in chans.iter().enumerate() {
            for (k, rms) in blocks.iter().enumerate() {
                let b = &stems[s].channels[c][k * want.block..(k + 1) * want.block];
                let got = (b.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>()
                    / want.block as f64)
                    .sqrt();
                assert!(
                    (got - rms).abs() <= 1e-3 * rms + 1e-5,
                    "source {s} channel {c} block {k}: {got} vs {rms}"
                );
            }
        }
    }
}

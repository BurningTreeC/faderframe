use super::*;
use faderframe_project::spectral::SpectralShape;

const RATE: f64 = 48_000.0;

/// `channels` of `frames` frames in memory, read and written through the
/// callbacks.
fn run(edits: &[SpectralEdit], input: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let frames = input[0].len() as i64;
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); input.len()];
    let mut read = |start: i64, buf: &mut [Vec<f32>]| {
        for (c, b) in buf.iter_mut().enumerate() {
            for (i, v) in b.iter_mut().enumerate() {
                let f = start + i as i64;
                *v = if f >= 0 && f < frames {
                    input[c][f as usize]
                } else {
                    0.0
                };
            }
        }
        Ok(())
    };
    let mut write = |chunk: &[&[f32]]| {
        for (o, c) in out.iter_mut().zip(chunk) {
            o.extend_from_slice(c);
        }
        Ok(())
    };
    apply(
        edits,
        input.len(),
        frames,
        RATE,
        &mut read,
        &mut write,
        &mut |_| {},
    )
    .unwrap();
    assert!(out.iter().all(|c| c.len() == frames as usize));
    out
}

fn sine(hz: f64, amp: f32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| amp * (std::f64::consts::TAU * hz * i as f64 / RATE).sin() as f32)
        .collect()
}

fn noise(amp: f32, frames: usize, mut seed: u32) -> Vec<f32> {
    (0..frames)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            amp * (seed as f32 / u32::MAX as f32 * 2.0 - 1.0)
        })
        .collect()
}

/// The amplitude of `hz` in `x[a..b]` (a Hann-windowed DFT).
fn amplitude(x: &[f32], a: usize, b: usize, hz: f64) -> f64 {
    let n = b - a;
    let (mut re, mut im, mut sum) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
        let ph = std::f64::consts::TAU * hz * (a + i) as f64 / RATE;
        re += f64::from(x[a + i]) * w * ph.cos();
        im -= f64::from(x[a + i]) * w * ph.sin();
        sum += w;
    }
    2.0 * (re * re + im * im).sqrt() / sum
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn db(a: f64, b: f64) -> f64 {
    20.0 * (a / b).log10()
}

fn rect(start: f64, end: f64, low: f32, high: f32, op: SpectralOp) -> SpectralEdit {
    SpectralEdit::new(
        SpectralShape::Rect {
            start: (start * RATE) as i64,
            end: (end * RATE) as i64,
            low,
            high,
        },
        op,
    )
}

#[test]
fn without_edits_the_audio_is_copied_bit_for_bit() {
    let input = vec![noise(0.5, 200_000, 1), noise(0.5, 200_000, 2)];
    assert_eq!(run(&[], &input), input);
}

#[test]
fn a_band_is_removed_and_the_rest_is_untouched() {
    let frames = 96_000;
    let x: Vec<f32> = sine(1000.0, 0.25, frames)
        .iter()
        .zip(sine(5000.0, 0.25, frames))
        .map(|(a, b)| a + b)
        .collect();
    let edit = rect(0.5, 1.0, 4000.0, 6000.0, SpectralOp::Remove);
    let y = run(std::slice::from_ref(&edit), std::slice::from_ref(&x));
    let (a, b) = (30_000, 43_000); // 0.625–0.9 s
    let removed = db(amplitude(&y[0], a, b, 5000.0), 0.25);
    assert!(removed < -40.0, "5 kHz: {removed:.1} dB");
    let kept = db(amplitude(&y[0], a, b, 1000.0), 0.25);
    assert!(kept.abs() < 0.1, "1 kHz: {kept:+.2} dB");
    // Outside the region (and a frame and the feather around it), every
    // sample as it was.
    let n = frame_size(RATE);
    let reach = (0.010 * RATE) as usize + n;
    let start = 24_000 - reach;
    assert_eq!(&y[0][..start], &x[..start]);
    assert_eq!(y[0].len(), x.len());
    // Before it: the 5 kHz tone is there.
    assert!(db(amplitude(&y[0], 2_000, 15_000, 5000.0), 0.25).abs() < 0.1);
}

#[test]
fn gain_lifts_only_its_band() {
    let frames = 96_000;
    let x: Vec<f32> = sine(1000.0, 0.1, frames)
        .iter()
        .zip(sine(5000.0, 0.1, frames))
        .map(|(a, b)| a + b)
        .collect();
    let edit = rect(0.3, 1.7, 800.0, 1250.0, SpectralOp::Gain { db: 6.0 });
    let y = run(&[edit], &[x]);
    let (a, b) = (30_000, 66_000);
    let up = db(amplitude(&y[0], a, b, 1000.0), 0.1);
    assert!((up - 6.0).abs() < 0.2, "1 kHz: {up:+.2} dB");
    let same = db(amplitude(&y[0], a, b, 5000.0), 0.1);
    assert!(same.abs() < 0.1, "5 kHz: {same:+.2} dB");
}

#[test]
fn attenuate_brings_a_burst_down_to_the_noise_around_it() {
    let frames = 96_000;
    let mut x = noise(0.05, frames, 7);
    let burst = noise(0.8, 960, 9);
    for (i, v) in burst.iter().enumerate() {
        x[48_000 + i] += v;
    }
    let mut edit = rect(0.995, 1.025, 20.0, 24_000.0, SpectralOp::Attenuate);
    edit.feather_ms = 5.0;
    let y = run(&[edit], &[x.clone()]);
    let around = rms(&x[30_000..44_000]);
    let before = rms(&x[48_000..48_960]);
    let after = rms(&y[0][48_000..48_960]);
    assert!(db(before, around) > 20.0, "the burst stands out");
    assert!(
        db(after, around).abs() < 3.0,
        "brought to the surroundings: {:+.1} dB",
        db(after, around)
    );
    // The noise around it, unchanged.
    assert_eq!(&y[0][..30_000], &x[..30_000]);
}

#[test]
fn heal_fills_a_dropout_from_around_it() {
    let frames = 96_000;
    let mut x = sine(440.0, 0.3, frames);
    for v in &mut x[48_000..50_400] {
        *v = 0.0;
    }
    let edit = rect(0.995, 1.055, 20.0, 24_000.0, SpectralOp::Heal);
    let y = run(&[edit], &[x]);
    let healed = amplitude(&y[0], 48_200, 50_200, 440.0);
    assert!(
        db(healed, 0.3).abs() < 2.0,
        "healed: {:+.1} dB",
        db(healed, 0.3)
    );
}

#[test]
fn an_edit_on_one_channel_leaves_the_other() {
    let frames = 96_000;
    let input = vec![noise(0.3, frames, 3), noise(0.3, frames, 4)];
    let mut edit = rect(0.2, 0.8, 20.0, 24_000.0, SpectralOp::Remove);
    edit.channel = Some(1);
    let y = run(&[edit], &input);
    assert_eq!(y[0], input[0]);
    assert!(rms(&y[1][20_000..36_000]) < 0.3 * rms(&input[1][20_000..36_000]));
}

#[test]
fn a_brush_and_a_lasso_reach_what_they_cover() {
    let frames = 96_000;
    let x = sine(2000.0, 0.25, frames);
    let brush = SpectralEdit::new(
        SpectralShape::Brush {
            points: vec![(24_000, 2000.0), (72_000, 2000.0)],
            radius_ms: 20.0,
            radius_st: 2.0,
        },
        SpectralOp::Remove,
    );
    let y = run(&[brush], std::slice::from_ref(&x));
    assert!(db(amplitude(&y[0], 36_000, 60_000, 2000.0), 0.25) < -40.0);
    let lasso = SpectralEdit::new(
        SpectralShape::Lasso {
            points: vec![
                (24_000, 1500.0),
                (72_000, 1500.0),
                (72_000, 2600.0),
                (24_000, 2600.0),
            ],
        },
        SpectralOp::Gain { db: -12.0 },
    );
    let y = run(&[lasso], &[x]);
    let g = db(amplitude(&y[0], 36_000, 60_000, 2000.0), 0.25);
    assert!((g + 12.0).abs() < 0.3, "{g:+.2} dB");
}

#[test]
fn the_spectrogram_reads_a_sine_at_its_level() {
    let x = [sine(1000.0, 0.5, 96_000)];
    let mut read = |start: i64, buf: &mut [Vec<f32>]| {
        for (i, v) in buf[0].iter_mut().enumerate() {
            let f = start + i as i64;
            *v = if (0..96_000).contains(&f) {
                x[0][f as usize]
            } else {
                0.0
            };
        }
        Ok(())
    };
    let s = Spectrogram::compute(&mut read, 1, 0, 96_000, 64, 256, RATE).unwrap();
    let row = s.row_at(1000.0) as usize;
    let col = 32;
    let peak = (row.saturating_sub(1)..=row + 1)
        .map(|r| s.db(r, col))
        .fold(f32::MIN, f32::max);
    assert!((peak + 6.0).abs() < 1.0, "1 kHz at −6 dBFS reads {peak:.1}");
    let far = s.db(s.row_at(100.0) as usize, col);
    assert!(far < -60.0, "100 Hz reads {far:.1}");
    assert!((s.hz_at(s.row_at(3000.0)) - 3000.0).abs() < 1.0);
}

/// Hiss with a tune in it from 1 s (two notes on and off), the hiss alone
/// before: learn there, remove everywhere.
#[test]
fn learned_noise_is_removed_from_the_whole_source() {
    let frames = (RATE * 5.0) as usize;
    let hiss = noise(0.03, frames, 77);
    let tune: Vec<f32> = (0..frames)
        .map(|i| {
            let t = i as f64 / RATE;
            if t < 1.0 {
                return 0.0;
            }
            let f = if ((t * 2.0) as usize).is_multiple_of(2) {
                330.0
            } else {
                440.0
            };
            (0.3 * (std::f64::consts::TAU * f * t).sin()) as f32
        })
        .collect();
    let input: Vec<f32> = hiss.iter().zip(&tune).map(|(a, b)| a + b).collect();
    let edit = SpectralEdit::new(
        SpectralShape::Rect {
            start: (0.1 * RATE) as i64,
            end: (0.9 * RATE) as i64,
            low: 20.0,
            high: 24_000.0,
        },
        SpectralOp::Denoise { reduction_db: 24.0 },
    );
    let out = run(std::slice::from_ref(&edit), std::slice::from_ref(&input));
    let snr = |x: &[f32], from: usize| {
        let (mut s, mut e) = (0.0f64, 0.0f64);
        for i in from..frames {
            s += f64::from(tune[i]).powi(2);
            e += f64::from(x[i] - tune[i]).powi(2);
        }
        10.0 * (s / e).log10()
    };
    let from = (1.5 * RATE) as usize;
    let before = snr(&input, from);
    let after = snr(&out[0], from);
    assert!(after > before + 10.0, "SNR {before:.1} → {after:.1} dB");
    // The hiss alone is turned down by about the reduction.
    let quiet = |x: &[f32]| {
        x[1_000..40_000]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
    };
    let down = 10.0 * (quiet(&out[0]) / quiet(&input)).log10();
    assert!(down < -18.0, "hiss {down:.1} dB");
}

/// A long source in chunks: the joins do not show, and an edit after the
/// de-noise (a Remove across a chunk's edge) still works.
#[test]
fn a_denoised_source_is_seamless_across_chunks() {
    let frames = (RATE * 45.0) as usize;
    let hiss = noise(0.02, frames, 5);
    let tone = sine(523.25, 0.25, frames);
    let input: Vec<f32> = hiss.iter().zip(&tone).map(|(a, b)| a + b).collect();
    let mut silence_hiss = input.clone();
    // The first half second is hiss alone (to learn from).
    for (v, h) in silence_hiss
        .iter_mut()
        .zip(&hiss)
        .take((0.5 * RATE) as usize)
    {
        *v = *h;
    }
    let denoise = SpectralEdit::new(
        SpectralShape::Rect {
            start: 0,
            end: (0.45 * RATE) as i64,
            low: 20.0,
            high: 24_000.0,
        },
        SpectralOp::Denoise { reduction_db: 18.0 },
    );
    let edge = (CHUNK_SECONDS * RATE) as i64;
    let remove = SpectralEdit::new(
        SpectralShape::Rect {
            start: edge - 4_800,
            end: edge + 4_800,
            low: 400.0,
            high: 700.0,
        },
        SpectralOp::Remove,
    );
    let out = run(&[denoise, remove], &[silence_hiss]);
    let x = &out[0];
    // Around the chunk join at 40 s: no step in the level (50 ms windows).
    let level = |a: usize| {
        let w = (0.05 * RATE) as usize;
        10.0 * (x[a..a + w]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            / w as f64)
            .log10()
    };
    let join = (2.0 * CHUNK_SECONDS * RATE) as usize;
    let (before, after) = (level(join - 2_400), level(join));
    assert!((before - after).abs() < 0.5, "{before:.2} vs {after:.2} dB");
    // The tone is gone where the Remove is, across the first join.
    let mid = edge as usize;
    let rms = |a: usize, b: usize| {
        (x[a..b].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / (b - a) as f64).sqrt()
    };
    assert!(
        rms(mid - 1_000, mid + 1_000) < 0.02,
        "{}",
        rms(mid - 1_000, mid + 1_000)
    );
    assert!(rms(join, join + 4_800) > 0.12);
}

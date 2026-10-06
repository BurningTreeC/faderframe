//! A recording's pitch classes (chroma): how much of each of the twelve
//! notes it holds, for finding its key (`faderframe_midi::theory::
//! detect_key` correlates the profile with the major and minor keys').
//!
//! 4096-point spectra of a copy decimated to about 11 kHz, every 2048
//! samples; each spectral peak from C2 to C7 within 40 dB of the frame's
//! loudest counts towards the pitch class it is nearest, fading out to a
//! quarter tone away (so slightly mistuned recordings still count), by
//! the square root of its magnitude; each frame is normalised (loud and
//! quiet passages count alike, silence not at all).

use crate::fft;

const ANALYSIS_RATE: f64 = 11_025.0;
const FRAME: usize = 4096;
const HOP: usize = 2048;
const LOWEST: f64 = 65.4;
const HIGHEST: f64 = 2_093.0;

/// The pitch-class profile of mono `x` at `rate` (C = 0), summing to one;
/// all zero for silence.
pub fn profile(x: &[f32], rate: f64) -> [f64; 12] {
    let d = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
    let low = crate::melody::decimate(x, d);
    let low_rate = rate / d as f64;
    let mut out = [0.0f64; 12];
    if low.len() < FRAME {
        return out;
    }
    let window: Vec<f64> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FRAME as f64).cos())
        .collect();
    // Each bin's pitch class and weight.
    let bins: Vec<(usize, usize, f64)> = (1..FRAME / 2)
        .filter_map(|b| {
            let f = b as f64 * low_rate / FRAME as f64;
            if !(LOWEST..=HIGHEST).contains(&f) {
                return None;
            }
            let note = 69.0 + 12.0 * (f / 440.0).log2();
            let near = note.round();
            let w = (1.0 - (note - near).abs() * 2.0).max(0.0);
            (w > 0.0).then_some((b, near.rem_euclid(12.0) as usize, w))
        })
        .collect();
    let (mut re, mut im) = (vec![0.0f64; FRAME], vec![0.0f64; FRAME]);
    let mut at = 0;
    while at + FRAME <= low.len() {
        for i in 0..FRAME {
            re[i] = f64::from(low[at + i]) * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        let mag = |b: usize| (re[b] * re[b] + im[b] * im[b]).sqrt();
        let loudest = bins.iter().map(|&(b, ..)| mag(b)).fold(0.0, f64::max);
        let mut frame = [0.0f64; 12];
        for &(b, pc, w) in &bins {
            let m = mag(b);
            if m > loudest * 0.01 && m > mag(b - 1) && m >= mag(b + 1) {
                frame[pc] += w * m.sqrt();
            }
        }
        let sum: f64 = frame.iter().sum();
        if sum > 1e-6 {
            for (o, f) in out.iter_mut().zip(frame) {
                *o += f / sum;
            }
        }
        at += HOP;
    }
    let total: f64 = out.iter().sum();
    if total > 0.0 {
        for o in &mut out {
            *o /= total;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// Chords (MIDI notes) a second each, harmonic tones.
    fn chords(list: &[&[u8]]) -> Vec<f32> {
        let mut x = Vec::new();
        for chord in list {
            for i in 0..SR as usize {
                let t = i as f64 / SR;
                let v: f64 = chord
                    .iter()
                    .map(|n| {
                        let f = 440.0 * 2f64.powf((f64::from(*n) - 69.0) / 12.0);
                        (1..=6)
                            .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64)
                            .sum::<f64>()
                    })
                    .sum();
                x.push((v * 0.1) as f32);
            }
        }
        x
    }

    #[test]
    fn a_progression_shows_its_scale() {
        // C major: C F G C.
        let p = profile(
            &chords(&[
                &[48, 52, 55],
                &[53, 57, 60],
                &[55, 59, 62],
                &[48, 52, 55, 60],
            ]),
            SR,
        );
        let (mut pcs, mut out): (Vec<usize>, Vec<usize>) =
            (0..12).partition(|i| matches!(i, 0 | 2 | 4 | 5 | 7 | 9 | 11));
        pcs.sort_by(|a, b| p[*b].total_cmp(&p[*a]));
        out.sort_by(|a, b| p[*b].total_cmp(&p[*a]));
        // The chord tones lead; nothing outside the scale beats C, E or G.
        for n in [0, 4, 7] {
            assert!(p[n] > p[out[0]] * 1.5, "{n}: {p:?}");
        }
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert_eq!(profile(&vec![0.0; 96_000], SR), [0.0; 12]);
    }
}

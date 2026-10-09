//! The pitch of each note of a chord over time: for every analysis frame,
//! the frequency within 70 cents of the note's key whose harmonics hold the
//! most energy (a weighted harmonic sum over the spectrum, refined by a
//! parabola), or nothing where the note is no longer heard.

use crate::fft::Fft;
use crate::render::{together, window_for};
use std::collections::HashMap;
use std::f64::consts::PI;

/// A note to follow: where it sounds (source frames) and its key (MIDI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Heard {
    pub start: i64,
    pub end: i64,
    pub key: f32,
}

/// Harmonics weighed.
const HARMONICS: u32 = 8;
/// The search: ± cents, in steps of.
const REACH_CENTS: i32 = 70;
const STEP_CENTS: i32 = 2;

/// Each note's pitch (MIDI, NaN where not heard) at `start + i * hop`.
pub fn pitch_tracks(mono: &[f32], rate: f64, notes: &[Heard], hop: usize) -> Vec<Vec<f32>> {
    // Each note's window: the long one where it sounds with others.
    let spans: Vec<(i64, i64)> = notes.iter().map(|h| (h.start, h.end)).collect();
    let windows: Vec<usize> = notes
        .iter()
        .map(|h| {
            window_for(
                rate,
                crate::hz(f64::from(h.key) - 1.0),
                together(&spans, h.start, h.end, rate),
            )
        })
        .collect();
    let mut plans: HashMap<usize, (Fft, Vec<f64>)> = HashMap::new();
    for &n in &windows {
        plans.entry(n).or_insert_with(|| {
            (
                Fft::new(n),
                (0..n)
                    .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos())
                    .collect(),
            )
        });
    }
    let mut out: Vec<Vec<f32>> = notes
        .iter()
        .map(|h| vec![f32::NAN; ((h.end - h.start).max(0) as usize) / hop.max(1) + 1])
        .collect();
    let longest = windows.iter().copied().max().unwrap_or(256);
    let (mut re, mut im) = (vec![0.0; longest], vec![0.0; longest]);
    let mut mag = vec![0.0; longest / 2 + 1];
    // Every note's frames (its own grid from its start), by time: each
    // spectrum made once for all the notes sounding then.
    let hop = hop.max(1) as i64;
    let mut wanted: Vec<(i64, usize, usize)> = notes
        .iter()
        .enumerate()
        .flat_map(|(i, h)| {
            let n = windows[i];
            (0..)
                .map(move |k| h.start + k * hop)
                .take_while(move |t| *t <= h.end)
                .map(move |t| (t, n, i))
        })
        .collect();
    wanted.sort_unstable();
    let mut at_time = 0;
    while at_time < wanted.len() {
        let (t, n) = (wanted[at_time].0, wanted[at_time].1);
        let mut until = at_time;
        while until < wanted.len() && wanted[until].0 == t && wanted[until].1 == n {
            until += 1;
        }
        let here: Vec<usize> = wanted[at_time..until].iter().map(|w| w.2).collect();
        at_time = until;
        let Some((fft, window)) = plans.get(&n) else {
            continue;
        };
        let bin_hz = rate / n as f64;
        let (re, im, mag) = (&mut re[..n], &mut im[..n], &mut mag[..=n / 2]);
        let half = n as i64 / 2;
        for i in 0..n {
            let s = t - half + i as i64;
            re[i] = if s >= 0 && (s as usize) < mono.len() {
                f64::from(mono[s as usize]) * window[i]
            } else {
                0.0
            };
            im[i] = 0.0;
        }
        fft.run(re, im, false);
        for k in 0..=n / 2 {
            mag[k] = (re[k] * re[k] + im[k] * im[k]).sqrt();
        }
        let loudest = mag.iter().copied().fold(0.0, f64::max);
        let at = |f: f64| -> f64 {
            let x = f / bin_hz;
            let i = x.floor() as usize;
            if i + 1 >= mag.len() {
                return 0.0;
            }
            let fr = x - i as f64;
            mag[i] * (1.0 - fr) + mag[i + 1] * fr
        };
        let sum = |f: f64| -> f64 {
            (1..=HARMONICS)
                .map(|h| at(f * f64::from(h)) / f64::from(h).sqrt())
                .sum()
        };
        for i in here {
            let h = notes[i];
            let base = 440.0 * 2f64.powf((f64::from(h.key) - 69.0) / 12.0);
            let grid: Vec<(i32, f64)> = (-REACH_CENTS..=REACH_CENTS)
                .step_by(STEP_CENTS as usize)
                .map(|c| (c, sum(base * 2f64.powf(f64::from(c) / 1200.0))))
                .collect();
            let Some(best) = (0..grid.len()).max_by(|a, b| grid[*a].1.total_cmp(&grid[*b].1))
            else {
                continue;
            };
            let (c, v) = grid[best];
            // Heard: its fundamental region well above the frame's floor.
            if v < loudest * 0.02 {
                continue;
            }
            let shift = if best > 0 && best + 1 < grid.len() {
                let (l, r) = (grid[best - 1].1, grid[best + 1].1);
                let d = l - 2.0 * v + r;
                if d.abs() > 1e-12 {
                    (0.5 * (l - r) / d).clamp(-0.5, 0.5)
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let coarse = base * 2f64.powf((f64::from(c) + shift * f64::from(STEP_CENTS)) / 1200.0);
            // Then each harmonic's own peak (between bins), the shared ones
            // left out, weighed by harmonic number: higher ones place the
            // pitch more finely.
            let others: Vec<f64> = notes
                .iter()
                .enumerate()
                .filter(|(j, o)| *j != i && o.start <= t && t <= o.end)
                .map(|(_, o)| crate::hz(f64::from(o.key)))
                .collect();
            let (mut num, mut den) = (0.0, 0.0);
            for hh in 1..=HARMONICS {
                let target = coarse * f64::from(hh) / bin_hz;
                let k0 = target.round() as usize;
                if k0 < 2 || k0 + 2 >= mag.len() {
                    continue;
                }
                let Some(k) = (k0 - 1..=k0 + 1).max_by(|a, b| mag[*a].total_cmp(&mag[*b])) else {
                    continue;
                };
                if mag[k] < mag[k - 1] || mag[k] < mag[k + 1] || mag[k] < loudest * 1e-4 {
                    continue;
                }
                let (l, m, r) = (
                    mag[k - 1].max(1e-30).ln(),
                    mag[k].max(1e-30).ln(),
                    mag[k + 1].max(1e-30).ln(),
                );
                let d = l - 2.0 * m + r;
                let p = k as f64
                    + if d.abs() > 1e-12 {
                        (0.5 * (l - r) / d).clamp(-0.5, 0.5)
                    } else {
                        0.0
                    };
                if (p - target).abs() > (target * 0.01).max(1.0) {
                    continue;
                }
                let shared = others.iter().any(|f| {
                    (1..=HARMONICS * 2).any(|g| (f * f64::from(g) / bin_hz - p).abs() < 2.0)
                });
                if shared {
                    continue;
                }
                let w = mag[k] * f64::from(hh);
                num += w * p * bin_hz / f64::from(hh);
                den += w;
            }
            let f0 = if den > 0.0 { num / den } else { coarse };
            let k = ((t - h.start) / hop) as usize;
            if let Some(slot) = out[i].get_mut(k) {
                *slot = (69.0 + 12.0 * (f0 / 440.0).log2()) as f32;
            }
        }
    }
    out
}

//! Moving each note of a chord on its own.
//!
//! An STFT (Hann, about 85 ms, hop an eighth of it). In every frame each
//! sounding note's harmonics are found as peaks of the spectrum near the
//! multiples of its pitch; a peak two notes share (an octave, a fifth's
//! common partials) is split between them by their levels. Every peak owns
//! the bins around it (to halfway to the next peak, at most four bins: the
//! window's main lobe and first side lobes); those bins move by the
//! owning note's ratio — shifted by whole bins, with their phase turned on
//! by the frequency change from frame to frame so the partial comes out at
//! exactly its new frequency (Laroche and Dolson's peak-locked shifting) —
//! and are scaled by the note's gain. Bins no note owns (noise, breath,
//! attacks) stay where they are.
//!
//! Only the spans of notes that change are processed; around each span the
//! result is crossfaded into the original over half a window, and outside
//! them the output is the input sample for sample.

use crate::fft::Fft;
use std::collections::HashMap;
use std::f64::consts::PI;

/// Bins either side of a peak it takes with it.
const REACH: usize = 4;
/// Partials looked for (and how high).
const MAX_HARMONICS: u32 = 64;
const MAX_PARTIAL_HZ: f64 = 14_000.0;

/// A note to move.
#[derive(Clone, Debug, PartialEq)]
pub struct Moved {
    /// Where it sounds (source frames).
    pub start: i64,
    pub end: i64,
    /// Source frames per value of the curves below.
    pub hop: usize,
    /// Its pitch (Hz) at `start + i * hop`; 0 where it is not heard.
    pub f0: Vec<f32>,
    /// The ratio to move it by (1: as played).
    pub ratio: Vec<f32>,
    /// Its gain (linear; 0 removes it).
    pub gain: Vec<f32>,
}

impl Moved {
    /// A curve's value at source frame `t` (between its points), `None`
    /// outside the note.
    fn at(&self, v: &[f32], t: f64) -> Option<f32> {
        if t < self.start as f64 || t > self.end as f64 || v.is_empty() {
            return None;
        }
        let x = (t - self.start as f64) / self.hop.max(1) as f64;
        let i = x.floor() as usize;
        if i + 1 >= v.len() {
            return v.last().copied();
        }
        let f = (x - i as f64) as f32;
        Some(v[i] + (v[i + 1] - v[i]) * f)
    }

    /// Whether it changes anything.
    pub fn changes(&self) -> bool {
        self.ratio.iter().any(|r| (r - 1.0).abs() > 1e-6)
            || self.gain.iter().any(|g| (g - 1.0).abs() > 1e-6)
    }
}

/// The analysis window at `rate` (about 85 ms, a power of two).
pub fn frame_size(rate: f64) -> usize {
    ((rate * 0.085).round() as usize)
        .next_power_of_two()
        .max(256)
}

/// The window for notes as low as `lowest_hz`. Where notes sound together
/// the long one ([`frame_size`]): close partials of different notes stay
/// apart. A note alone gets about four and a half of its periods (at least
/// 20 ms): time follows vibrato and glides closely.
pub fn window_for(rate: f64, lowest_hz: f64, together: bool) -> usize {
    if together {
        return frame_size(rate);
    }
    let secs = (4.5 / lowest_hz.max(20.0)).clamp(0.02, 0.085);
    ((rate * secs).round() as usize)
        .next_power_of_two()
        .clamp(256, frame_size(rate))
}

/// Whether notes sound together (by more than 20 ms) within `a..b`.
pub fn together(starts_ends: &[(i64, i64)], a: i64, b: i64, rate: f64) -> bool {
    let near = (0.02 * rate) as i64;
    let here: Vec<(i64, i64)> = starts_ends
        .iter()
        .copied()
        .filter(|(s, e)| *s < b && *e > a)
        .collect();
    here.iter().enumerate().any(|(i, x)| {
        here[i + 1..]
            .iter()
            .any(|y| x.1.min(y.1) - x.0.max(y.0) > near)
    })
}

/// The window [`render`] uses over `a..b`.
fn window_over(rate: f64, notes: &[Moved], a: i64, b: i64) -> usize {
    let near: Vec<&Moved> = notes.iter().filter(|n| n.start < b && n.end > a).collect();
    let lowest = near
        .iter()
        .flat_map(|n| n.f0.iter().copied())
        .filter(|f| *f > 0.0)
        .fold(f32::MAX, f32::min);
    let spans: Vec<(i64, i64)> = near.iter().map(|n| (n.start, n.end)).collect();
    window_for(rate, f64::from(lowest), together(&spans, a, b, rate))
}

/// The spans moved notes change (source frames, merged, with the longest
/// window's reach either side; clipped to `len`).
pub fn spans(notes: &[Moved], rate: f64, len: i64) -> Vec<(i64, i64)> {
    let half = frame_size(rate) as i64 / 2;
    let mut s: Vec<(i64, i64)> = notes
        .iter()
        .filter(|n| n.changes())
        .map(|n| ((n.start - half).max(0), (n.end + half).min(len)))
        .filter(|(a, b)| b > a)
        .collect();
    s.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (a, b) in s {
        match out.last_mut() {
            Some(last) if a <= last.1 + 2 * half => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// A note sounding in a frame: which, its pitch, ratio and gain.
#[derive(Clone, Copy, Debug)]
struct Sounding {
    note: usize,
    f0: f64,
    ratio: f64,
    gain: f64,
}

/// A spectral peak and the notes sharing it (note, harmonic, share).
struct Peak {
    bin: usize,
    owners: Vec<(usize, u32, f64)>,
}

struct Work {
    n: usize,
    hop: usize,
    rate: f64,
    fft: Fft,
    window: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    yre: Vec<f64>,
    yim: Vec<f64>,
    mag: Vec<f64>,
    owned: Vec<bool>,
}

impl Work {
    fn new(rate: f64, n: usize) -> Self {
        Self {
            n,
            hop: n / 8,
            rate,
            fft: Fft::new(n),
            window: (0..n)
                .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos())
                .collect(),
            re: vec![0.0; n],
            im: vec![0.0; n],
            yre: vec![0.0; n],
            yim: vec![0.0; n],
            mag: vec![0.0; n / 2 + 1],
            owned: vec![false; n / 2 + 1],
        }
    }

    /// Move the notes in the frame now in `re`/`im` (centred at source
    /// frame `t`); `phase` keeps each partial's turned phase.
    fn frame(&mut self, t: f64, notes: &[Moved], phase: &mut HashMap<(usize, u32), f64>) {
        let n = self.n;
        let bins = n / 2;
        let bin_hz = self.rate / n as f64;
        let sounding: Vec<Sounding> = notes
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let f0 = f64::from(m.at(&m.f0, t)?);
                (f0 > 0.0).then(|| Sounding {
                    note: i,
                    f0,
                    ratio: f64::from(m.at(&m.ratio, t).unwrap_or(1.0)),
                    gain: f64::from(m.at(&m.gain, t).unwrap_or(1.0)),
                })
            })
            .collect();
        if sounding
            .iter()
            .all(|s| (s.ratio - 1.0).abs() < 1e-9 && (s.gain - 1.0).abs() < 1e-9)
        {
            return;
        }
        for k in 0..=bins {
            self.mag[k] = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt();
        }
        let loudest = self.mag.iter().copied().fold(0.0, f64::max);
        let floor = loudest * 1e-5;
        // Each note's partials as candidate peaks.
        let mut found: Vec<(usize, usize, u32)> = Vec::new();
        let top = (self.rate * 0.45).min(MAX_PARTIAL_HZ);
        for (si, s) in sounding.iter().enumerate() {
            for h in 1..=MAX_HARMONICS {
                let f = f64::from(h) * s.f0;
                if f >= top {
                    break;
                }
                let c = f / bin_hz;
                let tol = (c * 0.012).max(1.5);
                let lo = ((c - tol).floor().max(1.0)) as usize;
                let hi = ((c + tol).ceil() as usize).min(bins - 1);
                let Some(k) = (lo..=hi).max_by(|a, b| self.mag[*a].total_cmp(&self.mag[*b])) else {
                    continue;
                };
                let m = self.mag[k];
                if m > floor && m >= self.mag[k - 1] && m >= self.mag[k + 1] {
                    found.push((k, si, h));
                }
            }
        }
        // Each note's level from the partials it does not share.
        let level: Vec<f64> = (0..sounding.len())
            .map(|si| {
                let own: Vec<f64> = found
                    .iter()
                    .filter(|(k, s, _)| {
                        *s == si
                            && !found
                                .iter()
                                .any(|(k2, s2, _)| *s2 != si && k.abs_diff(*k2) <= 1)
                    })
                    .map(|(k, _, h)| self.mag[*k] * f64::from(*h))
                    .collect();
                if own.is_empty() {
                    1.0
                } else {
                    own.iter().sum::<f64>() / own.len() as f64
                }
            })
            .collect();
        found.sort_by_key(|(k, _, _)| *k);
        let mut peaks: Vec<Peak> = Vec::new();
        for (k, si, h) in found {
            let w = level[si] / f64::from(h);
            match peaks.last_mut() {
                Some(p) if k.abs_diff(p.bin) <= 1 => {
                    if self.mag[k] > self.mag[p.bin] {
                        p.bin = k;
                    }
                    p.owners.push((si, h, w));
                }
                _ => peaks.push(Peak {
                    bin: k,
                    owners: vec![(si, h, w)],
                }),
            }
        }
        // Every bin stays, but those a peak takes along.
        self.owned.fill(false);
        self.yre.fill(0.0);
        self.yim.fill(0.0);
        let regions: Vec<(usize, usize)> = (0..peaks.len())
            .map(|i| {
                let k = peaks[i].bin;
                let lo = match i.checked_sub(1) {
                    Some(j) => (peaks[j].bin + k) / 2 + 1,
                    None => 0,
                }
                .max(k.saturating_sub(REACH));
                let hi = match peaks.get(i + 1) {
                    Some(p) => (k + p.bin) / 2,
                    None => bins,
                }
                .min(k + REACH);
                (lo, hi)
            })
            .collect();
        for &(lo, hi) in &regions {
            for o in &mut self.owned[lo..=hi] {
                *o = true;
            }
        }
        for k in 0..=bins {
            if !self.owned[k] {
                self.yre[k] = self.re[k];
                self.yim[k] = self.im[k];
            }
        }
        for (p, &(lo, hi)) in peaks.iter().zip(&regions) {
            let k = p.bin;
            // The peak's frequency between bins (a parabola through the
            // log magnitudes).
            let (l, c, r) = (
                self.mag[k - 1].max(1e-30).ln(),
                self.mag[k].max(1e-30).ln(),
                self.mag[k + 1].max(1e-30).ln(),
            );
            let d = l - 2.0 * c + r;
            let frac = if d.abs() > 1e-12 {
                (0.5 * (l - r) / d).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            let f = (k as f64 + frac) * bin_hz;
            let total: f64 = p.owners.iter().map(|o| o.2).sum::<f64>().max(1e-30);
            for &(si, h, w) in &p.owners {
                let s = sounding[si];
                let df = f * (s.ratio - 1.0);
                let dk = (df / bin_hz).round() as i64;
                let th = phase.entry((s.note, h)).or_insert(0.0);
                *th = (*th + 2.0 * PI * df * self.hop as f64 / self.rate) % (2.0 * PI);
                let (sin, cos) = th.sin_cos();
                let g = s.gain * w / total;
                let (gr, gi) = (cos * g, sin * g);
                for kk in lo..=hi {
                    let to = kk as i64 + dk;
                    if to < 0 || to > bins as i64 {
                        continue;
                    }
                    let to = to as usize;
                    let (xr, xi) = (self.re[kk], self.im[kk]);
                    self.yre[to] += xr * gr - xi * gi;
                    self.yim[to] += xr * gi + xi * gr;
                }
            }
        }
        // The other half: the conjugate mirror.
        self.yim[0] = 0.0;
        self.yim[bins] = 0.0;
        for k in 1..bins {
            self.yre[n - k] = self.yre[k];
            self.yim[n - k] = -self.yim[k];
        }
        self.re.copy_from_slice(&self.yre);
        self.im.copy_from_slice(&self.yim);
    }
}

/// Render `channels` (one source's) with `notes` moved; `progress` hears
/// the share done. Outside [`spans`] the output is the input.
pub fn render(
    channels: &[Vec<f32>],
    rate: f64,
    notes: &[Moved],
    progress: &mut dyn FnMut(f32),
) -> Vec<Vec<f32>> {
    let len = channels.iter().map(Vec::len).max().unwrap_or(0) as i64;
    let mut out: Vec<Vec<f32>> = channels.to_vec();
    let spans = spans(notes, rate, len);
    if spans.is_empty() {
        return out;
    }
    let total: i64 = spans.iter().map(|(a, b)| b - a).sum::<i64>() * channels.len() as i64;
    let mut done = 0i64;
    for &(a, b) in &spans {
        let mut w = Work::new(rate, window_over(rate, notes, a, b));
        let (n, hop) = (w.n as i64, w.hop as i64);
        let half = n / 2;
        let norm = 3.0 * n as f64 / (8.0 * hop as f64);
        // Frames centred from a window before the span to one after it:
        // every sample of the span (and its fades) fully overlapped.
        let c0 = a - n;
        let c1 = b + n;
        let base = c0 - half;
        let mut y = vec![0.0f64; (c1 - c0 + n) as usize];
        for (ch, src) in channels.iter().enumerate() {
            y.fill(0.0);
            let mut phase: HashMap<(usize, u32), f64> = HashMap::new();
            let mut c = c0;
            while c <= c1 {
                for i in 0..w.n {
                    let t = c - half + i as i64;
                    let x = if t >= 0 && t < src.len() as i64 {
                        f64::from(src[t as usize])
                    } else {
                        0.0
                    };
                    w.re[i] = x * w.window[i];
                    w.im[i] = 0.0;
                }
                w.fft.run(&mut w.re, &mut w.im, false);
                w.frame(c as f64, notes, &mut phase);
                w.fft.run(&mut w.re, &mut w.im, true);
                let at = (c - half - base) as usize;
                for i in 0..w.n {
                    y[at + i] += w.re[i] / n as f64 * w.window[i] / norm;
                }
                c += hop;
            }
            // Into the original: faded in before the span's notes and out
            // after them, over half a window.
            let dst = &mut out[ch];
            for t in a.max(0)..b.min(dst.len() as i64) {
                let g = if t < a + half {
                    fade((t - a) as f64 / half as f64)
                } else if t >= b - half {
                    fade((b - t) as f64 / half as f64)
                } else {
                    1.0
                };
                let v = y[(t - base) as usize];
                let o = f64::from(dst[t as usize]);
                dst[t as usize] = (o + (v - o) * g) as f32;
            }
            done += b - a;
            progress(done as f32 / total.max(1) as f32);
        }
    }
    out
}

/// A raised-cosine fade, 0 → 1 over 0 → 1.
fn fade(x: f64) -> f64 {
    0.5 - 0.5 * (PI * x.clamp(0.0, 1.0)).cos()
}

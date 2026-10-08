//! Spectral editing: [`apply`] runs a source through a short-time Fourier
//! transform where its edits reach and writes the result — everything the
//! edits leave alone is copied bit for bit — and [`Spectrogram`] makes the
//! picture the editor shows.
//!
//! Each edit is its own pass, in order, over the span it reaches: frames
//! are Hann-windowed, a quarter frame apart, as long as fits the edit (up
//! to about 85 ms, [`frame_size`]; a click gets short frames, so they do
//! not smear it over their length), each bin weighted by the edit's shape
//! and feather ([`mask`]). Gain and Remove scale the bins; Attenuate brings
//! them down to the level around the edit in time where they stand out;
//! Heal replaces them by it — the level before and after, interpolated per
//! bin in dB, each bin's phase carried on from the sound before (so a tone
//! continues through a dropout).

#![forbid(unsafe_code)]

pub mod mask;
pub mod spectrogram;

pub use mask::Mask;
pub use spectrogram::Spectrogram;

use faderframe_project::spectral::{SpectralEdit, SpectralOp};
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

/// Frames of surroundings averaged on each side for Attenuate and Heal.
const AROUND: usize = 8;
/// Frames copied per step outside the edits.
const CHUNK: usize = 1 << 16;
/// The shortest frame.
const MIN_FRAME: usize = 256;

/// The longest analysis frame at `rate`: about 85 ms (4096 at 44.1 and
/// 48 kHz).
pub fn frame_size(rate: f64) -> usize {
    ((rate * 0.085) as usize)
        .next_power_of_two()
        .clamp(512, 16384)
}

/// The frame for an edit: the largest power of two within its length (its
/// own extent, not the feather), between [`MIN_FRAME`] and
/// [`frame_size`].
pub fn frame_for(edit: &SpectralEdit, rate: f64) -> usize {
    let (a, b) = edit.shape.frames();
    let longest = frame_size(rate);
    let length = (b - a).max(1) as usize;
    if length >= longest {
        return longest;
    }
    let mut n = longest;
    while n > MIN_FRAME && n > length {
        n /= 2;
    }
    n
}

/// Reads frames `start..start + out[c].len()` of every channel (zeros
/// outside the source).
pub type Read<'a> = dyn FnMut(i64, &mut [Vec<f32>]) -> Result<(), String> + 'a;
/// Takes the next frames of every channel, in order.
pub type Write<'a> = dyn FnMut(&[&[f32]]) -> Result<(), String> + 'a;

/// A periodic Hann window.
fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
        .collect()
}

/// FFTs of one size and their buffers.
struct Stft {
    n: usize,
    hop: usize,
    window: Vec<f32>,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    spectrum: Vec<Complex32>,
    /// Each bin's frequency in octaves.
    octaves: Vec<f64>,
}

impl Stft {
    fn new(n: usize, rate: f64) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let octaves = (0..=n / 2)
            .map(|k| (k as f64 * rate / n as f64).max(1e-3).log2())
            .collect();
        Self {
            n,
            hop: n / 4,
            window: hann(n),
            time: fwd.make_input_vec(),
            spectrum: fwd.make_output_vec(),
            fwd,
            inv,
            octaves,
        }
    }

    /// The spectrum of `x` (n samples), windowed.
    fn analyse(&mut self, x: &[f32]) {
        for ((t, &v), &w) in self.time.iter_mut().zip(x).zip(&self.window) {
            *t = v * w;
        }
        let _ = self.fwd.process(&mut self.time, &mut self.spectrum);
    }

    /// The (unscaled) inverse of the spectrum into `time`.
    fn synthesise(&mut self) {
        let last = self.spectrum.len() - 1;
        self.spectrum[0].im = 0.0;
        self.spectrum[last].im = 0.0;
        let _ = self.inv.process(&mut self.spectrum, &mut self.time);
    }
}

/// Wrap a phase into −π…π.
fn principal(p: f32) -> f32 {
    p - TAU * ((p + PI) / TAU).floor()
}

/// The sound on one side of an edit: its mean magnitude per bin, and for
/// carrying phases on, the phase of the frame nearest the edit, the
/// frame's start and each bin's phase advance per hop.
struct Side {
    level: Vec<f32>,
    at: f64,
    phase: Vec<f32>,
    start: i64,
    advance: Vec<f32>,
}

/// Measure `AROUND` frames starting at `starts` (in order away from or
/// towards the edit; `nearest` is the index of the one next to it) in `x`
/// (`origin` its first source frame). `None` when they leave the source.
fn side(
    stft: &mut Stft,
    x: &[f32],
    origin: i64,
    frames: i64,
    starts: &[i64],
    nearest: usize,
) -> Option<Side> {
    let n = stft.n;
    let bins = n / 2 + 1;
    let mut level = vec![0.0f32; bins];
    let mut phases: Vec<Option<Vec<f32>>> = vec![None; starts.len()];
    let mut count = 0;
    for (j, &s) in starts.iter().enumerate() {
        if s < 0 || s + n as i64 > frames || s < origin || (s - origin) as usize + n > x.len() {
            continue;
        }
        let i = (s - origin) as usize;
        stft.analyse(&x[i..i + n]);
        for (l, c) in level.iter_mut().zip(&stft.spectrum) {
            *l += c.norm();
        }
        phases[j] = Some(stft.spectrum.iter().map(|c| c.im.atan2(c.re)).collect());
        count += 1;
    }
    if count == 0 {
        return None;
    }
    for l in &mut level {
        *l /= count as f32;
    }
    let phase = phases[nearest].clone()?;
    // Advance per hop from the nearest frame and its neighbour (the
    // expected advance for a bin's own frequency, corrected).
    let hop = stft.hop as f32;
    let expected: Vec<f32> = (0..bins).map(|k| TAU * k as f32 * hop / n as f32).collect();
    let neighbour = if nearest > 0 {
        nearest - 1
    } else {
        nearest + 1
    };
    let advance = match phases.get(neighbour).and_then(Option::as_ref) {
        Some(other) => {
            // Order: earlier frame → later frame.
            let (early, late) = if starts[neighbour] < starts[nearest] {
                (other, &phase)
            } else {
                (&phase, other)
            };
            (0..bins)
                .map(|k| expected[k] + principal(late[k] - early[k] - expected[k]))
                .collect()
        }
        None => expected,
    };
    let start = starts[nearest];
    let mid = starts.iter().sum::<i64>() as f64 / starts.len() as f64 + n as f64 / 2.0;
    Some(Side {
        level,
        at: mid,
        phase,
        start,
        advance,
    })
}

/// A cheap deterministic noise (phases where nothing can be carried on).
fn random_phase(state: &mut u32) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state as f32 / u32::MAX as f32 * TAU
}

/// The frames an edit reaches (its region widened by a frame each side).
fn reach(edit: &SpectralEdit, rate: f64) -> (Mask, usize, i64, i64) {
    let mask = Mask::new(edit, rate);
    let n = frame_for(edit, rate);
    let (a, b) = (
        mask.from.floor() as i64 - n as i64,
        mask.to.ceil() as i64 + n as i64,
    );
    (mask, n, a, b)
}

/// Run one edit over channel `x` (source frames from `origin`).
fn edit_channel(
    edit: &SpectralEdit,
    mask: &Mask,
    stft: &mut Stft,
    x: &mut [f32],
    origin: i64,
    frames: i64,
    seed: &mut u32,
) {
    let n = stft.n;
    let ni = n as i64;
    let hop = stft.hop as i64;
    let bins = n / 2 + 1;
    // Its surroundings, for Attenuate and Heal: frames wholly before the
    // region (the last one ending where it starts) and wholly after it.
    let (before, after) = if matches!(edit.op, SpectralOp::Attenuate | SpectralOp::Heal) {
        let last = mask.from.floor() as i64 - ni;
        let starts: Vec<i64> = (0..AROUND as i64)
            .map(|j| last - (AROUND as i64 - 1 - j) * hop)
            .collect();
        let before = side(stft, x, origin, frames, &starts, AROUND - 1);
        let first = mask.to.ceil() as i64;
        let starts: Vec<i64> = (0..AROUND as i64).map(|j| first + j * hop).collect();
        let after = side(stft, x, origin, frames, &starts, 0);
        (before, after)
    } else {
        (None, None)
    };
    let mut level = vec![0.0f32; bins];
    let surroundings = |t: f64, out: &mut [f32]| -> bool {
        match (&before, &after) {
            (Some(b), Some(a)) => {
                let k = ((t - b.at) / (a.at - b.at).max(1.0)).clamp(0.0, 1.0) as f32;
                for ((o, &x), &y) in out.iter_mut().zip(&b.level).zip(&a.level) {
                    let (x, y) = (x.max(1e-12).ln(), y.max(1e-12).ln());
                    *o = (x + (y - x) * k).exp();
                }
                true
            }
            (Some(s), None) | (None, Some(s)) => {
                out.copy_from_slice(&s.level);
                true
            }
            (None, None) => false,
        }
    };
    // Where to carry phases on from: the sound before, else after.
    let carrier = before.as_ref().or(after.as_ref());
    // The span written: the region and a frame each side, inside the
    // buffer (which has a frame more of margin).
    let wa = (mask.from.floor() as i64 - ni).max(origin + ni);
    let wb = (mask.to.ceil() as i64 + ni).min(origin + x.len() as i64 - ni);
    if wa >= wb {
        return;
    }
    let first = wa - ni;
    let len = (wb + ni - first) as usize;
    let base = (first - origin) as usize;
    let mut acc = vec![0.0f32; len];
    let mut norm = vec![0.0f32; len];
    let mut weights = vec![0.0f32; bins];
    let mut f = 0usize;
    while (f as i64) + first < wb {
        let start = first + f as i64;
        let centre = start as f64 + n as f64 / 2.0;
        let frame = &x[base + f..base + f + n];
        if !mask.covers(centre) {
            for i in 0..n {
                let w = stft.window[i];
                acc[f + i] += frame[i] * w * w;
                norm[f + i] += w * w;
            }
            f += hop as usize;
            continue;
        }
        stft.analyse(frame);
        mask.weights(centre, &stft.octaves, &mut weights);
        let around = surroundings(centre, &mut level);
        for (k, (c, &w)) in stft.spectrum.iter_mut().zip(&weights).enumerate() {
            if w == 0.0 {
                continue;
            }
            let s = level[k];
            match edit.op {
                SpectralOp::Gain { db } => *c *= 1.0 + w * (10f32.powf(db / 20.0) - 1.0),
                SpectralOp::Remove => *c *= 1.0 - w,
                SpectralOp::Attenuate if around => {
                    let m = c.norm();
                    if m > s {
                        *c *= 1.0 + w * (s / m - 1.0);
                    }
                }
                SpectralOp::Heal if around => {
                    let phase = match carrier {
                        Some(side) => {
                            let hops = (start - side.start) as f32 / hop as f32;
                            side.phase[k] + side.advance[k] * hops
                        }
                        None => random_phase(seed),
                    };
                    *c = *c * (1.0 - w) + Complex32::from_polar(w * s, phase);
                }
                SpectralOp::Attenuate | SpectralOp::Heal => {}
            }
        }
        stft.synthesise();
        let scale = 1.0 / n as f32;
        for i in 0..n {
            let w = stft.window[i];
            acc[f + i] += stft.time[i] * scale * w;
            norm[f + i] += w * w;
        }
        f += hop as usize;
    }
    // Back into the channel: the written span only.
    for i in (wa - first) as usize..(wb - first) as usize {
        if norm[i] > 1e-6 {
            x[base + i] = acc[i] / norm[i];
        }
    }
}

/// Apply `edits` (in order) to a source of `frames` frames and `channels`
/// channels at `rate`: `read` gives its audio, `write` takes the result in
/// order (exactly `frames` frames). `progress` hears 0…1.
pub fn apply(
    edits: &[SpectralEdit],
    channels: usize,
    frames: i64,
    rate: f64,
    read: &mut Read<'_>,
    write: &mut Write<'_>,
    progress: &mut dyn FnMut(f32),
) -> Result<(), String> {
    let reached: Vec<(Mask, usize, i64, i64)> = edits.iter().map(|e| reach(e, rate)).collect();
    // The spans the edits change, merged.
    let mut spans: Vec<(i64, i64)> = reached
        .iter()
        .map(|(_, _, a, b)| ((*a).max(0), (*b).min(frames)))
        .filter(|(a, b)| a < b)
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let total = frames.max(1) as f32;
    let mut pos = 0i64;
    let mut chunk = vec![vec![0.0f32; CHUNK]; channels];
    let mut copy = |from: i64, to: i64, read: &mut Read<'_>, write: &mut Write<'_>| {
        let mut at = from;
        while at < to {
            let len = CHUNK.min((to - at) as usize);
            for c in chunk.iter_mut() {
                c.resize(len, 0.0);
            }
            read(at, &mut chunk)?;
            let refs: Vec<&[f32]> = chunk.iter().map(|c| &c[..len]).collect();
            write(&refs)?;
            at += len as i64;
        }
        Ok::<(), String>(())
    };
    let mut seed = 0x9E37_79B9u32;
    let mut stfts: Vec<Stft> = Vec::new();
    for (wa, wb) in merged {
        copy(pos, wa, read, write)?;
        progress(wa as f32 / total);
        // The edits in this span, and the margin their frames and
        // surroundings need.
        let inside: Vec<usize> = (0..edits.len())
            .filter(|&i| reached[i].2 < wb && reached[i].3 > wa)
            .collect();
        let margin = inside
            .iter()
            .map(|&i| {
                let n = reached[i].1 as i64;
                2 * n + (AROUND as i64 + 2) * n / 4
            })
            .max()
            .unwrap_or(0);
        let origin = wa - margin;
        let len = (wb + margin - origin) as usize;
        let mut buffer = vec![vec![0.0f32; len]; channels];
        read(origin, &mut buffer)?;
        for &i in &inside {
            let (mask, n, _, _) = &reached[i];
            let stft = match stfts.iter().position(|s| s.n == *n) {
                Some(k) => &mut stfts[k],
                None => {
                    stfts.push(Stft::new(*n, rate));
                    let last = stfts.len() - 1;
                    &mut stfts[last]
                }
            };
            for (c, x) in buffer.iter_mut().enumerate() {
                if edits[i].channel.is_none_or(|e| usize::from(e) == c) {
                    edit_channel(&edits[i], mask, stft, x, origin, frames, &mut seed);
                }
            }
        }
        // (Channels no edit touched are exactly as read.)
        let out: Vec<&[f32]> = buffer
            .iter()
            .map(|x| &x[(wa - origin) as usize..(wb - origin) as usize])
            .collect();
        write(&out)?;
        pos = wb;
    }
    copy(pos, frames, read, write)?;
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests;

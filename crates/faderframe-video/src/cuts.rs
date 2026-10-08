//! Cut detection: where a picture changes shot. Frames are decoded in
//! order at a tiny size and compared by their colour histograms (16 soft
//! bins a channel); a frame whose histogram differs from the one before by much
//! more than the picture's own frame-to-frame motion starts a new shot.
//! Gradual changes (fades, dissolves) are not cuts.

use crate::{Decoder, Result, VideoError};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Histogram bins (16 per channel).
const BINS: usize = 48;

/// Colour histogram, each value shared between the two nearest bins (so a
/// flat picture whose brightness wobbles across a bin's edge changes it a
/// little, not all at once).
fn histogram(rgba: &[u8]) -> [f32; BINS] {
    let mut h = [0f32; BINS];
    let mut n = 0f32;
    let mut add = |base: usize, v: u8| {
        let x = (v as f32 / 16.0 - 0.5).clamp(0.0, 15.0);
        let lo = x.floor() as usize;
        let frac = x - lo as f32;
        h[base + lo] += 1.0 - frac;
        if lo + 1 < 16 {
            h[base + lo + 1] += frac;
        }
    };
    for px in rgba.as_chunks::<4>().0 {
        add(0, px[0]);
        add(16, px[1]);
        add(32, px[2]);
        n += 1.0;
    }
    if n > 0.0 {
        for v in &mut h {
            *v /= n;
        }
    }
    h
}

/// How different two histograms are: 0 (same) to 2 (nothing in common),
/// the channels averaged.
fn distance(a: &[f32; BINS], b: &[f32; BINS]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / 3.0
}

/// The starts (ns, the file's timeline) of the shots after the first, of
/// the picture of `path` between `from` and `to` (ns). `progress`: the
/// share done.
pub fn detect_cuts(
    path: &Path,
    from: i64,
    to: i64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f64),
) -> Result<Vec<i64>> {
    let mut dec = Decoder::open(path, 64, 36)?;
    dec.play_from(from)?;
    let mut prev: Option<[f32; BINS]> = None;
    // Recent frame-to-frame distances (the picture's own motion).
    let mut recent: Vec<f32> = Vec::new();
    let mut cuts = Vec::new();
    let span = (to - from).max(1) as f64;
    while let Some(f) = dec.next_frame()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(VideoError::Cancelled);
        }
        if f.time >= to {
            break;
        }
        progress(((f.time - from) as f64 / span).clamp(0.0, 1.0));
        let h = histogram(&f.rgba);
        if let Some(p) = prev {
            let d = distance(&p, &h);
            let mut sorted = recent.clone();
            sorted.sort_by(f32::total_cmp);
            let typical = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
            // Far beyond the motion so far, and a real change of picture.
            if d > 0.25 && d > 4.0 * typical + 0.1 && f.time > from {
                cuts.push(f.time);
            } else {
                recent.push(d);
                if recent.len() > 24 {
                    recent.remove(0);
                }
            }
        }
        prev = Some(h);
    }
    Ok(cuts)
}

/// Every frame's look for matching two pictures (see
/// `faderframe_conform::shots`): its start (ns) and its luma at 16×9. With
/// where the picture ends. `progress`: the share done.
pub fn signatures(
    path: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f64),
) -> Result<(Vec<i64>, Vec<[u8; 144]>, i64)> {
    let duration = crate::probe::probe(path)?.duration_ns.max(1);
    let mut dec = Decoder::open(path, 64, 36)?;
    dec.play_from(0)?;
    let (mut times, mut sigs) = (Vec::new(), Vec::new());
    let mut end = 0;
    while let Some(f) = dec.next_frame()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(VideoError::Cancelled);
        }
        progress((f.time as f64 / duration as f64).clamp(0.0, 1.0));
        let mut sig = [0u8; 144];
        let (w, h) = (f.width as usize, f.height as usize);
        for (i, v) in sig.iter_mut().enumerate() {
            let (gx, gy) = (i % 16, i / 16);
            let (x0, x1) = (gx * w / 16, ((gx + 1) * w / 16).max(gx * w / 16 + 1));
            let (y0, y1) = (gy * h / 9, ((gy + 1) * h / 9).max(gy * h / 9 + 1));
            let mut sum = 0u32;
            let mut n = 0u32;
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let p = f.pixel(x as u32, y as u32);
                    sum += (299 * p[0] as u32 + 587 * p[1] as u32 + 114 * p[2] as u32) / 1000;
                    n += 1;
                }
            }
            *v = (sum / n.max(1)) as u8;
        }
        if let Some(&last) = times.last()
            && f.time <= last
        {
            continue;
        }
        end = end.max(f.time);
        times.push(f.time);
        sigs.push(sig);
    }
    // The last frame lasts as long as the one before.
    if times.len() >= 2 {
        end += times[times.len() - 1] - times[times.len() - 2];
    }
    Ok((times, sigs, end))
}

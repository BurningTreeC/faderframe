//! Previews' audio made on a worker: a file decoded, resampled to the
//! stream's rate and — to play in the project's tempo or key — stretched
//! and transposed (Signalsmith Stretch, offline), published chunk by chunk
//! into the engine's [`PreviewAudio`] while it plays.

use faderframe_audio_files::import::ImportError;
use faderframe_audio_files::resample::StreamResampler;
use faderframe_engine::nodes::PreviewWriter;
use faderframe_stretch::{Preset, Stretcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The longest a preview plays (seconds of its audio kept).
pub const MAX_PREVIEW_SECONDS: f64 = 600.0;
/// Output frames stretched at a time.
const STRETCH_CHUNK: usize = 2048;

/// How a preview's audio is changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change {
    /// Input frames played per output frame (1: as it is; 1.25: faster).
    pub speed: f64,
    /// Transposition (semitones).
    pub semitones: f32,
    /// Stretch for drums (transients kept) rather than for anything.
    pub rhythmic: bool,
}

impl Change {
    pub const NONE: Change = Change {
        speed: 1.0,
        semitones: 0.0,
        rhythmic: false,
    };

    pub fn is_none(&self) -> bool {
        (self.speed - 1.0).abs() < 1e-9 && self.semitones == 0.0
    }
}

/// A worker filling a preview.
pub struct PreviewJob {
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl PreviewJob {
    /// Decode `path` at `rate` into `writer`, changed as `change` says.
    pub fn spawn(path: PathBuf, rate: u32, change: Change, writer: PreviewWriter) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let c = Arc::clone(&cancel);
        let handle = std::thread::Builder::new()
            .name("faderframe-preview".into())
            .spawn(move || fill(&path, rate, change, writer, &c))
            .ok();
        Self { cancel, handle }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    /// The worker's outcome (once finished).
    pub fn join(mut self) -> Result<(), String> {
        match self.handle.take() {
            Some(h) => h
                .join()
                .unwrap_or_else(|_| Err("the preview stopped".into())),
            None => Err("the preview could not start".into()),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for PreviewJob {
    fn drop(&mut self) {
        // Never waited for here: a cancelled worker ends on its own.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Stretches a stream of input offline: output frame `u` comes from input
/// `u · speed` (the stretcher's latencies taken out), so the output is the
/// input's length / speed.
pub struct OfflineStretch {
    stretcher: Stretcher,
    speed: f64,
    channels: usize,
    /// Input not yet fed, from input frame `base`.
    input: Vec<Vec<f32>>,
    base: usize,
    /// The next input frame to feed (fractional).
    feed: f64,
    /// Input frames given in all.
    taken: usize,
    /// Output frames made (and handed on).
    made: usize,
    primed: bool,
    out: Vec<Vec<f32>>,
}

impl OfflineStretch {
    pub fn new(channels: usize, rate: u32, change: Change) -> Option<Self> {
        let preset = if change.rhythmic {
            Preset::Rhythmic
        } else {
            Preset::Polyphonic
        };
        let channels = channels.clamp(1, 2);
        let mut stretcher = Stretcher::new(channels, f64::from(rate), preset)?;
        stretcher.set_transpose(2f32.powf(change.semitones / 12.0));
        Some(Self {
            stretcher,
            speed: change.speed.clamp(0.25, 4.0),
            channels,
            input: vec![Vec::new(); channels],
            base: 0,
            feed: 0.0,
            taken: 0,
            made: 0,
            primed: false,
            out: vec![vec![0.0; STRETCH_CHUNK]; channels],
        })
    }

    /// Input frames `[from, from + n)` (zeros past what was given, and
    /// before the start).
    fn slice(&self, c: usize, from: i64, n: usize, finished: bool, scratch: &mut Vec<f32>) {
        scratch.clear();
        scratch.resize(n, 0.0);
        let have = &self.input[c];
        for (i, v) in scratch.iter_mut().enumerate() {
            let f = from + i as i64;
            if f >= self.base as i64 {
                let k = f as usize - self.base;
                if let Some(x) = have.get(k) {
                    *v = *x;
                } else if !finished {
                    break;
                }
            }
        }
    }

    /// Feed `n` frames of `planes` (a mono source feeds every channel);
    /// what can be made is handed to `sink`.
    pub fn push(&mut self, planes: &[&[f32]], n: usize, sink: &mut dyn FnMut(&[&[f32]], usize)) {
        for c in 0..self.channels {
            let src = planes[c.min(planes.len().saturating_sub(1))];
            self.input[c].extend_from_slice(&src[..n.min(src.len())]);
        }
        self.taken += n;
        self.run(false, sink);
    }

    /// Everything is in: the rest made (up to the input's length / speed).
    pub fn finish(&mut self, sink: &mut dyn FnMut(&[&[f32]], usize)) {
        self.run(true, sink);
    }

    fn run(&mut self, finished: bool, sink: &mut dyn FnMut(&[&[f32]], usize)) {
        let li = self.stretcher.input_latency();
        let lo = self.stretcher.output_latency();
        let mut scratch: Vec<Vec<f32>> = vec![Vec::new(); self.channels];
        if !self.primed {
            if self.taken < li && !finished {
                return;
            }
            // Pre-roll ending where output frame 0's analysis is fed.
            let pre = self.stretcher.seek_length();
            for (c, s) in scratch.iter_mut().enumerate() {
                self.slice(c, li as i64 - pre as i64, pre, true, s);
            }
            let ins: Vec<&[f32]> = scratch.iter().map(Vec::as_slice).collect();
            self.stretcher.seek(&ins, self.speed);
            self.feed = li as f64;
            // The output latency made and dropped.
            let mut left = lo;
            while left > 0 {
                let k = left.min(STRETCH_CHUNK);
                self.step(k, finished, &mut scratch);
                left -= k;
            }
            self.primed = true;
        }
        let total = (self.taken as f64 / self.speed).round() as usize;
        loop {
            let mut k = STRETCH_CHUNK;
            if finished {
                if self.made >= total {
                    break;
                }
                k = k.min(total - self.made);
            } else {
                // Only what the input given so far covers.
                let need = self.feed + k as f64 * self.speed + li as f64;
                if need > self.taken as f64 {
                    break;
                }
            }
            self.step(k, finished, &mut scratch);
            let outs: Vec<&[f32]> = self.out.iter().map(|o| &o[..k]).collect();
            sink(&outs, k);
            self.made += k;
        }
        // Input fed long ago is let go (the pre-roll is done with).
        let keep_from = (self.feed.floor() as usize).saturating_sub(li);
        if keep_from > self.base {
            let drop = keep_from - self.base;
            for c in &mut self.input {
                c.drain(..drop.min(c.len()));
            }
            self.base = keep_from;
        }
    }

    /// `k` output frames into `self.out` from the input at `feed`.
    fn step(&mut self, k: usize, finished: bool, scratch: &mut [Vec<f32>]) {
        let from = self.feed.floor() as i64;
        let next = self.feed + k as f64 * self.speed;
        let n = (next.floor() as i64 - from).max(0) as usize;
        for (c, s) in scratch.iter_mut().enumerate() {
            self.slice(c, from, n, finished, s);
        }
        let ins: Vec<&[f32]> = scratch.iter().map(Vec::as_slice).collect();
        let mut outs: Vec<&mut [f32]> = self.out.iter_mut().map(|o| &mut o[..k]).collect();
        self.stretcher.process(&ins, n, &mut outs, k);
        self.feed = next;
    }
}

/// The worker: decode, resample, change, publish.
fn fill(
    path: &Path,
    rate: u32,
    change: Change,
    writer: PreviewWriter,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use std::cell::RefCell;
    let writer = RefCell::new(writer);
    let resampler: RefCell<Option<StreamResampler>> = RefCell::new(None);
    let stretch: RefCell<Option<OfflineStretch>> = RefCell::new(None);
    let limit = (f64::from(rate) * MAX_PREVIEW_SECONDS * change.speed.max(1.0)) as u64;
    let taken = std::cell::Cell::new(0u64);
    let full = std::cell::Cell::new(false);
    // Resampled (or not) frames on to the stretcher (or not), then out.
    let out = |planes: &[&[f32]], n: usize| {
        let mut w = writer.borrow_mut();
        let mut sink = |p: &[&[f32]], k: usize| w.push(p, k);
        match stretch.borrow_mut().as_mut() {
            Some(s) => s.push(planes, n, &mut sink),
            None => sink(planes, n),
        }
    };
    let result = faderframe_audio_files::decode::decode_file(
        path,
        cancel,
        |info| {
            let ch = info.channels.clamp(1, 2);
            if info.sample_rate != rate {
                *resampler.borrow_mut() = Some(StreamResampler::new(info.sample_rate, rate, ch)?);
            }
            if !change.is_none() {
                *stretch.borrow_mut() = OfflineStretch::new(ch, rate, change);
            }
            Ok(())
        },
        |planes, n| {
            let views: Vec<&[f32]> = planes.iter().take(2).map(Vec::as_slice).collect();
            match resampler.borrow_mut().as_mut() {
                Some(r) => r.push(planes, n, &mut |p: &[&[f32]], k| {
                    out(p, k);
                    Ok(())
                })?,
                None => out(&views, n),
            }
            taken.set(taken.get() + n as u64);
            if taken.get() >= limit || writer.borrow().full() {
                full.set(true);
                return Err(ImportError::Cancelled);
            }
            Ok(())
        },
    );
    match result {
        Ok(_) => {}
        Err(ImportError::Cancelled) if full.get() => {}
        Err(e) => return Err(e.to_string()),
    }
    if let Some(r) = resampler.borrow_mut().take() {
        r.finish(&mut |p: &[&[f32]], k| {
            out(p, k);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }
    if let Some(s) = stretch.borrow_mut().as_mut() {
        let mut w = writer.borrow_mut();
        s.finish(&mut |p: &[&[f32]], k| w.push(p, k));
    }
    writer.into_inner().finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn frequency(x: &[f32], rate: f64) -> f64 {
        let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f64 * rate / x.len() as f64
    }

    fn stretched(change: Change, input: &[f32], piece: usize) -> Vec<f32> {
        let mut s = OfflineStretch::new(1, 48_000, change).unwrap();
        let mut out = Vec::new();
        let mut sink = |p: &[&[f32]], k: usize| out.extend_from_slice(&p[0][..k]);
        for chunk in input.chunks(piece) {
            s.push(&[chunk], chunk.len(), &mut sink);
        }
        s.finish(&mut sink);
        out
    }

    #[test]
    fn a_stream_stretches_to_its_length_and_keeps_or_moves_its_pitch() {
        let rate = 48_000.0;
        let x: Vec<f32> = (0..48_000)
            .map(|i| (std::f64::consts::TAU * 440.0 * i as f64 / rate).sin() as f32 * 0.5)
            .collect();
        // Slower (a 100 BPM loop at 80 BPM), pitch kept; however it comes in.
        for piece in [1000, 48_000] {
            let out = stretched(
                Change {
                    speed: 0.8,
                    semitones: 0.0,
                    rhythmic: false,
                },
                &x,
                piece,
            );
            assert_eq!(out.len(), 60_000, "piece {piece}");
            let f = frequency(&out[10_000..50_000], rate);
            assert!((f - 440.0).abs() < 4.0, "{f}");
        }
        // Up a fourth, its length kept.
        let out = stretched(
            Change {
                speed: 1.0,
                semitones: 5.0,
                rhythmic: false,
            },
            &x,
            4096,
        );
        assert_eq!(out.len(), 48_000);
        let f = frequency(&out[8_000..40_000], rate);
        let want = 440.0 * 2f64.powf(5.0 / 12.0);
        assert!((f - want).abs() < 5.0, "{f} vs {want}");
        // No time lost at the start (the latencies taken out): the sound is
        // there from the first frames.
        let lead = out[..2_000].iter().map(|v| v.abs()).fold(0.0, f32::max);
        assert!(lead > 0.2, "{lead}");
    }
}

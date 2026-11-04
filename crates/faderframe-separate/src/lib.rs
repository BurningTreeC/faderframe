//! Stem separation: a stereo recording split into its sources (vocals,
//! drums, bass, the rest) by Sony's CrossNet-Open-Unmix in plain Rust.
//!
//! The pipeline is asteroid's X-UMX recipe (`egs/musdb18/X-UMX/eval.py`):
//! the mixture's STFT ([`stft`], as `torch.stft` makes it), the network's
//! magnitude estimates per source ([`xumx`]), norbert's multichannel
//! Wiener filter from the mixture's phase ([`wiener`], one EM iteration
//! by default), and the inverse STFT (as `scipy.signal.istft`). The
//! weights are read straight from the published PyTorch checkpoint
//! ([`torch`]: zip and pickle read as data, nothing in them run), which
//! the session downloads on request ([`models`]).
//!
//! Long recordings go through in segments of [`SEGMENT_SECONDS`] with
//! [`MARGIN_SECONDS`] of context either side, crossfaded over
//! [`FADE_SECONDS`] around each boundary, so memory stays bounded and the
//! network sees half a minute at a time (the reference takes a whole song
//! at once; a recording up to a segment long is separated exactly as it
//! does). With at least one EM iteration the stems sum back to the
//! mixture.

#![forbid(unsafe_code)]

pub mod models;
mod nn;
pub mod stft;
pub mod torch;
pub mod wiener;
pub mod xumx;

use realfft::num_complex::Complex;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum SeparateError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("the separation model: {0}")]
    Format(String),
    #[error("{0}")]
    Input(String),
    #[error("cancelled")]
    Cancelled,
}

/// The network sees this much at a time (seconds).
pub const SEGMENT_SECONDS: f64 = 30.0;
/// Context either side of a segment, not used itself (seconds).
pub const MARGIN_SECONDS: f64 = 2.0;
/// Neighbouring segments cross over this long around their boundary.
pub const FADE_SECONDS: f64 = 1.0;

/// How to separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Wiener filter EM iterations: 0 = the magnitudes with the mixture's
    /// phase (the stems no longer sum to the mixture), 1 = the reference's
    /// setting, more refine the stereo image further.
    pub iterations: usize,
    /// Threads to compute on.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            iterations: 1,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        }
    }
}

/// One separated source.
#[derive(Clone, Debug, PartialEq)]
pub struct Stem {
    pub name: String,
    pub channels: [Vec<f32>; 2],
}

pub struct Separator {
    model: xumx::XUmx,
}

/// A segment: the span it is computed over and the core it stands for
/// (sample positions, multiples of the hop).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Piece {
    from: usize,
    to: usize,
    core: (usize, usize),
}

/// The segments of a padded recording `len` long (a multiple of `hop`).
fn plan(len: usize, rate: f64, hop: usize) -> Vec<Piece> {
    let round = |s: f64| ((s * rate / hop as f64).round() as usize).max(1) * hop;
    let (seg, margin) = (round(SEGMENT_SECONDS), round(MARGIN_SECONDS));
    if len <= seg + 2 * margin {
        return vec![Piece {
            from: 0,
            to: len,
            core: (0, len),
        }];
    }
    let mut out = Vec::new();
    let mut a = 0;
    while a < len {
        let mut b = (a + seg).min(len);
        // No sliver of a last segment: the one before takes it.
        if len - b < margin {
            b = len;
        }
        out.push(Piece {
            from: a.saturating_sub(margin),
            to: (b + margin).min(len),
            core: (a, b),
        });
        a = b;
    }
    out
}

/// A piece's weight at sample `i`: rising over the fade around its core's
/// start, falling around its end (sin² and cos², so neighbours sum to 1).
fn weight(p: &Piece, i: usize, len: usize, fade: usize) -> f32 {
    let half = fade / 2;
    let ramp = |x: f64| {
        (x.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2)
            .sin()
            .powi(2)
    };
    let mut w = 1.0f64;
    if p.core.0 > 0 {
        w *= ramp((i as f64 - (p.core.0 as f64 - half as f64)) / fade as f64);
    }
    if p.core.1 < len {
        w *= 1.0 - ramp((i as f64 - (p.core.1 as f64 - half as f64)) / fade as f64);
    }
    w as f32
}

impl Separator {
    /// Load an X-UMX checkpoint as asteroid saves it.
    pub fn load(path: &Path) -> Result<Self, SeparateError> {
        let ck = torch::Checkpoint::open(path)?;
        Ok(Self {
            model: xumx::XUmx::load(&ck)?,
        })
    }

    pub fn config(&self) -> &xumx::Config {
        &self.model.config
    }

    /// The sources, in the order [`Self::separate`] gives them.
    pub fn sources(&self) -> &[String] {
        &self.model.config.sources
    }

    /// The rate the model separates at.
    pub fn sample_rate(&self) -> u32 {
        self.model.config.sample_rate
    }

    /// Separate a stereo recording at [`Self::sample_rate`]; `progress`
    /// hears how far it is (0…1) and answers whether to go on.
    pub fn separate(
        &self,
        left: &[f32],
        right: &[f32],
        options: &Options,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<Vec<Stem>, SeparateError> {
        if left.len() != right.len() {
            return Err(SeparateError::Input("the channels differ in length".into()));
        }
        let cfg = &self.model.config;
        let (n_fft, hop) = (cfg.n_fft, cfg.hop);
        let len = left.len();
        // Padded with silence to whole hops, and to more than a window.
        let padded = len.max(n_fft + hop).div_ceil(hop) * hop;
        let pad = |x: &[f32]| {
            let mut v = x.to_vec();
            v.resize(padded, 0.0);
            v
        };
        let input = [pad(left), pad(right)];
        let rate = f64::from(cfg.sample_rate);
        let pieces = plan(padded, rate, hop);
        let fade = ((FADE_SECONDS * rate) as usize).max(2);
        let ns = cfg.sources.len();
        let mut out: Vec<[Vec<f32>; 2]> = (0..ns)
            .map(|_| [vec![0.0f32; padded], vec![0.0f32; padded]])
            .collect();
        let total: usize = pieces.iter().map(|p| p.to - p.from).sum();
        let mut done = 0usize;
        let stages = self.model.stages() + 2;
        for p in &pieces {
            let span = p.to - p.from;
            let mut stage = 0usize;
            let mut step = || {
                stage += 1;
                let f = (done as f64 + span as f64 * stage as f64 / stages as f64) / total as f64;
                progress(f as f32)
            };
            let stems = self.segment(
                [&input[0][p.from..p.to], &input[1][p.from..p.to]],
                options,
                &mut step,
            )?;
            for (s, stem) in stems.iter().enumerate() {
                for c in 0..2 {
                    let dst = &mut out[s][c];
                    for (k, v) in stem[c].iter().enumerate() {
                        let i = p.from + k;
                        let w = if pieces.len() == 1 {
                            1.0
                        } else {
                            weight(p, i, padded, fade)
                        };
                        if w > 0.0 {
                            dst[i] += v * w;
                        }
                    }
                }
            }
            done += span;
        }
        progress(1.0);
        Ok(out
            .into_iter()
            .zip(&cfg.sources)
            .map(|([mut l, mut r], name)| {
                l.truncate(len);
                r.truncate(len);
                Stem {
                    name: name.clone(),
                    channels: [l, r],
                }
            })
            .collect())
    }

    /// One segment through the whole pipeline: per source, two channels
    /// as long as the segment.
    fn segment(
        &self,
        x: [&[f32]; 2],
        options: &Options,
        step: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<[Vec<f32>; 2]>, SeparateError> {
        let cfg = &self.model.config;
        let (n_fft, hop, threads) = (cfg.n_fft, cfg.hop, options.threads.max(1));
        let window = stft::hann(n_fft);
        let spec = [
            stft::forward(x[0], n_fft, hop, &window),
            stft::forward(x[1], n_fft, hop, &window),
        ];
        let frames = spec[0].frames;
        let mag = [spec[0].magnitude(), spec[1].magnitude()];
        let estimates = self
            .model
            .estimate(&mag, frames, threads, step)
            .ok_or(SeparateError::Cancelled)?;
        drop(mag);
        let filter = wiener::Wiener::fit(&spec, &estimates, options.iterations, threads);
        if !step() {
            return Err(SeparateError::Cancelled);
        }
        // Frames separated and inverse-transformed in runs, each run into
        // a buffer of its own, then overlap-added together.
        let ns = estimates.len();
        let streams = ns * 2;
        let bins = cfg.bins();
        const RUN: usize = 32;
        let runs: Vec<usize> = (0..frames.div_ceil(RUN)).collect();
        let run_len = (RUN - 1) * hop + n_fft;
        let mut buffers: Vec<Vec<Vec<f64>>> = runs
            .iter()
            .map(|_| vec![vec![0.0f64; run_len]; streams])
            .collect();
        {
            let jobs: Vec<(usize, &mut Vec<Vec<f64>>)> =
                runs.iter().copied().zip(buffers.iter_mut()).collect();
            nn::parallel(threads, jobs, |(run, buf)| {
                let mut inverse = stft::Inverse::new(n_fft, hop);
                let mut separated = vec![vec![Complex::<f64>::default(); bins]; streams];
                let first = run * RUN;
                for t in first..(first + RUN).min(frames) {
                    filter.frame(&spec, &estimates, t, &mut separated);
                    for (k, s) in separated.iter().enumerate() {
                        inverse.spectrum().copy_from_slice(s);
                        inverse.add(t, first, &mut buf[k]);
                    }
                }
            });
        }
        if !step() {
            return Err(SeparateError::Cancelled);
        }
        let inverse = stft::Inverse::new(n_fft, hop);
        let full = n_fft + (frames - 1) * hop;
        let len = x[0].len();
        let mut out: Vec<[Vec<f32>; 2]> = (0..ns)
            .map(|_| [Vec::with_capacity(len), Vec::with_capacity(len)])
            .collect();
        let half = n_fft / 2;
        let norms: Vec<f64> = (0..len).map(|i| inverse.norm(i + half, frames)).collect();
        let mut sum = vec![0.0f64; full];
        for k in 0..streams {
            sum.iter_mut().for_each(|v| *v = 0.0);
            for (run, buf) in buffers.iter().enumerate() {
                let at = run * RUN * hop;
                for (i, v) in buf[k].iter().enumerate() {
                    if at + i < full {
                        sum[at + i] += v;
                    }
                }
            }
            let dst = &mut out[k / 2][k % 2];
            for (i, norm) in norms.iter().enumerate() {
                let v = sum.get(i + half).copied().unwrap_or(0.0);
                dst.push(if *norm > 1e-10 { v / norm } else { v } as f32);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_recording_is_one_segment() {
        let p = plan(44_100 * 20, 44_100.0, 1024);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].core, (0, 44_100 * 20));
    }

    #[test]
    fn segments_cover_the_recording_and_weigh_one_together() {
        let hop = 1024;
        let len = (44_100 * 95 / hop) * hop;
        let p = plan(len, 44_100.0, hop);
        assert!(p.len() >= 3, "{p:?}");
        assert_eq!(p[0].core.0, 0);
        assert_eq!(p.last().unwrap().core.1, len);
        for w in p.windows(2) {
            assert_eq!(w[0].core.1, w[1].core.0);
            assert!(w[0].to > w[1].core.0 + 22_050, "margin past the boundary");
        }
        let fade = 44_100;
        for i in (0..len).step_by(997) {
            let total: f32 = p
                .iter()
                .filter(|q| (q.from..q.to).contains(&i))
                .map(|q| weight(q, i, len, fade))
                .sum();
            assert!((total - 1.0).abs() < 1e-5, "{i}: {total}");
            // Nothing is taken from outside a segment's span.
            for q in &p {
                if !(q.from..q.to).contains(&i) {
                    assert_eq!(weight(q, i, len, fade), 0.0, "{i} {q:?}");
                }
            }
        }
    }
}

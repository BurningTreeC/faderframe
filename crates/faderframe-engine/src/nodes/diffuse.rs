//! The diffuse field of a diffuse spread
//! (`faderframe_core::surround::diffuse_gain`): the source's channels,
//! summed, through a decorrelation filter of their own for every main
//! speaker of the bed. Each filter is four Schroeder all-pass sections
//! in a row — flat in level at every frequency, the phase scrambled by
//! delays that differ per speaker (about 2–20 ms), so the speakers get
//! copies that are not correlated with each other. Nothing is delayed as
//! a whole (no latency); transients are smeared over the filters' tails.
//! Made off the audio thread (its delay lines), only for tracks whose
//! spread is diffuse.

use faderframe_audio_graph::AudioBuffer;
use faderframe_core::ChannelLayout;
use faderframe_core::surround::speakers_of;

/// The all-pass sections' feedback.
const FEEDBACK: f32 = 0.5;
/// The sections' delays (ms), before each speaker's own offset.
const DELAYS_MS: [f32; 4] = [1.7, 3.7, 6.1, 9.3];
/// How much later each speaker's sections are than the one before
/// (ms × the section's number), so no two speakers share a filter.
const SPREAD_MS: f32 = 0.23;
/// Frames summed at a time (on the stack).
const CHUNK: usize = 64;

/// One Schroeder all-pass section: `v = x + g·v[n−D]`, `y = v[n−D] − g·v`.
struct Section {
    line: Vec<f32>,
    at: usize,
}

impl Section {
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let delayed = self.line[self.at];
        let v = x + FEEDBACK * delayed;
        self.line[self.at] = if v.abs() < 1e-20 { 0.0 } else { v };
        self.at += 1;
        if self.at == self.line.len() {
            self.at = 0;
        }
        delayed - FEEDBACK * v
    }
}

/// Decorrelation filters for the main speakers of a bed.
pub(crate) struct Diffuser {
    /// Per output channel: its filter (`None`: the LFE).
    chains: Vec<Option<[Section; 4]>>,
}

impl Diffuser {
    /// For the main speakers of `output` at `rate` (`None` when it is not
    /// a bed).
    pub(crate) fn new(output: ChannelLayout, rate: f64) -> Option<Self> {
        let speakers = speakers_of(output)?;
        let ms = |v: f32| ((f64::from(v) * rate / 1000.0).round() as usize).max(1);
        let chains = speakers
            .iter()
            .enumerate()
            .map(|(d, s)| {
                (!s.lfe).then(|| {
                    std::array::from_fn(|k| Section {
                        line: vec![0.0; ms(DELAYS_MS[k] + SPREAD_MS * (d * (k + 1)) as f32)],
                        at: 0,
                    })
                })
            })
            .collect();
        Some(Self { chains })
    }

    /// Add frames `off..off + m` of `input` (its channels summed) through
    /// each speaker's filter into `out`, the gain ramped `from` → `to`.
    pub(crate) fn mix(
        &mut self,
        input: &AudioBuffer,
        out: &mut AudioBuffer,
        off: usize,
        m: usize,
        from: f32,
        to: f32,
    ) {
        let step = super::ramp_step(from, to, m);
        let mut done = 0;
        let mut sum = [0.0f32; CHUNK];
        while done < m {
            let n = CHUNK.min(m - done);
            let at = off + done;
            sum[..n].fill(0.0);
            for c in 0..input.num_channels() {
                for (s, &x) in sum[..n].iter_mut().zip(&input.channel(c)[at..at + n]) {
                    *s += x;
                }
            }
            let g0 = from + step * done as f32;
            for (d, chain) in self.chains.iter_mut().enumerate() {
                let Some(chain) = chain else { continue };
                if d >= out.num_channels() {
                    break;
                }
                let mut g = g0;
                for (o, &x) in out.channel_mut(d)[at..at + n].iter_mut().zip(&sum[..n]) {
                    let mut y = x;
                    for s in chain.iter_mut() {
                        y = s.run(y);
                    }
                    g += step;
                    *o += y * g;
                }
            }
            done += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::SurroundFormat;

    fn run(d: &mut Diffuser, input: &[f32]) -> Vec<Vec<f32>> {
        let layout = ChannelLayout::Surround(SurroundFormat::S51);
        let n = input.len();
        let mut inb = AudioBuffer::new(ChannelLayout::Mono, n);
        inb.set_len(n);
        inb.channel_mut(0).copy_from_slice(input);
        let mut out = AudioBuffer::new(layout, n);
        out.set_len(n);
        out.clear();
        d.mix(&inb, &mut out, 0, n, 1.0, 1.0);
        (0..6).map(|c| out.channel(c).to_vec()).collect()
    }

    #[test]
    fn every_speaker_gets_a_flat_uncorrelated_copy_without_delay() {
        let layout = ChannelLayout::Surround(SurroundFormat::S51);
        let mut d = Diffuser::new(layout, 48_000.0).unwrap();
        // Noise (a fixed sequence).
        let mut seed = 0x1234_5678u32;
        let noise: Vec<f32> = (0..96_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed as f32 / u32::MAX as f32) - 0.5
            })
            .collect();
        let out = run(&mut d, &noise);
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let src = rms(&noise[48_000..]);
        // Flat: each copy as loud as the source (all-pass).
        for c in [0usize, 1, 2, 4, 5] {
            let r = rms(&out[c][48_000..]);
            assert!(
                (20.0 * (r / src).log10()).abs() < 0.3,
                "ch {c}: {r} vs {src}"
            );
        }
        assert!(out[3].iter().all(|v| *v == 0.0), "no LFE");
        // Uncorrelated with each other.
        let corr = |a: &[f32], b: &[f32]| {
            let (sa, sb) = (rms(a), rms(b));
            a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / a.len() as f32 / (sa * sb)
        };
        for (a, b) in [(0usize, 1usize), (0, 2), (1, 4), (4, 5)] {
            let c = corr(&out[a][48_000..], &out[b][48_000..]);
            assert!(c.abs() < 0.2, "{a}/{b}: {c}");
        }
        // No latency: an impulse answers at once (the sections' direct
        // term, −g³·…).
        let mut d = Diffuser::new(layout, 48_000.0).unwrap();
        let mut imp = vec![0.0f32; 4096];
        imp[0] = 1.0;
        let out = run(&mut d, &imp);
        assert!(out[0][0].abs() > 0.05, "{}", out[0][0]);
        // Energy kept (all-pass): Σ h² ≈ 1.
        let e: f32 = out[0].iter().map(|v| v * v).sum();
        assert!((e - 1.0).abs() < 0.05, "{e}");
    }
}

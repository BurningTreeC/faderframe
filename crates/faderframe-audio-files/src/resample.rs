//! Streaming sample-rate conversion (rubato's FFT resampler) with exact
//! output length and the resampler's start-up delay removed.

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Indexing, Resampler};

const CHUNK: usize = 4096;

/// Receives converted, non-interleaved output blocks.
pub type Sink<'a> = dyn FnMut(&[&[f32]], usize) -> Result<(), crate::import::ImportError> + 'a;

pub struct StreamResampler {
    inner: Fft<f32>,
    channels: usize,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    delay_left: usize,
    in_total: u64,
    out_total: u64,
    ratio: f64,
}

#[derive(Debug, thiserror::Error)]
#[error("resampler: {0}")]
pub struct ResampleError(pub String);

impl StreamResampler {
    pub fn new(from_rate: u32, to_rate: u32, channels: usize) -> Result<Self, ResampleError> {
        let channels = channels.max(1);
        let inner = Fft::<f32>::new(
            from_rate as usize,
            to_rate as usize,
            CHUNK,
            channels,
            FixedSync::Input,
        )
        .map_err(|e| ResampleError(e.to_string()))?;
        let out_max = inner.output_frames_max();
        Ok(Self {
            delay_left: inner.output_delay(),
            output: vec![vec![0.0; out_max]; channels],
            input: vec![Vec::with_capacity(CHUNK * 2); channels],
            inner,
            channels,
            in_total: 0,
            out_total: 0,
            ratio: to_rate as f64 / from_rate as f64,
        })
    }

    /// Exact number of output frames for the input seen so far.
    fn target_total(&self) -> u64 {
        (self.in_total as f64 * self.ratio).round() as u64
    }

    fn run(
        &mut self,
        partial: Option<usize>,
        cap: Option<u64>,
        sink: &mut Sink<'_>,
    ) -> Result<usize, crate::import::ImportError> {
        let in_frames = self.input[0].len().max(self.inner.input_frames_next());
        for ch in &mut self.input {
            ch.resize(in_frames, 0.0);
        }
        let input = SequentialSliceOfVecs::new(&self.input, self.channels, in_frames)
            .map_err(|e| ResampleError(format!("{e:?}")))?;
        let out_frames = self.output[0].len();
        let mut output =
            SequentialSliceOfVecs::new_mut(&mut self.output, self.channels, out_frames)
                .map_err(|e| ResampleError(format!("{e:?}")))?;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            active_channels_mask: None,
            partial_len: partial,
        };
        let (used, produced) = self
            .inner
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| ResampleError(e.to_string()))?;
        let used = partial.map_or(used, |p| p.min(used));
        for ch in &mut self.input {
            ch.drain(..used.min(ch.len()));
        }
        // Drop the start-up delay, then cap at the exact expected length.
        let skip = self.delay_left.min(produced);
        self.delay_left -= skip;
        let mut n = produced - skip;
        if let Some(cap) = cap {
            n = n.min(cap.saturating_sub(self.out_total) as usize);
        }
        if n > 0 {
            let slices: Vec<&[f32]> = self.output.iter().map(|c| &c[skip..skip + n]).collect();
            sink(&slices, n)?;
            self.out_total += n as u64;
        }
        Ok(used)
    }

    /// Feed `frames` input frames.
    pub fn push(
        &mut self,
        planes: &[Vec<f32>],
        frames: usize,
        sink: &mut Sink<'_>,
    ) -> Result<(), crate::import::ImportError> {
        for (c, ch) in self.input.iter_mut().enumerate() {
            match planes.get(c).or(planes.first()) {
                Some(p) => ch.extend_from_slice(&p[..frames.min(p.len())]),
                None => ch.extend(std::iter::repeat_n(0.0, frames)),
            }
        }
        self.in_total += frames as u64;
        while self.input[0].len() >= self.inner.input_frames_next() {
            self.run(None, None, sink)?;
        }
        Ok(())
    }

    /// Process the remaining input and flush the resampler.
    pub fn finish(mut self, sink: &mut Sink<'_>) -> Result<u64, crate::import::ImportError> {
        let target = self.target_total();
        let rest = self.input[0].len();
        if rest > 0 {
            self.run(Some(rest), Some(target), sink)?;
        }
        let mut guard = 0;
        while self.out_total < target && guard < 64 {
            for ch in &mut self.input {
                ch.clear();
            }
            self.run(Some(0), Some(target), sink)?;
            guard += 1;
        }
        Ok(self.out_total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        let mut r = StreamResampler::new(from, to, 1).unwrap();
        let mut out = Vec::new();
        let mut sink = |s: &[&[f32]], n: usize| {
            out.extend_from_slice(&s[0][..n]);
            Ok(())
        };
        for chunk in input.chunks(1000) {
            r.push(&[chunk.to_vec()], chunk.len(), &mut sink).unwrap();
        }
        r.finish(&mut sink).unwrap();
        out
    }

    #[test]
    fn exact_length_and_preserved_frequency() {
        let from = 44_100;
        let to = 48_000;
        let n = 44_100;
        let f = 1000.0;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * f * i as f32 / from as f32).sin() * 0.5)
            .collect();
        let out = convert(&input, from, to);
        assert_eq!(out.len(), 48_000);
        // Count rising zero crossings in the steady middle part: ~1000 per second.
        let mid = &out[4800..43_200];
        let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let expected = (f as usize) * mid.len() / to as usize;
        assert!(
            (crossings as i64 - expected as i64).abs() <= 2,
            "{crossings} vs {expected}"
        );
        let peak = mid.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "amplitude {peak}");
    }

    #[test]
    fn downsampling_and_short_input() {
        let input = vec![0.25f32; 300];
        let out = convert(&input, 96_000, 48_000);
        assert_eq!(out.len(), 150);
        let out = convert(&vec![0.1f32; 192_000], 192_000, 44_100);
        assert_eq!(out.len(), 44_100);
    }
}

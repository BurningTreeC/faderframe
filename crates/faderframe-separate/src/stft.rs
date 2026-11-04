//! The short-time Fourier transforms around the network, as the reference
//! computes them: `torch.stft` (periodic Hann window, centred frames, the
//! signal reflected at both ends, no normalisation) forwards, and
//! `scipy.signal.istft` (overlap-add of the windowed inverse transforms,
//! divided by the summed squared window) back.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// A periodic Hann window (`torch.hann_window`, scipy's `hann`).
pub fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
        .collect()
}

/// Frames a centred transform of `len` samples has.
pub fn frames(len: usize, hop: usize) -> usize {
    1 + len / hop
}

/// One channel's spectrogram: `frames × bins`, row-major.
pub struct Spectrum {
    pub frames: usize,
    pub bins: usize,
    pub re: Vec<f32>,
    pub im: Vec<f32>,
}

impl Spectrum {
    pub fn magnitude(&self) -> Vec<f32> {
        self.re
            .iter()
            .zip(&self.im)
            .map(|(r, i)| (r * r + i * i).sqrt())
            .collect()
    }
}

/// `torch.stft(x, n_fft, hop, window, center=True, pad_mode="reflect")`.
/// `x` must be longer than half a window.
pub fn forward(x: &[f32], n_fft: usize, hop: usize, window: &[f32]) -> Spectrum {
    let half = n_fft / 2;
    let len = x.len();
    // Reflect (without repeating the edge sample) at both ends.
    let at = |i: isize| -> f32 {
        let n = len as isize;
        let mut j = i;
        if j < 0 {
            j = -j;
        }
        if j >= n {
            j = 2 * (n - 1) - j;
        }
        x[j.clamp(0, n - 1) as usize]
    };
    let frames = frames(len, hop);
    let bins = half + 1;
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n_fft);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut re = vec![0.0f32; frames * bins];
    let mut im = vec![0.0f32; frames * bins];
    for t in 0..frames {
        let start = (t * hop) as isize - half as isize;
        for (n, v) in input.iter_mut().enumerate() {
            *v = at(start + n as isize) * window[n];
        }
        // Sizes come from the plan: the transform cannot fail.
        let _ = fft.process_with_scratch(&mut input, &mut output, &mut scratch);
        for (k, c) in output.iter().enumerate() {
            re[t * bins + k] = c.re;
            im[t * bins + k] = c.im;
        }
    }
    Spectrum {
        frames,
        bins,
        re,
        im,
    }
}

/// Overlap-adds inverse transforms of frames into a signal: what
/// `scipy.signal.istft(X / (n_fft / 2), nperseg=n_fft, noverlap=n_fft -
/// hop, boundary=True)` gives, in double precision.
pub struct Inverse {
    n_fft: usize,
    hop: usize,
    window: Vec<f64>,
    fft: std::sync::Arc<dyn realfft::ComplexToReal<f64>>,
    spectrum: Vec<Complex<f64>>,
    frame: Vec<f64>,
    scratch: Vec<Complex<f64>>,
}

impl Inverse {
    pub fn new(n_fft: usize, hop: usize) -> Self {
        let fft = RealFftPlanner::<f64>::new().plan_fft_inverse(n_fft);
        Self {
            n_fft,
            hop,
            window: hann(n_fft).iter().map(|w| f64::from(*w)).collect(),
            spectrum: fft.make_input_vec(),
            frame: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
        }
    }

    /// The spectrum to fill before [`Self::add`] (`n_fft / 2 + 1` bins).
    pub fn spectrum(&mut self) -> &mut [Complex<f64>] {
        &mut self.spectrum
    }

    /// Add the frame in [`Self::spectrum`] as frame `t` into `out`, which
    /// holds the padded signal from frame `first` on (sample 0 of `out` =
    /// the start of frame `first`).
    pub fn add(&mut self, t: usize, first: usize, out: &mut [f64]) {
        // The edges' imaginary parts mean nothing to a real signal (numpy's
        // irfft ignores them).
        let last = self.spectrum.len() - 1;
        self.spectrum[0].im = 0.0;
        self.spectrum[last].im = 0.0;
        let _ =
            self.fft
                .process_with_scratch(&mut self.spectrum, &mut self.frame, &mut self.scratch);
        // irfft's 1/N; the 1/(N/2) and the window's sum (N/2) cancel.
        let k = 1.0 / self.n_fft as f64;
        let at = (t - first) * self.hop;
        for (n, (v, w)) in self.frame.iter().zip(&self.window).enumerate() {
            out[at + n] += v * k * w;
        }
    }

    /// The summed squared window at padded sample `i` of a transform of
    /// `frames` frames.
    pub fn norm(&self, i: usize, frames: usize) -> f64 {
        let (n, hop) = (self.n_fft, self.hop);
        let first = (i + 1).saturating_sub(n).div_ceil(hop);
        let last = (i / hop).min(frames.saturating_sub(1));
        (first..=last)
            .map(|t| {
                let w = self.window[i - t * hop];
                w * w
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_comes_back_through_both_transforms() {
        let (n_fft, hop) = (256, 64);
        let x: Vec<f32> = (0..2048)
            .map(|i| ((i as f32 * 0.05).sin() + (i as f32 * 0.31).cos() * 0.3) * 0.5)
            .collect();
        let w = hann(n_fft);
        let s = forward(&x, n_fft, hop, &w);
        assert_eq!(s.frames, 2048 / hop + 1);
        let mut inv = Inverse::new(n_fft, hop);
        let padded = n_fft + (s.frames - 1) * hop;
        let mut out = vec![0.0f64; padded];
        for t in 0..s.frames {
            for (k, c) in inv.spectrum().iter_mut().enumerate() {
                *c = Complex::new(
                    f64::from(s.re[t * s.bins + k]),
                    f64::from(s.im[t * s.bins + k]),
                );
            }
            inv.add(t, 0, &mut out);
        }
        for (i, v) in x.iter().enumerate() {
            let p = i + n_fft / 2;
            let got = out[p] / inv.norm(p, s.frames);
            assert!((got - f64::from(*v)).abs() < 1e-5, "{i}: {got} vs {v}");
        }
    }

    #[test]
    fn the_norm_sums_the_frames_over_a_sample() {
        let inv = Inverse::new(16, 4);
        // In the middle four frames overlap: 1.5 for a Hann window.
        assert!((inv.norm(40, 20) - 1.5).abs() < 1e-12);
        // The first sample: frame 0 alone, whose window starts at 0.
        assert_eq!(inv.norm(0, 20), 0.0);
    }
}

//! The multichannel Wiener filter of norbert (`norbert.wiener`, MIT;
//! Uhlich et al., ICASSP 2017; Nugraha, Liutkus and Vincent, 2016) that
//! turns the network's magnitude estimates into complex spectra.
//!
//! The first estimate of each source is its magnitude with the mixture's
//! phase. Each expectation-maximisation iteration then models every
//! source by its power (the mean over the channels of its estimate's
//! squared magnitude, per frame and bin) and its spatial covariance (per
//! bin: the estimate's covariance summed over the frames, divided by its
//! summed power), and separates the mixture again: a source's estimate is
//! `v·R·C⁻¹·x`, `C` the sum of every source's `v·R` (plus √ε on the
//! diagonal). As in norbert, the spectra are scaled down to a tenth of the
//! loudest bin while iterating and the arithmetic is in double precision.
//!
//! Since a frame's estimate depends only on its own mixture, its own
//! magnitudes and the covariances of the iterations before, nothing is
//! kept between iterations but the covariances: [`Wiener::fit`] works
//! through blocks of bins, recomputing earlier iterations' estimates as it
//! goes, and [`Wiener::frame`] separates one frame at a time.

use crate::nn::parallel;
use crate::stft::Spectrum;
use realfft::num_complex::Complex;

type C64 = Complex<f64>;

/// A 2 × 2 complex matrix, row-major.
type Mat = [C64; 4];

const EPS: f64 = f64::EPSILON;
/// Bins a fitting task takes.
const BLOCK: usize = 32;

pub struct Wiener {
    sources: usize,
    bins: usize,
    iterations: usize,
    /// The spectra's scale while iterating (norbert's `max_abs`).
    scale: f64,
    /// Spatial covariances, `(bin · iterations + iteration) · sources +
    /// source`.
    r: Vec<Mat>,
}

/// The mixture's two channels at one frame and bin.
fn mixture(spec: &[Spectrum], t: usize, f: usize) -> [C64; 2] {
    let i = t * spec[0].bins + f;
    [
        C64::new(f64::from(spec[0].re[i]), f64::from(spec[0].im[i])),
        C64::new(f64::from(spec[1].re[i]), f64::from(spec[1].im[i])),
    ]
}

/// A source's estimate's magnitude (`frames × channels × bins`).
fn magnitude(est: &[f32], bins: usize, t: usize, c: usize, f: usize) -> f64 {
    f64::from(est[(t * 2 + c) * bins + f])
}

impl Wiener {
    /// The covariances of `iterations` EM steps for the mixture `spec` (two
    /// channels) and the sources' magnitude estimates.
    pub fn fit(
        spec: &[Spectrum],
        estimates: &[Vec<f32>],
        iterations: usize,
        threads: usize,
    ) -> Self {
        let (frames, bins) = (spec[0].frames, spec[0].bins);
        let ns = estimates.len();
        let mut loudest = 0.0f64;
        for s in spec {
            for (re, im) in s.re.iter().zip(&s.im) {
                loudest = loudest.max(C64::new(f64::from(*re), f64::from(*im)).norm());
            }
        }
        let mut w = Self {
            sources: ns,
            bins,
            iterations,
            scale: (loudest / 10.0).max(1.0),
            r: vec![[C64::default(); 4]; bins * iterations * ns],
        };
        if iterations == 0 {
            return w;
        }
        let per_bin = iterations * ns;
        let mut r = std::mem::take(&mut w.r);
        let tasks: Vec<(usize, &mut [Mat])> = r
            .chunks_mut(BLOCK * per_bin)
            .enumerate()
            .map(|(i, c)| (i * BLOCK, c))
            .collect();
        let me = &w;
        parallel(threads, tasks, |(first, out)| {
            let n = out.len() / per_bin;
            let mut y = vec![[C64::default(); 2]; ns];
            let mut v = vec![0.0f64; ns];
            for k in 0..iterations {
                let mut sums = vec![([C64::default(); 4], EPS); n * ns];
                for t in 0..frames {
                    for b in 0..n {
                        let f = first + b;
                        let x = me.start(spec, estimates, t, f, &mut y);
                        for j in 0..k {
                            let r = &out[(b * iterations + j) * ns..][..ns];
                            Self::refine(x, &mut y, r, &mut v);
                        }
                        for (s, ys) in y.iter().enumerate() {
                            let (m, weight) = &mut sums[b * ns + s];
                            m[0] += ys[0] * ys[0].conj();
                            m[1] += ys[0] * ys[1].conj();
                            m[2] += ys[1] * ys[0].conj();
                            m[3] += ys[1] * ys[1].conj();
                            *weight += (ys[0].norm_sqr() + ys[1].norm_sqr()) / 2.0;
                        }
                    }
                }
                for b in 0..n {
                    for s in 0..ns {
                        let (m, weight) = sums[b * ns + s];
                        out[(b * iterations + k) * ns + s] = m.map(|v| v / weight);
                    }
                }
            }
        });
        w.r = r;
        w
    }

    /// The scaled mixture at `(t, f)` and the first estimates in `y`.
    fn start(
        &self,
        spec: &[Spectrum],
        est: &[Vec<f32>],
        t: usize,
        f: usize,
        y: &mut [[C64; 2]],
    ) -> [C64; 2] {
        let x = mixture(spec, t, f);
        // The mixture's phase (0 where it is silent, as numpy's angle).
        let phase = x.map(|v| {
            let n = v.norm();
            if n > 0.0 { v / n } else { C64::new(1.0, 0.0) }
        });
        for (s, ys) in y.iter_mut().enumerate() {
            for c in 0..2 {
                ys[c] = phase[c] * (magnitude(&est[s], self.bins, t, c, f) / self.scale);
            }
        }
        x.map(|v| v / self.scale)
    }

    /// One EM step at one frame and bin: the sources' estimates `y`
    /// separated again with covariances `r` (one per source).
    /// `v` is room for the sources' powers.
    fn refine(x: [C64; 2], y: &mut [[C64; 2]], r: &[Mat], v: &mut [f64]) {
        let reg = EPS.sqrt();
        let mut cxx = [
            C64::new(reg, 0.0),
            C64::default(),
            C64::default(),
            C64::new(reg, 0.0),
        ];
        for (s, ys) in y.iter().enumerate() {
            v[s] = (ys[0].norm_sqr() + ys[1].norm_sqr()) / 2.0;
            for i in 0..4 {
                cxx[i] += r[s][i] * v[s];
            }
        }
        let det = cxx[0] * cxx[3] - cxx[1] * cxx[2];
        let inv_det = C64::new(1.0, 0.0) / det;
        let inv = [
            cxx[3] * inv_det,
            -cxx[1] * inv_det,
            -cxx[2] * inv_det,
            cxx[0] * inv_det,
        ];
        for (s, ys) in y.iter_mut().enumerate() {
            let rj = &r[s];
            // G = v·R·C⁻¹, the estimate G·x.
            let g = [
                (rj[0] * inv[0] + rj[1] * inv[2]) * v[s],
                (rj[0] * inv[1] + rj[1] * inv[3]) * v[s],
                (rj[2] * inv[0] + rj[3] * inv[2]) * v[s],
                (rj[2] * inv[1] + rj[3] * inv[3]) * v[s],
            ];
            ys[0] = g[0] * x[0] + g[1] * x[1];
            ys[1] = g[2] * x[0] + g[3] * x[1];
        }
    }

    /// Frame `t` separated: per source and channel (`source · 2 +
    /// channel`), its spectrum into `out`.
    // The bin indexes the mixture, the estimates, the covariances and
    // every stream alike.
    #[allow(clippy::needless_range_loop)]
    pub fn frame(&self, spec: &[Spectrum], estimates: &[Vec<f32>], t: usize, out: &mut [Vec<C64>]) {
        let ns = self.sources;
        let mut y = vec![[C64::default(); 2]; ns];
        let mut v = vec![0.0f64; ns];
        for f in 0..self.bins {
            let x = self.start(spec, estimates, t, f, &mut y);
            let per_bin = self.iterations * ns;
            for k in 0..self.iterations {
                let r = &self.r[f * per_bin + k * ns..][..ns];
                Self::refine(x, &mut y, r, &mut v);
            }
            for (s, ys) in y.iter().enumerate() {
                for c in 0..2 {
                    out[s * 2 + c][f] = ys[c] * self.scale;
                }
            }
        }
    }
}

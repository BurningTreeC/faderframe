//! Between the project's rate and the codec's 48 kHz, with a delay that is
//! known to the sample.
//!
//! Opus codes at 48 kHz; a project at another rate goes there and back
//! through two rational polyphase resamplers (`L/M` with `L·fs = M·48 kHz
//! = V`, the rate both work at). Their prototypes are Kaiser-windowed
//! sincs designed at `V`, linear phase, so each delays by exactly
//! `(N − 1)/2` samples of `V`; with the codec's own delay in between, the
//! whole way is `R + C·M` samples of `V` (`R` the two filters, `C` the
//! codec's 48 kHz samples). [`RatePlan::delay_for`] pads the codec's
//! delay by a few 48 kHz samples so that this is a whole number of project
//! samples: the decoded signal then lines up with the input delayed by an
//! integer — which is what lets the dry path be the input itself, delayed,
//! and the difference of the two null everywhere but where the codec
//! changed something.
//!
//! The passband reaches 92 % of the lower Nyquist frequency (20.3 kHz at
//! 44.1 kHz, Opus' own band ends at 20 kHz), the stopband is 90 dB down
//! from that Nyquist frequency on: what the round trip itself leaves in
//! the difference is far below anything a codec does.
//!
//! Everything is allocated when made (control thread);
//! [`Resampler::process`] allocates nothing.

use std::f64::consts::PI;
use std::sync::Arc;

/// The codec's rate.
pub const CODEC_RATE: u32 = 48_000;

/// Stopband attenuation of the filters (dB).
const STOPBAND_DB: f64 = 90.0;
/// Transition band as a share of the lower Nyquist frequency.
const TRANSITION: f64 = 0.08;

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The zeroth-order modified Bessel function (Kaiser window).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// One direction's polyphase filter (every channel shares it).
#[derive(Debug)]
pub struct Filter {
    /// Upsampling factor (phases).
    up: usize,
    /// Downsampling factor.
    down: usize,
    /// Taps per phase.
    taps: usize,
    /// Per phase, its taps in the order of the history window (oldest
    /// sample first), scaled by `up`: `up × taps`.
    coef: Box<[f32]>,
}

impl Filter {
    /// By `up/down` at `v` Hz (`up · rate in`), passing below `pass` Hz
    /// and stopping from `stop` Hz on; `taps` per phase.
    fn design(up: usize, down: usize, taps: usize, v: f64, pass: f64, stop: f64) -> Self {
        let n = taps * up;
        let fc = 0.5 * (pass + stop) / v;
        let beta = 0.1102 * (STOPBAND_DB - 8.7);
        let i0b = bessel_i0(beta);
        let centre = (n - 1) as f64 / 2.0;
        let mut h = vec![0.0f64; n];
        for (t, c) in h.iter_mut().enumerate() {
            let x = t as f64 - centre;
            let sinc = if x.abs() < 1e-12 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * x).sin() / (PI * x)
            };
            let r = x / centre.max(1.0);
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            *c = sinc * w;
        }
        // Unity gain at DC once zero-stuffed: the taps sum to `up`.
        let sum: f64 = h.iter().sum();
        let scale = if sum.abs() > 1e-12 {
            up as f64 / sum
        } else {
            0.0
        };
        let mut coef = vec![0.0f32; n];
        for p in 0..up {
            for q in 0..taps {
                // Tap `p + q·up` meets input `i − q`: oldest first.
                coef[p * taps + (taps - 1 - q)] = (h[p + q * up] * scale) as f32;
            }
        }
        Self {
            up,
            down,
            taps,
            coef: coef.into_boxed_slice(),
        }
    }

    /// Its length in samples of `V`.
    fn len(&self) -> usize {
        self.taps * self.up
    }

    /// The most outputs `n` inputs make.
    pub fn max_out(&self, n: usize) -> usize {
        (n * self.up).div_ceil(self.down) + 1
    }
}

/// How a project rate goes to 48 kHz and back.
#[derive(Clone, Debug)]
pub struct RatePlan {
    rate: u32,
    /// `L` and `M`: `L · rate = M · 48 kHz` (1 and 1 at 48 kHz).
    l: usize,
    m: usize,
    to: Option<Arc<Filter>>,
    from: Option<Arc<Filter>>,
}

impl RatePlan {
    /// For a project at `rate` (8 kHz … 768 kHz).
    pub fn new(rate: u32) -> Result<Self, crate::CodecError> {
        if !(8_000..=768_000).contains(&rate) {
            return Err(crate::CodecError::Rate(rate));
        }
        let g = gcd(u64::from(rate), u64::from(CODEC_RATE));
        let l = (u64::from(CODEC_RATE) / g) as usize;
        let m = (u64::from(rate) / g) as usize;
        if l == 1 && m == 1 {
            return Ok(Self {
                rate,
                l,
                m,
                to: None,
                from: None,
            });
        }
        if l > 4_000 || m > 4_000 {
            // Rates with no small ratio to 48 kHz (not a rate interfaces
            // run at).
            return Err(crate::CodecError::Rate(rate));
        }
        let v = l as f64 * f64::from(rate);
        let nyquist = 0.5 * f64::from(rate.min(CODEC_RATE));
        let width = TRANSITION * nyquist;
        let (pass, stop) = (nyquist - width, nyquist);
        // Kaiser: N ≈ (A − 8) / (2.285 · 2π · Δf / V).
        let n = ((STOPBAND_DB - 8.0) / (2.285 * 2.0 * PI * width / v)).ceil() as usize + 1;
        let mut k_to = n.div_ceil(l).max(2);
        let mut k_from = n.div_ceil(m).max(2);
        // The two delays add up to whole samples of V: N1 + N2 even.
        if (k_to * l + k_from * m) % 2 == 1 {
            if m % 2 == 1 {
                k_from += 1;
            } else {
                k_to += 1;
            }
        }
        let to = Filter::design(l, m, k_to, v, pass, stop);
        let from = Filter::design(m, l, k_from, v, pass, stop);
        Ok(Self {
            rate,
            l,
            m,
            to: Some(Arc::new(to)),
            from: Some(Arc::new(from)),
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The project rate is the codec's: nothing is resampled.
    pub fn is_direct(&self) -> bool {
        self.to.is_none()
    }

    /// For a codec delaying by `codec` samples at 48 kHz: the padding (48
    /// kHz samples) to add to it, and the whole way's delay then (project
    /// samples, exact).
    pub fn delay_for(&self, codec: usize) -> (usize, usize) {
        let (Some(to), Some(from)) = (&self.to, &self.from) else {
            return (0, codec);
        };
        let (l, m) = (self.l as u64, self.m as u64);
        // The filters' delays in samples of V.
        let r = ((to.len() + from.len()) / 2 - 1) as u64;
        let c = codec as u64;
        // (r + (c + pad)·m) ≡ 0 (mod l): pad ≡ −(r + c·m)·m⁻¹.
        let m_inv = (1..l.max(2)).find(|x| (x * m) % l == 1).unwrap_or(1) % l.max(1);
        let need = (l - (r + c * m) % l) % l;
        let pad = (need * m_inv) % l;
        let total = r + (c + pad) * m;
        debug_assert_eq!(total % l, 0);
        (pad as usize, (total / l) as usize)
    }

    /// The resampler to 48 kHz (`None` at 48 kHz).
    pub fn to_codec(&self) -> Option<Resampler> {
        self.to.as_ref().map(|f| Resampler::new(Arc::clone(f)))
    }

    /// The resampler back from 48 kHz (`None` at 48 kHz).
    pub fn from_codec(&self) -> Option<Resampler> {
        self.from.as_ref().map(|f| Resampler::new(Arc::clone(f)))
    }

    /// The most 48 kHz samples `n` project samples make.
    pub fn max_codec_frames(&self, n: usize) -> usize {
        self.to.as_ref().map_or(n, |f| f.max_out(n))
    }

    /// The most project samples `n` 48 kHz samples make.
    pub fn max_project_frames(&self, n: usize) -> usize {
        self.from.as_ref().map_or(n, |f| f.max_out(n))
    }
}

/// One channel's way through a [`Filter`]: output `k` is made from the
/// inputs up to `⌊k·M/L⌋` as soon as that one has come in.
#[derive(Debug)]
pub struct Resampler {
    filter: Arc<Filter>,
    /// The last `taps` inputs twice over (a window is always contiguous).
    history: Box<[f32]>,
    /// Where the newest input went (and `+ taps`).
    at: usize,
    /// `k·M − i·L` for the next output `k` and the input `i` last taken.
    phase: usize,
}

impl Resampler {
    fn new(filter: Arc<Filter>) -> Self {
        let taps = filter.taps;
        Self {
            filter,
            history: vec![0.0; 2 * taps].into_boxed_slice(),
            at: taps - 1,
            phase: 0,
        }
    }

    /// Resample `input` into `out` (room for [`Filter::max_out`]);
    /// returns the outputs made.
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) -> usize {
        let f = &*self.filter;
        let taps = f.taps;
        let mut made = 0;
        for &x in input {
            self.at = (self.at + 1) % taps;
            self.history[self.at] = x;
            self.history[self.at + taps] = x;
            let window = &self.history[self.at + 1..self.at + 1 + taps];
            while self.phase < f.up {
                let c = &f.coef[self.phase * taps..(self.phase + 1) * taps];
                let mut acc = 0.0f32;
                for (a, b) in c.iter().zip(window) {
                    acc += a * b;
                }
                if let Some(o) = out.get_mut(made) {
                    *o = acc;
                }
                made += 1;
                self.phase += f.down;
            }
            self.phase -= f.up;
        }
        made.min(out.len())
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_way_is_a_whole_number_of_project_samples() {
        for rate in [44_100, 88_200, 96_000, 176_400, 192_000, 32_000, 22_050] {
            let plan = RatePlan::new(rate).unwrap();
            for codec in [0, 1, 1272, 1273, 2000] {
                let (pad, d) = plan.delay_for(codec);
                assert!(pad < plan.l, "{rate}: pad {pad}");
                assert!(d > 0, "{rate}");
            }
        }
        assert_eq!(RatePlan::new(48_000).unwrap().delay_for(1272), (0, 1272));
        assert!(RatePlan::new(48_001).is_err());
    }

    #[test]
    fn a_tone_goes_there_and_back_unchanged_but_delayed() {
        for rate in [44_100u32, 96_000, 32_000, 88_200] {
            let plan = RatePlan::new(rate).unwrap();
            let (mut to, mut from) = (plan.to_codec().unwrap(), plan.from_codec().unwrap());
            let (pad, d) = plan.delay_for(0);
            let n = rate as usize;
            let x: Vec<f32> = (0..n)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    (0.4 * (2.0 * PI * 997.0 * t).sin() + 0.3 * (2.0 * PI * 9_100.0 * t).sin())
                        as f32
                })
                .collect();
            let mut y = Vec::new();
            let mut mid = vec![0.0; plan.max_codec_frames(256)];
            let mut back = vec![0.0; plan.max_project_frames(mid.len().max(pad))];
            // The padding the codec would have added, at 48 kHz.
            let k = from.process(&vec![0.0; pad], &mut back);
            y.extend_from_slice(&back[..k]);
            for piece in x.chunks(256) {
                let m = to.process(piece, &mut mid);
                let k = from.process(&mid[..m], &mut back);
                y.extend_from_slice(&back[..k]);
            }
            // At least as many come out as went in (and the padding).
            assert!(
                y.len() >= n && y.len() <= n + plan.max_project_frames(pad) + 3,
                "{rate}: {}",
                y.len()
            );
            let mut err = 0.0f64;
            let mut sig = 0.0f64;
            for i in d + 2000..n - 100 {
                let e = f64::from(y[i] - x[i - d]);
                err += e * e;
                sig += f64::from(x[i - d]).powi(2);
            }
            let db = 10.0 * (err / sig).log10();
            assert!(db < -80.0, "{rate}: the round trip leaves {db:.1} dB");
        }
    }
}

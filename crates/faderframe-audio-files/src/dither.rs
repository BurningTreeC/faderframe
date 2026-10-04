//! Dither for writing integer samples: plain TPDF, or TPDF with
//! error-feedback noise shaping that moves the noise out of the ear's most
//! sensitive range (Lipshitz's 5-tap E-weighted filter: about −16 dB below
//! TPDF up to 3 kHz, −27 dB at 4 kHz, more above 12 kHz).

use serde::{Deserialize, Serialize};

/// Dither applied when quantising to an integer format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dither {
    Off,
    /// Triangular-PDF noise of ±1 LSB.
    #[default]
    Tpdf,
    /// TPDF with noise shaping (at 44.1 and 48 kHz; plain TPDF at other
    /// rates, where the shaping curve would not fit the hearing range).
    Shaped,
}

impl Dither {
    pub const ALL: [Dither; 3] = [Dither::Off, Dither::Tpdf, Dither::Shaped];

    pub fn label(self) -> &'static str {
        match self {
            Dither::Off => "Off",
            Dither::Tpdf => "TPDF",
            Dither::Shaped => "TPDF, noise shaped",
        }
    }
}

/// Error-feedback coefficients (Lipshitz et al., E-weighted, 44.1 kHz).
const SHAPE: [f64; 5] = [2.033, -2.165, 1.959, -1.590, 0.6149];

/// Deterministic xorshift noise.
#[derive(Clone, Debug)]
struct Tpdf(u64);

impl Tpdf {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    }

    /// Noise in LSB units, range (−1, 1).
    fn next(&mut self) -> f64 {
        self.uniform() + self.uniform()
    }
}

/// Turns float samples into integers of `bits` bits, per channel.
#[derive(Clone, Debug)]
pub struct Quantizer {
    scale: f64,
    min: f64,
    max: f64,
    noise: Option<Tpdf>,
    /// Past errors per channel, newest first (noise shaping only).
    errors: Vec<[f64; 5]>,
    shaped: bool,
}

impl Quantizer {
    pub fn new(bits: u16, channels: usize, sample_rate: u32, dither: Dither) -> Self {
        let full = (1i64 << (bits.clamp(2, 32) - 1)) as f64;
        let shaped = dither == Dither::Shaped && matches!(sample_rate, 44_100 | 48_000);
        Self {
            scale: full - 1.0,
            min: -full,
            max: full - 1.0,
            noise: (dither != Dither::Off).then_some(Tpdf(0x2545_f491_4f6c_dd1d)),
            errors: vec![[0.0; 5]; channels.max(1)],
            shaped,
        }
    }

    /// The integer for `sample` (−1..1) of channel `ch`.
    #[inline]
    pub fn quantize(&mut self, ch: usize, sample: f32) -> i32 {
        let mut x = sample as f64 * self.scale;
        if self.shaped
            && let Some(e) = self.errors.get(ch)
        {
            x -= SHAPE.iter().zip(e).map(|(h, e)| h * e).sum::<f64>();
        }
        let d = self.noise.as_mut().map_or(0.0, Tpdf::next);
        let q = (x + d).round().clamp(self.min, self.max);
        if self.shaped
            && let Some(e) = self.errors.get_mut(ch)
        {
            // Clipped samples would feed huge errors back: bounded.
            e.rotate_right(1);
            e[0] = (q - x).clamp(-2.0, 2.0);
        }
        q as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Error power (dB, arbitrary reference) around `freq`, averaged over
    /// Hann-windowed blocks.
    fn band_power(err: &[f64], rate: f64, freq: f64) -> f64 {
        let n = 4096;
        let mut total = 0.0;
        let mut blocks = 0;
        for block in err.chunks_exact(n) {
            for df in [-150.0, -75.0, 0.0, 75.0, 150.0] {
                let w = 2.0 * PI * (freq + df) / rate;
                let (mut re, mut im) = (0.0, 0.0);
                for (i, e) in block.iter().enumerate() {
                    let win = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
                    re += e * win * (w * i as f64).cos();
                    im -= e * win * (w * i as f64).sin();
                }
                total += re * re + im * im;
            }
            blocks += 1;
        }
        10.0 * (total / blocks as f64).log10()
    }

    fn quantization_error(dither: Dither, rate: u32) -> Vec<f64> {
        let mut q = Quantizer::new(16, 1, rate, dither);
        (0..rate as usize)
            .map(|i| {
                // A quiet tone a few LSB high.
                let s = 4.0 / 32767.0 * (2.0 * PI * 997.0 * i as f64 / rate as f64).sin();
                q.quantize(0, s as f32) as f64 - s * 32767.0
            })
            .collect()
    }

    #[test]
    fn shaping_moves_the_noise_out_of_the_sensitive_range() {
        for rate in [44_100, 48_000] {
            let flat = quantization_error(Dither::Tpdf, rate);
            let shaped = quantization_error(Dither::Shaped, rate);
            let r = rate as f64;
            let at = |e: &[f64], f| band_power(e, r, f);
            assert!(at(&shaped, 4000.0) < at(&flat, 4000.0) - 15.0, "{rate}");
            assert!(at(&shaped, 1500.0) < at(&flat, 1500.0) - 10.0, "{rate}");
            assert!(at(&shaped, 19_000.0) > at(&flat, 19_000.0) + 10.0, "{rate}");
        }
        // Elsewhere it is plain TPDF.
        assert_eq!(
            quantization_error(Dither::Shaped, 96_000),
            quantization_error(Dither::Tpdf, 96_000)
        );
    }

    #[test]
    fn dither_is_unbiased_and_off_is_rounding() {
        let mut off = Quantizer::new(16, 1, 48_000, Dither::Off);
        assert_eq!(off.quantize(0, 0.5 / 32767.0 * 0.9), 0);
        assert_eq!(off.quantize(0, 1.0), 32767);
        assert_eq!(off.quantize(0, -1.5), -32768);
        let mut q = Quantizer::new(16, 1, 48_000, Dither::Tpdf);
        // A constant between two steps averages out to it.
        let v = 0.3 / 32767.0;
        let mean = (0..100_000).map(|_| q.quantize(0, v) as f64).sum::<f64>() / 100_000.0;
        assert!((mean - 0.3).abs() < 0.02, "{mean}");
        // 24-bit range.
        let mut q24 = Quantizer::new(24, 2, 48_000, Dither::Shaped);
        assert_eq!(q24.quantize(1, 2.0), 8_388_607);
    }
}

//! An in-place radix-2 complex FFT with its tables made once.

use std::f64::consts::PI;

pub struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    rev: Vec<usize>,
}

impl Fft {
    /// For `n` points (a power of two).
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2);
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let (cos, sin) = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                (a.cos(), a.sin())
            })
            .unzip();
        Self { n, cos, sin, rev }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Forward (`inverse` false) or inverse (unscaled) transform.
    pub fn run(&self, re: &mut [f64], im: &mut [f64], inverse: bool) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let sign = if inverse { -1.0 } else { 1.0 };
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (c, s) = (self.cos[k * step], sign * self.sin[k * step]);
                    let (a, b) = (start + k, start + k + len / 2);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_then_inverse_is_the_signal() {
        let f = Fft::new(64);
        let x: Vec<f64> = (0..64)
            .map(|i| (i as f64 * 0.37).sin() + 0.1 * i as f64)
            .collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; 64]);
        f.run(&mut re, &mut im, false);
        // A sine of bin 5 lands in bins 5 and 59.
        let (mut sr, mut si) = (
            (0..64)
                .map(|i| (2.0 * PI * 5.0 * i as f64 / 64.0).cos())
                .collect::<Vec<_>>(),
            vec![0.0; 64],
        );
        f.run(&mut sr, &mut si, false);
        assert!((sr[5] - 32.0).abs() < 1e-9 && (sr[59] - 32.0).abs() < 1e-9);
        f.run(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(&x) {
            assert!((a / 64.0 - b).abs() < 1e-9);
        }
    }
}

//! Autoregressive models: fitting one to a stretch of audio (autocorrelation
//! method, Levinson–Durbin), its prediction error, and filling a gap with
//! the samples that make the prediction error over and round it smallest
//! (least-squares AR interpolation, Vaseghi and Rayner; Janssen et al.).
//!
//! The model of order `p` predicts each sample from the `p` before it:
//! `e[n] = Σ_{i=0}^{p} a[i] x[n−i]`, `a[0] = 1`. For a gap `[s, e)` the
//! errors `e[n]`, `s ≤ n < e + p`, are the ones its samples take part in;
//! their energy is a quadratic in the unknown samples whose matrix is the
//! autocorrelation of `a` — symmetric Toeplitz, solved in `O(m²)` by
//! Levinson's recursion. Everything is allocation free once made.

/// Autocorrelation of `x` for lags `0..r.len()`.
pub fn autocorrelation(x: &[f64], r: &mut [f64]) {
    for (lag, v) in r.iter_mut().enumerate() {
        *v = if lag < x.len() {
            x[lag..].iter().zip(x).map(|(a, b)| a * b).sum()
        } else {
            0.0
        };
    }
}

/// Levinson–Durbin: the AR coefficients (`a[0] = 1`, order `a.len() − 1`)
/// of the autocorrelation `r` (at least as long as `a`); `tmp` as long as
/// `a`. Returns the prediction error power (0 for silence: then `a` is
/// `[1, 0, …]`).
pub fn levinson(r: &[f64], a: &mut [f64], tmp: &mut [f64]) -> f64 {
    let p = a.len().saturating_sub(1);
    a.fill(0.0);
    if a.is_empty() {
        return 0.0;
    }
    a[0] = 1.0;
    let mut err = r.first().copied().unwrap_or(0.0);
    if err <= 1e-30 {
        return 0.0;
    }
    for i in 1..=p.min(r.len().saturating_sub(1)) {
        let mut acc = r[i];
        for j in 1..i {
            acc += a[j] * r[i - j];
        }
        let k = -acc / err;
        tmp[..=i].copy_from_slice(&a[..=i]);
        for j in 1..i {
            a[j] = tmp[j] + k * tmp[i - j];
        }
        a[i] = k;
        err *= 1.0 - k * k;
        if err <= r[0] * 1e-12 {
            // Perfectly predictable (a pure tone): stop before it divides
            // by nothing.
            break;
        }
    }
    err.max(0.0)
}

/// Solve `T x = b` for the symmetric Toeplitz matrix whose first column is
/// `t` (positive definite; `t`, `b` and `x` the same length) by Levinson's
/// recursion (Golub and Van Loan, algorithm 4.7.2). `y` is scratch as long.
/// Returns `false` (and leaves `x` undefined) when the matrix is not
/// positive definite to working precision.
pub fn solve_toeplitz(t: &[f64], b: &[f64], x: &mut [f64], y: &mut [f64]) -> bool {
    let m = t.len().min(b.len()).min(x.len()).min(y.len());
    if m == 0 {
        return true;
    }
    let t0 = t[0];
    if t0 <= 0.0 || !t0.is_finite() {
        return false;
    }
    let r = |k: usize| t[k] / t0;
    x[0] = b[0] / t0;
    if m == 1 {
        return true;
    }
    y[0] = -r(1);
    let mut alpha = -r(1);
    let mut beta = 1.0;
    for k in 1..m {
        beta *= 1.0 - alpha * alpha;
        if beta <= 1e-14 {
            return false;
        }
        let mut acc = b[k] / t0;
        for i in 0..k {
            acc -= r(i + 1) * x[k - 1 - i];
        }
        let mu = acc / beta;
        for i in 0..k {
            x[i] += mu * y[k - 1 - i];
        }
        x[k] = mu;
        if k + 1 < m {
            let mut acc = r(k + 1);
            for i in 0..k {
                acc += r(i + 1) * y[k - 1 - i];
            }
            alpha = -acc / beta;
            // y ← [y + α · reverse(y); α], in place pairwise.
            let (mut i, mut j) = (0, k - 1);
            while i < j {
                let (a, b) = (y[i], y[j]);
                y[i] = a + alpha * b;
                y[j] = b + alpha * a;
                i += 1;
                j -= 1;
            }
            if i == j {
                y[i] += alpha * y[i];
            }
            y[k] = alpha;
        }
    }
    x[..m].iter().all(|v| v.is_finite())
}

/// Fits AR models of one order to windows of one length.
#[derive(Clone, Debug)]
pub struct ArFit {
    window: Vec<f64>,
    buf: Vec<f64>,
    r: Vec<f64>,
    tmp: Vec<f64>,
    /// The coefficients of the last fit (`a[0] = 1`).
    pub a: Vec<f64>,
    /// Its prediction error power per sample.
    pub error: f64,
}

impl ArFit {
    /// For windows of `len` samples and models of `order`.
    pub fn new(len: usize, order: usize) -> Self {
        let window = (0..len)
            .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * (i as f64 + 0.5) / len as f64).cos())
            .collect();
        let mut a = vec![0.0; order + 1];
        a[0] = 1.0;
        Self {
            window,
            buf: vec![0.0; len],
            r: vec![0.0; order + 1],
            tmp: vec![0.0; order + 1],
            a,
            error: 0.0,
        }
    }

    pub fn order(&self) -> usize {
        self.a.len() - 1
    }

    /// Fit the model to `x` (the window's length; shorter is zero padded),
    /// Hann windowed, with a little white noise added (a ridge: a model of
    /// a pure tone stays usable).
    pub fn fit(&mut self, x: &[f64]) {
        let n = self.buf.len().min(x.len());
        for ((b, &v), &w) in self.buf.iter_mut().zip(x).zip(&self.window) {
            *b = v * w;
        }
        self.buf[n..].fill(0.0);
        autocorrelation(&self.buf, &mut self.r);
        self.r[0] *= 1.0 + 1e-9;
        self.r[0] += 1e-20;
        let energy = self.window.iter().map(|w| w * w).sum::<f64>().max(1e-9);
        self.error = levinson(&self.r, &mut self.a, &mut self.tmp) / energy;
    }

    /// The prediction error of `x` at `n` (`n ≥ order`).
    #[inline]
    pub fn residual(&self, x: &[f64], n: usize) -> f64 {
        let mut e = 0.0;
        for (i, a) in self.a.iter().enumerate() {
            e += a * x[n - i];
        }
        e
    }
}

/// Fills gaps by least-squares AR interpolation (buffers for gaps up to
/// `max_gap` samples and models up to `max_order`).
#[derive(Clone, Debug)]
pub struct Interpolator {
    t: Vec<f64>,
    b: Vec<f64>,
    x: Vec<f64>,
    y: Vec<f64>,
    c: Vec<f64>,
}

impl Interpolator {
    pub fn new(max_gap: usize, max_order: usize) -> Self {
        Self {
            t: vec![0.0; max_gap],
            b: vec![0.0; max_gap],
            x: vec![0.0; max_gap],
            y: vec![0.0; max_gap],
            c: vec![0.0; max_gap + max_order + 1],
        }
    }

    /// The longest gap it fills.
    pub fn max_gap(&self) -> usize {
        self.t.len()
    }

    /// Fill `x[s..e]` from the samples round it with the model `a`. The
    /// `p = a.len() − 1` samples before and after the gap must be in `x`
    /// (`s ≥ p`, `e + p ≤ x.len()`). Returns `false` (and leaves `x`
    /// alone) when the gap is too long, lacks context or the system does
    /// not solve.
    pub fn fill(&mut self, x: &mut [f64], s: usize, e: usize, a: &[f64]) -> bool {
        let p = a.len().saturating_sub(1);
        let m = e.saturating_sub(s);
        if m == 0 {
            return true;
        }
        if m > self.t.len() || s < p || e + p > x.len() || m + p + 1 > self.c.len() {
            return false;
        }
        // The Toeplitz column: the autocorrelation of the model.
        for k in 0..m {
            self.t[k] = if k <= p {
                (0..=p - k).map(|i| a[i] * a[i + k]).sum()
            } else {
                0.0
            };
        }
        self.t[0] *= 1.0 + 1e-10;
        // The errors from the known samples alone, n = s .. e + p.
        for (j, n) in (s..e + p).enumerate() {
            let mut acc = 0.0;
            for (i, ai) in a.iter().enumerate() {
                let k = n - i;
                if k < s || k >= e {
                    acc += ai * x[k];
                }
            }
            self.c[j] = acc;
        }
        // b_i = −Σ_n a[n − (s+i)] c_n.
        for i in 0..m {
            let mut acc = 0.0;
            for (q, ai) in a.iter().enumerate() {
                acc += ai * self.c[i + q];
            }
            self.b[i] = -acc;
        }
        if !solve_toeplitz(
            &self.t[..m],
            &self.b[..m],
            &mut self.x[..m],
            &mut self.y[..m],
        ) {
            return false;
        }
        x[s..e].copy_from_slice(&self.x[..m]);
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn levinson_finds_the_model_of_an_ar_process() {
        // x[n] = 1.6 x[n−1] − 0.8 x[n−2] + w[n].
        let mut noise = crate::Noise::new(7);
        let mut x = vec![0.0f64; 20_000];
        for n in 2..x.len() {
            x[n] = 1.6 * x[n - 1] - 0.8 * x[n - 2] + noise.next();
        }
        let mut fit = ArFit::new(x.len(), 2);
        fit.fit(&x);
        assert!((fit.a[1] + 1.6).abs() < 0.02, "{:?}", fit.a);
        assert!((fit.a[2] - 0.8).abs() < 0.02, "{:?}", fit.a);
    }

    #[test]
    fn the_toeplitz_solver_agrees_with_elimination() {
        let m = 37;
        // A positive definite Toeplitz matrix: an AR polynomial's
        // autocorrelation plus a ridge.
        let a = [1.0, -1.2, 0.5, 0.1, -0.05];
        let mut t = vec![0.0; m];
        for k in 0..m {
            if k < a.len() {
                t[k] = (0..a.len() - k).map(|i| a[i] * a[i + k]).sum();
            }
        }
        t[0] += 0.01;
        let b: Vec<f64> = (0..m).map(|i| ((i * 7) % 5) as f64 - 2.0).collect();
        let (mut x, mut y) = (vec![0.0; m], vec![0.0; m]);
        assert!(solve_toeplitz(&t, &b, &mut x, &mut y));
        // Multiply back.
        for i in 0..m {
            let row: f64 = (0..m).map(|j| t[i.abs_diff(j)] * x[j]).sum();
            assert!((row - b[i]).abs() < 1e-8, "row {i}: {row} vs {}", b[i]);
        }
    }

    #[test]
    fn a_gap_in_a_tone_is_filled_with_the_tone() {
        let rate = 48_000.0;
        let x: Vec<f64> = (0..4096)
            .map(|n| {
                let t = n as f64 / rate;
                0.5 * (std::f64::consts::TAU * 440.0 * t).sin()
                    + 0.2 * (std::f64::consts::TAU * 1_330.0 * t).sin()
            })
            .collect();
        let mut fit = ArFit::new(x.len(), 24);
        fit.fit(&x);
        let mut damaged = x.clone();
        for v in &mut damaged[2000..2060] {
            *v = 0.9;
        }
        let mut interp = Interpolator::new(256, 64);
        assert!(interp.fill(&mut damaged, 2000, 2060, &fit.a));
        let err: f64 = (2000..2060)
            .map(|n| (damaged[n] - x[n]).powi(2))
            .sum::<f64>()
            .sqrt();
        let sig: f64 = (2000..2060).map(|n| x[n].powi(2)).sum::<f64>().sqrt();
        assert!(err < 0.01 * sig, "error {err} of {sig}");
    }
}

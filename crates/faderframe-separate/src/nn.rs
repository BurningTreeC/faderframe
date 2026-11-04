//! The network's pieces: matrix products, inference batch norms, LSTMs —
//! in f32 as PyTorch computes them — and a small work queue spreading
//! them over threads.

use std::sync::Mutex;

/// Run `work` on every item, on up to `threads` threads taking items in
/// turn. A panicking item takes the whole call down with it.
pub(crate) fn parallel<T: Send>(threads: usize, items: Vec<T>, work: impl Fn(T) + Sync) {
    let n = items.len();
    if n == 0 {
        return;
    }
    let threads = threads.clamp(1, n);
    if threads == 1 {
        items.into_iter().for_each(work);
        return;
    }
    let queue = Mutex::new(items.into_iter());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let next = queue.lock().unwrap_or_else(|e| e.into_inner()).next();
                    match next {
                        Some(item) => work(item),
                        None => break,
                    }
                }
            });
        }
    });
}

/// Rows a matrix-product task takes.
const ROWS_PER_TASK: usize = 64;

/// `y = x · wᵀ` for `rows` rows of `x` (`inp` each) and `w` (`out × inp`):
/// `rows × out` into `y`, the rows spread over `threads`.
pub(crate) fn matmul(x: &[f32], inp: usize, w: &[f32], out: usize, y: &mut [f32], threads: usize) {
    let rows = y.len() / out.max(1);
    debug_assert_eq!(x.len(), rows * inp);
    debug_assert_eq!(w.len(), out * inp);
    let tasks: Vec<(usize, &mut [f32])> = y
        .chunks_mut(ROWS_PER_TASK * out)
        .enumerate()
        .map(|(i, c)| (i * ROWS_PER_TASK, c))
        .collect();
    parallel(threads, tasks, |(first, dst)| {
        let n = dst.len() / out;
        product(&x[first * inp..(first + n) * inp], inp, w, out, dst);
    });
}

/// [`matmul`] on the calling thread.
pub(crate) fn product(x: &[f32], inp: usize, w: &[f32], out: usize, y: &mut [f32]) {
    let rows = y.len() / out.max(1);
    let mut r = 0;
    // Four rows at a time share each weight row's pass through memory.
    while r + 4 <= rows {
        let xs = [
            &x[r * inp..(r + 1) * inp],
            &x[(r + 1) * inp..(r + 2) * inp],
            &x[(r + 2) * inp..(r + 3) * inp],
            &x[(r + 3) * inp..(r + 4) * inp],
        ];
        for o in 0..out {
            let d = dot4(xs, &w[o * inp..(o + 1) * inp]);
            for (k, v) in d.iter().enumerate() {
                y[(r + k) * out + o] = *v;
            }
        }
        r += 4;
    }
    while r < rows {
        let xr = &x[r * inp..(r + 1) * inp];
        for o in 0..out {
            y[r * out + o] = dot(xr, &w[o * inp..(o + 1) * inp]);
        }
        r += 1;
    }
}

const LANES: usize = 8;

#[inline]
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; LANES];
    let chunks = a.len() / LANES;
    for c in 0..chunks {
        let (x, y) = (
            &a[c * LANES..(c + 1) * LANES],
            &b[c * LANES..(c + 1) * LANES],
        );
        for i in 0..LANES {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for i in chunks * LANES..a.len() {
        s += a[i] * b[i];
    }
    s
}

#[inline]
fn dot4(x: [&[f32]; 4], w: &[f32]) -> [f32; 4] {
    let mut acc = [[0.0f32; LANES]; 4];
    let chunks = w.len() / LANES;
    for c in 0..chunks {
        let wc = &w[c * LANES..(c + 1) * LANES];
        for (a, xr) in acc.iter_mut().zip(&x) {
            let xc = &xr[c * LANES..(c + 1) * LANES];
            for i in 0..LANES {
                a[i] += xc[i] * wc[i];
            }
        }
    }
    let mut out = [0.0f32; 4];
    for (k, a) in acc.iter().enumerate() {
        let mut s: f32 = a.iter().sum();
        for i in chunks * LANES..w.len() {
            s += x[k][i] * w[i];
        }
        out[k] = s;
    }
    out
}

/// An inference batch norm (or any per-feature affine map): `x · scale +
/// shift`.
#[derive(Clone, Debug)]
pub(crate) struct Affine {
    pub scale: Vec<f32>,
    pub shift: Vec<f32>,
}

impl Affine {
    /// `BatchNorm1d` in eval mode: `(x − mean) / √(var + ε) · γ + β`.
    pub fn batch_norm(gamma: &[f32], beta: &[f32], mean: &[f32], var: &[f32]) -> Self {
        const EPS: f32 = 1e-5;
        let scale: Vec<f32> = gamma
            .iter()
            .zip(var)
            .map(|(g, v)| g / (v + EPS).sqrt())
            .collect();
        let shift = beta
            .iter()
            .zip(mean)
            .zip(&scale)
            .map(|((b, m), s)| b - m * s)
            .collect();
        Self { scale, shift }
    }

    /// Apply to each row (`scale.len()` wide) of `x`.
    pub fn apply(&self, x: &mut [f32]) {
        let n = self.scale.len();
        for row in x.chunks_mut(n) {
            for ((v, s), b) in row.iter_mut().zip(&self.scale).zip(&self.shift) {
                *v = *v * s + b;
            }
        }
    }
}

/// One direction of one LSTM layer (PyTorch's gate order i, f, g, o).
pub(crate) struct LstmCell {
    /// `4H × input`.
    pub w_ih: Vec<f32>,
    /// `4H × H`.
    pub w_hh: Vec<f32>,
    /// `b_ih + b_hh`, `4H`.
    pub bias: Vec<f32>,
    pub hidden: usize,
    pub input: usize,
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

impl LstmCell {
    /// The input's share of the gates for every frame: `frames × 4H`.
    pub fn project(&self, x: &[f32], frames: usize, threads: usize) -> Vec<f32> {
        let g = 4 * self.hidden;
        let mut p = vec![0.0f32; frames * g];
        matmul(x, self.input, &self.w_ih, g, &mut p, threads);
        for row in p.chunks_mut(g) {
            for (v, b) in row.iter_mut().zip(&self.bias) {
                *v += b;
            }
        }
        p
    }

    /// Run over the projected frames (backwards when `reverse`), writing
    /// each frame's hidden state into `out` at `out_stride`, from column
    /// `column`.
    pub fn run(
        &self,
        projected: &[f32],
        frames: usize,
        reverse: bool,
        out: &mut [f32],
        out_stride: usize,
        column: usize,
    ) {
        let h_n = self.hidden;
        let mut h = vec![0.0f32; h_n];
        let mut c = vec![0.0f32; h_n];
        let mut gates = vec![0.0f32; 4 * h_n];
        for step in 0..frames {
            let t = if reverse { frames - 1 - step } else { step };
            gates.copy_from_slice(&projected[t * 4 * h_n..(t + 1) * 4 * h_n]);
            for (j, g) in gates.iter_mut().enumerate() {
                *g += dot(&self.w_hh[j * h_n..(j + 1) * h_n], &h);
            }
            for k in 0..h_n {
                let i = sigmoid(gates[k]);
                let f = sigmoid(gates[h_n + k]);
                let g = gates[2 * h_n + k].tanh();
                let o = sigmoid(gates[3 * h_n + k]);
                c[k] = f * c[k] + i * g;
                h[k] = o * c[k].tanh();
            }
            out[t * out_stride + column..t * out_stride + column + h_n].copy_from_slice(&h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn products_match_the_plain_sum() {
        let (rows, inp, out) = (11, 37, 5);
        let x: Vec<f32> = (0..rows * inp)
            .map(|i| ((i * 7) % 13) as f32 - 6.0)
            .collect();
        let w: Vec<f32> = (0..out * inp)
            .map(|i| ((i * 5) % 11) as f32 * 0.25)
            .collect();
        let mut y = vec![0.0f32; rows * out];
        matmul(&x, inp, &w, out, &mut y, 3);
        for r in 0..rows {
            for o in 0..out {
                let want: f32 = (0..inp).map(|i| x[r * inp + i] * w[o * inp + i]).sum();
                assert!((y[r * out + o] - want).abs() < 1e-3, "{r},{o}");
            }
        }
    }

    /// A one-unit LSTM worked by hand.
    #[test]
    fn an_lstm_step_follows_the_gates() {
        let cell = LstmCell {
            w_ih: vec![1.0, 0.5, -1.0, 2.0],
            w_hh: vec![0.0, 0.0, 0.0, 0.0],
            bias: vec![0.0, 0.0, 0.0, 0.0],
            hidden: 1,
            input: 1,
        };
        let p = cell.project(&[0.5], 1, 1);
        let mut out = [0.0f32];
        cell.run(&p, 1, false, &mut out, 1, 0);
        let s = |x: f32| 1.0 / (1.0 + (-x).exp());
        let c = s(0.5) * (-0.5f32).tanh();
        let want = s(1.0) * c.tanh();
        assert!((out[0] - want).abs() < 1e-6);
    }
}

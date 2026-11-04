//! Windows for the time-domain restorers (de-click, de-clip): each window
//! is `margin` samples already processed, `hop` samples to process now and
//! `margin` samples ahead, so a repair near either end of the hop has the
//! model's context on both sides. A window is ready every `hop` samples;
//! the latency is `hop + margin`.
//!
//! Two copies travel together: the work buffer (repairs written into it
//! stay as it shifts on, so a repair reaching into the samples ahead is not
//! made twice) and the input as it came (what the repairs are measured
//! against, and what "listen to what is taken" subtracts).

/// Several channels' windows.
pub struct Blocks {
    pub hop: usize,
    pub margin: usize,
    len: usize,
    work: Vec<Vec<f64>>,
    raw: Vec<Vec<f64>>,
    out: Vec<Vec<f64>>,
    out_raw: Vec<Vec<f64>>,
    at: usize,
}

impl Blocks {
    pub fn new(hop: usize, margin: usize, channels: usize) -> Self {
        let len = hop + 2 * margin;
        let lanes = |n: usize| (0..channels).map(|_| vec![0.0; n]).collect::<Vec<_>>();
        Self {
            hop,
            margin,
            len,
            work: lanes(len),
            raw: lanes(len),
            out: lanes(hop),
            out_raw: lanes(hop),
            at: 0,
        }
    }

    pub fn channels(&self) -> usize {
        self.work.len()
    }

    /// The window's length.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The delay from input to output (samples).
    pub fn latency(&self) -> usize {
        self.hop + self.margin
    }

    /// One sample in per channel; out the processed sample (`y`) and the
    /// input as it came (`dry`), `latency()` later. `true` when a window is
    /// ready: process [`Self::work`] (the hop is `margin..margin + hop`),
    /// then [`Self::finish`] before the next push.
    #[inline]
    pub fn push(&mut self, x: &[f64], y: &mut [f64], dry: &mut [f64]) -> bool {
        let i = self.len - self.hop + self.at;
        for c in 0..self.work.len() {
            let v = x.get(c).copied().unwrap_or(0.0);
            self.work[c][i] = v;
            self.raw[c][i] = v;
            if let Some(o) = y.get_mut(c) {
                *o = self.out[c][self.at];
            }
            if let Some(o) = dry.get_mut(c) {
                *o = self.out_raw[c][self.at];
            }
        }
        self.at += 1;
        if self.at < self.hop {
            return false;
        }
        self.at = 0;
        true
    }

    /// A channel's work buffer and the input as it came.
    pub fn window(&mut self, c: usize) -> (&mut [f64], &[f64]) {
        (&mut self.work[c], &self.raw[c])
    }

    /// The hop's samples out; the windows move on by a hop.
    pub fn finish(&mut self) {
        let (m, h) = (self.margin, self.hop);
        for c in 0..self.work.len() {
            self.out[c].copy_from_slice(&self.work[c][m..m + h]);
            self.out_raw[c].copy_from_slice(&self.raw[c][m..m + h]);
            self.work[c].copy_within(h.., 0);
            self.raw[c].copy_within(h.., 0);
        }
    }

    /// Silence (the windows are a few thousand samples: cleared).
    pub fn reset(&mut self) {
        self.at = 0;
        for v in self
            .work
            .iter_mut()
            .chain(self.raw.iter_mut())
            .chain(self.out.iter_mut())
            .chain(self.out_raw.iter_mut())
        {
            v.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_windows_give_the_input_back_after_the_latency() {
        let mut b = Blocks::new(64, 16, 1);
        let mut out = Vec::new();
        for n in 0..1000 {
            let (mut y, mut d) = ([0.0], [0.0]);
            if b.push(&[n as f64], &mut y, &mut d) {
                b.finish();
            }
            assert_eq!(y, d);
            out.push(y[0]);
        }
        let lat = b.latency();
        for (n, v) in out.iter().enumerate().skip(lat) {
            assert_eq!(*v, (n - lat) as f64, "at {n}");
        }
    }
}

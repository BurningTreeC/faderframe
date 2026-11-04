//! The adaptive resampler between a follower's frames and the master's:
//! a polyphase windowed-sinc FIR read at a fractional position that
//! advances by a step the drift loop may change from call to call.
//!
//! The prototype is a Kaiser-windowed sinc (β 10, about 100 dB down in the
//! stop band) with [`HALF`] taps on each side of the read position at a
//! ratio of one, more when the rate goes down (the filter then cuts at the
//! lower rate's band). Its passband reaches 84 % of the lower Nyquist
//! frequency (20.2 kHz at 48 kHz), the cut-off is at 92 %. The prototype
//! is tabled at [`PHASES`] fractional offsets, and a read interpolates
//! linearly between the two nearest: the error of that is about 110 dB
//! down. Each table row is normalised to a gain of exactly one.
//!
//! The delay is [`Resampler::delay`] input frames (the taps ahead of the
//! read position). Everything is allocated when made; [`Resampler::push`],
//! [`Resampler::read`] and the rest allocate nothing.

use std::f64::consts::PI;

/// Taps on each side of the read position at a ratio of one or above.
pub const HALF: usize = 40;
/// Fractional offsets tabled.
pub const PHASES: usize = 512;
/// Cut-off as a share of the lower rate's Nyquist frequency.
const CUTOFF: f64 = 0.92;
/// Kaiser window β.
const BETA: f64 = 10.0;

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

/// See the module docs. Planar: every channel has its own history, all
/// share the read position.
#[derive(Debug)]
pub struct Resampler {
    channels: usize,
    /// Taps on each side of the read position.
    half: usize,
    taps: usize,
    /// `(PHASES + 1) × taps`: row `p` holds the weights for a read
    /// position `p / PHASES` past a frame, oldest tap first.
    table: Box<[f32]>,
    /// Input frames per channel, oldest first.
    fifo: Box<[Box<[f32]>]>,
    /// Frames held.
    len: usize,
    /// Read position of the next output (frames into `fifo`).
    pos: f64,
    /// Input frames per output frame.
    step: f64,
    /// Interpolated weights of one output frame.
    kernel: Box<[f32]>,
}

impl Resampler {
    /// For `channels` at about `nominal` input frames per output frame;
    /// up to `max_input` input frames are pushed at once (or needed for
    /// one read).
    pub fn new(channels: usize, nominal: f64, max_input: usize) -> Self {
        let nominal = if nominal.is_finite() && nominal > 0.0 {
            nominal
        } else {
            1.0
        };
        // Going down in rate, the filter cuts at the output's band and
        // takes as many more taps (the same transition in output terms).
        let scale = nominal.max(1.0);
        let half = (HALF as f64 * scale).ceil() as usize;
        let taps = 2 * half;
        let fc = 0.5 * CUTOFF / scale; // cycles per input frame
        let i0b = bessel_i0(BETA);
        let mut table = vec![0.0f32; (PHASES + 1) * taps].into_boxed_slice();
        let mut row = vec![0.0f64; taps];
        for p in 0..=PHASES {
            let f = p as f64 / PHASES as f64;
            let mut sum = 0.0;
            for (m, w) in row.iter_mut().enumerate() {
                // Tap m reads the frame `m − half + 1` from the one
                // before the read position.
                let t = f + half as f64 - 1.0 - m as f64;
                let u = t / half as f64;
                let window = if u.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(BETA * (1.0 - u * u).sqrt()) / i0b
                };
                let x = 2.0 * fc * t;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                *w = 2.0 * fc * sinc * window;
                sum += *w;
            }
            for (m, w) in row.iter().enumerate() {
                table[p * taps + m] = (w / sum) as f32;
            }
        }
        // Room for the taps, what one call brings and what is left over
        // from the last one.
        let capacity = 2 * taps + max_input + nominal.ceil() as usize + 8;
        let mut r = Self {
            channels,
            half,
            taps,
            table,
            fifo: (0..channels)
                .map(|_| vec![0.0f32; capacity].into_boxed_slice())
                .collect(),
            len: 0,
            pos: 0.0,
            step: nominal,
            kernel: vec![0.0f32; taps].into_boxed_slice(),
        };
        r.reset();
        r
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Input frames between a frame pushed and the output read at it.
    pub fn delay(&self) -> usize {
        self.half
    }

    /// Start again: silence before, the next frame pushed is the next one
    /// read (after [`Self::delay`] more).
    pub fn reset(&mut self) {
        for c in self.fifo.iter_mut() {
            c[..self.half].fill(0.0);
        }
        self.len = self.half;
        self.pos = self.half as f64;
    }

    pub fn step(&self) -> f64 {
        self.step
    }

    pub fn set_step(&mut self, step: f64) {
        if step.is_finite() && step > 0.0 {
            self.step = step;
        }
    }

    fn capacity(&self) -> usize {
        self.fifo.first().map_or(0, |c| c.len())
    }

    /// Room for more input frames.
    pub fn room(&self) -> usize {
        self.capacity().saturating_sub(self.len)
    }

    /// Input frames held from the read position on (frames, fractional).
    pub fn ahead(&self) -> f64 {
        self.len as f64 - self.pos
    }

    /// Append `frames` frames: channel `c`'s from `channel(c)` (missing
    /// samples are silence). Frames beyond [`Self::room`] are dropped.
    pub fn push<'a>(
        &mut self,
        frames: usize,
        mut channel: impl FnMut(usize) -> &'a [f32],
    ) -> usize {
        let n = frames.min(self.room());
        let at = self.len;
        for (c, d) in self.fifo.iter_mut().enumerate() {
            let src = channel(c);
            let k = n.min(src.len());
            d[at..at + k].copy_from_slice(&src[..k]);
            d[at + k..at + n].fill(0.0);
        }
        self.len += n;
        n
    }

    /// Append frames from interleaved slices (a ring's two halves),
    /// `channels` samples a frame. Returns the frames taken.
    pub fn push_interleaved(&mut self, first: &[f32], second: &[f32]) -> usize {
        let ch = self.channels.max(1);
        let frames = ((first.len() + second.len()) / ch).min(self.room());
        let mut i = 0usize;
        let at = self.len;
        for (s, v) in first.iter().chain(second).take(frames * ch).enumerate() {
            let c = s % ch;
            if let Some(d) = self.fifo.get_mut(c) {
                d[at + i] = *v;
            }
            if c + 1 == ch {
                i += 1;
            }
        }
        self.len += frames;
        frames
    }

    /// Input frames to push before `n` outputs can be read at the
    /// current step.
    pub fn needed(&self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let last = self.pos + (n - 1) as f64 * self.step;
        (last.floor() as usize + self.half + 1).saturating_sub(self.len)
    }

    /// Outputs that can be read at the current step with what is held.
    pub fn readable(&self) -> usize {
        let z = (self.len as f64 - self.half as f64 - self.pos) / self.step;
        if z <= 0.0 {
            return 0;
        }
        // The read position of output k must stay below `len − half`.
        let k = z.ceil() as usize;
        let mut k = k;
        while k > 0
            && ((self.pos + (k - 1) as f64 * self.step).floor() as usize + self.half) >= self.len
        {
            k -= 1;
        }
        k
    }

    /// Read `n` outputs into `out[c][at..at + n]` (fewer when not enough
    /// is held: the rest is silence). Then drop what no later read needs.
    pub fn read(&mut self, n: usize, out: &mut [Box<[f32]>], at: usize) -> usize {
        let n = n.min(self.readable());
        let taps = self.taps;
        for j in 0..n {
            let i = self.pos.floor();
            let f = self.pos - i;
            let pf = f * PHASES as f64;
            let p = (pf as usize).min(PHASES - 1);
            let a = (pf - p as f64) as f32;
            let r0 = &self.table[p * taps..(p + 1) * taps];
            let r1 = &self.table[(p + 1) * taps..(p + 2) * taps];
            for ((k, w0), w1) in self.kernel.iter_mut().zip(r0).zip(r1) {
                *k = w0 + a * (w1 - w0);
            }
            let start = (i as usize + 1).saturating_sub(self.half);
            for (c, d) in self.fifo.iter().enumerate() {
                let Some(o) = out.get_mut(c).and_then(|o| o.get_mut(at + j)) else {
                    continue;
                };
                let src = &d[start..start + taps];
                let mut acc = 0.0f32;
                for (w, x) in self.kernel.iter().zip(src) {
                    acc += w * x;
                }
                *o = acc;
            }
            self.pos += self.step;
        }
        self.compact();
        n
    }

    /// Drop the frames before the next read's first tap.
    fn compact(&mut self) {
        let keep_from = (self.pos.floor() as usize + 1)
            .saturating_sub(self.half)
            .min(self.len);
        if keep_from == 0 {
            return;
        }
        for d in self.fifo.iter_mut() {
            d.copy_within(keep_from..self.len, 0);
        }
        self.len -= keep_from;
        self.pos -= keep_from as f64;
    }
}

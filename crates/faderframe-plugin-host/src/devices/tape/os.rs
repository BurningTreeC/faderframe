//! The tape's oversampler: halfband stages up and down (linear phase, as
//! [`crate::dsp::oversample`]), with the transport between them.
//!
//! The way down is the standard cascade (its first stage long enough to
//! keep 20 kHz flat at 44.1 kHz: the aliases of the tape's harmonics are
//! what it must stop). The way up has a shorter first stage (65 taps,
//! Kaiser β 7: images 70 dB down, flat to 0.43 fs) and the time it saves
//! is the transport's room: the tape is read back through a delay that
//! swings round its middle with the wow and flutter, so the whole round
//! trip is exactly the standard oversampler's latency — 64, 72 or 74
//! samples at 2×, 4× or 8× — and the transport adds none of its own.

/// (half length `M`, Kaiser β) of the stages: up, down.
const UP: [(usize, f64); 3] = [(32, 7.0), (16, 8.0), (8, 7.0)];
const DOWN: [(usize, f64); 3] = [(64, 10.0), (16, 8.0), (8, 7.0)];

/// The transport's room each way (host samples).
pub const ROOM: usize = 16;

/// The supported factors and their latencies (host samples).
pub fn latency(factor: usize) -> u32 {
    let stages = stages(factor);
    let one = |set: &[(usize, f64); 3]| -> usize {
        set.iter()
            .take(stages)
            .enumerate()
            .map(|(i, &(m, _))| (m / 2) >> i)
            .sum()
    };
    (one(&UP) + one(&DOWN) + if stages > 0 { ROOM } else { 0 }) as u32
}

fn stages(factor: usize) -> usize {
    match factor {
        0..=1 => 0,
        2..=3 => 1,
        4..=7 => 2,
        _ => 3,
    }
}

/// Odd taps of a Kaiser-windowed halfband of length `2M + 1`, scaled for
/// unity gain at DC (the centre tap 0.5 is implicit).
fn halfband(m: usize, beta: f64) -> Vec<f64> {
    let n = 2 * m + 1;
    let i0 = |x: f64| {
        let (mut sum, mut term, half) = (1.0, 1.0, x / 2.0);
        for k in 1..50 {
            term *= (half / k as f64).powi(2);
            sum += term;
            if term < 1e-18 * sum {
                break;
            }
        }
        sum
    };
    let denom = i0(beta);
    let mut taps: Vec<f64> = (0..m)
        .map(|k| {
            let j = 2 * k + 1;
            let t = (j as f64 - m as f64) / 2.0;
            let px = std::f64::consts::PI * t;
            let ideal = 0.5 * if t.abs() < 1e-12 { 1.0 } else { px.sin() / px };
            let r = 2.0 * j as f64 / (n - 1) as f64 - 1.0;
            ideal * i0(beta * (1.0 - r * r).max(0.0).sqrt()) / denom
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter_mut().for_each(|c| *c *= 0.5 / sum);
    taps
}

/// A history read as a contiguous window (twice its length).
struct Fir {
    taps: Vec<f64>,
    buf: Vec<f64>,
    pos: usize,
}

impl Fir {
    fn new(taps: Vec<f64>) -> Self {
        let m = taps.len();
        Self {
            taps,
            buf: vec![0.0; 2 * m],
            pos: 0,
        }
    }

    #[inline]
    fn dot(&self) -> f64 {
        let m = self.taps.len();
        self.taps
            .iter()
            .zip(&self.buf[self.pos..self.pos + m])
            .map(|(c, x)| c * x)
            .sum()
    }

    #[inline]
    fn push(&mut self, x: f64) {
        let m = self.taps.len();
        self.pos = if self.pos == 0 { m - 1 } else { self.pos - 1 };
        self.buf[self.pos] = x;
        self.buf[self.pos + m] = x;
    }

    fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.pos = 0;
    }
}

/// A whole-sample delay.
struct Delay {
    buf: Vec<f64>,
    pos: usize,
}

impl Delay {
    fn new(n: usize) -> Self {
        Self {
            buf: vec![0.0; n.max(1)],
            pos: 0,
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.buf[self.pos];
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }

    fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.pos = 0;
    }
}

struct Up {
    fir: Fir,
    delay: Delay,
}

impl Up {
    fn new((m, beta): (usize, f64)) -> Self {
        Self {
            fir: Fir::new(halfband(m, beta).into_iter().map(|c| 2.0 * c).collect()),
            delay: Delay::new(m / 2),
        }
    }

    #[inline]
    fn up(&mut self, x: f64) -> (f64, f64) {
        self.fir.push(x);
        (self.delay.process(x), self.fir.dot())
    }

    fn reset(&mut self) {
        self.fir.reset();
        self.delay.reset();
    }
}

struct Down {
    fir: Fir,
    delay: Delay,
}

impl Down {
    fn new((m, beta): (usize, f64)) -> Self {
        Self {
            fir: Fir::new(halfband(m, beta)),
            delay: Delay::new(m / 2),
        }
    }

    #[inline]
    fn down(&mut self, even: f64, odd: f64) -> f64 {
        let y = 0.5 * self.delay.process(even) + self.fir.dot();
        self.fir.push(odd);
        y
    }

    fn reset(&mut self) {
        self.fir.reset();
        self.delay.reset();
    }
}

/// The tape's read-back at the oversampled rate: a delay line read at a
/// fractional delay (sixth-order Lagrange: within 0.1 dB to 20 kHz at 2×).
struct Reader {
    buf: [f64; 512],
    pos: usize,
}

impl Reader {
    #[inline]
    fn push(&mut self, x: f64) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & 511;
    }

    /// `n` samples back (0: the last pushed).
    #[inline]
    fn tap(&self, n: usize) -> f64 {
        self.buf[(self.pos + 511 - (n & 511)) & 511]
    }

    /// The whole part and the weights of a read at a delay `d` (at least
    /// 2, at most 500 samples): nodes at delays i−2 … i+3, evaluated at
    /// i + t.
    #[inline]
    fn weights(d: f64) -> (usize, [f64; 6]) {
        const DEN: [f64; 6] = [-120.0, 24.0, -12.0, 12.0, -24.0, 120.0];
        let d = d.clamp(2.0, 500.0);
        let i = d.floor();
        let x = d - i + 2.0;
        let mut pre = [1.0f64; 6];
        let mut post = [1.0f64; 6];
        for k in 1..6 {
            pre[k] = pre[k - 1] * (x - (k - 1) as f64);
            post[5 - k] = post[6 - k] * (x - (6 - k) as f64);
        }
        let mut w = [0.0; 6];
        for k in 0..6 {
            w[k] = pre[k] * post[k] / DEN[k];
        }
        (i as usize, w)
    }

    #[inline]
    fn read(&self, (i, w): (usize, [f64; 6])) -> f64 {
        let mut y = 0.0;
        for (k, w) in w.iter().enumerate() {
            y += w * self.tap(i + k - 2);
        }
        y
    }
}

/// One channel's oversampler.
pub struct TapeOversampler {
    factor: usize,
    up: [Up; 3],
    down: [Down; 3],
    reader: Reader,
}

/// The most host samples the transport may displace the tape.
pub fn reach(factor: usize) -> f64 {
    ROOM as f64 - 3.0 / factor.max(1) as f64 - 0.5
}

impl TapeOversampler {
    pub fn new(factor: usize) -> Self {
        let factor = 1 << stages(factor).max(1);
        Self {
            factor,
            up: UP.map(Up::new),
            down: DOWN.map(Down::new),
            reader: Reader {
                buf: [0.0; 512],
                pos: 0,
            },
        }
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    pub fn latency(&self) -> u32 {
        latency(self.factor)
    }

    /// One host sample `x` up, read back `displace` host samples later than
    /// the middle of the room, `f` run at the oversampled rate on each, and
    /// down again.
    #[inline]
    pub fn process(&mut self, x: f64, displace: f64, f: &mut impl FnMut(f64) -> f64) -> f64 {
        let mut hi = [0.0f64; 8];
        let n = self.factor;
        // Up.
        let (e, o) = self.up[0].up(x);
        match n {
            2 => {
                hi[0] = e;
                hi[1] = o;
            }
            4 => {
                (hi[0], hi[1]) = self.up[1].up(e);
                (hi[2], hi[3]) = self.up[1].up(o);
            }
            _ => {
                let (a, b) = self.up[1].up(e);
                let (c, d) = self.up[1].up(o);
                for (i, v) in [a, b, c, d].into_iter().enumerate() {
                    (hi[2 * i], hi[2 * i + 1]) = self.up[2].up(v);
                }
            }
        }
        // Read back through the transport, run the tape.
        let room = (ROOM * n) as f64;
        let d = (room + displace.clamp(-reach(n), reach(n)) * n as f64).max(2.0);
        let at = Reader::weights(d);
        for v in hi.iter_mut().take(n) {
            self.reader.push(*v);
            *v = f(self.reader.read(at));
        }
        // Down.
        match n {
            2 => self.down[0].down(hi[0], hi[1]),
            4 => {
                let a = self.down[1].down(hi[0], hi[1]);
                let b = self.down[1].down(hi[2], hi[3]);
                self.down[0].down(a, b)
            }
            _ => {
                let mut q = [0.0f64; 4];
                for (i, q) in q.iter_mut().enumerate() {
                    *q = self.down[2].down(hi[2 * i], hi[2 * i + 1]);
                }
                let a = self.down[1].down(q[0], q[1]);
                let b = self.down[1].down(q[2], q[3]);
                self.down[0].down(a, b)
            }
        }
    }

    pub fn reset(&mut self) {
        for u in &mut self.up {
            u.reset();
        }
        for d in &mut self.down {
            d.reset();
        }
        self.reader.buf = [0.0; 512];
    }
}

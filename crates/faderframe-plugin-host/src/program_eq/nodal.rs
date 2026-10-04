//! A miniature circuit simulator.
//!
//! From PultEQFx by Simon Huber, used in FaderFrame under the MIT licence.
//!
//! The passive Pultec network is a bridge: the output node is fed from two
//! paths that rejoin, so it cannot be folded into a series/parallel tree. It is
//! solved the way SPICE solves a linear circuit instead. Every reactive element
//! is replaced by its trapezoidal companion (a conductance in parallel with a
//! current source that carries the element's history), which turns the whole
//! thing into a resistive network described by
//!
//! ```text
//!     G v = i
//! ```
//!
//! `G` only changes when a knob moves, so it is factorised at control rate and
//! each sample costs one forward and one back substitution.
//!
//! Reactive elements are discretised with a constant `c`, which is `2 / T` for
//! a plain bilinear transform. Elements whose behaviour hinges on a particular
//! frequency instead use `c = w0 / tan(w0 * T / 2)`, which makes the digital
//! response exact at `w0`. Without that the 16 kHz bell would sit near 12 kHz
//! at a 44.1 kHz sample rate.

/// Node index standing in for the ground rail.
pub const GND: usize = usize::MAX;

/// A nodal network with `N` unknown node voltages.
pub struct Network<const N: usize> {
    /// Conductance matrix.
    g: [[f64; N]; N],
    /// Lower triangular Cholesky factor of `g`.
    l: [[f64; N]; N],
    /// Right hand side, rebuilt every sample.
    rhs: [f64; N],
    /// Solution.
    v: [f64; N],
}

impl<const N: usize> Default for Network<N> {
    fn default() -> Self {
        Self {
            g: [[0.0; N]; N],
            l: [[0.0; N]; N],
            rhs: [0.0; N],
            v: [0.0; N],
        }
    }
}

impl<const N: usize> Network<N> {
    /// Start building a new conductance matrix.
    pub fn clear(&mut self) {
        self.g = [[0.0; N]; N];
    }

    /// Add a conductance between two nodes.
    pub fn conductance(&mut self, a: usize, b: usize, g: f64) {
        if a != GND {
            self.g[a][a] += g;
        }
        if b != GND {
            self.g[b][b] += g;
        }
        if a != GND && b != GND {
            self.g[a][b] -= g;
            self.g[b][a] -= g;
        }
    }

    /// Add a resistor. Values are clamped away from zero so a pot at its end
    /// stop cannot produce an infinite conductance.
    pub fn resistor(&mut self, a: usize, b: usize, ohms: f64) {
        self.conductance(a, b, 1.0 / ohms.max(MIN_R));
    }

    /// Factorise the conductance matrix. The matrix of a passive network with
    /// every node tied to ground through some path is symmetric positive
    /// definite, so a Cholesky decomposition is enough.
    pub fn factorize(&mut self) {
        for i in 0..N {
            for j in 0..=i {
                let mut sum = self.g[i][j];
                for k in 0..j {
                    sum -= self.l[i][k] * self.l[j][k];
                }
                if i == j {
                    // The clamp is belt and braces: a well formed network
                    // cannot get here with a non-positive pivot.
                    self.l[i][j] = sum.max(1e-30).sqrt();
                } else {
                    self.l[i][j] = sum / self.l[j][j];
                }
            }
            for j in i + 1..N {
                self.l[i][j] = 0.0;
            }
        }
    }

    /// Start a new sample by zeroing the excitation.
    #[inline]
    pub fn begin(&mut self) {
        self.rhs = [0.0; N];
    }

    /// Inject a current source flowing from `b` into `a`.
    #[inline]
    pub fn current(&mut self, a: usize, b: usize, amps: f64) {
        if a != GND {
            self.rhs[a] += amps;
        }
        if b != GND {
            self.rhs[b] -= amps;
        }
    }

    /// Drive a node through a conductance from a known voltage. The
    /// conductance itself must already be part of the matrix.
    #[inline]
    pub fn drive(&mut self, node: usize, conductance: f64, volts: f64) {
        if node != GND {
            self.rhs[node] += conductance * volts;
        }
    }

    /// Solve for the node voltages.
    #[inline]
    pub fn solve(&mut self) {
        // Forward substitution, L y = rhs.
        for i in 0..N {
            let mut sum = self.rhs[i];
            for k in 0..i {
                sum -= self.l[i][k] * self.v[k];
            }
            self.v[i] = sum / self.l[i][i];
        }
        // Back substitution, L^T v = y.
        for i in (0..N).rev() {
            let mut sum = self.v[i];
            for k in i + 1..N {
                sum -= self.l[k][i] * self.v[k];
            }
            self.v[i] = sum / self.l[i][i];
        }
    }

    #[inline]
    pub fn voltage(&self, node: usize) -> f64 {
        if node == GND { 0.0 } else { self.v[node] }
    }

    #[inline]
    pub fn across(&self, a: usize, b: usize) -> f64 {
        self.voltage(a) - self.voltage(b)
    }

    pub fn reset(&mut self) {
        self.v = [0.0; N];
        self.rhs = [0.0; N];
    }
}

/// Smallest resistance any element may take, in ohm.
const MIN_R: f64 = 1e-3;

/// The discretisation constant for an element that does not need prewarping.
pub fn bilinear(sample_rate: f64) -> f64 {
    2.0 * sample_rate
}

/// A discretisation constant that makes the bilinear transform exact at
/// `freq_hz`.
pub fn prewarped(freq_hz: f64, sample_rate: f64) -> f64 {
    let w0 = std::f64::consts::TAU * freq_hz.clamp(1.0, 0.45 * sample_rate);
    w0 / (w0 / (2.0 * sample_rate)).tan()
}

/// A capacitor discretised with the trapezoidal rule.
#[derive(Clone, Copy)]
pub struct Capacitor {
    pub a: usize,
    pub b: usize,
    /// Companion conductance, `c * C`.
    g: f64,
    /// Voltage and current from the previous sample.
    v: f64,
    i: f64,
}

impl Capacitor {
    pub fn new(a: usize, b: usize) -> Self {
        Self {
            a,
            b,
            g: 0.0,
            v: 0.0,
            i: 0.0,
        }
    }

    pub fn set(&mut self, farads: f64, c: f64) {
        self.g = farads * c;
    }

    pub fn conductance(&self) -> f64 {
        self.g
    }

    /// The history current for this sample.
    #[inline]
    fn source(&self) -> f64 {
        self.g * self.v + self.i
    }

    #[inline]
    pub fn stamp<const N: usize>(&self, net: &mut Network<N>) {
        net.current(self.a, self.b, self.source());
    }

    #[inline]
    pub fn update<const N: usize>(&mut self, net: &Network<N>) {
        let source = self.source();
        self.v = net.across(self.a, self.b);
        self.i = self.g * self.v - source;
    }

    pub fn reset(&mut self) {
        self.v = 0.0;
        self.i = 0.0;
    }
}

/// An inductor discretised with the trapezoidal rule.
#[derive(Clone, Copy)]
pub struct Inductor {
    pub a: usize,
    pub b: usize,
    /// Companion conductance, `1 / (c * L)`.
    g: f64,
    v: f64,
    i: f64,
}

impl Inductor {
    pub fn new(a: usize, b: usize) -> Self {
        Self {
            a,
            b,
            g: 0.0,
            v: 0.0,
            i: 0.0,
        }
    }

    pub fn set(&mut self, henries: f64, c: f64) {
        self.g = 1.0 / (henries * c);
    }

    pub fn conductance(&self) -> f64 {
        self.g
    }

    #[inline]
    fn source(&self) -> f64 {
        self.i + self.g * self.v
    }

    #[inline]
    pub fn stamp<const N: usize>(&self, net: &mut Network<N>) {
        net.current(self.a, self.b, -self.source());
    }

    #[inline]
    pub fn update<const N: usize>(&mut self, net: &Network<N>) {
        let source = self.source();
        self.v = net.across(self.a, self.b);
        self.i = self.g * self.v + source;
    }

    pub fn reset(&mut self) {
        self.v = 0.0;
        self.i = 0.0;
    }
}

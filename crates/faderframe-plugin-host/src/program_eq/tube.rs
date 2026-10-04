//! The make-up amplifier.
//!
//! From PultEQFx by Simon Huber, used in FaderFrame under the MIT licence.
//!
//! The passive EQ network throws away roughly 22 dB, which the EQP-1A's tube
//! amplifier puts back. That amplifier is the other half of the unit's
//! reputation: a push-pull triode stage into an output transformer, so it
//! contributes mostly odd order harmonics with a little second order from the
//! stage imbalance, plus the bandwidth limits of the iron at both ends.
//!
//! The stage itself is fixed, as the hardware's is: clean at the level the
//! unit is run at, and running gently out of headroom as the peaks approach
//! digital full scale. What it sounds like is decided by how hard it is hit.
//!
//! The `drive` control is the one liberty this plugin takes with the hardware,
//! and it does what hitting the hardware harder does: it raises the level into
//! the stage, so the sound gets louder and dirtier together, and the output
//! trim after it brings the level back. It used to be a saturation amount that
//! held quiet signals at the same level and squashed loud ones, which read as a
//! compressor that made things quieter the further it was turned up.

use std::f64::consts::TAU;

/// A one pole filter, for the coupling roll off at the bottom.
#[derive(Default)]
struct OnePole {
    a: f64,
    z: f64,
}

impl OnePole {
    fn set_cutoff(&mut self, freq: f64, sample_rate: f64) {
        let f = freq.clamp(1.0, 0.45 * sample_rate);
        // Matched one pole coefficient.
        self.a = (-TAU * f / sample_rate).exp();
    }

    #[inline]
    fn lowpass(&mut self, x: f64) -> f64 {
        self.z = x * (1.0 - self.a) + self.z * self.a;
        self.z
    }

    #[inline]
    fn highpass(&mut self, x: f64) -> f64 {
        x - self.lowpass(x)
    }

    fn reset(&mut self) {
        self.z = 0.0;
    }
}

/// The output transformer's band limit, a first order low pass.
///
/// Its corner is above Nyquist whenever the amplifier runs at the host rate,
/// and a plain one pole cannot go there. This used to squeeze it down to 45 %
/// of the sample rate instead, which with the oversampling off cost half a
/// decibel at 10 kHz and a whole one at 20 kHz: the oversampling setting
/// changed the tone. Instead the pole goes where impulse invariance puts it
/// and a zero is added, placed so the magnitude matches the analog filter's
/// exactly at DC and at `MATCHED_HZ` (or Nyquist, if that is lower). In
/// between it stays within a tenth of a decibel at the host rate, and a
/// hundredth once oversampled.
#[derive(Default)]
struct BandLimit {
    b0: f64,
    b1: f64,
    pole: f64,
    x1: f64,
    y1: f64,
}

impl BandLimit {
    fn set_cutoff(&mut self, freq: f64, sample_rate: f64) {
        let pole = (-TAU * freq / sample_rate).exp();
        let at = MATCHED_HZ.min(0.5 * sample_rate);
        let (cos, k) = {
            let cos = (TAU * at / sample_rate).cos();
            (cos, 1.0 - cos)
        };
        // The analog filter's power gain at the matched frequency, carried
        // over the pole's own response there.
        let want = (1.0 + pole * pole - 2.0 * pole * cos) / (1.0 + (at / freq).powi(2));
        // The zeros must sum to `1 - pole` for unity gain at DC. With that
        // fixed, |b0 + b1 e^-jw|^2 = want is a quadratic in their difference.
        let sum = 1.0 - pole;
        let spread = (sum * sum - 2.0 * (sum * sum - want) / k).max(0.0).sqrt();
        self.b0 = 0.5 * (sum + spread);
        self.b1 = 0.5 * (sum - spread);
        self.pole = pole;
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.pole * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

pub struct TubeStage {
    /// Coupling transformer roll off.
    coupling: OnePole,
    /// Output transformer roll off.
    bandwidth: BandLimit,
    /// How much the signal is raised going in: the drive control, as a gain.
    gain: f64,
    /// The shaper's output at rest, taken off so silence stays silent.
    rest: f64,
    /// The shaper's small signal gain, divided back out, so that with no drive
    /// a quiet signal comes out at the level it went in.
    norm: f64,
}

impl TubeStage {
    pub fn new(sample_rate: f64) -> Self {
        let rest = (HARDNESS * BIAS).tanh();
        let mut stage = Self {
            coupling: OnePole::default(),
            bandwidth: BandLimit::default(),
            gain: 1.0,
            rest,
            // d/dx of the shaper at x = 0.
            norm: HARDNESS * (1.0 - rest * rest),
        };
        stage.set_sample_rate(sample_rate);
        stage
    }

    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.coupling.set_cutoff(COUPLING_HZ, sample_rate);
        self.bandwidth.set_cutoff(BANDWIDTH_HZ, sample_rate);
    }

    /// How far the level into the stage is raised, in decibels. The level out
    /// rises with it, less whatever the stage squashes.
    pub fn set_drive(&mut self, db: f64) {
        if db.is_finite() {
            self.gain = 10f64.powf(db / 20.0);
        }
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let x = self.coupling.highpass(x) * self.gain;
        let y = ((HARDNESS * (x + BIAS)).tanh() - self.rest) / self.norm;
        self.bandwidth.process(y)
    }

    pub fn reset(&mut self) {
        self.coupling.reset();
        self.bandwidth.reset();
    }
}

/// Input coupling roll off, in Hz.
const COUPLING_HZ: f64 = 3.0;
/// Output bandwidth limit, in Hz.
const BANDWIDTH_HZ: f64 = 60e3;
/// Where the band limit is made to agree with the analog filter exactly: the
/// top of the audio band, where the difference would show first.
const MATCHED_HZ: f64 = 20e3;
/// How hard the stage bends. With no drive, a sine at digital full scale loses
/// a third of a decibel to it and comes out with 1.3 % of third harmonic and
/// 0.9 % of second, while one at -18 dBFS, where the unit's +4 dBu operating
/// level usually sits, comes out with 0.12 % and 0.02 %: clean where it is run,
/// and out of headroom where the digital headroom ends, as the hardware is.
const HARDNESS: f64 = 0.4;
/// Operating point offset, the source of the second harmonic: the push-pull
/// stage's imbalance.
const BIAS: f64 = 0.12;

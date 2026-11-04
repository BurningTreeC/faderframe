//! The drift loop: how fast to read a follower's ring so that it holds
//! what it should.
//!
//! A follower runs on a clock of its own, a few to a few hundred parts per
//! million off the master's. Its ring's fill — estimated at the master's
//! callback from both sides' timestamps, so that the follower's own block
//! size does not saw it up and down — is compared with its target; a
//! second-order loop (proportional and integral, critically damped) turns
//! the error into a correction of the resampler's step. The integral part
//! settles on the drift itself, so the fill settles on the target with no
//! error left.
//!
//! The bandwidth starts at [`BW_START`] (locking within a few seconds),
//! stays there for [`BW_HOLD`] seconds and narrows to [`BW_END`] over
//! [`BW_SETTLE`] seconds (PipeWire's ALSA followers use the same two
//! bandwidths), so that timing jitter barely moves the
//! ratio once locked: a callback whose timing is 10 frames off moves the
//! step by about 4 ppm for some 50 ms — the read position by a hundredth
//! of a frame.

use std::f64::consts::PI;

/// Loop bandwidth at the start (Hz).
pub const BW_START: f64 = 0.128;
/// Loop bandwidth once settled (Hz).
pub const BW_END: f64 = 0.016;
/// Seconds at the start bandwidth …
pub const BW_HOLD: f64 = 4.0;
/// … and over which it then narrows (time constant).
pub const BW_SETTLE: f64 = 10.0;
/// The error is smoothed over this long (seconds) before the loop …
const SMOOTHING: f64 = 0.05;
/// … and averaged over this long for what the fill is (the latency
/// reported, whether the loop has settled).
const AVERAGE: f64 = 2.0;
/// Damping of the loop (1: critical).
const DAMPING: f64 = 1.0;
/// The largest correction the loop makes (0.5 %: far beyond any clock).
pub const MAX_CORRECTION: f64 = 0.005;

/// See the module docs. Updated on the master's audio thread.
#[derive(Clone, Debug)]
pub struct DriftLoop {
    /// Follower frames a second (the error's unit per second).
    rate: f64,
    /// The smoothed error (frames).
    error: f64,
    /// The error averaged over [`AVERAGE`] (frames).
    average: f64,
    /// The integral part (the drift once settled).
    integral: f64,
    correction: f64,
    bandwidth: f64,
    /// Seconds run.
    seconds: f64,
}

impl DriftLoop {
    /// For a follower at `rate` frames a second, starting from a drift of
    /// `drift` (a share: what the loop settled on the last time this
    /// device followed, else 0).
    pub fn new(rate: f64, drift: f64) -> Self {
        let drift = if drift.is_finite() {
            drift.clamp(-MAX_CORRECTION, MAX_CORRECTION)
        } else {
            0.0
        };
        Self {
            rate: rate.max(1.0),
            error: 0.0,
            average: 0.0,
            integral: drift,
            correction: drift,
            bandwidth: BW_START,
            seconds: 0.0,
        }
    }

    /// The fill is `error` frames above its target, `dt` seconds after
    /// the last update: the correction to apply now (a share of the
    /// nominal step; positive: read the follower's frames faster).
    pub fn update(&mut self, error: f64, dt: f64) -> f64 {
        if !error.is_finite() || !dt.is_finite() || dt <= 0.0 {
            return self.correction;
        }
        let a = 1.0 - (-dt / SMOOTHING).exp();
        self.error += a * (error - self.error);
        self.average += (1.0 - (-dt / AVERAGE).exp()) * (error - self.average);
        let wn = 2.0 * PI * self.bandwidth;
        let kp = 2.0 * DAMPING * wn / self.rate;
        let ki = wn * wn / self.rate;
        self.integral =
            (self.integral + ki * self.error * dt).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.correction = (kp * self.error + self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        if self.seconds >= BW_HOLD {
            self.bandwidth = BW_END + (self.bandwidth - BW_END) * (-dt / BW_SETTLE).exp();
        }
        self.seconds += dt;
        self.correction
    }

    /// The fill was set back on its target (a start after a gap): what
    /// the error was is forgotten, the drift found is kept.
    pub fn restart(&mut self) {
        self.error = 0.0;
        self.average = 0.0;
        self.correction = self.integral;
    }

    pub fn correction(&self) -> f64 {
        self.correction
    }

    /// The drift found (the integral part: what the correction settles on).
    pub fn drift(&self) -> f64 {
        self.integral
    }

    /// The smoothed error (frames).
    pub fn error(&self) -> f64 {
        self.error
    }

    /// The error averaged over a couple of seconds: how far the fill is
    /// off its target, timing jitter averaged out (frames).
    pub fn average_error(&self) -> f64 {
        self.average
    }

    pub fn seconds(&self) -> f64 {
        self.seconds
    }

    pub fn bandwidth(&self) -> f64 {
        self.bandwidth
    }
}

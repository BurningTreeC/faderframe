//! Broadband noise reduction: a gain per frequency from how far a frame
//! stands over the noise.
//!
//! [`Denoiser::gains`] is a Wiener filter on the decision-directed a priori
//! signal-to-noise ratio (Ephraim and Malah): this frame's excess over the
//! noise blended with what the last frame kept, so a bin does not flicker
//! open and shut on the noise's own ups and downs — what makes the "musical
//! noise" of plain spectral subtraction. The blend is the Smoothing control
//! (with how fast a gain may close again); the gains are then smoothed
//! across frequency (a width growing with frequency, up to half an
//! octave), onsets may open them (Transients), and they never go below the
//! floor — Reduction, tilted across the spectrum by Tilt. Outside the band
//! it works in the gain is one.
//!
//! The noise comes from a learned profile ([`crate::profile`]) or from
//! [`NoiseTracker`], which follows it adaptively: minima-controlled
//! recursive averaging (Cohen and Berdugo) — the power is averaged into the
//! noise only where it is near the smoothed spectrum's recent minimum, so
//! sound that is there does not count as noise, and the estimate is held
//! under a few times that minimum. A sound held unchanged for longer than
//! the minimum's window (1.5 s) looks like noise to any tracker: that is
//! what learning a profile is for.

/// How a [`Denoiser`] works (per frame; cheap to change).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// How far over the noise estimate a bin must stand to be kept (dB; 0:
    /// the noise as measured).
    pub threshold_db: f64,
    /// The most a bin is turned down (dB, positive).
    pub reduction_db: f64,
    /// 0…1: steadier gains (fewer artefacts) or quicker ones.
    pub smoothing: f64,
    /// 0…1: gains smoothed across up to half an octave.
    pub freq_smoothing: f64,
    /// More reduction above 1 kHz (positive) or below it (negative), dB at
    /// four octaves from it.
    pub tilt_db: f64,
    /// 0…1: how far onsets open the gains.
    pub transients: f64,
    /// The band it works in (Hz).
    pub low_hz: f64,
    pub high_hz: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            threshold_db: 0.0,
            reduction_db: 12.0,
            smoothing: 0.5,
            freq_smoothing: 0.3,
            tilt_db: 0.0,
            transients: 0.3,
            low_hz: 0.0,
            high_hz: 1e9,
        }
    }
}

/// The smallest a priori ratio considered (−25 dB).
const XI_MIN: f64 = 0.003_162;

/// The gains of one stream of frames (one or several linked channels).
#[derive(Clone, Debug)]
pub struct Denoiser {
    bins: usize,
    /// Seconds per hop and Hz per bin.
    hop_s: f64,
    bin_hz: f64,
    prev_gain: Vec<f64>,
    prev_power: Vec<f64>,
    released: Vec<f64>,
    raw: Vec<f64>,
    smoothed: Vec<f64>,
    coeff: Vec<f64>,
    floor: Vec<f64>,
    /// The settings `coeff` and `floor` were made for.
    made_for: Option<(f64, f64, f64)>,
    prev_total: f64,
    /// False after a reset: the next frame starts the state again.
    live: bool,
}

impl Denoiser {
    /// For spectra of `bins` bins at `rate`, a frame every `hop` samples.
    pub fn new(bins: usize, hop: usize, rate: f64) -> Self {
        let n = (bins.max(2) - 1) * 2;
        Self {
            bins,
            hop_s: hop as f64 / rate.max(1.0),
            bin_hz: rate / n as f64,
            prev_gain: vec![1.0; bins],
            prev_power: vec![0.0; bins],
            released: vec![1.0; bins],
            raw: vec![1.0; bins],
            smoothed: vec![1.0; bins],
            coeff: vec![0.0; bins],
            floor: vec![0.0; bins],
            made_for: None,
            prev_total: 0.0,
            live: false,
        }
    }

    /// Start again with the next frame (nothing is cleared now).
    pub fn reset(&mut self) {
        self.live = false;
    }

    fn prepare(&mut self, s: &Settings) {
        let key = (s.reduction_db, s.tilt_db, s.freq_smoothing);
        if self.made_for == Some(key) {
            return;
        }
        self.made_for = Some(key);
        let span = 2f64.powf(0.5 * s.freq_smoothing.clamp(0.0, 1.0)) - 1.0;
        for k in 0..self.bins {
            let f = (k as f64 * self.bin_hz).max(1.0);
            let x = ((f / 1_000.0).log2() / 4.0).clamp(-1.0, 1.0);
            let red = (s.reduction_db + s.tilt_db * x).clamp(0.0, 80.0);
            self.floor[k] = 10f64.powf(-red / 20.0);
            let tau = 0.5 * k as f64 * span;
            self.coeff[k] = if tau > 0.05 { (-1.0 / tau).exp() } else { 0.0 };
        }
    }

    /// The gains (`out`, 0…1) for a frame of `power` per bin over the
    /// `noise` per bin (both per sample units).
    pub fn gains(&mut self, power: &[f64], noise: &[f64], s: &Settings, out: &mut [f64]) {
        self.prepare(s);
        let bins = self.bins.min(power.len()).min(noise.len()).min(out.len());
        let smooth = s.smoothing.clamp(0.0, 1.0);
        if !self.live {
            self.live = true;
            self.prev_gain[..bins].fill(1.0);
            self.prev_power[..bins].copy_from_slice(&power[..bins]);
            self.released[..bins].fill(1.0);
            self.prev_total = power[..bins].iter().sum();
        }
        let alpha = 0.92 + 0.07 * smooth;
        let over = 10f64.powf(s.threshold_db / 10.0);
        let total: f64 = power[..bins].iter().sum();
        // Onsets: the broadband rise over the last frame.
        let rise = 10.0 * (total.max(1e-30) / self.prev_total.max(1e-30)).log10();
        let onset = ((rise - 3.0) / 9.0).clamp(0.0, 1.0) * s.transients.clamp(0.0, 1.0);
        for k in 0..bins {
            let d = (over * noise[k]).max(1e-30);
            let gamma = power[k] / d;
            let xi = (alpha * self.prev_gain[k] * self.prev_gain[k] * self.prev_power[k] / d
                + (1.0 - alpha) * (gamma - 1.0).max(0.0))
            .max(XI_MIN);
            let g = xi / (1.0 + xi);
            self.raw[k] = g;
            self.prev_gain[k] = g.max(self.floor[k]);
            self.prev_power[k] = power[k];
        }
        self.prev_total = total;
        // Across frequency: forward and backward one-pole passes. A bin that
        // is surely sound (its gain near one) keeps it: the smoothing is for
        // the doubtful ones, where lone bins flicker.
        if s.freq_smoothing > 0.0 {
            let mut y = self.raw[0];
            for k in 0..bins {
                let a = self.coeff[k];
                y = (1.0 - a) * self.raw[k] + a * y;
                self.smoothed[k] = y;
            }
            let mut z = self.smoothed[bins - 1];
            for k in (0..bins).rev() {
                let a = self.coeff[k];
                z = (1.0 - a) * self.smoothed[k] + a * z;
                let g = self.raw[k];
                self.raw[k] = z.max(g * g);
            }
        }
        // Closing again takes the release time; opening is at once.
        let release = 0.015 + 0.25 * smooth;
        let keep = (-self.hop_s / release).exp();
        for k in 0..bins {
            let mut g = self.raw[k];
            if onset > 0.0 && power[k] > 2.0 * noise[k] * over {
                g += (1.0 - g) * onset;
            }
            let r = &mut self.released[k];
            *r = if g >= *r { g } else { g + (*r - g) * keep };
            let f = k as f64 * self.bin_hz;
            out[k] = if f < s.low_hz || f > s.high_hz {
                1.0
            } else {
                r.max(self.floor[k]).min(1.0)
            };
        }
    }
}

/// Follows a noise's power per bin from the audio itself (MCRA).
#[derive(Clone, Debug)]
pub struct NoiseTracker {
    bins: usize,
    smoothed: Vec<f64>,
    noise: Vec<f64>,
    presence: Vec<f64>,
    /// The minimum of each finished sub-window, and of the one under way.
    window_min: Vec<f64>,
    sub_min: Vec<f64>,
    current: Vec<f64>,
    subs: usize,
    frames_per_sub: usize,
    in_sub: usize,
    sub: usize,
    alpha_s: f64,
    alpha_d: f64,
    alpha_p: f64,
    frames: u64,
    live: bool,
}

/// Sub-windows of the minimum search.
const SUBS: usize = 8;
/// The minimum search's length (seconds).
const MIN_WINDOW: f64 = 1.5;
/// A bin this far over its recent minimum holds sound (power ratio).
const PRESENT: f64 = 5.0;
/// The noise is held under this many times the recent minimum of the
/// smoothed power (the minimum of a smoothed noise lies about 2 dB under
/// its mean).
const BOUND: f64 = 2.5;

impl NoiseTracker {
    pub fn new(bins: usize, hop: usize, rate: f64) -> Self {
        let hop_s = hop as f64 / rate.max(1.0);
        let per = |tau: f64| (-hop_s / tau).exp();
        Self {
            bins,
            smoothed: vec![0.0; bins],
            noise: vec![0.0; bins],
            presence: vec![0.0; bins],
            window_min: vec![f64::MAX; bins],
            sub_min: vec![f64::MAX; bins * SUBS],
            current: vec![f64::MAX; bins],
            subs: SUBS,
            frames_per_sub: ((MIN_WINDOW / SUBS as f64 / hop_s).ceil() as usize).max(1),
            in_sub: 0,
            sub: 0,
            alpha_s: per(0.03),
            alpha_d: per(0.15),
            alpha_p: per(0.02),
            frames: 0,
            live: false,
        }
    }

    pub fn reset(&mut self) {
        self.live = false;
    }

    /// Frames tracked since the last reset.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The noise estimate per bin.
    pub fn noise(&self) -> &[f64] {
        &self.noise
    }

    /// Take a frame's power per bin; the estimate follows.
    #[allow(clippy::needless_range_loop)] // Several arrays per bin.
    pub fn update(&mut self, power: &[f64]) {
        let bins = self.bins.min(power.len());
        if !self.live {
            self.live = true;
            self.frames = 0;
            self.smoothed[..bins].copy_from_slice(&power[..bins]);
            self.noise[..bins].copy_from_slice(&power[..bins]);
            self.presence.fill(0.0);
            self.window_min.fill(f64::MAX);
            self.sub_min.fill(f64::MAX);
            self.current.fill(f64::MAX);
            self.in_sub = 0;
            self.sub = 0;
        }
        self.frames += 1;
        for k in 0..bins {
            let s = self.alpha_s * self.smoothed[k] + (1.0 - self.alpha_s) * power[k];
            self.smoothed[k] = s;
            self.current[k] = self.current[k].min(s);
            let min = self.window_min[k].min(self.current[k]);
            let present = if s > PRESENT * min { 1.0 } else { 0.0 };
            let p = self.alpha_p * self.presence[k] + (1.0 - self.alpha_p) * present;
            self.presence[k] = p;
            let a = self.alpha_d + (1.0 - self.alpha_d) * p;
            let mut noise = a * self.noise[k] + (1.0 - a) * power[k];
            // Never above what the recent minimum allows: sound that comes
            // and goes does not leak in while the presence rises.
            if self.window_min[k] < f64::MAX {
                noise = noise.min(BOUND * self.window_min[k]);
            }
            self.noise[k] = noise;
        }
        self.in_sub += 1;
        if self.in_sub >= self.frames_per_sub {
            self.in_sub = 0;
            let base = self.sub * self.bins;
            self.sub_min[base..base + bins].copy_from_slice(&self.current[..bins]);
            self.sub = (self.sub + 1) % self.subs;
            for k in 0..bins {
                let mut m = f64::MAX;
                for j in 0..self.subs {
                    m = m.min(self.sub_min[j * self.bins + k]);
                }
                self.window_min[k] = m;
            }
            self.current.fill(f64::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stft::Stft;

    fn power_of(stft: &mut Stft, frame: &[f64], out: &mut [f64]) {
        let mut spec = vec![crate::stft::C64::new(0.0, 0.0); stft.bins];
        stft.forward(frame, &mut spec);
        let scale = stft.power_scale();
        for (o, c) in out.iter_mut().zip(&spec) {
            *o = c.norm_sqr() / scale;
        }
    }

    #[test]
    fn noise_alone_is_turned_down_by_the_reduction_and_a_tone_kept() {
        let rate = 48_000.0;
        let mut stft = Stft::new(2048);
        let mut d = Denoiser::new(stft.bins, stft.hop, rate);
        let mut noise = crate::Noise::new(11);
        let profile = vec![1e-4; stft.bins];
        let s = Settings {
            reduction_db: 20.0,
            ..Settings::default()
        };
        let mut power = vec![0.0; stft.bins];
        let mut gains = vec![0.0; stft.bins];
        for i in 0..200 {
            let frame: Vec<f64> = (0..2048)
                .map(|n| {
                    let t = (i * 512 + n) as f64 / rate;
                    0.01 * noise.next() + 0.3 * (std::f64::consts::TAU * 1_000.0 * t).sin()
                })
                .collect();
            power_of(&mut stft, &frame, &mut power);
            d.gains(&power, &profile, &s, &mut gains);
        }
        let k_tone = (1_000.0 / (rate / 2048.0)).round() as usize;
        assert!(gains[k_tone] > 0.95, "tone: {}", gains[k_tone]);
        let away: f64 = gains[300..800].iter().sum::<f64>() / 500.0;
        assert!(away < 0.15, "noise: {away}");
    }

    #[test]
    fn the_tracker_finds_the_noise_under_a_melody() {
        let rate = 48_000.0;
        let mut stft = Stft::new(2048);
        let mut tracker = NoiseTracker::new(stft.bins, stft.hop, rate);
        let mut noise = crate::Noise::new(5);
        let mut power = vec![0.0; stft.bins];
        for i in 0..400 {
            // A tone that changes every 0.3 s over noise of variance 1e-4.
            let f = 300.0 + 150.0 * ((i / 28) % 5) as f64;
            let frame: Vec<f64> = (0..2048)
                .map(|n| {
                    let t = (i * 512 + n) as f64 / rate;
                    0.01 * noise.next() + 0.2 * (std::f64::consts::TAU * f * t).sin()
                })
                .collect();
            power_of(&mut stft, &frame, &mut power);
            tracker.update(&power);
        }
        // Away from the tones the estimate is the noise's variance.
        let est = tracker.noise()[200..900].iter().sum::<f64>() / 700.0;
        let db = 10.0 * (est / 1e-4).log10();
        assert!(db.abs() < 2.5, "{db:.2} dB off");
    }
}

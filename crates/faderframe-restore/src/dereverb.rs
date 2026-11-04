//! Late reverberation taken down: the reverberant tail's power in each bin
//! is predicted from the room's decay (Lebart, Boucher and Denbigh; the
//! recursive model of Habets), and a gain per bin takes it out where it
//! dominates.
//!
//! Per bin the smoothed power `λ_x` is split into its reverberant part,
//! followed recursively: the last frame's reverberation decayed by a hop,
//! plus what the direct sound (`λ_x − λ_rev`) excites, at the share of the
//! room in the sound — so a free decay is followed exactly and a held note
//! settles at that share. The late part at frame `m` is the reverberation
//! `early` milliseconds before, decayed by that much:
//! `λ_late(m) = e^{−2δ·T} λ_rev(m − D)`, `δ = 3 ln 10 / RT60` (RT60 the
//! tail length, shorter towards the highs by Damping). The gain is a power
//! subtraction of the late part from the smoothed power (low variance, so
//! no musical noise and no decision-directed bias against held notes),
//! closing at the release time, floored by Amount (up to 30 dB taken); the
//! room's share and the over-subtraction grow with Amount too. Amount 0
//! leaves the sound alone.
//!
//! [`DecayEstimator`] estimates the tail length from the sound itself: in
//! seven octave bands it follows the level, finds stretches where it falls
//! steadily (free decays: a note stops and the room rings out), skips each
//! decay's first 5 dB (the direct sound's own fall) and fits the rest; the
//! median of a band's recent decays is its RT60, the median of the bands
//! the estimate.

/// How late reverb is taken down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbSettings {
    /// 0…1: up to 30 dB.
    pub amount: f64,
    /// The tail's RT60 (seconds).
    pub tail_s: f64,
    /// Where the late tail begins after the sound (ms).
    pub early_ms: f64,
    /// 0…1: the highs' tail shorter (down to a fifth at 16 kHz).
    pub damping: f64,
    /// 0…1: steadier gains or quicker ones.
    pub smoothing: f64,
}

/// The longest early part (frames of history kept).
const MAX_DELAY: usize = 32;

pub struct Dereverb {
    bins: usize,
    hop_s: f64,
    bin_hz: f64,
    smoothed: Vec<f64>,
    reverb: Vec<f64>,
    history: Vec<f64>,
    hist_at: usize,
    filled: usize,
    released: Vec<f64>,
    /// Per bin: the decay over the early part, and over a hop.
    decay: Vec<f64>,
    per_hop: Vec<f64>,
    made_for: Option<(f64, f64, usize)>,
    delay: usize,
    live: bool,
}

impl Dereverb {
    pub fn new(bins: usize, hop: usize, rate: f64) -> Self {
        let n = (bins.max(2) - 1) * 2;
        Self {
            bins,
            hop_s: hop as f64 / rate.max(1.0),
            bin_hz: rate / n as f64,
            smoothed: vec![0.0; bins],
            reverb: vec![0.0; bins],
            history: vec![0.0; bins * MAX_DELAY],
            hist_at: 0,
            filled: 0,
            released: vec![1.0; bins],
            decay: vec![0.0; bins],
            per_hop: vec![0.0; bins],
            made_for: None,
            delay: 1,
            live: false,
        }
    }

    /// Start again with the next frame (nothing is cleared now).
    pub fn reset(&mut self) {
        self.live = false;
    }

    fn prepare(&mut self, s: &ReverbSettings) {
        let delay = ((s.early_ms * 0.001 / self.hop_s).round() as usize).clamp(1, MAX_DELAY - 1);
        let key = (s.tail_s, s.damping, delay);
        if self.made_for == Some(key) {
            return;
        }
        self.made_for = Some(key);
        self.delay = delay;
        let t = delay as f64 * self.hop_s;
        for k in 0..self.bins {
            let f = (k as f64 * self.bin_hz).max(1.0);
            let x = ((f / 500.0).log2() / 5.0).clamp(0.0, 1.0);
            let rt = (s.tail_s * (1.0 - 0.8 * s.damping.clamp(0.0, 1.0) * x)).max(0.05);
            let delta = 3.0 * std::f64::consts::LN_10 / rt;
            self.decay[k] = (-2.0 * delta * t).exp();
            self.per_hop[k] = (-2.0 * delta * self.hop_s).exp();
        }
    }

    /// The gains (`out`) for a frame's power per bin.
    #[allow(clippy::needless_range_loop)] // Several arrays per bin.
    pub fn gains(&mut self, power: &[f64], s: &ReverbSettings, out: &mut [f64]) {
        self.prepare(s);
        let bins = self.bins.min(power.len()).min(out.len());
        let smooth = s.smoothing.clamp(0.0, 1.0);
        let amount = s.amount.clamp(0.0, 1.0);
        if !self.live {
            self.live = true;
            self.smoothed[..bins].copy_from_slice(&power[..bins]);
            self.reverb[..bins].fill(0.0);
            self.filled = 0;
            self.hist_at = 0;
            self.released.fill(1.0);
        }
        // The room's share of a held sound, and the direct sound's weight
        // in exciting it.
        let share = 0.2 + 0.5 * amount;
        let excite = share / (1.0 - share);
        let keep_x = (-self.hop_s / (0.015 + 0.04 * smooth)).exp();
        for k in 0..bins {
            let a = self.per_hop[k];
            let direct = (self.smoothed[k] - self.reverb[k]).max(0.0);
            let r = a * self.reverb[k] + (1.0 - a) * excite * direct;
            self.smoothed[k] = keep_x * self.smoothed[k] + (1.0 - keep_x) * power[k];
            self.reverb[k] = r.min(self.smoothed[k]);
        }
        // The reverberation `delay` frames ago (history before it: none).
        let then = (self.hist_at + MAX_DELAY - self.delay) % MAX_DELAY;
        let have = self.filled >= self.delay;
        let floor = 10f64.powf(-30.0 * amount / 20.0);
        let over = 1.0 + amount;
        let release = 0.01 + 0.15 * smooth;
        let keep = (-self.hop_s / release).exp();
        for k in 0..bins {
            let late = if have {
                self.decay[k] * self.history[then * self.bins + k]
            } else {
                0.0
            };
            let g = if late <= 1e-30 || floor >= 1.0 {
                1.0
            } else {
                (1.0 - over * late / self.smoothed[k].max(1e-30))
                    .max(0.0)
                    .sqrt()
            };
            let r = &mut self.released[k];
            *r = if g >= *r { g } else { g + (*r - g) * keep };
            out[k] = r.max(floor).min(1.0);
        }
        let base = self.hist_at * self.bins;
        self.history[base..base + bins].copy_from_slice(&self.reverb[..bins]);
        self.hist_at = (self.hist_at + 1) % MAX_DELAY;
        self.filled = (self.filled + 1).min(MAX_DELAY);
    }
}

/// Octave bands the decay is measured in (centres, Hz).
const BAND_CENTRES: [f64; 7] = [125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0];
/// Recent decays kept per band.
const KEPT: usize = 16;

#[derive(Clone, Copy, Debug)]
struct Run {
    start_db: f64,
    /// Regression sums over the fitted part.
    n: f64,
    st: f64,
    se: f64,
    stt: f64,
    ste: f64,
    last_db: f64,
    frames: usize,
}

/// Estimates the tail length from free decays (see the module).
pub struct DecayEstimator {
    hop_s: f64,
    bin_hz: f64,
    level: [f64; 7],
    run: [Option<Run>; 7],
    slopes: [[f64; KEPT]; 7],
    count: [usize; 7],
    at: [usize; 7],
    scratch: [f64; KEPT],
    frame: u64,
}

impl DecayEstimator {
    pub fn new(bins: usize, hop: usize, rate: f64) -> Self {
        let n = (bins.max(2) - 1) * 2;
        Self {
            hop_s: hop as f64 / rate.max(1.0),
            bin_hz: rate / n as f64,
            level: [-200.0; 7],
            run: [None; 7],
            slopes: [[0.0; KEPT]; 7],
            count: [0; 7],
            at: [0; 7],
            scratch: [0.0; KEPT],
            frame: 0,
        }
    }

    pub fn reset(&mut self) {
        self.level = [-200.0; 7];
        self.run = [None; 7];
    }

    /// Take a frame's power per bin.
    pub fn update(&mut self, power: &[f64]) {
        self.frame += 1;
        let t = self.frame as f64 * self.hop_s;
        let k_smooth = 1.0 - (-self.hop_s / 0.02).exp();
        for (b, &centre) in BAND_CENTRES.iter().enumerate() {
            let (lo, hi) = (
                centre / std::f64::consts::SQRT_2,
                centre * std::f64::consts::SQRT_2,
            );
            let (a, z) = (
                (lo / self.bin_hz).ceil() as usize,
                (hi / self.bin_hz).floor() as usize,
            );
            let sum: f64 = power
                .get(a.max(1)..(z + 1).min(power.len()))
                .map_or(0.0, |p| p.iter().sum());
            let db = 10.0 * sum.max(1e-30).log10();
            let prev = self.level[b];
            let lv = if prev < -199.0 {
                db
            } else {
                prev + k_smooth * (db - prev)
            };
            self.level[b] = lv;
            let continues = self.run[b].is_some_and(|r| lv <= r.last_db + 0.3);
            if continues {
                if let Some(run) = &mut self.run[b] {
                    run.last_db = run.last_db.min(lv);
                    run.frames += 1;
                    if lv < run.start_db - 5.0 {
                        run.n += 1.0;
                        run.st += t;
                        run.se += lv;
                        run.stt += t * t;
                        run.ste += t * lv;
                    }
                }
                continue;
            }
            if let Some(run) = self.run[b].take() {
                self.finish(b, &run);
            }
            self.run[b] = Some(Run {
                start_db: lv,
                n: 0.0,
                st: 0.0,
                se: 0.0,
                stt: 0.0,
                ste: 0.0,
                last_db: lv,
                frames: 0,
            });
        }
    }

    fn finish(&mut self, b: usize, run: &Run) {
        let span = run.n * self.hop_s;
        let drop = run.start_db - 5.0 - run.last_db;
        if run.n < 4.0 || span < 0.06 || drop < 8.0 {
            return;
        }
        let den = run.n * run.stt - run.st * run.st;
        if den.abs() < 1e-12 {
            return;
        }
        let slope = (run.n * run.ste - run.st * run.se) / den;
        if slope < -2.0 {
            self.slopes[b][self.at[b]] = slope;
            self.at[b] = (self.at[b] + 1) % KEPT;
            self.count[b] = (self.count[b] + 1).min(KEPT);
        }
    }

    /// The estimated RT60 (seconds), once enough decays were heard.
    pub fn estimate(&mut self) -> Option<f64> {
        let mut rts = [0.0f64; 7];
        let mut n = 0;
        for b in 0..BAND_CENTRES.len() {
            let c = self.count[b];
            if c < 3 {
                continue;
            }
            self.scratch[..c].copy_from_slice(&self.slopes[b][..c]);
            let mid = c / 2;
            let (_, m, _) = self.scratch[..c].select_nth_unstable_by(mid, f64::total_cmp);
            rts[n] = 60.0 / -*m;
            n += 1;
        }
        if n == 0 {
            return None;
        }
        let mid = n / 2;
        let (_, m, _) = rts[..n].select_nth_unstable_by(mid, f64::total_cmp);
        Some(m.clamp(0.1, 8.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stft::Streaming;

    /// Noise bursts in a room of RT60 `rt`: the dry bursts and the wet.
    fn reverberant(rt: f64, rate: f64, seconds: f64) -> (Vec<f64>, Vec<f64>) {
        let len = (rate * seconds) as usize;
        let mut noise = crate::Noise::new(21);
        let dry: Vec<f64> = (0..len)
            .map(|n| {
                let t = n as f64 / rate;
                if t % 1.0 < 0.15 {
                    0.2 * noise.next()
                } else {
                    0.0
                }
            })
            .collect();
        // An exponentially decaying noise impulse response (late part from
        // 30 ms).
        let ir_len = (rate * rt * 1.2) as usize;
        let delta = 3.0 * std::f64::consts::LN_10 / rt;
        let mut ir_noise = crate::Noise::new(4);
        let ir: Vec<f64> = (0..ir_len)
            .map(|n| {
                let t = n as f64 / rate;
                if n == 0 {
                    1.0
                } else if t < 0.03 {
                    0.0
                } else {
                    0.03 * ir_noise.next() * (-delta * t).exp()
                }
            })
            .collect();
        let mut wet = vec![0.0; len];
        for (n, &x) in dry.iter().enumerate() {
            if x == 0.0 {
                continue;
            }
            for (k, &h) in ir.iter().enumerate() {
                if n + k < len {
                    wet[n + k] += x * h;
                }
            }
        }
        (dry, wet)
    }

    #[test]
    fn the_tail_is_estimated_and_taken_down() {
        let rate = 16_000.0;
        let rt = 0.8;
        let (dry, wet) = reverberant(rt, rate, 12.0);
        let mut s = Streaming::new(512, 1);
        let bins = s.stft.bins;
        let mut d = Dereverb::new(bins, s.stft.hop, rate);
        let mut est = DecayEstimator::new(bins, s.stft.hop, rate);
        let settings = ReverbSettings {
            amount: 1.0,
            tail_s: rt,
            early_ms: 40.0,
            damping: 0.0,
            smoothing: 0.3,
        };
        let mut power = vec![0.0; bins];
        let mut gains = vec![0.0; bins];
        let mut out = vec![0.0; wet.len()];
        let scale = s.stft.power_scale();
        for (n, &x) in wet.iter().enumerate() {
            let mut y = [0.0];
            if s.push(&[x], &mut y) {
                for (p, c) in power.iter_mut().zip(&s.spectra[0]) {
                    *p = c.norm_sqr() / scale;
                }
                est.update(&power);
                d.gains(&power, &settings, &mut gains);
                for (c, g) in s.spectra[0].iter_mut().zip(&gains) {
                    *c *= *g;
                }
                s.finish();
            }
            out[n] = y[0];
        }
        let estimate = est.estimate().unwrap();
        assert!(
            (estimate / rt - 1.0).abs() < 0.3,
            "RT60 estimate {estimate:.2}"
        );
        // The tails (0.4–0.9 s after each burst starts) are much quieter;
        // the bursts themselves keep most of their level.
        let lat = s.latency();
        let energy = |x: &[f64], from: f64, to: f64, shift: usize| {
            (2..11)
                .map(|sec| {
                    let a = ((sec as f64 + from) * rate) as usize + shift;
                    let b = ((sec as f64 + to) * rate) as usize + shift;
                    x[a..b].iter().map(|v| v * v).sum::<f64>()
                })
                .sum::<f64>()
        };
        let tail_before = energy(&wet, 0.4, 0.9, 0);
        let tail_after = energy(&out, 0.4, 0.9, lat);
        let burst_before = energy(&wet, 0.02, 0.13, 0);
        let burst_after = energy(&out, 0.02, 0.13, lat);
        let tail_db = 10.0 * (tail_after / tail_before).log10();
        let burst_db = 10.0 * (burst_after / burst_before).log10();
        assert!(tail_db < -10.0, "tail {tail_db:.1} dB");
        assert!(burst_db > -1.5, "burst {burst_db:.1} dB");
        let _ = dry;
    }
}

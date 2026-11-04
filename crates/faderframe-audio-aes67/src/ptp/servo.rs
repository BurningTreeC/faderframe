//! Keeping the software clock on the grandmaster's time.
//!
//! Each Sync (with its Follow_Up) gives an offset — our clock's reading
//! when it arrived, less the master's time it was sent at and the path
//! delay ([`DelayFilter`]: the median of the last request–response
//! measurements). The [`Servo`] turns offsets into a rate and, rarely, a
//! step, after ptp4l's PI servo: the first two samples measure the
//! frequency and step the clock onto the master's time, then a
//! proportional–integral loop steers the rate alone, so the media clock
//! never jumps while locked. Its gains are per second (the loop behaves
//! the same at any Sync interval): wide for the first seconds, then
//! narrow, because software timestamps carry tens of microseconds of
//! jitter. Offsets far outside the recent noise are taken as late
//! wake-ups and left out — unless they keep coming (the master moved),
//! and an offset past [`STEP_NS`] then steps again.

/// The loop's proportional gain while it settles, then tracking (per
/// second: a fraction of the offset taken out each second).
const KP_WIDE: f64 = 0.7;
const KP_NARROW: f64 = 0.2;
/// Integral gains (per second²): critically damped with the above.
const KI_WIDE: f64 = KP_WIDE * KP_WIDE / 4.0;
const KI_NARROW: f64 = KP_NARROW * KP_NARROW / 4.0;
/// Seconds of wide loop after the first lock.
const WIDE_SECONDS: f64 = 16.0;
/// The first samples span at least this long (ns) and number at least
/// [`FIRST_SAMPLES`] before the frequency is fitted through them.
const FIRST_NS: u64 = 1_000_000_000;
const FIRST_SAMPLES: usize = 4;
const FIRST_MAX: usize = 32;
/// Largest rate correction (parts per million).
const MAX_PPM: f64 = 500.0;
/// An offset this large while locked steps the clock (nanoseconds).
pub const STEP_NS: i64 = 2_000_000;
/// Offsets within this many nanoseconds are never outliers.
const OUTLIER_FLOOR_NS: f64 = 50_000.0;
/// How many times the recent noise an offset may be off before it is
/// taken for a late wake-up.
const OUTLIER_FACTOR: f64 = 5.0;
/// Outliers in a row that are believed after all.
const OUTLIERS_BELIEVED: u32 = 4;
/// The largest |offset| of the last [`LOCK_WINDOW`] samples below which
/// the clock counts as locked (nanoseconds).
pub const LOCKED_NS: i64 = 100_000;
const LOCK_WINDOW: usize = 8;

/// Request–response path delays, the median of the last few (one late
/// timestamp does not move it).
#[derive(Clone, Debug, Default)]
pub struct DelayFilter {
    values: [i64; 8],
    len: usize,
    next: usize,
}

impl DelayFilter {
    pub fn push(&mut self, delay: i64) {
        self.values[self.next] = delay;
        self.next = (self.next + 1) % self.values.len();
        self.len = (self.len + 1).min(self.values.len());
    }

    /// The median so far (`None` before the first).
    pub fn delay(&self) -> Option<i64> {
        if self.len == 0 {
            return None;
        }
        let mut v = self.values;
        let v = &mut v[..self.len];
        v.sort_unstable();
        Some(if self.len % 2 == 1 {
            v[self.len / 2]
        } else {
            (v[self.len / 2 - 1] + v[self.len / 2]) / 2
        })
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.next = 0;
    }
}

/// What the servo asks of the clock after a sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Adjust {
    /// Nothing yet (the first sample, an outlier).
    Hold,
    /// Run at this rate (PTP ns per local ns).
    Rate(f64),
    /// Jump by `step` nanoseconds and run at `rate`.
    Step { step: i64, rate: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Gathering the first samples (in `first`).
    First,
    Tracking,
}

/// The PI servo (see the module docs).
#[derive(Clone, Debug)]
pub struct Servo {
    state: State,
    /// The first samples: (local ns, offset ns).
    first: [(u64, i64); FIRST_MAX],
    firsts: usize,
    /// The rate correction the integral holds (ppb: positive slows the
    /// clock).
    drift: f64,
    /// Local time of the last sample taken, and when tracking began.
    last: u64,
    since: u64,
    /// Recent |offset| (ns, smoothed): what counts as noise.
    noise: f64,
    outliers: u32,
    recent: [i64; LOCK_WINDOW],
    taken: usize,
    pub last_offset: i64,
}

impl Default for Servo {
    fn default() -> Self {
        Self::new()
    }
}

impl Servo {
    pub fn new() -> Self {
        Self {
            state: State::First,
            first: [(0, 0); FIRST_MAX],
            firsts: 0,
            drift: 0.0,
            last: 0,
            since: 0,
            noise: OUTLIER_FLOOR_NS,
            outliers: 0,
            recent: [i64::MAX; LOCK_WINDOW],
            taken: 0,
            last_offset: 0,
        }
    }

    /// Start again (a new grandmaster): the drift found is kept.
    pub fn reset(&mut self) {
        let drift = self.drift;
        *self = Self::new();
        self.drift = drift;
    }

    /// The rate the drift alone gives.
    fn rate_of(ppb: f64) -> f64 {
        1.0 - ppb.clamp(-MAX_PPM * 1e3, MAX_PPM * 1e3) * 1e-9
    }

    /// The rate the integral alone gives: the best estimate of the
    /// master's frequency (a clock that starts leading keeps it).
    pub fn steady_rate(&self) -> f64 {
        Self::rate_of(self.drift)
    }

    /// The clock's rate correction found so far (parts per million;
    /// positive: the local clock runs fast against the grandmaster).
    pub fn drift_ppm(&self) -> f64 {
        self.drift * 1e-3
    }

    pub fn is_tracking(&self) -> bool {
        self.state == State::Tracking
    }

    /// Locked: tracking, and the last few offsets all small.
    pub fn locked(&self) -> bool {
        self.state == State::Tracking
            && self.taken >= LOCK_WINDOW
            && self.recent.iter().all(|o| o.abs() < LOCKED_NS)
    }

    /// One offset (our clock − the master's, ns) measured at local time
    /// `local` (ns).
    pub fn sample(&mut self, offset: i64, local: u64) -> Adjust {
        match self.state {
            State::First => {
                if self.firsts > 0 && local <= self.first[self.firsts - 1].0 {
                    return Adjust::Hold;
                }
                if self.firsts == FIRST_MAX {
                    self.first.copy_within(1.., 0);
                    self.firsts -= 1;
                }
                self.first[self.firsts] = (local, offset);
                self.firsts += 1;
                let span = local - self.first[0].0;
                if self.firsts < FIRST_SAMPLES || span < FIRST_NS {
                    return Adjust::Hold;
                }
                // A line through the offsets: its slope is the frequency
                // error, its value now the offset to step away.
                let pts = &self.first[..self.firsts];
                let n = pts.len() as f64;
                let t0 = pts[0].0;
                let (mut sx, mut sy) = (0.0, 0.0);
                for &(l, o) in pts {
                    sx += (l - t0) as f64;
                    sy += o as f64;
                }
                let (mx, my) = (sx / n, sy / n);
                let (mut sxx, mut sxy) = (0.0, 0.0);
                for &(l, o) in pts {
                    let dx = (l - t0) as f64 - mx;
                    sxx += dx * dx;
                    sxy += dx * (o as f64 - my);
                }
                let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
                let now = my + slope * ((local - t0) as f64 - mx);
                self.drift = (self.drift + slope * 1e9).clamp(-MAX_PPM * 1e3, MAX_PPM * 1e3);
                self.state = State::Tracking;
                self.firsts = 0;
                self.last = local;
                self.since = local;
                self.noise = OUTLIER_FLOOR_NS;
                self.outliers = 0;
                self.recent = [i64::MAX; LOCK_WINDOW];
                self.taken = 0;
                self.last_offset = 0;
                Adjust::Step {
                    step: -(now.round() as i64),
                    rate: Self::rate_of(self.drift),
                }
            }
            State::Tracking => {
                // Late wake-ups make lone large offsets: leave them out
                // unless they keep coming.
                let limit = (OUTLIER_FACTOR * self.noise).max(OUTLIER_FLOOR_NS);
                if (offset.abs() as f64) > limit && self.outliers < OUTLIERS_BELIEVED {
                    self.outliers += 1;
                    return Adjust::Hold;
                }
                self.outliers = 0;
                if offset.abs() > STEP_NS {
                    // The master moved (or a long stall): start over.
                    self.reset();
                    return self.sample(offset, local);
                }
                let dt = (local.saturating_sub(self.last) as f64 * 1e-9).clamp(1e-3, 4.0);
                self.last = local;
                let wide = (local.saturating_sub(self.since) as f64 * 1e-9) < WIDE_SECONDS;
                let (kp, ki) = if wide {
                    (KP_WIDE, KI_WIDE)
                } else {
                    (KP_NARROW, KI_NARROW)
                };
                let x = offset as f64;
                self.drift = (self.drift + ki * dt * x).clamp(-MAX_PPM * 1e3, MAX_PPM * 1e3);
                let ppb = kp * x + self.drift;
                self.noise += 0.1 * (x.abs() - self.noise);
                self.recent[self.taken % LOCK_WINDOW] = offset;
                self.taken += 1;
                self.last_offset = offset;
                Adjust::Rate(Self::rate_of(ppb))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_is_the_median() {
        let mut f = DelayFilter::default();
        assert_eq!(f.delay(), None);
        for d in [100, 102, 5_000, 101, 99] {
            f.push(d);
        }
        assert_eq!(f.delay(), Some(101));
        for d in 0..20 {
            f.push(200 + d);
        }
        assert_eq!(f.delay(), Some(215));
    }

    /// A clock 80 ppm fast and 3 ms off, sampled every 125 ms with 20 µs
    /// of noise and a late wake-up now and then.
    #[test]
    fn the_servo_locks_and_finds_the_drift() {
        let mut servo = Servo::new();
        let mut ours = crate::ptp::clock::VirtualClock::at(0, 3_000_000);
        let fast = 1.0 + 80e-6;
        let mut seed = 7u64;
        let mut noise = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 40_000.0
        };
        let mut steps = 0;
        for k in 0..2_000u64 {
            let master = k * 125_000_000;
            // The local clock reads `master · fast` then.
            let local = (master as f64 * fast) as u64;
            let mut offset = ours.ptp(local) as i64 - master as i64 + noise() as i64;
            if k % 97 == 50 {
                offset += 900_000;
            }
            match servo.sample(offset, local) {
                Adjust::Hold => {}
                Adjust::Rate(r) => ours = ours.retuned(local, r, 0),
                Adjust::Step { step, rate } => {
                    steps += 1;
                    ours = ours.retuned(local, rate, step)
                }
            }
        }
        assert_eq!(steps, 1, "one step, at the first lock");
        assert!(servo.locked());
        assert!(
            (servo.drift_ppm() - 80.0).abs() < 0.5,
            "drift {}",
            servo.drift_ppm()
        );
        let master = 2_000u64 * 125_000_000;
        let local = (master as f64 * fast) as u64;
        let off = ours.ptp(local) as i64 - master as i64;
        assert!(off.abs() < 30_000, "offset {off} ns");
    }
}

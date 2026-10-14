//! How loud the input is, against itself: the trackers' automatic gate (a
//! noise floor followed as the input plays, so the gate sits above what
//! the room makes whatever the input's gain), mains hum, velocities
//! relative to the performer's own loud notes, and the levels a listener
//! hears counted for calibration ([`Meter`] → [`Levels`]).

use std::sync::atomic::{AtomicU32, Ordering};

/// The gate sits this far (dB) over the followed noise floor.
pub(crate) const FLOOR_MARGIN_DB: f32 = 6.0;
/// The floor rises at most this fast (dB/s) while nothing is played, and
/// this while a note sounds (a held note is not the room); it falls at
/// once. It starts where it makes the gate the threshold.
const FLOOR_RISE_DB_PER_S: f32 = 10.0;
const FLOOR_RISE_PLAYING_DB_PER_S: f32 = 0.5;
/// The quietest floor (dBFS): under any converter.
const FLOOR_LEAST_DB: f32 = -120.0;
/// Velocity 127 at the loud reference, 1 this far under it (dB) when no
/// calibration says how wide the performer's dynamics are.
pub(crate) const DYNAMICS_DB: f32 = 36.0;
/// The automatic loud reference is at least this far over the gate,
/// follows louder notes at once and falls this fast (dB/s) toward quieter
/// ones.
const LOUD_OVER_GATE_DB: f32 = 12.0;
const LOUD_FALL_DB_PER_S: f32 = 1.0;
/// A pitch within this many cents of 50 or 60 Hz, or of their next three
/// harmonics, is mains hum's. The fundamentals are only roughly heard (few
/// periods in a window, few bins in a spectrum); no equal-tempered note is
/// nearer than 35 cents to a multiple of 50 Hz or 47 to one of 60.
const HUM_CENTS: [(f64, f64); 2] = [(50.0, 18.0), (60.0, 30.0)];
const HUM_HARMONIC_CENTS: f64 = 12.0;

/// The levels velocities span: a note starting at `soft_db` (the level of
/// the analysis frames, dBFS) plays at velocity 40, at `loud_db` at 127.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dynamics {
    pub soft_db: f32,
    pub loud_db: f32,
}

impl Dynamics {
    /// The velocity of a note starting at `level_db`.
    pub fn velocity(self, level_db: f32) -> u8 {
        let span = (self.loud_db - self.soft_db).max(6.0);
        // 40 at soft, 127 at loud, on down to 1 as far under soft.
        let t = (level_db - self.soft_db) / span;
        (40.0 + t * 87.0).clamp(1.0, 127.0).round() as u8
    }
}

/// The automatic gate and loud reference of one tracker.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LevelGate {
    /// The noise floor followed (NaN until the first frame).
    floor_db: f32,
    margin: f32,
    /// Notes this far under the loud reference are not heard (dB), if set.
    under_loud: Option<f32>,
    rise: f32,
    rise_playing: f32,
    loud_db: Option<f32>,
    loud_fall: f32,
}

impl LevelGate {
    /// A gate for analysis frames `frame_seconds` apart.
    pub(crate) fn new(frame_seconds: f64) -> Self {
        let s = frame_seconds as f32;
        Self {
            floor_db: f32::NAN,
            margin: FLOOR_MARGIN_DB,
            under_loud: None,
            rise: FLOOR_RISE_DB_PER_S * s,
            rise_playing: FLOOR_RISE_PLAYING_DB_PER_S * s,
            loud_db: None,
            loud_fall: LOUD_FALL_DB_PER_S * s,
        }
    }

    /// The gate this far over the floor instead (dB).
    pub(crate) fn with_margin(mut self, margin_db: f32) -> Self {
        self.margin = margin_db;
        self
    }

    /// Notes more than `db` under the loudest lately are not heard either
    /// (the gate follows the performance's level).
    pub(crate) fn under_loud(mut self, db: f32) -> Self {
        self.under_loud = Some(db);
        self
    }

    /// Take a frame's level (dBFS); `playing`: a note sounds or is about
    /// to.
    pub(crate) fn hear(&mut self, level_db: f32, playing: bool, threshold_db: f32) {
        if self.floor_db.is_nan() {
            self.floor_db = (threshold_db - self.margin).max(FLOOR_LEAST_DB);
        }
        if level_db < self.floor_db {
            self.floor_db = level_db.max(FLOOR_LEAST_DB);
        } else {
            let rise = if playing {
                self.rise_playing
            } else {
                self.rise
            };
            self.floor_db += (level_db - self.floor_db).min(rise);
        }
        if let Some(l) = &mut self.loud_db {
            *l -= self.loud_fall;
        }
    }

    /// The noise floor followed (dBFS).
    #[cfg(test)]
    pub(crate) fn floor_db(&self) -> f32 {
        self.floor_db
    }

    /// The gate: the configured threshold, or above the room (or under the
    /// loud notes) when that is louder.
    pub(crate) fn gate_db(&self, threshold_db: f32) -> f32 {
        let loud = match (self.under_loud, self.loud_db) {
            (Some(under), Some(loud)) => loud - under,
            _ => f32::MIN,
        };
        self.room_db(threshold_db).max(loud)
    }

    /// The gate of the room alone: the threshold, or over the floor.
    pub(crate) fn room_db(&self, threshold_db: f32) -> f32 {
        if self.floor_db.is_nan() {
            threshold_db
        } else {
            threshold_db.max(self.floor_db + self.margin)
        }
    }

    /// The velocity of a note starting at `level_db`, by the calibrated
    /// dynamics or else against the loudest notes lately (which this one
    /// joins).
    pub(crate) fn velocity(
        &mut self,
        level_db: f32,
        threshold_db: f32,
        dynamics: Option<Dynamics>,
    ) -> u8 {
        let least = self.gate_db(threshold_db) + LOUD_OVER_GATE_DB;
        let loud = self.loud_db.unwrap_or(least).max(least).max(level_db);
        self.loud_db = Some(loud);
        dynamics
            .unwrap_or(Dynamics {
                soft_db: loud - DYNAMICS_DB * 87.0 / 126.0,
                loud_db: loud,
            })
            .velocity(level_db)
    }
}

/// Is `hz` mains hum's (on 50 or 60 Hz or one of its next three
/// harmonics)?
pub(crate) fn on_mains(hz: f64) -> bool {
    HUM_CENTS.iter().any(|&(mains, cents)| {
        let k = (hz / mains).round();
        let cents = if k == 1.0 { cents } else { HUM_HARMONIC_CENTS };
        (1.0..=4.0).contains(&k) && (1200.0 * (hz / (k * mains)).log2()).abs() <= cents
    })
}

/// Bins of [`Levels`]: one per dB from −127 dBFS (everything quieter
/// counts there) to 0 dBFS (and louder).
pub const METER_BINS: usize = 128;

fn bin(level_db: f32) -> usize {
    if level_db.is_nan() {
        return 0;
    }
    (level_db.round() + 127.0).clamp(0.0, (METER_BINS - 1) as f32) as usize
}

/// What a listener hears, shared with the control side: every analysis
/// frame's level counted by decibel, and those with a pitch clear enough
/// to be played, with the newest level and gate (atomics: the audio thread
/// only adds and stores).
pub struct Meter {
    all: [AtomicU32; METER_BINS],
    voiced: [AtomicU32; METER_BINS],
    level: AtomicU32,
    gate: AtomicU32,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    pub fn new() -> Self {
        Self {
            all: [const { AtomicU32::new(0) }; METER_BINS],
            voiced: [const { AtomicU32::new(0) }; METER_BINS],
            level: AtomicU32::new(f32::NEG_INFINITY.to_bits()),
            gate: AtomicU32::new(f32::NEG_INFINITY.to_bits()),
        }
    }

    /// Count one frame (audio thread).
    pub(crate) fn count(&self, level_db: f32, gate_db: f32, voiced: bool) {
        let b = bin(level_db);
        self.all[b].fetch_add(1, Ordering::Relaxed);
        if voiced {
            self.voiced[b].fetch_add(1, Ordering::Relaxed);
        }
        self.level.store(level_db.to_bits(), Ordering::Relaxed);
        self.gate.store(gate_db.to_bits(), Ordering::Relaxed);
    }

    /// The newest frame's level (dBFS).
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// The gate the newest frame was heard against (dBFS).
    pub fn gate_db(&self) -> f32 {
        f32::from_bits(self.gate.load(Ordering::Relaxed))
    }

    /// The frames counted since the last take (and start counting anew).
    pub fn take(&self) -> Levels {
        let mut out = Levels::default();
        for i in 0..METER_BINS {
            out.all[i] = self.all[i].swap(0, Ordering::Relaxed);
            out.voiced[i] = self.voiced[i].swap(0, Ordering::Relaxed);
        }
        out
    }
}

/// Frames counted by level (see [`Meter`]): all of them, and the voiced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Levels {
    pub all: [u32; METER_BINS],
    pub voiced: [u32; METER_BINS],
}

impl Default for Levels {
    fn default() -> Self {
        Self {
            all: [0; METER_BINS],
            voiced: [0; METER_BINS],
        }
    }
}

impl Levels {
    /// The levels of `x` (frames of `frame` samples), every one voiced
    /// when `voiced`, for tests and offline use.
    pub fn of(x: &[f32], frame: usize, voiced: bool) -> Self {
        let mut out = Self::default();
        for chunk in x.chunks(frame.max(1)) {
            let power =
                chunk.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / chunk.len() as f64;
            out.count((10.0 * power.max(1e-14).log10()) as f32, voiced);
        }
        out
    }

    pub fn count(&mut self, level_db: f32, voiced: bool) {
        let b = bin(level_db);
        self.all[b] += 1;
        if voiced {
            self.voiced[b] += 1;
        }
    }

    pub fn add(&mut self, other: &Levels) {
        for i in 0..METER_BINS {
            self.all[i] += other.all[i];
            self.voiced[i] += other.voiced[i];
        }
    }

    /// Frames counted (voiced ones only, or all).
    pub fn frames(&self, voiced: bool) -> u32 {
        self.bins(voiced).iter().sum()
    }

    fn bins(&self, voiced: bool) -> &[u32; METER_BINS] {
        if voiced { &self.voiced } else { &self.all }
    }

    /// The level (dBFS) a share `p` (0–1) of the frames are at or under.
    pub fn percentile(&self, voiced: bool, p: f64) -> Option<f32> {
        let bins = self.bins(voiced);
        let total = self.frames(voiced);
        if total == 0 {
            return None;
        }
        let want = (f64::from(total) * p.clamp(0.0, 1.0)).ceil().max(1.0) as u32;
        let mut seen = 0;
        for (i, n) in bins.iter().enumerate() {
            seen += n;
            if seen >= want {
                return Some(i as f32 - 127.0);
            }
        }
        Some(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_follows_the_room_and_not_a_held_note() {
        let mut g = LevelGate::new(0.001);
        // Four seconds of a room at −70 dB: the floor rises to it.
        for _ in 0..4000 {
            g.hear(-70.0, false, -90.0);
        }
        assert!((g.floor_db() + 70.0).abs() < 0.5, "{}", g.floor_db());
        assert_eq!(g.gate_db(-90.0), -70.0 + FLOOR_MARGIN_DB);
        assert_eq!(g.gate_db(-40.0), -40.0, "the threshold when louder");
        // A note held for four seconds hardly moves it.
        for _ in 0..4000 {
            g.hear(-20.0, true, -90.0);
        }
        assert!(g.floor_db() < -67.0, "{}", g.floor_db());
        // Quieter at once.
        g.hear(-80.0, false, -90.0);
        assert_eq!(g.floor_db(), -80.0);
    }

    #[test]
    fn velocities_follow_the_loud_notes_or_the_calibration() {
        let mut g = LevelGate::new(0.001);
        for _ in 0..1000 {
            g.hear(-90.0, false, -60.0);
        }
        // A note barely over the gate is soft while nothing louder came.
        assert!(g.velocity(-56.0, -60.0, None) < 110);
        // The first loud note is loud, a note 18 dB softer about half.
        assert_eq!(g.velocity(-20.0, -60.0, None), 127);
        let half = g.velocity(-38.0, -60.0, None);
        assert!((55..=75).contains(&half), "{half}");
        // The same notes 30 dB quieter (a lower gain) play the same.
        let mut quiet = LevelGate::new(0.001);
        for _ in 0..1000 {
            quiet.hear(-120.0, false, -90.0);
        }
        assert_eq!(quiet.velocity(-50.0, -90.0, None), 127);
        assert_eq!(quiet.velocity(-68.0, -90.0, None), half);
        let d = Dynamics {
            soft_db: -40.0,
            loud_db: -16.0,
        };
        assert_eq!(g.velocity(-16.0, -60.0, Some(d)), 127);
        assert_eq!(g.velocity(-40.0, -60.0, Some(d)), 40);
    }

    #[test]
    fn mains_hum_is_known_by_its_pitch() {
        for hz in [
            50.0, 60.0, 59.0, 61.0, 100.0, 120.2, 150.0, 179.5, 200.0, 240.0,
        ] {
            assert!(on_mains(hz), "{hz}");
        }
        // No equal-tempered note is mains hum.
        for key in 24..96 {
            let hz = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
            assert!(!on_mains(hz), "{key}");
        }
    }

    #[test]
    fn the_meter_counts_levels_for_the_control_side() {
        let m = Meter::new();
        for _ in 0..90 {
            m.count(-60.0, -50.0, false);
        }
        for _ in 0..10 {
            m.count(-20.4, -50.0, true);
        }
        assert_eq!(m.level_db(), -20.4);
        let l = m.take();
        assert_eq!(l.frames(false), 100);
        assert_eq!(l.frames(true), 10);
        assert_eq!(l.percentile(false, 0.5), Some(-60.0));
        assert_eq!(l.percentile(false, 0.95), Some(-20.0));
        assert_eq!(l.percentile(true, 0.1), Some(-20.0));
        assert_eq!(m.take().frames(false), 0, "taken");
        assert_eq!(Levels::default().percentile(true, 0.5), None);
    }
}

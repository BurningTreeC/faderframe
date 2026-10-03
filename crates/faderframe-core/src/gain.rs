//! Gain, decibel and fader-law handling.
//!
//! Faders are **not** linear multipliers. The persisted value of a fader is a
//! level in dB; a [`FaderLaw`] maps that level to a normalised travel position
//! (0.0 = bottom, 1.0 = top) the way a hardware console's fader scale does,
//! with a generous resolution around unity and a compressed low end.
//!
//! "Minus infinity" is represented by [`SILENCE_DB`] rather than
//! `f32::NEG_INFINITY` so that values remain finite in project files.

/// A level in decibels. Values `<= SILENCE_DB` mean "-inf" (silence).
pub type Decibels = f32;

/// Levels at or below this are treated as silence (-inf dB).
pub const SILENCE_DB: Decibels = -144.0;

/// Convert a level in dB to a linear amplitude factor.
#[inline]
pub fn db_to_gain(db: Decibels) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        10.0f32.powf(db / 20.0)
    }
}

/// Convert a linear amplitude factor to dB; zero/negative maps to [`SILENCE_DB`].
#[inline]
pub fn gain_to_db(gain: f32) -> Decibels {
    if gain <= 0.0 {
        SILENCE_DB
    } else {
        (20.0 * gain.log10()).max(SILENCE_DB)
    }
}

/// Is this level "-inf"?
#[inline]
pub fn is_silent_db(db: Decibels) -> bool {
    db <= SILENCE_DB
}

/// Format a dB value for display ("-inf", "+3.0", "-12.5").
pub fn format_db(db: Decibels) -> String {
    if is_silent_db(db) {
        "-inf".to_string()
    } else if db.abs() < 0.05 {
        "0.0".to_string()
    } else {
        format!("{db:+.1}")
    }
}

/// Table-driven fader law mapping dB levels to normalised fader travel.
///
/// The table must be sorted by ascending position and dB. Below the lowest
/// table point the scale continues linearly for another
/// [`FaderLaw::TAIL_RANGE_DB`] down to position 0, which is "-inf".
#[derive(Clone, Debug, PartialEq)]
pub struct FaderLaw {
    /// `(position, db)` pairs, ascending.
    points: Vec<(f32, Decibels)>,
}

impl Default for FaderLaw {
    fn default() -> Self {
        Self::console()
    }
}

impl FaderLaw {
    /// dB covered by the linear tail between the lowest table point and position 0.
    pub const TAIL_RANGE_DB: f32 = 60.0;

    /// Console-style law: +12 dB at the top, unity at 75 % travel.
    pub fn console() -> Self {
        Self::from_points(vec![
            (0.040, -70.0),
            (0.100, -50.0),
            (0.200, -40.0),
            (0.300, -30.0),
            (0.420, -20.0),
            (0.570, -10.0),
            (0.660, -5.0),
            (0.750, 0.0),
            (0.875, 6.0),
            (1.000, 12.0),
        ])
    }

    /// Build a law from `(position, db)` points (sorted ascending in both).
    ///
    /// # Panics
    /// Panics if fewer than two points are given or the points are not
    /// strictly ascending; that is a programming error in a law definition.
    pub fn from_points(points: Vec<(f32, Decibels)>) -> Self {
        assert!(points.len() >= 2, "fader law needs at least two points");
        assert!(
            points
                .windows(2)
                .all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1),
            "fader law points must be strictly ascending"
        );
        Self { points }
    }

    /// Level at the top of the fader travel.
    pub fn max_db(&self) -> Decibels {
        self.points[self.points.len() - 1].1
    }

    /// Fader position (0..=1) at which the law reaches unity gain.
    pub fn unity_position(&self) -> f32 {
        self.db_to_position(0.0)
    }

    /// Map a normalised fader position (0..=1) to a level in dB.
    pub fn position_to_db(&self, position: f32) -> Decibels {
        let pos = position.clamp(0.0, 1.0);
        if pos <= 0.0 {
            return SILENCE_DB;
        }
        let (p0, d0) = self.points[0];
        if pos < p0 {
            // Linear tail towards -inf.
            let db = d0 - Self::TAIL_RANGE_DB * (1.0 - pos / p0);
            return db.max(SILENCE_DB + 1.0);
        }
        for w in self.points.windows(2) {
            let (pa, da) = w[0];
            let (pb, db) = w[1];
            if pos <= pb {
                let t = (pos - pa) / (pb - pa);
                return da + t * (db - da);
            }
        }
        self.max_db()
    }

    /// Map a level in dB to a normalised fader position (0..=1).
    pub fn db_to_position(&self, level: Decibels) -> f32 {
        if is_silent_db(level) {
            return 0.0;
        }
        let (p0, d0) = self.points[0];
        if level < d0 {
            let pos = p0 * (1.0 + (level - d0) / Self::TAIL_RANGE_DB);
            return pos.clamp(0.0, p0);
        }
        for w in self.points.windows(2) {
            let (pa, da) = w[0];
            let (pb, db) = w[1];
            if level <= db {
                let t = (level - da) / (db - da);
                return pa + t * (pb - pa);
            }
        }
        1.0
    }

    /// Clamp a level to the range the fader can represent.
    pub fn clamp_db(&self, level: Decibels) -> Decibels {
        if is_silent_db(level) {
            SILENCE_DB
        } else {
            level.min(self.max_db())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_gain_round_trip() {
        for db in [-60.0f32, -12.0, -6.0, 0.0, 3.0, 12.0] {
            let g = db_to_gain(db);
            assert!((gain_to_db(g) - db).abs() < 1e-3, "db={db}");
        }
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert_eq!(db_to_gain(SILENCE_DB), 0.0);
        assert_eq!(gain_to_db(0.0), SILENCE_DB);
    }

    #[test]
    fn fader_law_unity_and_extremes() {
        let law = FaderLaw::console();
        assert!((law.position_to_db(0.75)).abs() < 1e-6);
        assert!((law.unity_position() - 0.75).abs() < 1e-6);
        assert_eq!(law.position_to_db(0.0), SILENCE_DB);
        assert_eq!(law.db_to_position(SILENCE_DB), 0.0);
        assert!((law.position_to_db(1.0) - 12.0).abs() < 1e-6);
        assert_eq!(law.db_to_position(40.0), 1.0);
    }

    #[test]
    fn fader_law_is_monotonic_and_invertible() {
        let law = FaderLaw::console();
        let mut last = f32::NEG_INFINITY;
        for i in 1..=1000 {
            let pos = i as f32 / 1000.0;
            let db = law.position_to_db(pos);
            assert!(db > last, "not monotonic at {pos}");
            last = db;
            let back = law.db_to_position(db);
            assert!((back - pos).abs() < 1e-4, "pos={pos} db={db} back={back}");
        }
    }

    #[test]
    fn format_db_values() {
        assert_eq!(format_db(SILENCE_DB), "-inf");
        assert_eq!(format_db(0.01), "0.0");
        assert_eq!(format_db(-3.0), "-3.0");
        assert_eq!(format_db(6.0), "+6.0");
    }
}

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

/// Internal tick resolution per quarter note.
///
/// 960 000 = 960 PPQ × 1000. It is divisible by 2⁹, 3 and 5⁴ (straight,
/// triplet and quintuplet grids are exact) and finer than one sample at
/// 192 kHz down to ~20 BPM, so audio clips can be placed sample-accurately in
/// musical time.
pub const TICKS_PER_QUARTER: i64 = 960_000;

/// A position (or duration) in musical time, measured in ticks from the
/// project origin. Quarter-note based and independent of the tempo.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MusicalTime(pub i64);

/// Durations use the same representation as positions.
pub type MusicalDuration = MusicalTime;

impl MusicalTime {
    pub const ZERO: MusicalTime = MusicalTime(0);
    pub const QUARTER: MusicalTime = MusicalTime(TICKS_PER_QUARTER);
    pub const MAX: MusicalTime = MusicalTime(i64::MAX / 4);

    #[inline]
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    #[inline]
    pub const fn ticks(self) -> i64 {
        self.0
    }

    /// Whole quarter notes.
    #[inline]
    pub const fn from_quarters_i(q: i64) -> Self {
        Self(q * TICKS_PER_QUARTER)
    }

    /// Fractional quarter notes (rounded to the nearest tick).
    #[inline]
    pub fn from_quarters(q: f64) -> Self {
        Self((q * TICKS_PER_QUARTER as f64).round() as i64)
    }

    /// Position in (fractional) quarter notes.
    #[inline]
    pub fn quarters(self) -> f64 {
        self.0 as f64 / TICKS_PER_QUARTER as f64
    }

    /// Convert from a MIDI-file style PPQ tick count.
    #[inline]
    pub fn from_ppq(ticks: i64, ppq: u32) -> Self {
        Self(ticks * TICKS_PER_QUARTER / ppq as i64)
    }

    /// Convert to a MIDI-file style PPQ tick count (truncating).
    #[inline]
    pub fn to_ppq(self, ppq: u32) -> i64 {
        self.0 * ppq as i64 / TICKS_PER_QUARTER
    }

    #[inline]
    pub fn max(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }

    #[inline]
    pub fn min(self, other: Self) -> Self {
        if self <= other { self } else { other }
    }
}

impl fmt::Debug for MusicalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4}q", self.quarters())
    }
}

impl Add for MusicalTime {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for MusicalTime {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl AddAssign for MusicalTime {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl SubAssign for MusicalTime {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl Neg for MusicalTime {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl Mul<i64> for MusicalTime {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: i64) -> Self {
        Self(self.0 * rhs)
    }
}

impl Div<i64> for MusicalTime {
    type Output = Self;
    #[inline]
    fn div(self, rhs: i64) -> Self {
        Self(self.0 / rhs)
    }
}

/// Format seconds as `m:ss.mmm` (or `h:mm:ss.mmm` past an hour).
pub fn format_seconds(seconds: f64) -> String {
    let sign = if seconds < 0.0 { "-" } else { "" };
    let total_ms = (seconds.abs() * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    if h > 0 {
        format!("{sign}{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{sign}{m}:{s:02}.{ms:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_and_ppq_conversion() {
        assert_eq!(MusicalTime::from_quarters(1.5).ticks(), 1_440_000);
        assert_eq!(
            MusicalTime::from_ppq(480, 960),
            MusicalTime::from_quarters(0.5)
        );
        assert_eq!(MusicalTime::from_quarters(2.0).to_ppq(960), 1920);
        // Triplet eighths are exact.
        let triplet = MusicalTime::QUARTER / 3;
        assert_eq!(triplet * 3, MusicalTime::QUARTER);
    }

    #[test]
    fn seconds_formatting() {
        assert_eq!(format_seconds(0.0), "0:00.000");
        assert_eq!(format_seconds(61.25), "1:01.250");
        assert_eq!(format_seconds(3725.5), "1:02:05.500");
        assert_eq!(format_seconds(-1.0), "-0:01.000");
    }
}

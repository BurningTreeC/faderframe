use crate::time::{MusicalTime, TICKS_PER_QUARTER};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A time signature such as 4/4, 6/8 or 7/8.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    /// Note value of one beat; always a power of two.
    pub denominator: u8,
}

impl TimeSignature {
    pub const FOUR_FOUR: TimeSignature = TimeSignature {
        numerator: 4,
        denominator: 4,
    };

    /// Validated constructor: numerator 1..=64, denominator a power of two 1..=64.
    pub fn new(numerator: u8, denominator: u8) -> Option<Self> {
        let ok = (1..=64).contains(&numerator)
            && (1..=64).contains(&denominator)
            && denominator.is_power_of_two();
        ok.then_some(Self {
            numerator,
            denominator,
        })
    }

    /// Length of one beat (one denominator note).
    #[inline]
    pub fn beat_length(self) -> MusicalTime {
        MusicalTime(TICKS_PER_QUARTER * 4 / self.denominator.max(1) as i64)
    }

    /// Length of one bar.
    #[inline]
    pub fn bar_length(self) -> MusicalTime {
        self.beat_length() * self.numerator.max(1) as i64
    }

    fn sanitised(self) -> Self {
        Self::new(self.numerator, self.denominator).unwrap_or(Self::FOUR_FOUR)
    }
}

impl fmt::Display for TimeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

/// A time-signature change taking effect at the start of `bar` (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterChange {
    pub bar: i32,
    pub signature: TimeSignature,
}

/// Bar / beat / tick position. All fields are 0-based; [`fmt::Display`]
/// renders the conventional 1-based `bar.beat.tick` form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bbt {
    pub bar: i32,
    pub beat: u32,
    /// Ticks into the beat (internal resolution).
    pub tick: i64,
}

impl fmt::Display for Bbt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display ticks at 960 PPQ like most DAWs.
        let display_tick = self.tick * 960 / TICKS_PER_QUARTER;
        write!(f, "{}.{}.{:03}", self.bar + 1, self.beat + 1, display_tick)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TimeSignatureMapData {
    changes: Vec<MeterChange>,
}

/// Sequence of time-signature changes at bar boundaries.
///
/// Invariants: at least one change, the first at bar 0, bars strictly
/// ascending. `starts[i]` caches the musical position of change `i`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "TimeSignatureMapData", into = "TimeSignatureMapData")]
pub struct TimeSignatureMap {
    changes: Vec<MeterChange>,
    starts: Vec<MusicalTime>,
}

impl From<TimeSignatureMapData> for TimeSignatureMap {
    fn from(data: TimeSignatureMapData) -> Self {
        let mut map = Self {
            changes: data.changes,
            starts: Vec::new(),
        };
        map.normalise();
        map
    }
}

impl From<TimeSignatureMap> for TimeSignatureMapData {
    fn from(map: TimeSignatureMap) -> Self {
        Self {
            changes: map.changes,
        }
    }
}

impl TimeSignatureMap {
    pub fn new(signature: TimeSignature) -> Self {
        let mut map = Self {
            changes: vec![MeterChange { bar: 0, signature }],
            starts: Vec::new(),
        };
        map.normalise();
        map
    }

    pub fn changes(&self) -> &[MeterChange] {
        &self.changes
    }

    /// Insert or replace the change at `change.bar`.
    pub fn set_change(&mut self, change: MeterChange) {
        match self.changes.binary_search_by_key(&change.bar, |c| c.bar) {
            Ok(i) => self.changes[i] = change,
            Err(i) => self.changes.insert(i, change),
        }
        self.normalise();
    }

    fn normalise(&mut self) {
        if self.changes.is_empty() {
            self.changes.push(MeterChange {
                bar: 0,
                signature: TimeSignature::FOUR_FOUR,
            });
        }
        self.changes.sort_by_key(|c| c.bar);
        self.changes.dedup_by_key(|c| c.bar);
        self.changes.retain(|c| c.bar >= 0);
        if self.changes.first().map(|c| c.bar) != Some(0) {
            let sig = self
                .changes
                .first()
                .map(|c| c.signature)
                .unwrap_or(TimeSignature::FOUR_FOUR);
            self.changes.insert(
                0,
                MeterChange {
                    bar: 0,
                    signature: sig,
                },
            );
        }
        for c in &mut self.changes {
            c.signature = c.signature.sanitised();
        }
        self.starts.clear();
        let mut pos = MusicalTime::ZERO;
        for i in 0..self.changes.len() {
            self.starts.push(pos);
            if let Some(next) = self.changes.get(i + 1) {
                let bars = (next.bar - self.changes[i].bar) as i64;
                pos += self.changes[i].signature.bar_length() * bars;
            }
        }
    }

    #[inline]
    fn index_for_bar(&self, bar: i32) -> usize {
        self.changes
            .partition_point(|c| c.bar <= bar)
            .saturating_sub(1)
    }

    #[inline]
    fn index_for_position(&self, pos: MusicalTime) -> usize {
        self.starts.partition_point(|&s| s <= pos).saturating_sub(1)
    }

    /// Time signature in effect at `pos`.
    pub fn signature_at(&self, pos: MusicalTime) -> TimeSignature {
        self.changes[self.index_for_position(pos)].signature
    }

    /// Time signature of `bar`.
    pub fn signature_of_bar(&self, bar: i32) -> TimeSignature {
        self.changes[self.index_for_bar(bar)].signature
    }

    /// Musical position where `bar` (0-based, may be negative) starts.
    pub fn bar_start(&self, bar: i32) -> MusicalTime {
        let i = self.index_for_bar(bar);
        let c = &self.changes[i];
        self.starts[i] + c.signature.bar_length() * (bar - c.bar) as i64
    }

    /// 0-based bar containing `pos`.
    pub fn bar_at(&self, pos: MusicalTime) -> i32 {
        let i = self.index_for_position(pos);
        let c = &self.changes[i];
        let offset = (pos - self.starts[i]).ticks();
        let bar_len = c.signature.bar_length().ticks();
        c.bar + offset.div_euclid(bar_len) as i32
    }

    pub fn to_bbt(&self, pos: MusicalTime) -> Bbt {
        let bar = self.bar_at(pos);
        let sig = self.signature_of_bar(bar);
        let into_bar = (pos - self.bar_start(bar)).ticks();
        let beat_len = sig.beat_length().ticks();
        Bbt {
            bar,
            beat: (into_bar / beat_len) as u32,
            tick: into_bar % beat_len,
        }
    }

    pub fn from_bbt(&self, bbt: Bbt) -> MusicalTime {
        let sig = self.signature_of_bar(bbt.bar);
        self.bar_start(bbt.bar) + sig.beat_length() * bbt.beat as i64 + MusicalTime(bbt.tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: i64) -> MusicalTime {
        MusicalTime::from_quarters_i(n)
    }

    #[test]
    fn four_four_bars() {
        let map = TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        assert_eq!(map.bar_start(0), q(0));
        assert_eq!(map.bar_start(3), q(12));
        assert_eq!(map.bar_at(q(13)), 3);
        assert_eq!(map.bar_at(q(-1)), -1);
        let bbt = map.to_bbt(q(13) + MusicalTime::QUARTER / 2);
        assert_eq!((bbt.bar, bbt.beat, bbt.tick), (3, 1, TICKS_PER_QUARTER / 2));
        assert_eq!(bbt.to_string(), "4.2.480");
        assert_eq!(map.from_bbt(bbt), q(13) + MusicalTime::QUARTER / 2);
    }

    #[test]
    fn mixed_meters() {
        let mut map = TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        map.set_change(MeterChange {
            bar: 2,
            signature: TimeSignature::new(6, 8).unwrap(),
        });
        map.set_change(MeterChange {
            bar: 4,
            signature: TimeSignature::new(7, 8).unwrap(),
        });
        // Two 4/4 bars = 8 q, two 6/8 bars = 6 q.
        assert_eq!(map.bar_start(2), q(8));
        assert_eq!(map.bar_start(4), q(14));
        assert_eq!(map.bar_start(5), q(14) + MusicalTime::QUARTER * 7 / 2);
        assert_eq!(map.bar_at(q(13)), 3);
        assert_eq!(map.signature_at(q(9)).to_string(), "6/8");
        let bbt = map.to_bbt(q(9));
        // One quarter into a 6/8 bar = beat index 2 (eighths).
        assert_eq!((bbt.bar, bbt.beat, bbt.tick), (2, 2, 0));
        for t in [q(0), q(5), q(9), q(14), q(15) + MusicalTime(123)] {
            assert_eq!(map.from_bbt(map.to_bbt(t)), t);
        }
    }

    #[test]
    fn invalid_signatures_are_rejected_or_sanitised() {
        assert!(TimeSignature::new(4, 3).is_none());
        assert!(TimeSignature::new(0, 4).is_none());
        let json = r#"{"changes":[{"bar":3,"signature":{"numerator":5,"denominator":3}}]}"#;
        let map: TimeSignatureMap = serde_json::from_str(json).unwrap();
        assert_eq!(map.changes()[0].bar, 0);
        assert_eq!(map.signature_of_bar(5), TimeSignature::FOUR_FOUR);
    }
}

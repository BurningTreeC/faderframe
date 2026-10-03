//! Per-note expression (MPE).
//!
//! With MPE each sounding note gets a MIDI channel of its own, so pitch
//! bend, channel pressure and CC 74 ("timbre", "slide") shape that one
//! note. FaderFrame stores the shapes with the notes — a [`NoteExpression`]
//! per note in [`MidiClip::expressions`](crate::MidiClip), times relative to
//! the note's start — and assigns channels only when playing (tracks with
//! an [`MpeConfig`]). Notes can therefore be moved, copied, transposed and
//! split freely; their expression goes with them.

use faderframe_core::NoteId;
use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// The dimensions of per-note expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExpressionKind {
    /// Pitch offset in semitones (pitch bend on the note's channel).
    Pitch,
    /// 0–1 (channel pressure).
    Pressure,
    /// 0–1 (CC 74).
    Timbre,
}

impl ExpressionKind {
    pub const ALL: [ExpressionKind; 3] = [Self::Pitch, Self::Pressure, Self::Timbre];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pitch => "Pitch",
            Self::Pressure => "Pressure",
            Self::Timbre => "Timbre",
        }
    }

    /// Value before any point (and of notes without expression).
    pub fn rest(self) -> f32 {
        match self {
            Self::Pitch | Self::Pressure => 0.0,
            Self::Timbre => 0.5,
        }
    }

    /// Range of values (pitch: the MPE maximum of ±96 semitones).
    pub fn range(self) -> (f32, f32) {
        match self {
            Self::Pitch => (-96.0, 96.0),
            Self::Pressure | Self::Timbre => (0.0, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpressionPoint {
    /// From the note's start.
    pub time: MusicalTime,
    pub value: f32,
}

/// The expression curves of one note (linear between points; before the
/// first point the curve has the kind's rest value, after the last it
/// holds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteExpression {
    pub note: NoteId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pitch: Vec<ExpressionPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pressure: Vec<ExpressionPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timbre: Vec<ExpressionPoint>,
}

impl NoteExpression {
    pub fn new(note: NoteId) -> Self {
        Self {
            note,
            pitch: Vec::new(),
            pressure: Vec::new(),
            timbre: Vec::new(),
        }
    }

    pub fn curve(&self, kind: ExpressionKind) -> &[ExpressionPoint] {
        match kind {
            ExpressionKind::Pitch => &self.pitch,
            ExpressionKind::Pressure => &self.pressure,
            ExpressionKind::Timbre => &self.timbre,
        }
    }

    pub fn curve_mut(&mut self, kind: ExpressionKind) -> &mut Vec<ExpressionPoint> {
        match kind {
            ExpressionKind::Pitch => &mut self.pitch,
            ExpressionKind::Pressure => &mut self.pressure,
            ExpressionKind::Timbre => &mut self.timbre,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pitch.is_empty() && self.pressure.is_empty() && self.timbre.is_empty()
    }

    /// The curve's value at `time` (from the note's start).
    pub fn value_at(&self, kind: ExpressionKind, time: MusicalTime) -> f32 {
        let c = self.curve(kind);
        let Some(first) = c.first() else {
            return kind.rest();
        };
        if time < first.time {
            return first.value;
        }
        match c.iter().position(|p| p.time > time) {
            None => c.last().map_or(kind.rest(), |p| p.value),
            Some(i) => {
                let (a, b) = (c[i - 1], c[i]);
                let span = (b.time - a.time).ticks().max(1) as f32;
                let f = (time - a.time).ticks() as f32 / span;
                a.value + (b.value - a.value) * f
            }
        }
    }

    /// Replace the points of `kind` in `from..to` with `points` (sorted,
    /// clamped to the kind's range).
    pub fn replace_range(
        &mut self,
        kind: ExpressionKind,
        from: MusicalTime,
        to: MusicalTime,
        points: &[ExpressionPoint],
    ) {
        let (lo, hi) = kind.range();
        let c = self.curve_mut(kind);
        c.retain(|p| p.time < from || p.time >= to);
        c.extend(points.iter().map(|p| ExpressionPoint {
            time: p.time.max(MusicalTime::ZERO),
            value: p.value.clamp(lo, hi),
        }));
        c.sort_by_key(|p| p.time);
        c.dedup_by_key(|p| p.time);
    }

    /// Split at `at` (from the note's start): `self` keeps what is before,
    /// the result (for note `right`) gets the rest, starting with the value
    /// at the cut.
    pub fn split_off(&mut self, at: MusicalTime, right: NoteId) -> NoteExpression {
        let mut out = NoteExpression::new(right);
        for kind in ExpressionKind::ALL {
            if self.curve(kind).is_empty() {
                continue;
            }
            let carried = self.value_at(kind, at);
            let rest: Vec<ExpressionPoint> = self
                .curve(kind)
                .iter()
                .filter(|p| p.time > at)
                .map(|p| ExpressionPoint {
                    time: p.time - at,
                    value: p.value,
                })
                .collect();
            let c = out.curve_mut(kind);
            c.push(ExpressionPoint {
                time: MusicalTime::ZERO,
                value: carried,
            });
            c.extend(rest);
            let left = self.curve_mut(kind);
            left.retain(|p| p.time <= at);
            if left.last().is_none_or(|p| p.time < at) {
                left.push(ExpressionPoint {
                    time: at,
                    value: carried,
                });
            }
        }
        out
    }
}

/// An MPE zone on a track's instrument or MIDI output (the lower zone:
/// channel 1 is the master channel, 2… are member channels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MpeConfig {
    /// Member channels (1–15).
    pub members: u8,
    /// Pitch bend range of the member channels in semitones (MPE default
    /// 48).
    pub bend_range: u8,
}

impl Default for MpeConfig {
    fn default() -> Self {
        Self {
            members: 15,
            bend_range: 48,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: i64, v: f32) -> ExpressionPoint {
        ExpressionPoint {
            time: MusicalTime(t),
            value: v,
        }
    }

    #[test]
    fn curves_interpolate_hold_and_split() {
        let mut e = NoteExpression::new(NoteId(1));
        assert_eq!(e.value_at(ExpressionKind::Timbre, MusicalTime(5)), 0.5);
        e.replace_range(
            ExpressionKind::Pitch,
            MusicalTime::ZERO,
            MusicalTime(1000),
            &[p(0, 0.0), p(100, 2.0), p(200, 2.0)],
        );
        assert_eq!(e.value_at(ExpressionKind::Pitch, MusicalTime(50)), 1.0);
        assert_eq!(e.value_at(ExpressionKind::Pitch, MusicalTime(500)), 2.0);
        let right = e.split_off(MusicalTime(50), NoteId(2));
        assert_eq!(right.note, NoteId(2));
        assert_eq!(
            right.value_at(ExpressionKind::Pitch, MusicalTime::ZERO),
            1.0
        );
        assert_eq!(right.value_at(ExpressionKind::Pitch, MusicalTime(50)), 2.0);
        assert_eq!(e.value_at(ExpressionKind::Pitch, MusicalTime(50)), 1.0);
        assert!(right.pressure.is_empty(), "untouched curves stay empty");
        // Values are clamped to the kind's range.
        e.replace_range(
            ExpressionKind::Pressure,
            MusicalTime::ZERO,
            MusicalTime(10),
            &[p(0, 3.0)],
        );
        assert_eq!(e.pressure[0].value, 1.0);
    }
}

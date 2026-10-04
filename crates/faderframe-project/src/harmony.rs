//! The project's harmony: key changes along the timeline and the chord
//! track. Both are plain sorted lists edited whole (`Command::SetKeys`,
//! `Command::SetChords`), travel with section moves and copies
//! ([`crate::arrange`]), and reach the engine in the timeline snapshot for
//! MIDI effects that follow them.

use crate::Project;
pub use faderframe_midi::theory::{Chord, Key, Quality, Scale, pc_name};
use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// From `at` on the music is in `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyChange {
    pub at: MusicalTime,
    pub key: Key,
}

/// A chord on the chord track over `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChordEvent {
    pub start: MusicalTime,
    pub end: MusicalTime,
    pub chord: Chord,
}

/// Sorted by position, one change a position, none repeating the key
/// before it.
pub fn normalize_keys(keys: &mut Vec<KeyChange>) {
    keys.sort_by_key(|k| k.at);
    // The last of several at one position wins.
    let mut out: Vec<KeyChange> = Vec::with_capacity(keys.len());
    for k in keys.drain(..) {
        match out.last_mut() {
            Some(last) if last.at == k.at => *last = k,
            _ => out.push(k),
        }
    }
    out.dedup_by(|b, a| a.key == b.key);
    *keys = out;
}

/// Sorted by start, positive lengths, none overlapping (a later chord cuts
/// the one before it short).
pub fn normalize_chords(chords: &mut Vec<ChordEvent>) {
    chords.retain(|c| c.end > c.start);
    chords.sort_by_key(|c| c.start);
    let mut out: Vec<ChordEvent> = Vec::with_capacity(chords.len());
    for c in chords.drain(..) {
        if let Some(last) = out.last_mut()
            && last.end > c.start
        {
            if last.start == c.start {
                *last = c;
                continue;
            }
            last.end = c.start;
        }
        out.push(c);
    }
    *chords = out;
}

impl Project {
    /// The key at `t` (`None`: no key set before it).
    pub fn key_at(&self, t: MusicalTime) -> Option<Key> {
        self.keys.iter().rev().find(|k| k.at <= t).map(|k| k.key)
    }

    /// The key a project starts in, or of its first change.
    pub fn main_key(&self) -> Option<Key> {
        self.keys.first().map(|k| k.key)
    }

    /// The chord sounding at `t`.
    pub fn chord_at(&self, t: MusicalTime) -> Option<&ChordEvent> {
        self.chords.iter().find(|c| c.start <= t && t < c.end)
    }

    /// Whether `t` sits in a key that spells with flats.
    pub fn flats_at(&self, t: MusicalTime) -> bool {
        self.key_at(t).is_some_and(Key::flats)
    }
}

/// `chords` with `start..end` given to `chord` (or emptied): chords over
/// the range are cut round it.
pub fn set_chord(
    chords: &[ChordEvent],
    start: MusicalTime,
    end: MusicalTime,
    chord: Option<Chord>,
) -> Vec<ChordEvent> {
    let (start, end) = (start.min(end), start.max(end));
    let mut out = Vec::with_capacity(chords.len() + 2);
    for c in chords {
        if c.end <= start || c.start >= end {
            out.push(*c);
            continue;
        }
        if c.start < start {
            out.push(ChordEvent { end: start, ..*c });
        }
        if c.end > end {
            out.push(ChordEvent { start: end, ..*c });
        }
    }
    if let Some(chord) = chord
        && end > start
    {
        out.push(ChordEvent { start, end, chord });
    }
    normalize_chords(&mut out);
    out
}

/// `keys` with the key from `at` on set (or that change removed).
pub fn set_key(keys: &[KeyChange], at: MusicalTime, key: Option<Key>) -> Vec<KeyChange> {
    let mut out: Vec<KeyChange> = keys.iter().copied().filter(|k| k.at != at).collect();
    if let Some(key) = key {
        out.push(KeyChange { at, key });
    }
    normalize_keys(&mut out);
    out
}

/// A note for detection: where it sounds and its pitch.
#[derive(Clone, Copy, Debug)]
pub struct Sounding {
    pub start: MusicalTime,
    pub end: MusicalTime,
    pub key: u8,
}

/// The chords `notes` make in windows of `step` over `start..end` (a
/// window's chord from the notes sounding longest in it; equal chords in a
/// row join).
pub fn detect_chords(
    notes: &[Sounding],
    start: MusicalTime,
    end: MusicalTime,
    step: MusicalTime,
) -> Vec<ChordEvent> {
    let mut out: Vec<ChordEvent> = Vec::new();
    if step <= MusicalTime::ZERO {
        return out;
    }
    let mut t = start;
    while t < end {
        let w_end = (t + step).min(end);
        // Time each pitch sounds in the window; the chord is made of the
        // pitches sounding for at least a quarter of it.
        let mut by_key: std::collections::BTreeMap<u8, i64> = Default::default();
        for n in notes {
            let overlap = n.end.min(w_end).ticks() - n.start.max(t).ticks();
            if overlap > 0 {
                *by_key.entry(n.key).or_default() += overlap;
            }
        }
        let need = (w_end.ticks() - t.ticks()) / 4;
        let keys: Vec<i32> = by_key
            .iter()
            .filter(|(_, d)| **d >= need)
            .map(|(k, _)| i32::from(*k))
            .collect();
        if let Some(chord) = Chord::recognise(&keys) {
            match out.last_mut() {
                Some(last) if last.chord == chord && last.end == t => last.end = w_end,
                _ => out.push(ChordEvent {
                    start: t,
                    end: w_end,
                    chord,
                }),
            }
        }
        t = w_end;
    }
    out
}

/// The key of `notes` (weighted by duration).
pub fn detect_key(notes: &[Sounding]) -> Option<Key> {
    let mut h = [0.0; 12];
    for n in notes {
        h[usize::from(n.key % 12)] += (n.end.ticks() - n.start.ticks()).max(0) as f64;
    }
    faderframe_midi::theory::detect_key(&h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: f64) -> MusicalTime {
        MusicalTime::from_quarters(n)
    }

    #[test]
    fn keys_and_chords_normalise() {
        let a = Key::new(9, Scale::Minor);
        let c = Key::new(0, Scale::Major);
        let mut keys = vec![
            KeyChange { at: q(8.0), key: c },
            KeyChange { at: q(0.0), key: a },
            KeyChange { at: q(4.0), key: a },
            KeyChange { at: q(8.0), key: a },
        ];
        normalize_keys(&mut keys);
        assert_eq!(keys, vec![KeyChange { at: q(0.0), key: a }], "repeats go");
        let ch = |s: f64, e: f64, name: &str| ChordEvent {
            start: q(s),
            end: q(e),
            chord: faderframe_midi::theory::Chord::parse(name)
                .unwrap_or(Chord::new(0, Quality::Major)),
        };
        let mut chords = vec![
            ch(4.0, 8.0, "F"),
            ch(0.0, 6.0, "Am"),
            ch(8.0, 8.0, "G"),
            ch(4.0, 6.0, "G"),
        ];
        normalize_chords(&mut chords);
        assert_eq!(chords.len(), 2);
        assert_eq!((chords[0].start, chords[0].end), (q(0.0), q(4.0)));
        assert_eq!(
            chords[1].chord.name(false),
            "G",
            "the later one at a position wins"
        );
        let mut p = Project::new("x", 48_000);
        p.keys = vec![
            KeyChange { at: q(0.0), key: a },
            KeyChange {
                at: q(16.0),
                key: c,
            },
        ];
        p.chords = chords;
        assert_eq!(p.key_at(q(15.0)), Some(a));
        assert_eq!(p.key_at(q(16.0)), Some(c));
        assert_eq!(
            p.chord_at(q(1.0)).map(|c| c.chord.name(false)),
            Some("Am".into())
        );
        assert!(p.chord_at(q(7.0)).is_none());
    }

    #[test]
    fn chords_are_set_over_ranges_and_detected_from_notes() {
        let am = Chord::parse("Am").unwrap_or(Chord::new(9, Quality::Minor));
        let f = Chord::parse("F").unwrap_or(Chord::new(5, Quality::Major));
        let base = vec![ChordEvent {
            start: q(0.0),
            end: q(8.0),
            chord: am,
        }];
        let split = set_chord(&base, q(2.0), q(4.0), Some(f));
        let names: Vec<(f64, f64, String)> = split
            .iter()
            .map(|c| (c.start.quarters(), c.end.quarters(), c.chord.name(false)))
            .collect();
        assert_eq!(
            names,
            vec![
                (0.0, 2.0, "Am".into()),
                (2.0, 4.0, "F".into()),
                (4.0, 8.0, "Am".into())
            ]
        );
        assert_eq!(set_chord(&split, q(0.0), q(8.0), None), vec![]);
        // Bar 1: A C E; bar 2: F A C (with a passing G); bar 3: A C E.
        let n = |s: f64, e: f64, key: u8| Sounding {
            start: q(s),
            end: q(e),
            key,
        };
        let notes = vec![
            n(0.0, 4.0, 57),
            n(0.0, 4.0, 60),
            n(0.0, 4.0, 64),
            n(4.0, 8.0, 53),
            n(4.0, 8.0, 57),
            n(4.0, 8.0, 60),
            n(5.0, 5.25, 67),
            n(8.0, 12.0, 57),
            n(8.0, 12.0, 60),
            n(8.0, 12.0, 64),
        ];
        let chords = detect_chords(&notes, q(0.0), q(12.0), q(4.0));
        let names: Vec<String> = chords.iter().map(|c| c.chord.name(false)).collect();
        assert_eq!(names, ["Am", "F", "Am"]);
        assert_eq!(detect_key(&notes).map(|k| k.name()), Some("A Minor".into()));
    }
}

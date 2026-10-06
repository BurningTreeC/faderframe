//! The project's key and chord track as devices see them: in timeline
//! samples, read on the audio thread (binary search, no allocation).

use faderframe_midi::theory::{Chord, Key};

/// Key changes and chords in timeline samples.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Harmony {
    /// `(sample, key)`, sorted by sample.
    pub keys: Vec<(i64, Key)>,
    /// `(start, end, chord)`, sorted, not overlapping.
    pub chords: Vec<(i64, i64, Chord)>,
}

/// No key, no chords (processors outside a project, tests).
pub static NO_HARMONY: Harmony = Harmony {
    keys: Vec::new(),
    chords: Vec::new(),
};

impl Harmony {
    /// The key at `sample` (`None`: none set before it).
    pub fn key_at(&self, sample: i64) -> Option<Key> {
        let i = self.keys.partition_point(|(at, _)| *at <= sample);
        i.checked_sub(1).map(|i| self.keys[i].1)
    }

    /// The chord sounding at `sample`.
    pub fn chord_at(&self, sample: i64) -> Option<Chord> {
        let i = self
            .chords
            .partition_point(|(start, _, _)| *start <= sample);
        let (_, end, chord) = *self.chords.get(i.checked_sub(1)?)?;
        (sample < end).then_some(chord)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_midi::theory::{Quality, Scale};

    #[test]
    fn keys_and_chords_are_found_by_sample() {
        let h = Harmony {
            keys: vec![
                (0, Key::new(0, Scale::Major)),
                (1000, Key::new(9, Scale::Minor)),
            ],
            chords: vec![
                (0, 500, Chord::new(0, Quality::Major)),
                (600, 900, Chord::new(7, Quality::Major)),
            ],
        };
        assert_eq!(h.key_at(-1), None);
        assert_eq!(h.key_at(999).map(|k| k.root), Some(0));
        assert_eq!(h.key_at(1000).map(|k| k.root), Some(9));
        assert_eq!(h.chord_at(499).map(|c| c.root), Some(0));
        assert_eq!(h.chord_at(550), None);
        assert_eq!(h.chord_at(600).map(|c| c.root), Some(7));
        assert_eq!(h.chord_at(900), None);
        assert_eq!(NO_HARMONY.key_at(0), None);
    }
}

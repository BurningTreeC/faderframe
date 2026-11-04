//! How the score editor writes a MIDI clip: its display quantise and, note
//! by note, what the score shows that MIDI does not say — the voice a note
//! is written in, the letter it is spelt on, the syllable sung on it, its
//! articulations and a dynamic.
//!
//! A side table by clip ([`Project::notation`]), so `MidiClip` and
//! `MidiNote` literals stay as they are; the notes themselves (and so what
//! is heard) stay the clip's, edited by the same commands the piano roll's
//! edits are. Dynamics and articulations are also played: entering one
//! sets the notes' velocities and lengths in the same step (see
//! `faderframe_session::score`), the entry here only says how they are
//! written. Entries of notes a clip no longer has are dropped when a
//! project is opened ([`Project::repair`]); edits keep them while the
//! session runs (an undone deletion brings the note back as it was
//! written).

use crate::Project;
use faderframe_core::{ClipId, NoteId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The grid a clip's rhythms are written on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreGrid {
    /// Sixteenths, or eighth triplets in beats that swing that way (by
    /// what fits each beat's notes best).
    #[default]
    Auto,
    /// Sixteenths only.
    Straight,
    /// Eighth triplets only.
    Triplets,
}

impl ScoreGrid {
    pub const ALL: [ScoreGrid; 3] = [ScoreGrid::Auto, ScoreGrid::Straight, ScoreGrid::Triplets];

    pub fn label(self) -> &'static str {
        match self {
            ScoreGrid::Auto => "Automatic (16ths or Triplets)",
            ScoreGrid::Straight => "16ths",
            ScoreGrid::Triplets => "Triplets",
        }
    }
}

/// A note's articulations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteMarks {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staccato: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accent: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tenuto: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub marcato: bool,
}

impl NoteMarks {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// An articulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Staccato,
    Accent,
    Tenuto,
    Marcato,
}

impl Mark {
    pub const ALL: [Mark; 4] = [Mark::Staccato, Mark::Accent, Mark::Tenuto, Mark::Marcato];

    pub fn label(self) -> &'static str {
        match self {
            Mark::Staccato => "Staccato",
            Mark::Accent => "Accent",
            Mark::Tenuto => "Tenuto",
            Mark::Marcato => "Marcato",
        }
    }

    pub fn of(self, marks: &NoteMarks) -> bool {
        match self {
            Mark::Staccato => marks.staccato,
            Mark::Accent => marks.accent,
            Mark::Tenuto => marks.tenuto,
            Mark::Marcato => marks.marcato,
        }
    }

    pub fn set(self, marks: &mut NoteMarks, on: bool) {
        match self {
            Mark::Staccato => marks.staccato = on,
            Mark::Accent => marks.accent = on,
            Mark::Tenuto => marks.tenuto = on,
            Mark::Marcato => marks.marcato = on,
        }
    }
}

/// A dynamic marking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dynamic {
    Ppp,
    Pp,
    P,
    Mp,
    Mf,
    F,
    Ff,
    Fff,
}

impl Dynamic {
    pub const ALL: [Dynamic; 8] = [
        Dynamic::Ppp,
        Dynamic::Pp,
        Dynamic::P,
        Dynamic::Mp,
        Dynamic::Mf,
        Dynamic::F,
        Dynamic::Ff,
        Dynamic::Fff,
    ];

    /// As written ("mf").
    pub fn name(self) -> &'static str {
        match self {
            Dynamic::Ppp => "ppp",
            Dynamic::Pp => "pp",
            Dynamic::P => "p",
            Dynamic::Mp => "mp",
            Dynamic::Mf => "mf",
            Dynamic::F => "f",
            Dynamic::Ff => "ff",
            Dynamic::Fff => "fff",
        }
    }

    pub fn of_name(name: &str) -> Option<Dynamic> {
        Self::ALL.into_iter().find(|d| d.name() == name)
    }

    /// The velocity notes marked so are played at.
    pub fn velocity(self) -> u8 {
        match self {
            Dynamic::Ppp => 16,
            Dynamic::Pp => 33,
            Dynamic::P => 49,
            Dynamic::Mp => 64,
            Dynamic::Mf => 80,
            Dynamic::F => 96,
            Dynamic::Ff => 112,
            Dynamic::Fff => 127,
        }
    }
}

/// How one note is written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteNotation {
    /// The voice it is written in (1 or 2; `None`: by the notes around
    /// it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<u8>,
    /// The letter it is spelt on (0 = C … 6 = B; `None`: as the key spells
    /// it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u8>,
    /// The syllable sung on it (a trailing `-`: the word goes on; `_`: held
    /// over the notes after it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyric: Option<String>,
    #[serde(default, skip_serializing_if = "NoteMarks::is_empty")]
    pub marks: NoteMarks,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<Dynamic>,
}

impl NoteNotation {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How a clip is written in the score.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipNotation {
    #[serde(default)]
    pub grid: ScoreGrid,
    /// A played note's short gap to the next is closed (a legato line
    /// reads as one).
    #[serde(default = "legato_default")]
    pub legato: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<NoteId, NoteNotation>,
}

fn legato_default() -> bool {
    true
}

impl Default for ClipNotation {
    fn default() -> Self {
        Self {
            grid: ScoreGrid::Auto,
            legato: true,
            notes: BTreeMap::new(),
        }
    }
}

impl ClipNotation {
    /// Nothing set: no entry is needed.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// A note's entry (the default for one without).
    pub fn note(&self, note: NoteId) -> NoteNotation {
        self.notes.get(&note).cloned().unwrap_or_default()
    }

    /// Change a note's entry (removed when it ends up empty).
    pub fn edit(&mut self, note: NoteId, f: impl FnOnce(&mut NoteNotation)) {
        let mut n = self.note(note);
        f(&mut n);
        if n.is_empty() {
            self.notes.remove(&note);
        } else {
            self.notes.insert(note, n);
        }
    }

    /// Sane values: voices 1 or 2, letters 0–6, no empty entries.
    pub fn tidy(&mut self) {
        for n in self.notes.values_mut() {
            n.voice = n.voice.filter(|v| (1..=2).contains(v));
            n.step = n.step.filter(|s| *s < 7);
            n.lyric = n.lyric.take().filter(|l| !l.trim().is_empty());
        }
        self.notes.retain(|_, n| !n.is_empty());
    }
}

impl Project {
    /// How a clip is written in the score (the default for one without an
    /// entry).
    pub fn clip_notation(&self, clip: ClipId) -> ClipNotation {
        self.notation.get(&clip).cloned().unwrap_or_default()
    }

    /// Drop the entries of clips the project has not got, and of notes
    /// their clips have not got (on opening).
    pub(crate) fn repair_notation(&mut self) -> Vec<String> {
        let before: usize = self.notation.values().map(|n| n.notes.len() + 1).sum();
        let clips = &self.clips;
        self.notation.retain(|id, n| {
            let Some(m) = clips.get(id).and_then(|c| c.as_midi()) else {
                return false;
            };
            n.notes
                .retain(|note, _| m.notes.iter().any(|x| x.id == *note));
            n.tidy();
            !n.is_default()
        });
        let after: usize = self.notation.values().map(|n| n.notes.len() + 1).sum();
        if after < before {
            vec![format!(
                "dropped {} score notation entr{} of clips or notes that are gone",
                before - after,
                if before - after == 1 { "y" } else { "ies" }
            )]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_kept_only_while_it_says_something() {
        let mut n = ClipNotation::default();
        assert!(n.is_default());
        n.edit(NoteId(5), |x| x.voice = Some(2));
        assert_eq!(n.note(NoteId(5)).voice, Some(2));
        n.edit(NoteId(5), |x| x.voice = None);
        assert!(n.notes.is_empty());
        n.edit(NoteId(6), |x| {
            x.voice = Some(7);
            x.step = Some(9);
            x.lyric = Some("  ".into());
        });
        n.tidy();
        assert!(n.notes.is_empty());
    }

    #[test]
    fn notation_reads_and_writes_plainly() {
        let mut n = ClipNotation {
            grid: ScoreGrid::Triplets,
            legato: false,
            ..ClipNotation::default()
        };
        n.edit(NoteId(3), |x| {
            x.dynamic = Some(Dynamic::Mf);
            x.marks.staccato = true;
            x.lyric = Some("la-".into());
        });
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains("\"mf\"") && json.contains("\"triplets\""));
        let back: ClipNotation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, n);
        // An entry from before any of it: the defaults.
        let old: ClipNotation = serde_json::from_str("{}").unwrap();
        assert!(old.is_default());
        assert_eq!(Dynamic::of_name("ff"), Some(Dynamic::Ff));
        assert!(
            Dynamic::ALL
                .windows(2)
                .all(|w| w[0].velocity() < w[1].velocity())
        );
    }
}

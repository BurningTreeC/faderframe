//! MusicXML scores read as music to play: each part's notes in quarters
//! from the score's start (tied notes joined, chords and voices placed by
//! `<backup>`/`<forward>`, tuplets by their real durations, transposing
//! parts at sounding pitch, unpitched percussion on its MIDI keys), the
//! bars with their time signatures, tempo and key changes, each part's
//! lyrics (by verse) and chord symbols.
//!
//! * Repeats are played out: forward/backward bar lines (`times`) and
//!   numbered endings; on the n-th pass a note sings verse n (else its
//!   first). Da capo, dal segno and codas are noted, not followed.
//! * A short first bar (a pickup) starts late in a whole bar, so bar lines
//!   stay where the music has them; any other bar that does not fill its
//!   time signature gets a signature of its own for that bar
//!   ([`Measure::time`]).
//! * Velocities come from dynamics (marks, `<sound dynamics>` and a
//!   note's own `dynamics`): `pppp` … `ffff` on MuseScore's scale, wedges
//!   ramping to the next mark, `sf`-like marks on their note only; mf (80)
//!   before any mark. Accents add to a note's velocity, staccato and its
//!   kin shorten it ([`Articulation`]).
//! * Grace and cue notes are left out (noted).
//!
//! [`read_file`] takes plain `.musicxml`/`.xml` and compressed `.mxl`
//! (its `META-INF/container.xml` names the score).

mod read;

pub use read::{read_bytes, read_file, read_mxl, read_xml};

use faderframe_midi::theory::{Chord, Key};

/// A bar as played (repeats played out): where it starts, how long it is
/// and the time signature it is counted in.
#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    /// Its number as the score writes it.
    pub number: String,
    /// Quarters from the score's start.
    pub start: f64,
    pub length: f64,
    /// The signature the bar is counted in: the score's, or one that
    /// fits a bar shorter or longer than it (when one does).
    pub time: (u8, u8),
    /// The time signature the score writes there.
    pub written: (u8, u8),
}

/// A tempo from `at` on (quarter notes a minute).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoAt {
    pub at: f64,
    pub bpm: f64,
}

/// A key from `at` on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyAt {
    pub at: f64,
    pub key: Key,
}

/// A note as it sounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub start: f64,
    pub length: f64,
    pub key: u8,
    pub velocity: u8,
    /// MIDI channel (0-based) when the part's instrument names one.
    pub channel: Option<u8>,
    pub voice: u8,
    pub staff: u8,
}

/// Where a syllable sits in its word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Syllabic {
    #[default]
    Single,
    Begin,
    Middle,
    End,
}

impl Syllabic {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "begin" => Syllabic::Begin,
            "middle" => Syllabic::Middle,
            "end" => Syllabic::End,
            _ => Syllabic::Single,
        }
    }

    /// Does a word go on after it?
    pub fn continues(self) -> bool {
        matches!(self, Syllabic::Begin | Syllabic::Middle)
    }
}

/// A syllable sung on a note.
#[derive(Clone, Debug, PartialEq)]
pub struct Syllable {
    pub at: f64,
    /// The end of its note, or of the last note it is held over
    /// (`extend`).
    pub end: f64,
    pub text: String,
    pub syllabic: Syllabic,
    pub extend: bool,
    /// The score ends a line (or a paragraph) after it.
    pub end_line: bool,
}

/// A chord symbol from `at` on (`None`: no chord, N.C.).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChordAt {
    pub at: f64,
    pub chord: Option<Chord>,
}

/// A part: an instrument's (or a voice's) notes, words and chords.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub id: String,
    pub name: String,
    pub abbreviation: String,
    pub notes: Vec<Note>,
    /// The verse sung (the first on single passes, the pass's on repeats).
    pub lyrics: Vec<Syllable>,
    pub chords: Vec<ChordAt>,
    /// MIDI channel (0-based) and program (0-based) its instrument names.
    pub channel: Option<u8>,
    pub program: Option<u8>,
    /// Unpitched (percussion) notes, or channel 10.
    pub percussion: bool,
    /// Staves (2: a grand staff).
    pub staves: u8,
    /// A tablature staff.
    pub tab: bool,
}

/// A line of words.
#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

impl Part {
    /// The part's words as lines: syllables joined into words (no
    /// hyphens), lines where the score ends them — or, when it never
    /// does, at rests of a beat or more and before a word that would take
    /// a line past two bars.
    pub fn lyric_lines(&self, measures: &[Measure]) -> Vec<LyricLine> {
        let marked = self.lyrics.iter().any(|s| s.end_line);
        let bar_at = |t: f64| {
            measures
                .iter()
                .rposition(|m| m.start <= t + 1e-9)
                .unwrap_or(0)
        };
        let mut out: Vec<LyricLine> = Vec::new();
        let mut line: Option<LyricLine> = None;
        let mut line_bar = 0;
        let mut joined = false;
        for (i, s) in self.lyrics.iter().enumerate() {
            let text = s.text.trim();
            if text.is_empty() {
                continue;
            }
            // Before a new word: does the line end here?
            if let Some(l) = &line
                && !joined
            {
                let gap = s.at - l.end;
                let long = bar_at(s.at) >= line_bar + 2;
                if !marked && (gap >= 1.0 - 1e-9 || long) {
                    out.extend(line.take());
                }
            }
            match &mut line {
                Some(l) => {
                    if !joined {
                        l.text.push(' ');
                    }
                    l.text.push_str(text);
                    l.end = l.end.max(s.end);
                }
                None => {
                    line_bar = bar_at(s.at);
                    line = Some(LyricLine {
                        start: s.at,
                        end: s.end,
                        text: text.to_string(),
                    });
                }
            }
            joined = s.syllabic.continues();
            let last = i + 1 == self.lyrics.len();
            if s.end_line || last {
                out.extend(line.take());
                joined = false;
            }
        }
        out.extend(line);
        out
    }
}

/// A score read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Score {
    pub title: String,
    pub composer: String,
    /// The bars as played.
    pub measures: Vec<Measure>,
    pub tempos: Vec<TempoAt>,
    pub keys: Vec<KeyAt>,
    pub parts: Vec<Part>,
    /// What was left out or approximated.
    pub notes: Vec<String>,
}

impl Score {
    /// Quarters to the end of the last bar.
    pub fn length(&self) -> f64 {
        self.measures.last().map_or(0.0, |m| m.start + m.length)
    }

    /// The bars where the time signature changes (bar index from 0,
    /// signature), the first included.
    pub fn meter_changes(&self) -> Vec<(usize, (u8, u8))> {
        let mut out: Vec<(usize, (u8, u8))> = Vec::new();
        for (i, m) in self.measures.iter().enumerate() {
            if out.last().is_none_or(|(_, t)| *t != m.time) {
                out.push((i, m.time));
            }
        }
        out
    }

    /// Chord symbols of every part over the whole score: each from its
    /// start to the next (or the end), N.C. ending the one before.
    pub fn chord_spans(&self) -> Vec<(f64, f64, Chord)> {
        let mut all: Vec<ChordAt> = self.parts.iter().flat_map(|p| p.chords.clone()).collect();
        all.sort_by(|a, b| a.at.total_cmp(&b.at));
        all.dedup_by(|b, a| (a.at - b.at).abs() < 1e-9);
        let end = self.length();
        let mut out = Vec::new();
        for (i, c) in all.iter().enumerate() {
            let next = all.get(i + 1).map_or(end, |n| n.at);
            if let Some(chord) = c.chord
                && next > c.at
            {
                out.push((c.at, next, chord));
            }
        }
        out
    }
}

/// How an articulation changes a note: its sounding share of the written
/// length and velocity added.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Articulation {
    pub length: f64,
    pub velocity: i32,
}

/// The articulations read and what they do.
pub const ARTICULATIONS: &[(&str, Articulation)] = &[
    (
        "staccato",
        Articulation {
            length: 0.5,
            velocity: 0,
        },
    ),
    (
        "staccatissimo",
        Articulation {
            length: 0.25,
            velocity: 0,
        },
    ),
    (
        "spiccato",
        Articulation {
            length: 0.4,
            velocity: 0,
        },
    ),
    (
        "detached-legato",
        Articulation {
            length: 0.75,
            velocity: 0,
        },
    ),
    (
        "tenuto",
        Articulation {
            length: 1.0,
            velocity: 4,
        },
    ),
    (
        "accent",
        Articulation {
            length: 1.0,
            velocity: 16,
        },
    ),
    (
        "strong-accent",
        Articulation {
            length: 0.8,
            velocity: 24,
        },
    ),
    (
        "stress",
        Articulation {
            length: 1.0,
            velocity: 10,
        },
    ),
    (
        "unstress",
        Articulation {
            length: 1.0,
            velocity: -10,
        },
    ),
];

/// MIDI velocities of dynamic marks (MuseScore's scale; `sf`-like marks
/// are accents of one note).
pub const DYNAMICS: &[(&str, u8)] = &[
    ("pppppp", 1),
    ("ppppp", 5),
    ("pppp", 10),
    ("ppp", 16),
    ("pp", 33),
    ("p", 49),
    ("mp", 64),
    ("mf", 80),
    ("f", 96),
    ("ff", 112),
    ("fff", 126),
    ("ffff", 127),
    ("fffff", 127),
    ("ffffff", 127),
    ("fp", 96),
    ("pf", 49),
    ("sf", 112),
    ("sfz", 112),
    ("sffz", 126),
    ("sfp", 112),
    ("sfpp", 112),
    ("sfzp", 112),
    ("fz", 112),
    ("rf", 104),
    ("rfz", 104),
    ("n", 1),
];

/// Marks that accent their note only (the level goes back after it).
pub(crate) const ACCENT_MARKS: &[&str] = &["sf", "sfz", "sffz", "fz", "rf", "rfz"];

/// The velocity before any dynamic (mf).
pub const DEFAULT_VELOCITY: u8 = 80;

#[cfg(test)]
mod tests;

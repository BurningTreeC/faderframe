//! Harmony: pitch classes, scales, keys and chords — what a project's key
//! and chord track say, what the piano roll highlights and snaps to, and
//! what MIDI effects follow. Pure functions on note numbers (60 = C4).

use serde::{Deserialize, Serialize};

/// Pitch class names with sharps and with flats.
const SHARPS: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
const FLATS: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

/// The pitch class (0 = C … 11 = B) of a note.
pub fn pitch_class(note: i32) -> u8 {
    note.rem_euclid(12) as u8
}

/// A pitch class's name, spelt with flats or sharps.
pub fn pc_name(pc: u8, flats: bool) -> &'static str {
    (if flats { FLATS } else { SHARPS })[usize::from(pc % 12)]
}

/// A pitch class from its name (`C`, `F#`, `Bb`, `eb`, `C♯`, `B♭`); the
/// rest of the text after it.
pub fn parse_pc(text: &str) -> Option<(u8, &str)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    let base: i32 = match first.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut rest = &text[first.len_utf8()..];
    let mut shift = 0;
    loop {
        if let Some(r) = rest.strip_prefix('#').or_else(|| rest.strip_prefix('♯')) {
            shift += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('♭') {
            shift -= 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('b') {
            // No quality starts with "b": after the letter it is a flat.
            shift -= 1;
            rest = r;
        } else {
            break;
        }
    }
    Some(((base + shift).rem_euclid(12) as u8, rest))
}

/// A scale (a mode, or a pentatonic/blues/symmetric one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scale {
    Major,
    Minor,
    HarmonicMinor,
    MelodicMinor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    WholeTone,
    Chromatic,
}

impl Scale {
    pub const ALL: [Scale; 14] = [
        Scale::Major,
        Scale::Minor,
        Scale::HarmonicMinor,
        Scale::MelodicMinor,
        Scale::Dorian,
        Scale::Phrygian,
        Scale::Lydian,
        Scale::Mixolydian,
        Scale::Locrian,
        Scale::MajorPentatonic,
        Scale::MinorPentatonic,
        Scale::Blues,
        Scale::WholeTone,
        Scale::Chromatic,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scale::Major => "Major",
            Scale::Minor => "Minor",
            Scale::HarmonicMinor => "Harmonic Minor",
            Scale::MelodicMinor => "Melodic Minor",
            Scale::Dorian => "Dorian",
            Scale::Phrygian => "Phrygian",
            Scale::Lydian => "Lydian",
            Scale::Mixolydian => "Mixolydian",
            Scale::Locrian => "Locrian",
            Scale::MajorPentatonic => "Major Pentatonic",
            Scale::MinorPentatonic => "Minor Pentatonic",
            Scale::Blues => "Blues",
            Scale::WholeTone => "Whole Tone",
            Scale::Chromatic => "Chromatic",
        }
    }

    /// Semitones of its degrees above the root.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Scale::Major => &[0, 2, 4, 5, 7, 9, 11],
            Scale::Minor => &[0, 2, 3, 5, 7, 8, 10],
            Scale::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            Scale::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            Scale::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Scale::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Scale::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            Scale::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            Scale::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            Scale::MajorPentatonic => &[0, 2, 4, 7, 9],
            Scale::MinorPentatonic => &[0, 3, 5, 7, 10],
            Scale::Blues => &[0, 3, 5, 6, 7, 10],
            Scale::WholeTone => &[0, 2, 4, 6, 8, 10],
            Scale::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        }
    }

    /// Whether its third is minor (for spelling and the name's case).
    pub fn is_minor(self) -> bool {
        self.intervals().contains(&3) && !self.intervals().contains(&4)
    }
}

/// A key: a tonic and a scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    pub root: u8,
    pub scale: Scale,
}

impl Key {
    pub fn new(root: u8, scale: Scale) -> Self {
        Self {
            root: root % 12,
            scale,
        }
    }

    /// Spelt with flats (F, B♭, E♭ … major; D, G, C … minor)?
    pub fn flats(self) -> bool {
        // The major key a scale shares its notes with decides.
        let ionian = match self.scale {
            Scale::Minor
            | Scale::HarmonicMinor
            | Scale::MelodicMinor
            | Scale::MinorPentatonic
            | Scale::Blues => (self.root + 3) % 12,
            Scale::Dorian => (self.root + 10) % 12,
            Scale::Phrygian => (self.root + 8) % 12,
            Scale::Lydian => (self.root + 7) % 12,
            Scale::Mixolydian => (self.root + 5) % 12,
            Scale::Locrian => (self.root + 1) % 12,
            _ => self.root,
        };
        matches!(ionian, 1 | 3 | 5 | 8 | 10)
    }

    /// "A Minor", "E♭ Major".
    pub fn name(self) -> String {
        format!("{} {}", pc_name(self.root, self.flats()), self.scale.name())
    }

    /// Parse "Am", "A minor", "F# dorian", "Eb", "C major pentatonic".
    pub fn parse(text: &str) -> Option<Key> {
        let (root, rest) = parse_pc(text.trim())?;
        let rest = rest.trim().to_ascii_lowercase();
        let scale = match rest.as_str() {
            "" | "maj" | "major" | "ionian" => Scale::Major,
            "m" | "min" | "minor" | "aeolian" => Scale::Minor,
            _ => *Scale::ALL
                .iter()
                .find(|s| s.name().to_ascii_lowercase() == rest)?,
        };
        Some(Key::new(root, scale))
    }

    pub fn contains(self, note: i32) -> bool {
        let pc = (pitch_class(note) + 12 - self.root) % 12;
        self.scale.intervals().contains(&pc)
    }

    /// The scale degree (0 = the tonic) of a note in the key.
    pub fn degree(self, note: i32) -> Option<usize> {
        let pc = (pitch_class(note) + 12 - self.root) % 12;
        self.scale.intervals().iter().position(|&i| i == pc)
    }

    /// The nearest note of the key (a tie goes down).
    pub fn snap(self, note: i32) -> i32 {
        (0..12)
            .flat_map(|d| [note - d, note + d])
            .find(|n| self.contains(*n))
            .unwrap_or(note)
    }

    /// The note `steps` scale steps from `note` (which is snapped first).
    pub fn step(self, note: i32, steps: i32) -> i32 {
        let n = self.scale.intervals().len() as i32;
        let start = self.snap(note);
        let Some(d) = self.degree(start) else {
            return note;
        };
        let target = d as i32 + steps;
        let octave = target.div_euclid(n);
        let degree = target.rem_euclid(n) as usize;
        let base = start - self.scale.intervals()[d] as i32;
        base + 12 * octave + self.scale.intervals()[degree] as i32
    }

    /// The diatonic chord on a degree (0 = I): stacked thirds of the scale
    /// (triads, or sevenths), named by what they are.
    pub fn chord_on(self, degree: usize, sevenths: bool) -> Option<Chord> {
        let iv = self.scale.intervals();
        if iv.len() != 7 {
            return None;
        }
        let note = |k: usize| i32::from(iv[(degree + k) % 7]) + 12 * ((degree + k) / 7) as i32;
        let root = note(0);
        let mut steps = vec![note(2) - root, note(4) - root];
        if sevenths {
            steps.push(note(6) - root);
        }
        let quality = Quality::ALL.iter().copied().find(|q| {
            q.intervals()[1..] == steps.iter().map(|s| *s as u8).collect::<Vec<_>>()[..]
        })?;
        Some(Chord::new((self.root + iv[degree % 7]) % 12, quality))
    }
}

/// A chord's quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Quality {
    Major,
    Minor,
    Diminished,
    Augmented,
    Sus2,
    Sus4,
    Power,
    Major6,
    Minor6,
    Dominant7,
    Major7,
    Minor7,
    HalfDiminished7,
    Diminished7,
    MinorMajor7,
    Augmented7,
    Add9,
    MinorAdd9,
    Dominant9,
    Major9,
    Minor9,
    Seven4,
    Dominant11,
    Dominant13,
}

impl Quality {
    pub const ALL: [Quality; 24] = [
        Quality::Major,
        Quality::Minor,
        Quality::Diminished,
        Quality::Augmented,
        Quality::Sus2,
        Quality::Sus4,
        Quality::Power,
        Quality::Major6,
        Quality::Minor6,
        Quality::Dominant7,
        Quality::Major7,
        Quality::Minor7,
        Quality::HalfDiminished7,
        Quality::Diminished7,
        Quality::MinorMajor7,
        Quality::Augmented7,
        Quality::Add9,
        Quality::MinorAdd9,
        Quality::Dominant9,
        Quality::Major9,
        Quality::Minor9,
        Quality::Seven4,
        Quality::Dominant11,
        Quality::Dominant13,
    ];

    /// Semitones above the root, root first.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Quality::Major => &[0, 4, 7],
            Quality::Minor => &[0, 3, 7],
            Quality::Diminished => &[0, 3, 6],
            Quality::Augmented => &[0, 4, 8],
            Quality::Sus2 => &[0, 2, 7],
            Quality::Sus4 => &[0, 5, 7],
            Quality::Power => &[0, 7],
            Quality::Major6 => &[0, 4, 7, 9],
            Quality::Minor6 => &[0, 3, 7, 9],
            Quality::Dominant7 => &[0, 4, 7, 10],
            Quality::Major7 => &[0, 4, 7, 11],
            Quality::Minor7 => &[0, 3, 7, 10],
            Quality::HalfDiminished7 => &[0, 3, 6, 10],
            Quality::Diminished7 => &[0, 3, 6, 9],
            Quality::MinorMajor7 => &[0, 3, 7, 11],
            Quality::Augmented7 => &[0, 4, 8, 10],
            Quality::Add9 => &[0, 4, 7, 14],
            Quality::MinorAdd9 => &[0, 3, 7, 14],
            Quality::Dominant9 => &[0, 4, 7, 10, 14],
            Quality::Major9 => &[0, 4, 7, 11, 14],
            Quality::Minor9 => &[0, 3, 7, 10, 14],
            Quality::Seven4 => &[0, 5, 7, 10],
            Quality::Dominant11 => &[0, 4, 7, 10, 14, 17],
            Quality::Dominant13 => &[0, 4, 7, 10, 14, 21],
        }
    }

    /// The symbol after the root ("" for major, "m7", "maj7", "m7♭5" …).
    pub fn symbol(self) -> &'static str {
        match self {
            Quality::Major => "",
            Quality::Minor => "m",
            Quality::Diminished => "dim",
            Quality::Augmented => "aug",
            Quality::Sus2 => "sus2",
            Quality::Sus4 => "sus4",
            Quality::Power => "5",
            Quality::Major6 => "6",
            Quality::Minor6 => "m6",
            Quality::Dominant7 => "7",
            Quality::Major7 => "maj7",
            Quality::Minor7 => "m7",
            Quality::HalfDiminished7 => "m7b5",
            Quality::Diminished7 => "dim7",
            Quality::MinorMajor7 => "mMaj7",
            Quality::Augmented7 => "aug7",
            Quality::Add9 => "add9",
            Quality::MinorAdd9 => "madd9",
            Quality::Dominant9 => "9",
            Quality::Major9 => "maj9",
            Quality::Minor9 => "m9",
            Quality::Seven4 => "7sus4",
            Quality::Dominant11 => "11",
            Quality::Dominant13 => "13",
        }
    }

    /// Other spellings people type.
    fn aliases(self) -> &'static [&'static str] {
        match self {
            Quality::Major => &["maj", "M", "major"],
            Quality::Minor => &["min", "-", "minor"],
            Quality::Diminished => &["°", "o", "dim"],
            Quality::Augmented => &["+", "aug"],
            Quality::Sus4 => &["sus"],
            Quality::Major7 => &["M7", "Δ", "Δ7", "ma7"],
            Quality::Minor7 => &["min7", "-7"],
            Quality::HalfDiminished7 => &["ø", "ø7", "m7♭5", "-7b5"],
            Quality::Diminished7 => &["°7", "o7"],
            Quality::MinorMajor7 => &["mM7", "m(maj7)", "minmaj7"],
            Quality::Augmented7 => &["+7", "7#5"],
            Quality::Major9 => &["M9", "Δ9"],
            Quality::Minor9 => &["min9", "-9"],
            Quality::Minor6 => &["min6", "-6"],
            _ => &[],
        }
    }
}

/// A chord: a root, a quality and perhaps another bass note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chord {
    pub root: u8,
    pub quality: Quality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bass: Option<u8>,
}

impl Chord {
    pub fn new(root: u8, quality: Quality) -> Self {
        Self {
            root: root % 12,
            quality,
            bass: None,
        }
    }

    pub fn with_bass(self, bass: Option<u8>) -> Self {
        Self {
            bass: bass.map(|b| b % 12).filter(|b| *b != self.root),
            ..self
        }
    }

    /// "Am7", "F/A", "B♭maj7" (flats when asked).
    pub fn name(self, flats: bool) -> String {
        let mut s = format!("{}{}", pc_name(self.root, flats), self.quality.symbol());
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(pc_name(b, flats));
        }
        s
    }

    /// Parse a chord symbol ("Am7", "F#m7b5", "Bb/D", "Cmaj7", "G7sus4").
    pub fn parse(text: &str) -> Option<Chord> {
        let text = text.trim();
        let (body, bass) = match text.rsplit_once('/') {
            Some((b, s)) if parse_pc(s).is_some_and(|(_, r)| r.is_empty()) => {
                (b, parse_pc(s).map(|(p, _)| p))
            }
            _ => (text, None),
        };
        let (root, rest) = parse_pc(body)?;
        let quality = Quality::ALL
            .iter()
            .copied()
            .find(|q| q.symbol() == rest || q.aliases().contains(&rest))
            .or_else(|| {
                let lower = rest.to_ascii_lowercase();
                Quality::ALL
                    .iter()
                    .copied()
                    .find(|q| q.symbol().to_ascii_lowercase() == lower && !q.symbol().is_empty())
            })?;
        Some(Chord::new(root, quality).with_bass(bass))
    }

    pub fn pitch_classes(self) -> impl Iterator<Item = u8> {
        self.quality
            .intervals()
            .iter()
            .map(move |i| (self.root + i % 12) % 12)
            .chain(self.bass)
    }

    pub fn contains(self, note: i32) -> bool {
        self.pitch_classes().any(|pc| pc == pitch_class(note))
    }

    /// Its notes in close position from the root nearest `around` (the
    /// bass, if any, an octave below the root).
    pub fn voicing(self, around: i32) -> Vec<i32> {
        let r = i32::from(self.root);
        let root = around - (around - r).rem_euclid(12)
            + if (around - r).rem_euclid(12) > 6 {
                12
            } else {
                0
            };
        let mut notes: Vec<i32> = self
            .quality
            .intervals()
            .iter()
            .map(|i| root + i32::from(*i))
            .collect();
        if let Some(b) = self.bass {
            let b = i32::from(b);
            let below = root - 12 + (b - root + 12).rem_euclid(12);
            notes.insert(0, if below >= root { below - 12 } else { below });
        }
        notes
    }

    /// The chord these notes make, if they make one (the lowest note is
    /// the bass; inversions are named over it, "C/E").
    pub fn recognise(notes: &[i32]) -> Option<Chord> {
        let lowest = *notes.iter().min()?;
        let mut pcs: Vec<u8> = notes.iter().map(|n| pitch_class(*n)).collect();
        pcs.sort_unstable();
        pcs.dedup();
        if pcs.len() < 2 {
            return None;
        }
        let bass = pitch_class(lowest);
        // Prefer the root in the bass, then the simplest quality.
        let mut best: Option<(usize, Chord)> = None;
        for &root in &pcs {
            for (rank, q) in Quality::ALL.iter().enumerate() {
                let mut chord_pcs: Vec<u8> =
                    q.intervals().iter().map(|i| (root + i % 12) % 12).collect();
                chord_pcs.sort_unstable();
                chord_pcs.dedup();
                if chord_pcs == pcs {
                    let cost = rank + if root == bass { 0 } else { 100 };
                    if best.is_none_or(|(c, _)| cost < c) {
                        best = Some((cost, Chord::new(root, *q).with_bass(Some(bass))));
                    }
                }
            }
        }
        best.map(|(_, c)| c)
    }

    /// The Roman numeral of this chord in `key` ("vi", "V7", "♭VII"), if
    /// its root is in the key.
    pub fn numeral(self, key: Key) -> Option<String> {
        const NUMERALS: [&str; 7] = ["I", "II", "III", "IV", "V", "VI", "VII"];
        let d = key.degree(i32::from(self.root))?;
        if key.scale.intervals().len() != 7 {
            return None;
        }
        let minor = matches!(
            self.quality,
            Quality::Minor
                | Quality::Minor6
                | Quality::Minor7
                | Quality::Minor9
                | Quality::MinorAdd9
                | Quality::MinorMajor7
                | Quality::Diminished
                | Quality::HalfDiminished7
                | Quality::Diminished7
        );
        let mut s = NUMERALS[d].to_string();
        if minor {
            s = s.to_lowercase();
        }
        s.push_str(match self.quality {
            Quality::Major | Quality::Minor => "",
            Quality::Diminished => "°",
            Quality::HalfDiminished7 => "ø7",
            Quality::Diminished7 => "°7",
            Quality::Minor7 | Quality::Dominant7 => "7",
            q => q.symbol().trim_start_matches('m'),
        });
        Some(s)
    }
}

/// The key a pitch-class histogram (durations, weighted as wanted) most
/// likely is in: the Krumhansl–Kessler profiles of major and minor keys
/// correlated with it, the best of the 24 (`None` for silence).
pub fn detect_key(histogram: &[f64; 12]) -> Option<Key> {
    const MAJOR: [f64; 12] = [
        6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
    ];
    const MINOR: [f64; 12] = [
        6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
    ];
    if histogram.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    let corr = |profile: &[f64; 12], root: usize| {
        let x: Vec<f64> = (0..12).map(|i| histogram[(i + root) % 12]).collect();
        let (mx, my) = (
            x.iter().sum::<f64>() / 12.0,
            profile.iter().sum::<f64>() / 12.0,
        );
        let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
        for i in 0..12 {
            num += (x[i] - mx) * (profile[i] - my);
            dx += (x[i] - mx).powi(2);
            dy += (profile[i] - my).powi(2);
        }
        num / (dx * dy).sqrt().max(1e-12)
    };
    let mut best = (f64::MIN, Key::new(0, Scale::Major));
    for root in 0..12 {
        for (profile, scale) in [(&MAJOR, Scale::Major), (&MINOR, Scale::Minor)] {
            let r = corr(profile, root);
            if r > best.0 {
                best = (r, Key::new(root as u8, scale));
            }
        }
    }
    Some(best.1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn keys_contain_snap_and_step() {
        let a_minor = Key::parse("Am").unwrap();
        assert_eq!(a_minor, Key::new(9, Scale::Minor));
        assert_eq!(a_minor.name(), "A Minor");
        assert!(a_minor.contains(60) && !a_minor.contains(61));
        assert_eq!(a_minor.snap(61), 60, "a tie goes down");
        assert_eq!(a_minor.snap(66), 65);
        assert_eq!(a_minor.step(57, 2), 60, "A up two steps is C");
        assert_eq!(a_minor.step(57, -1), 55);
        assert_eq!(a_minor.step(57, 7), 69, "an octave");
        assert_eq!(Key::parse("eb major").unwrap().name(), "Eb Major");
        assert_eq!(Key::parse("F# dorian").unwrap(), Key::new(6, Scale::Dorian));
        assert!(Key::new(5, Scale::Major).flats());
        assert!(!Key::new(4, Scale::Minor).flats());
        assert!(Key::new(2, Scale::Minor).flats(), "D minor: one flat");
    }

    #[test]
    fn diatonic_chords_and_numerals() {
        let c = Key::new(0, Scale::Major);
        let names: Vec<String> = (0..7)
            .map(|d| c.chord_on(d, false).unwrap().name(false))
            .collect();
        assert_eq!(names, ["C", "Dm", "Em", "F", "G", "Am", "Bdim"]);
        let sevenths: Vec<String> = (0..7)
            .map(|d| c.chord_on(d, true).unwrap().name(false))
            .collect();
        assert_eq!(
            sevenths,
            ["Cmaj7", "Dm7", "Em7", "Fmaj7", "G7", "Am7", "Bm7b5"]
        );
        assert_eq!(Chord::parse("Am").unwrap().numeral(c).unwrap(), "vi");
        assert_eq!(Chord::parse("G7").unwrap().numeral(c).unwrap(), "V7");
        assert_eq!(c.chord_on(6, false).unwrap().numeral(c).unwrap(), "vii°");
        assert!(
            Key::new(0, Scale::MajorPentatonic)
                .chord_on(0, false)
                .is_none()
        );
    }

    #[test]
    fn chords_parse_name_voice_and_are_recognised() {
        for (s, flats) in [
            ("C", false),
            ("Am7", false),
            ("F#m7b5", false),
            ("Bbmaj7", true),
            ("G7sus4", false),
            ("D/F#", false),
            ("Ebm", true),
            ("C5", false),
            ("Cadd9", false),
            ("Abdim7", true),
        ] {
            assert_eq!(Chord::parse(s).unwrap().name(flats), s, "{s}");
        }
        assert_eq!(Chord::parse("CΔ7").unwrap().quality, Quality::Major7);
        assert_eq!(Chord::parse("A-7").unwrap().quality, Quality::Minor7);
        assert!(Chord::parse("H7").is_none());
        // Close position round middle C; the bass below.
        assert_eq!(Chord::parse("Am").unwrap().voicing(60), vec![57, 60, 64]);
        assert_eq!(Chord::parse("G").unwrap().voicing(60), vec![55, 59, 62]);
        assert_eq!(
            Chord::parse("C/E").unwrap().voicing(60),
            vec![52, 60, 64, 67]
        );
        // Recognised from notes, inversions over their bass.
        assert_eq!(Chord::recognise(&[57, 60, 64]).unwrap().name(false), "Am");
        assert_eq!(Chord::recognise(&[52, 60, 67]).unwrap().name(false), "C/E");
        assert_eq!(
            Chord::recognise(&[43, 59, 62, 65]).unwrap().name(false),
            "G7"
        );
        assert!(Chord::recognise(&[60]).is_none());
        // A G major scale with the tonic held longest reads G major; an A
        // minor arpeggio, A minor.
        let mut h = [0.0; 12];
        for (pc, w) in [
            (7, 4.0),
            (9, 1.0),
            (11, 2.0),
            (0, 1.0),
            (2, 3.0),
            (4, 1.0),
            (6, 1.0),
        ] {
            h[pc] = w;
        }
        assert_eq!(detect_key(&h), Some(Key::new(7, Scale::Major)));
        let mut h = [0.0; 12];
        for (pc, w) in [(9, 4.0), (0, 3.0), (4, 3.0), (2, 1.0), (7, 1.0), (5, 1.0)] {
            h[pc] = w;
        }
        assert_eq!(detect_key(&h), Some(Key::new(9, Scale::Minor)));
        assert_eq!(detect_key(&[0.0; 12]), None);
        assert!(Chord::recognise(&[60, 61, 62]).is_none());
    }
}

//! What a lead sheet says: bars of notes and rests with their values, ties,
//! beams and words, the chord symbols over them, the key, the clef, the
//! metre and the tempo. Both the MusicXML writer and the engraver read
//! this, so they agree note for note.

/// Time inside a bar, in divisions of a quarter: 12 to the quarter (a
/// sixteenth is 3, a triplet eighth 4).
pub const DIVISIONS: u32 = 12;

#[derive(Clone, Debug, PartialEq)]
pub struct LeadSheet {
    pub title: String,
    /// Under the title on the right (empty: none).
    pub composer: String,
    pub key: KeySig,
    pub clef: Clef,
    /// Quarter notes a minute at the start.
    pub tempo: Option<f64>,
    pub measures: Vec<Measure>,
}

/// A key signature: sharps (> 0) or flats (< 0), and whether minor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySig {
    pub fifths: i8,
    pub minor: bool,
}

impl KeySig {
    pub const C: KeySig = KeySig {
        fifths: 0,
        minor: false,
    };

    /// The key signature's alteration of a letter (0 = C … 6 = B).
    pub fn alter_of(self, step: u8) -> i8 {
        // Sharps in order F C G D A E B, flats the reverse.
        const SHARPS: [u8; 7] = [3, 0, 4, 1, 5, 2, 6];
        let n = self.fifths.unsigned_abs().min(7) as usize;
        if self.fifths > 0 && SHARPS[..n].contains(&step) {
            1
        } else if self.fifths < 0 && SHARPS[7 - n..].contains(&step) {
            -1
        } else {
            0
        }
    }

    /// Whether chromatic notes are better spelt with flats.
    pub fn flats(self) -> bool {
        self.fifths < 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clef {
    Treble,
    /// Treble sounding an octave lower (a tenor's or a guitar's part).
    Treble8vb,
    Bass,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    /// Its time signature (numerator, denominator).
    pub time: (u8, u8),
    /// The signature is shown (the first bar, or a change).
    pub show_time: bool,
    pub events: Vec<Event>,
    pub chords: Vec<ChordSymbol>,
}

impl Measure {
    /// Its length in divisions.
    pub fn length(&self) -> u32 {
        bar_length(self.time)
    }
}

/// A bar's length in divisions.
pub fn bar_length((num, den): (u8, u8)) -> u32 {
    u32::from(num) * 4 * DIVISIONS / u32::from(den.max(1))
}

/// A note's or a rest's written value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Value {
    Whole,
    Half,
    Quarter,
    Eighth,
    Sixteenth,
}

impl Value {
    pub fn divisions(self) -> u32 {
        match self {
            Value::Whole => 4 * DIVISIONS,
            Value::Half => 2 * DIVISIONS,
            Value::Quarter => DIVISIONS,
            Value::Eighth => DIVISIONS / 2,
            Value::Sixteenth => DIVISIONS / 4,
        }
    }

    /// MusicXML's name.
    pub fn name(self) -> &'static str {
        match self {
            Value::Whole => "whole",
            Value::Half => "half",
            Value::Quarter => "quarter",
            Value::Eighth => "eighth",
            Value::Sixteenth => "16th",
        }
    }

    /// Beams it takes (an eighth one, a sixteenth two).
    pub fn beams(self) -> u8 {
        match self {
            Value::Eighth => 1,
            Value::Sixteenth => 2,
            _ => 0,
        }
    }
}

/// A value, dotted or not, and whether a triplet's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Duration {
    pub value: Value,
    pub dots: u8,
    pub triplet: bool,
}

impl Duration {
    pub const fn plain(value: Value) -> Self {
        Self {
            value,
            dots: 0,
            triplet: false,
        }
    }

    pub fn divisions(self) -> u32 {
        let base = self.value.divisions();
        let full = match self.dots {
            0 => base,
            1 => base * 3 / 2,
            _ => base * 7 / 4,
        };
        if self.triplet { full * 2 / 3 } else { full }
    }
}

/// A written pitch: its letter (0 = C … 6 = B), alteration and octave
/// (4 from middle C up), the accidental shown in front of it (if any) and
/// the sounding key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pitch {
    pub step: u8,
    pub alter: i8,
    pub octave: i8,
    pub accidental: Option<i8>,
    pub midi: u8,
}

impl Pitch {
    pub const STEPS: [&'static str; 7] = ["C", "D", "E", "F", "G", "A", "B"];

    /// Diatonic steps from C0.
    pub fn diatonic(self) -> i32 {
        i32::from(self.step) + 7 * i32::from(self.octave)
    }
}

/// A beam's part at one beam level (MusicXML's words).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beam {
    Begin,
    Continue,
    End,
    /// A short beam pointing forward or back (a lone sixteenth's second).
    ForwardHook,
    BackwardHook,
}

impl Beam {
    pub fn name(self) -> &'static str {
        match self {
            Beam::Begin => "begin",
            Beam::Continue => "continue",
            Beam::End => "end",
            Beam::ForwardHook => "forward hook",
            Beam::BackwardHook => "backward hook",
        }
    }
}

/// Where a note stands in a triplet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tuplet {
    Start,
    Middle,
    Stop,
}

/// A word (or a syllable) under a note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lyric {
    pub text: String,
    /// The word is held over the next notes (a melisma's line).
    pub extend: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Where it starts in the bar (divisions).
    pub at: u32,
    pub duration: Duration,
    /// None: a rest.
    pub pitch: Option<Pitch>,
    /// Tied to the next note.
    pub tie: bool,
    /// Tied from the one before (the same pitch carried on).
    pub tied_from: bool,
    /// A whole bar's rest (drawn in the middle of the bar).
    pub bar_rest: bool,
    /// Its beams, level 1 then 2.
    pub beams: [Option<Beam>; 2],
    pub tuplet: Option<Tuplet>,
    pub lyric: Option<Lyric>,
    /// A melisma's line runs under it (the word started earlier).
    pub held: bool,
}

impl Event {
    pub fn end(&self) -> u32 {
        self.at + self.duration.divisions()
    }

    pub fn is_rest(&self) -> bool {
        self.pitch.is_none()
    }
}

/// A chord symbol over the bar at `at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordSymbol {
    pub at: u32,
    pub root: (u8, i8),
    pub bass: Option<(u8, i8)>,
    /// MusicXML's kind ("major", "minor-seventh", …).
    pub kind: &'static str,
    /// The kind as written after the root ("m7", "maj7", "").
    pub suffix: String,
    /// Degrees added (MusicXML `degree`: value, alteration, "add" or
    /// "subtract").
    pub degrees: Vec<(u8, i8, &'static str)>,
}

impl ChordSymbol {
    /// The symbol as text, accidentals as ♭ and ♯ ("B♭m7", "F/A").
    pub fn text(&self) -> String {
        let name = |(step, alter): (u8, i8)| {
            let acc = match alter {
                -2 => "𝄫",
                -1 => "♭",
                1 => "♯",
                2 => "𝄪",
                _ => "",
            };
            format!("{}{acc}", Pitch::STEPS[usize::from(step % 7)])
        };
        let mut s = format!("{}{}", name(self.root), self.suffix);
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(&name(b));
        }
        s
    }
}

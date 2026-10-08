//! From a melody (notes in quarters), the bars it sits in, its chords and
//! its words to a [`LeadSheet`]:
//!
//! 1. One voice: overlapping notes end where the next begins.
//! 2. Quantised per beat to sixteenths or eighth triplets (whichever fits
//!    the beat's onsets better; triplets only in simple metres), short gaps
//!    closed (a sung line is legato on paper).
//! 3. Cut into bars (ties across bar lines), gaps filled with rests, and
//!    every note and rest split into values a reader expects: nothing
//!    crosses a beat unless it starts on one, nothing crosses the middle of
//!    a 4/4 bar unless it starts the bar.
//! 4. Spelt in the key (accidentals once a bar), beamed by the beat,
//!    triplets marked.
//! 5. Words laid under the notes line by line (aligned by time; a word
//!    over several notes draws its line), chord symbols on the nearest
//!    beat.

use crate::score::{
    Beam, ChordSymbol, Clef, DIVISIONS, Duration, Event, KeySig, LeadSheet, Lyric, Measure, Pitch,
    Tuplet, Value, bar_length,
};
use faderframe_midi::theory::{Chord, Key, Quality};

/// A bar to write: where it starts (quarters) and its time signature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    pub start: f64,
    pub time: (u8, u8),
}

/// A melody note (quarters, on the bars' clock).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub start: f64,
    pub end: f64,
    pub key: u8,
}

/// A chord from `start` on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChordAt {
    pub start: f64,
    pub chord: Chord,
}

/// A line of words over `start..end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// How rhythms are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Grid {
    /// Sixteenths, or eighth triplets in beats that swing that way.
    #[default]
    Auto,
    Straight,
    Triplets,
}

#[derive(Clone, Debug, Default)]
pub struct Input {
    pub title: String,
    pub composer: String,
    /// None: found from the notes.
    pub key: Option<Key>,
    pub bars: Vec<Bar>,
    pub tempo: Option<f64>,
    pub notes: Vec<Note>,
    pub chords: Vec<ChordAt>,
    pub lines: Vec<Line>,
    pub grid: Grid,
}

/// The beat a metre counts in (divisions) and whether its beats take
/// triplets.
fn beat_of((num, den): (u8, u8)) -> (u32, bool) {
    match den {
        8 if num % 3 == 0 && num > 3 => (DIVISIONS * 3 / 2, false),
        8 | 16 => (DIVISIONS / 2, false),
        2 => (DIVISIONS, true),
        _ => (DIVISIONS, true),
    }
}

/// The key signature of a key.
pub fn key_signature(key: Key) -> KeySig {
    // The major key sharing its notes, by its tonic's pitch class.
    let minor = key.scale.is_minor();
    let ionian = if minor { (key.root + 3) % 12 } else { key.root };
    let fifths = match ionian {
        0 => 0,
        7 => 1,
        2 => 2,
        9 => 3,
        4 => 4,
        11 => 5,
        6 if key.flats() => -6,
        6 => 6,
        1 if key.flats() => -5,
        1 => 7,
        5 => -1,
        10 => -2,
        3 => -3,
        8 => -4,
        _ => 0,
    };
    KeySig { fifths, minor }
}

/// Spell `midi` in `key` (sounding pitch): the key's own notes as the key
/// has them, others sharp or flat as the key leans (in C: C♯, E♭, F♯, G♯,
/// B♭).
pub fn spell(midi: u8, key: KeySig) -> (u8, i8, i8) {
    const NATURAL: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
    let pc = midi % 12;
    let pick = |step: u8, alter: i8| {
        let octave = (i32::from(midi) - i32::from(alter) - i32::from(NATURAL[usize::from(step)]))
            .div_euclid(12)
            - 1;
        (step, alter, octave as i8)
    };
    // In the key.
    for step in 0..7u8 {
        let alter = key.alter_of(step);
        if (i32::from(NATURAL[usize::from(step)]) + i32::from(alter)).rem_euclid(12)
            == i32::from(pc)
        {
            return pick(step, alter);
        }
    }
    // A natural against the key.
    if let Some(step) = NATURAL.iter().position(|n| *n == pc) {
        return pick(step as u8, 0);
    }
    let flats = if key.fifths == 0 {
        matches!(pc, 3 | 10)
    } else {
        key.flats()
    };
    if flats {
        let step = NATURAL
            .iter()
            .position(|n| *n == (pc + 1) % 12)
            .unwrap_or(0);
        pick(step as u8, -1)
    } else {
        let step = NATURAL
            .iter()
            .position(|n| (*n + 1) % 12 == pc)
            .unwrap_or(0);
        pick(step as u8, 1)
    }
}

/// MusicXML's kind of a chord quality, how it is written after the root,
/// and degrees beyond the kind.
fn kind_of(q: Quality) -> (&'static str, Vec<(u8, i8, &'static str)>) {
    match q {
        Quality::Major => ("major", vec![]),
        Quality::Minor => ("minor", vec![]),
        Quality::Diminished => ("diminished", vec![]),
        Quality::Augmented => ("augmented", vec![]),
        Quality::Sus2 => ("suspended-second", vec![]),
        Quality::Sus4 => ("suspended-fourth", vec![]),
        Quality::Power => ("power", vec![]),
        Quality::Major6 => ("major-sixth", vec![]),
        Quality::Minor6 => ("minor-sixth", vec![]),
        Quality::Dominant7 => ("dominant", vec![]),
        Quality::Major7 => ("major-seventh", vec![]),
        Quality::Minor7 => ("minor-seventh", vec![]),
        Quality::HalfDiminished7 => ("half-diminished", vec![]),
        Quality::Diminished7 => ("diminished-seventh", vec![]),
        Quality::MinorMajor7 => ("major-minor", vec![]),
        Quality::Augmented7 => ("augmented-seventh", vec![]),
        Quality::Add9 => ("major", vec![(9, 0, "add")]),
        Quality::MinorAdd9 => ("minor", vec![(9, 0, "add")]),
        Quality::Dominant9 => ("dominant-ninth", vec![]),
        Quality::Major9 => ("major-ninth", vec![]),
        Quality::Minor9 => ("minor-ninth", vec![]),
        Quality::Seven4 => ("suspended-fourth", vec![(7, -1, "add")]),
        Quality::Dominant11 => ("dominant-11th", vec![]),
        Quality::Dominant13 => ("dominant-13th", vec![]),
    }
}

/// A chord's symbol at `at`, spelt in `key`.
pub fn chord_symbol(chord: Chord, at: u32, key: KeySig) -> ChordSymbol {
    let letter = |pc: u8| {
        let (step, alter, _) = spell(60 + pc, key);
        (step, alter)
    };
    let (kind, degrees) = kind_of(chord.quality);
    ChordSymbol {
        at,
        root: letter(chord.root),
        bass: chord.bass.map(letter),
        kind,
        suffix: chord.quality.symbol().replace('b', "♭").replace('#', "♯"),
        degrees,
    }
}

/// The values a note or rest of `len` divisions starting at `pos` in a bar
/// is written with (tied in order).
pub fn split(pos: u32, len: u32, time: (u8, u8), triplet_beats: &[bool]) -> Vec<Duration> {
    let bar = bar_length(time);
    let (beat, _) = beat_of(time);
    let compound = beat == DIVISIONS * 3 / 2;
    let small = beat == DIVISIONS / 2;
    let straight: [(u32, Duration); 8] = [
        (48, Duration::plain(Value::Whole)),
        (36, dotted(Value::Half)),
        (24, Duration::plain(Value::Half)),
        (18, dotted(Value::Quarter)),
        (12, Duration::plain(Value::Quarter)),
        (9, dotted(Value::Eighth)),
        (6, Duration::plain(Value::Eighth)),
        (3, Duration::plain(Value::Sixteenth)),
    ];
    let triplet: [(u32, Duration); 3] = [
        (12, Duration::plain(Value::Quarter)),
        (8, trip(Value::Quarter)),
        (4, trip(Value::Eighth)),
    ];
    let mut out = Vec::new();
    let (mut p, mut left) = (pos, len);
    while left > 0 {
        let in_beat = p % beat;
        let b = (p / beat) as usize;
        let swung = !compound && !small && triplet_beats.get(b).copied().unwrap_or(false);
        let fits = |v: u32| -> bool {
            if v > left || p + v > bar {
                return false;
            }
            // Nothing crosses the middle of a 4/4 bar unless it starts it.
            if bar == 4 * DIVISIONS && p < bar / 2 && p + v > bar / 2 && p != 0 {
                return false;
            }
            if v >= beat {
                in_beat == 0 && v.is_multiple_of(beat)
                    || in_beat == 0 && !compound && v == DIVISIONS * 3 / 2 && beat == DIVISIONS
            } else {
                in_beat + v <= beat
            }
        };
        let choice = if swung {
            triplet
                .iter()
                .find(|(v, _)| fits(*v) && p.is_multiple_of(4))
                .copied()
                .unwrap_or((4.min(left), trip(Value::Eighth)))
        } else {
            straight
                .iter()
                .find(|(v, _)| fits(*v) && aligned(in_beat, *v))
                .copied()
                .unwrap_or((3.min(left), Duration::plain(Value::Sixteenth)))
        };
        out.push(choice.1);
        p += choice.0;
        left -= choice.0.min(left);
    }
    out
}

/// Short values start where a reader looks for them (a dotted eighth on
/// the beat or its second sixteenth, an eighth on any sixteenth).
fn aligned(in_beat: u32, _v: u32) -> bool {
    in_beat.is_multiple_of(3)
}

const fn dotted(value: Value) -> Duration {
    Duration {
        value,
        dots: 1,
        triplet: false,
    }
}

const fn trip(value: Value) -> Duration {
    Duration {
        value,
        dots: 0,
        triplet: true,
    }
}

/// Bars with their offsets (divisions from the first bar's start).
struct Grid2 {
    bars: Vec<(Bar, u32, u32)>,
}

impl Grid2 {
    fn new(bars: &[Bar]) -> Self {
        let mut off = 0;
        let bars = bars
            .iter()
            .map(|b| {
                let len = bar_length(b.time);
                let r = (*b, off, len);
                off += len;
                r
            })
            .collect();
        Self { bars }
    }

    fn total(&self) -> u32 {
        self.bars.last().map_or(0, |(_, o, l)| o + l)
    }

    /// The bar `q` (quarters) falls in and its position there (divisions,
    /// unrounded); before the first bar: the first, after the last: the
    /// last's end.
    fn place(&self, q: f64) -> (usize, f64) {
        let Some(first) = self.bars.first() else {
            return (0, 0.0);
        };
        if q <= first.0.start {
            return (0, 0.0);
        }
        for (i, (b, _, len)) in self.bars.iter().enumerate() {
            let p = (q - b.start) * f64::from(DIVISIONS);
            if p < f64::from(*len) {
                return (i, p.max(0.0));
            }
        }
        let last = self.bars.len() - 1;
        (last, f64::from(self.bars[last].2))
    }
}

/// Build the lead sheet.
pub fn build(input: &Input) -> LeadSheet {
    let grid = Grid2::new(&input.bars);
    // 1. One voice.
    let mut notes: Vec<Note> = input
        .notes
        .iter()
        .copied()
        .filter(|n| n.end > n.start)
        .collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start));
    for i in 1..notes.len() {
        if notes[i - 1].end > notes[i].start {
            notes[i - 1].end = notes[i].start;
        }
    }
    notes.retain(|n| n.end - n.start > 1e-6);
    // 2. Each bar's beats: straight or triplets, by their onsets.
    let mut swung: Vec<Vec<bool>> = grid
        .bars
        .iter()
        .map(|(b, _, len)| {
            let (beat, _) = beat_of(b.time);
            vec![false; (*len).div_ceil(beat) as usize]
        })
        .collect();
    if input.grid != Grid::Straight {
        let mut errs: Vec<Vec<(f64, f64, u32)>> =
            swung.iter().map(|v| vec![(0.0, 0.0, 0); v.len()]).collect();
        for n in &notes {
            for q in [n.start, n.end] {
                let (b, p) = grid.place(q);
                let (beat, triplets) = beat_of(grid.bars[b].0.time);
                if !triplets {
                    continue;
                }
                let i = ((p / f64::from(beat)) as usize).min(errs[b].len().saturating_sub(1));
                let off = p - f64::from(i as u32 * beat);
                let e = &mut errs[b][i];
                e.0 += (off - (off / 3.0).round() * 3.0).abs();
                e.1 += (off - (off / 4.0).round() * 4.0).abs();
                let t = (off / 4.0).round() * 4.0;
                if (t - 4.0).abs() < 0.5 || (t - 8.0).abs() < 0.5 {
                    e.2 += 1;
                }
            }
        }
        for (b, beats) in swung.iter_mut().enumerate() {
            for (i, s) in beats.iter_mut().enumerate() {
                let (straight, triplet, on_triplets) = errs[b][i];
                *s = match input.grid {
                    Grid::Triplets => beat_of(grid.bars[b].0.time).1,
                    _ => on_triplets >= 2 && triplet * 1.6 < straight,
                };
            }
        }
    }
    let quantise = |q: f64| -> u32 {
        let (b, p) = grid.place(q);
        let (bar, off, len) = grid.bars[b];
        let (beat, _) = beat_of(bar.time);
        let i = ((p / f64::from(beat)) as usize).min(swung[b].len().saturating_sub(1));
        let unit = if swung[b][i] { 4.0 } else { 3.0 };
        let start = f64::from(i as u32 * beat);
        let snapped = start + ((p - start) / unit).round() * unit;
        off + (snapped.max(0.0) as u32).min(len)
    };
    // Starts and ends on the grid; one note a start; short gaps closed.
    let mut placed: Vec<(u32, u32, u8)> = Vec::new();
    for n in &notes {
        let s = quantise(n.start);
        let e = quantise(n.end).max(s + 3);
        match placed.last_mut() {
            Some(last) if last.0 == s => {
                if e - s > last.1 - last.0 {
                    *last = (s, e, n.key);
                }
            }
            _ => placed.push((s, e, n.key)),
        }
    }
    let total = grid.total();
    for i in 0..placed.len() {
        let next = placed.get(i + 1).map_or(total, |n| n.0);
        let (s, e, _) = placed[i];
        let e = e.min(next);
        let gap = next.saturating_sub(e);
        let e = if i + 1 < placed.len() && gap > 0 && gap < DIVISIONS / 2 && gap < e - s {
            next
        } else {
            e
        };
        placed[i].1 = e.max(s + 1).min(total);
    }
    placed.retain(|p| p.0 < total);
    // The key and the clef.
    let key = input.key.unwrap_or_else(|| {
        let mut h = [0.0f64; 12];
        for (s, e, k) in &placed {
            h[usize::from(k % 12)] += f64::from(e - s);
        }
        // The chords weigh in (their notes for as long as they last, the
        // root twice): a tune alone can lean on its fifth's key.
        let end = input.bars.last().map_or(0.0, |b| {
            b.start + f64::from(bar_length(b.time)) / f64::from(DIVISIONS)
        });
        for (i, c) in input.chords.iter().enumerate() {
            let until = input.chords.get(i + 1).map_or(end, |n| n.start);
            let span = (until - c.start).max(0.0) * f64::from(DIVISIONS);
            for pc in c.chord.pitch_classes() {
                h[usize::from(pc % 12)] += span;
            }
            h[usize::from(c.chord.root % 12)] += span;
        }
        faderframe_midi::theory::detect_key(&h)
            .unwrap_or(Key::new(0, faderframe_midi::theory::Scale::Major))
    });
    let sig = key_signature(key);
    let mut pitches: Vec<u8> = placed.iter().map(|p| p.2).collect();
    pitches.sort_unstable();
    let median = pitches.get(pitches.len() / 2).copied().unwrap_or(67);
    // A high part on the treble staff, a tenor's an octave down on it, a
    // bass line (or a baritone) on the bass staff.
    let clef = if median >= 59 {
        Clef::Treble
    } else if median >= 52 {
        Clef::Treble8vb
    } else {
        Clef::Bass
    };
    // 3. Bars of notes and rests.
    let mut measures = Vec::new();
    // Where each placed note's first event went (bar, event).
    let mut first_event: Vec<Option<(usize, usize)>> = vec![None; placed.len()];
    let mut last_time = None;
    for (b, (bar, off, len)) in grid.bars.iter().enumerate() {
        let end = off + len;
        let mut events = Vec::new();
        let mut cursor = *off;
        let push_rest = |events: &mut Vec<Event>, from: u32, to: u32| {
            for d in split(from - off, to - from, bar.time, &swung[b]) {
                let at = events.last().map_or(from - off, |e: &Event| e.end());
                events.push(rest(at.max(from - off), d));
            }
        };
        for (i, (s, e, k)) in placed.iter().enumerate() {
            if *e <= *off || *s >= end {
                continue;
            }
            let (a, z) = ((*s).max(*off), (*e).min(end));
            if a > cursor {
                push_rest(&mut events, cursor, a);
            }
            let pieces = split(a - off, z - a, bar.time, &swung[b]);
            let n = pieces.len();
            let mut at = a - off;
            for (j, d) in pieces.into_iter().enumerate() {
                if j == 0 && *s >= *off {
                    first_event[i] = Some((b, events.len()));
                }
                let (step, alter, octave) = spell(*k, sig);
                events.push(Event {
                    at,
                    duration: d,
                    pitch: Some(Pitch {
                        step,
                        alter,
                        octave,
                        accidental: None,
                        midi: *k,
                    }),
                    tie: j + 1 < n || *e > end,
                    tied_from: j > 0 || *s < *off,
                    bar_rest: false,
                    beams: [None; 2],
                    tuplet: None,
                    lyric: None,
                    held: false,
                });
                at += d.divisions();
            }
            cursor = z;
        }
        if cursor < end {
            if events.is_empty() {
                events.push(Event {
                    bar_rest: true,
                    ..rest(0, Duration::plain(Value::Whole))
                });
            } else {
                push_rest(&mut events, cursor, end);
            }
        }
        let time = bar.time;
        measures.push(Measure {
            time,
            show_time: last_time != Some(time),
            events,
            chords: Vec::new(),
        });
        last_time = Some(time);
    }
    // 4. Accidentals, beams, triplets.
    for m in &mut measures {
        accidentals(m, sig);
        beam(m);
        triplets(m);
    }
    // 5. Words and chords.
    let onsets: Vec<(f64, usize, usize)> = placed
        .iter()
        .zip(&first_event)
        .filter_map(|((s, _, _), f)| f.map(|(b, e)| (f64::from(*s) / f64::from(DIVISIONS), b, e)))
        .collect();
    let origin = input.bars.first().map_or(0.0, |b| b.start);
    lay_words(&mut measures, &onsets, &input.lines, origin);
    for c in &input.chords {
        let (b, p) = grid.place(c.start);
        let (bar, _, len) = grid.bars.get(b).copied().unwrap_or((
            Bar {
                start: 0.0,
                time: (4, 4),
            },
            0,
            0,
        ));
        let (beat, _) = beat_of(bar.time);
        let mut at = ((p / f64::from(beat)).round() as u32) * beat;
        let mut b = b;
        if at >= len {
            if b + 1 < measures.len() {
                b += 1;
                at = 0;
            } else {
                continue;
            }
        }
        if b >= measures.len() {
            continue;
        }
        let symbol = chord_symbol(c.chord, at, sig);
        let chords = &mut measures[b].chords;
        chords.retain(|s| s.at != at);
        chords.push(symbol);
        chords.sort_by_key(|s| s.at);
    }
    // A chord the same as the one sounding is not written again.
    let mut sounding: Option<ChordSymbol> = None;
    for m in &mut measures {
        m.chords.retain(|c| {
            let same = sounding.as_ref().is_some_and(|s| {
                s.root == c.root && s.kind == c.kind && s.bass == c.bass && s.degrees == c.degrees
            });
            if !same {
                sounding = Some(c.clone());
            }
            !same
        });
    }
    LeadSheet {
        title: input.title.clone(),
        composer: input.composer.clone(),
        key: sig,
        clef,
        tempo: input.tempo,
        measures,
    }
}

fn rest(at: u32, duration: Duration) -> Event {
    Event {
        at,
        duration,
        pitch: None,
        tie: false,
        tied_from: false,
        bar_rest: false,
        beams: [None; 2],
        tuplet: None,
        lyric: None,
        held: false,
    }
}

/// Accidentals where a note differs from the key or from what the bar
/// had on that line before.
fn accidentals(m: &mut Measure, key: KeySig) {
    let mut state: Vec<((u8, i8), i8)> = Vec::new();
    for e in &mut m.events {
        let Some(p) = &mut e.pitch else { continue };
        let place = (p.step, p.octave);
        let current = state
            .iter()
            .find(|(k, _)| *k == place)
            .map_or(key.alter_of(p.step), |(_, a)| *a);
        if p.alter != current && !e.tied_from {
            p.accidental = Some(p.alter);
        }
        state.retain(|(k, _)| *k != place);
        state.push((place, p.alter));
    }
}

/// Beams within each beat: runs of two or more eighths or shorter.
fn beam(m: &mut Measure) {
    let (beat, _) = beat_of(m.time);
    let group = if beat == DIVISIONS / 2 {
        DIVISIONS
    } else {
        beat
    };
    let n = m.events.len();
    let mut i = 0;
    while i < n {
        let g = m.events[i].at / group;
        let mut j = i;
        while j < n
            && m.events[j].at / group == g
            && !m.events[j].is_rest()
            && m.events[j].duration.value.beams() > 0
            && (m.events[j].end() - 1) / group == g
        {
            j += 1;
        }
        if j - i >= 2 {
            for k in i..j {
                m.events[k].beams[0] = Some(if k == i {
                    Beam::Begin
                } else if k + 1 == j {
                    Beam::End
                } else {
                    Beam::Continue
                });
                if m.events[k].duration.value.beams() >= 2 {
                    let prev = k > i && m.events[k - 1].duration.value.beams() >= 2;
                    let next = k + 1 < j && m.events[k + 1].duration.value.beams() >= 2;
                    m.events[k].beams[1] = Some(match (prev, next) {
                        (false, true) => Beam::Begin,
                        (true, true) => Beam::Continue,
                        (true, false) => Beam::End,
                        (false, false) if k == i => Beam::ForwardHook,
                        (false, false) => Beam::BackwardHook,
                    });
                }
            }
            i = j;
        } else {
            i = i.max(j) + usize::from(j == i);
        }
    }
}

/// Triplet brackets: the triplet values of each beat.
fn triplets(m: &mut Measure) {
    let mut i = 0;
    let n = m.events.len();
    while i < n {
        if !m.events[i].duration.triplet {
            i += 1;
            continue;
        }
        let beat = m.events[i].at / DIVISIONS;
        let mut j = i;
        while j < n && m.events[j].duration.triplet && m.events[j].at / DIVISIONS == beat {
            j += 1;
        }
        for k in i..j {
            m.events[k].tuplet = Some(if k == i {
                Tuplet::Start
            } else if k + 1 == j {
                Tuplet::Stop
            } else {
                Tuplet::Middle
            });
        }
        if j - i == 1 {
            m.events[i].tuplet = Some(Tuplet::Start);
        }
        i = j;
    }
}

/// Lay each line's words under the notes starting in it: aligned by
/// time (the words spread evenly over the line, matched in order to the
/// nearest onsets), more words than notes shared out; a word followed by
/// notes without words draws its line under them.
fn lay_words(
    measures: &mut [Measure],
    onsets: &[(f64, usize, usize)],
    lines: &[Line],
    origin: f64,
) {
    let mut used = vec![false; onsets.len()];
    for line in lines {
        let words: Vec<&str> = line.text.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let (a, z) = (line.start - origin, line.end - origin);
        // The notes starting in the line (a note on its end is the next
        // line's).
        let notes: Vec<usize> = (0..onsets.len())
            .filter(|i| !used[*i] && onsets[*i].0 >= a - 0.25 && onsets[*i].0 < z - 0.05)
            .collect();
        if notes.is_empty() {
            continue;
        }
        let w = words.len();
        let n = notes.len();
        // Which note each word goes to (or each note's words).
        let mut text: Vec<Option<String>> = vec![None; n];
        if w <= n {
            // Where each word should start: the line's time shared out by
            // syllables (longer words take longer).
            let syllables: Vec<f64> = words.iter().map(|w| f64::from(syllables(w))).collect();
            let total: f64 = syllables.iter().sum::<f64>().max(1.0);
            let starts: Vec<f64> = syllables
                .iter()
                .scan(0.0, |acc, s| {
                    let at = *acc;
                    *acc += s;
                    Some(at)
                })
                .collect();
            let est = |i: usize| a + (z - a) * starts[i] / total;
            // Least total distance, words in order on distinct notes.
            let mut cost = vec![vec![f64::INFINITY; n + 1]; w + 1];
            let mut from = vec![vec![0usize; n + 1]; w + 1];
            cost[0] = vec![0.0; n + 1];
            for i in 1..=w {
                for j in i..=n {
                    let here = cost[i - 1][j - 1] + (onsets[notes[j - 1]].0 - est(i - 1)).abs();
                    let skip = cost[i][j - 1];
                    if here <= skip {
                        cost[i][j] = here;
                        from[i][j] = j - 1;
                    } else {
                        cost[i][j] = skip;
                        from[i][j] = from[i][j - 1];
                    }
                }
            }
            let mut j = n;
            for i in (1..=w).rev() {
                let k = from[i][j];
                text[k] = Some(words[i - 1].to_string());
                j = k;
            }
        } else {
            for (k, t) in text.iter_mut().enumerate() {
                let (lo, hi) = (k * w / n, (k + 1) * w / n);
                *t = Some(words[lo..hi.max(lo + 1)].join(" "));
            }
        }
        let mut last_word: Option<usize> = None;
        for (k, t) in text.into_iter().enumerate() {
            let (_, b, e) = onsets[notes[k]];
            used[notes[k]] = true;
            match t {
                Some(t) => {
                    measures[b].events[e].lyric = Some(Lyric {
                        text: t,
                        extend: false,
                    });
                    last_word = Some(k);
                }
                None => {
                    if let Some(lw) = last_word {
                        let (_, lb, le) = onsets[notes[lw]];
                        if let Some(l) = &mut measures[lb].events[le].lyric {
                            l.extend = true;
                        }
                        measures[b].events[e].held = true;
                    }
                }
            }
        }
    }
}

/// Syllables in a word, roughly: its groups of vowels (a final silent e
/// not counted), at least one.
pub fn syllables(word: &str) -> u32 {
    let w: Vec<char> = word
        .chars()
        .filter(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .collect();
    let vowel = |c: char| "aeiouyàáâäèéêëìíîïòóôöùúûüæøå".contains(c);
    let mut n = 0;
    let mut prev = false;
    for (i, &c) in w.iter().enumerate() {
        let v = vowel(c);
        if v && !prev && !(c == 'e' && i + 1 == w.len() && n > 0) {
            n += 1;
        }
        prev = v;
    }
    n.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_midi::theory::Scale;

    #[test]
    fn syllables_are_counted() {
        assert_eq!(syllables("Sing"), 1);
        assert_eq!(syllables("beautiful"), 3);
        assert_eq!(syllables("love"), 1);
        assert_eq!(syllables("tomorrow"), 3);
        assert_eq!(syllables("..."), 1);
    }

    fn bars(n: usize, time: (u8, u8)) -> Vec<Bar> {
        let len = f64::from(time.0) * 4.0 / f64::from(time.1);
        (0..n)
            .map(|i| Bar {
                start: i as f64 * len,
                time,
            })
            .collect()
    }

    fn note(start: f64, end: f64, key: u8) -> Note {
        Note { start, end, key }
    }

    fn values(m: &Measure) -> Vec<(u32, u32, bool, bool)> {
        m.events
            .iter()
            .map(|e| (e.at, e.duration.divisions(), e.is_rest(), e.tie))
            .collect()
    }

    #[test]
    fn spelling_follows_the_key() {
        let c = KeySig::C;
        assert_eq!(spell(60, c), (0, 0, 4));
        assert_eq!(spell(61, c), (0, 1, 4));
        assert_eq!(spell(70, c), (6, -1, 4));
        let f = key_signature(Key::new(5, Scale::Major));
        assert_eq!(f.fifths, -1);
        assert_eq!(spell(70, f), (6, -1, 4), "B♭ in F");
        let e = key_signature(Key::new(4, Scale::Major));
        assert_eq!(e.fifths, 4);
        assert_eq!(spell(68, e), (4, 1, 4), "G♯ in E");
        assert_eq!(
            spell(
                71,
                KeySig {
                    fifths: -7,
                    minor: false
                }
            ),
            (0, -1, 5),
            "C♭ in C♭"
        );
        let a = key_signature(Key::new(9, Scale::Minor));
        assert_eq!((a.fifths, a.minor), (0, true));
        assert_eq!(spell(68, a), (4, 1, 4), "the leading note, G♯");
    }

    #[test]
    fn values_keep_to_the_beats() {
        let t = (4, 4);
        // A quarter on the second eighth: eighth tied to eighth.
        assert_eq!(
            split(6, 12, t, &[])
                .iter()
                .map(|d| d.divisions())
                .collect::<Vec<_>>(),
            [6, 6]
        );
        // A half on beat two crosses the middle: quarter + quarter.
        assert_eq!(
            split(12, 24, t, &[])
                .iter()
                .map(|d| d.divisions())
                .collect::<Vec<_>>(),
            [12, 12]
        );
        // From the bar's start a dotted half is fine.
        assert_eq!(split(0, 36, t, &[]).len(), 1);
        assert_eq!(split(0, 48, t, &[])[0].value, Value::Whole);
        // In 6/8, the beat is a dotted quarter.
        assert_eq!(
            split(0, 36, (6, 8), &[])
                .iter()
                .map(|d| d.divisions())
                .collect::<Vec<_>>(),
            [36]
        );
        assert_eq!(
            split(6, 18, (6, 8), &[])
                .iter()
                .map(|d| d.divisions())
                .collect::<Vec<_>>(),
            [12, 6]
        );
        // A triplet beat.
        let d = split(12, 4, t, &[false, true]);
        assert!(d[0].triplet && d[0].value == Value::Eighth);
    }

    #[test]
    fn a_melody_becomes_bars_with_ties_rests_and_beams() {
        let input = Input {
            bars: bars(2, (4, 4)),
            key: Some(Key::new(0, Scale::Major)),
            notes: vec![
                // Two eighths, a quarter, a half over the bar line.
                note(0.02, 0.5, 60),
                note(0.5, 0.98, 62),
                note(1.0, 2.0, 64),
                note(3.0, 5.0, 65),
                // After a quarter's rest.
                note(6.0, 7.0, 67),
            ],
            ..Input::default()
        };
        let s = build(&input);
        assert_eq!(s.measures.len(), 2);
        let m = &s.measures[0];
        assert_eq!(
            values(m),
            [
                (0, 6, false, false),
                (6, 6, false, false),
                (12, 12, false, false),
                (24, 12, true, false),
                (36, 12, false, true)
            ]
        );
        assert_eq!(m.events[0].beams[0], Some(Beam::Begin));
        assert_eq!(m.events[1].beams[0], Some(Beam::End));
        let m2 = &s.measures[1];
        assert!(m2.events[0].tied_from && !m2.events[0].is_rest());
        assert_eq!(m2.events[0].duration.divisions(), 12);
        assert!(m2.events[1].is_rest());
        assert!(!m2.events[2].is_rest());
        assert!(m2.events.last().is_some_and(|e| e.end() == 48));
        // A bar with nothing: one bar rest.
        let empty = build(&Input {
            bars: bars(1, (3, 4)),
            ..Input::default()
        });
        assert!(empty.measures[0].events[0].bar_rest);
    }

    #[test]
    fn triplets_are_heard_where_they_are_played() {
        let third = 1.0 / 3.0;
        let input = Input {
            bars: bars(1, (4, 4)),
            notes: vec![
                note(0.0, third, 60),
                note(third, 2.0 * third, 62),
                note(2.0 * third, 1.0, 64),
                note(1.0, 2.0, 65),
            ],
            ..Input::default()
        };
        let s = build(&input);
        let e = &s.measures[0].events;
        assert!(e[0].duration.triplet && e[1].duration.triplet && e[2].duration.triplet);
        assert_eq!(e[0].tuplet, Some(Tuplet::Start));
        assert_eq!(e[2].tuplet, Some(Tuplet::Stop));
        assert!(!e[3].duration.triplet);
    }

    #[test]
    fn accidentals_once_a_bar_and_words_under_the_notes() {
        let input = Input {
            bars: bars(1, (4, 4)),
            key: Some(Key::new(0, Scale::Major)),
            notes: vec![
                note(0.0, 1.0, 66),
                note(1.0, 2.0, 66),
                note(2.0, 3.0, 65),
                note(3.0, 4.0, 67),
            ],
            lines: vec![Line {
                start: 0.0,
                end: 4.0,
                text: "Sing it".into(),
            }],
            ..Input::default()
        };
        let s = build(&input);
        let e = &s.measures[0].events;
        assert_eq!(e[0].pitch.unwrap().accidental, Some(1), "F♯");
        assert_eq!(e[1].pitch.unwrap().accidental, None, "still sharp");
        assert_eq!(e[2].pitch.unwrap().accidental, Some(0), "F natural");
        assert_eq!(e[0].lyric.as_ref().unwrap().text, "Sing");
        let it = e
            .iter()
            .position(|e| e.lyric.as_ref().is_some_and(|l| l.text == "it"))
            .unwrap();
        assert!(it >= 1);
    }

    #[test]
    fn chords_land_on_beats_and_are_not_repeated() {
        let input = Input {
            bars: bars(2, (4, 4)),
            key: Some(Key::new(5, Scale::Major)),
            chords: vec![
                ChordAt {
                    start: 0.1,
                    chord: Chord::new(5, Quality::Major),
                },
                ChordAt {
                    start: 1.9,
                    chord: Chord::new(10, Quality::Major7),
                },
                ChordAt {
                    start: 4.0,
                    chord: Chord::new(10, Quality::Major7),
                },
            ],
            ..Input::default()
        };
        let s = build(&input);
        let c = &s.measures[0].chords;
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].at, 0);
        assert_eq!(c[1].at, 24);
        assert_eq!(c[1].text(), "B♭maj7");
        assert!(s.measures[1].chords.is_empty(), "the same chord");
    }
}

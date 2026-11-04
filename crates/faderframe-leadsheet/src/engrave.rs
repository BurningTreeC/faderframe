//! Engraving: a lead sheet laid out on pages as a display list (filled
//! outlines, lines and text in points, y down) that the PDF writer and
//! the app's view both draw.
//!
//! Systems hold up to four bars (fewer when they are crowded) and are
//! justified; each note's room grows with its value, its accidental, its
//! word and the chord over it, so words and symbols never collide. A
//! score's systems stack its staves (each drawn on its own, then spaced
//! by what it reaches so nothing of one meets the next), bracketed, the
//! parts named at the left, notes sounding together one above the other,
//! the bar lines through the system unless words are sung between
//! staves; a grand staff (a piano's hands) is braced, named once and its
//! bar lines run through both its staves; a system that does not fit the
//! page starts the next. Chords
//! stack their noteheads on one stem (a second's upper note beside it),
//! their accidentals in columns. Tablature has a line a string (broken
//! where a fret number stands), its clef and metre, and — standing alone —
//! its rhythm under it as stems and beams. The glyphs are Bravura's
//! ([`crate::assets`]), placed by SMuFL's rules (stems at the noteheads'
//! anchors, staff spaces as the unit).

use crate::assets::{WIDTHS, engraving as E};
use crate::glyph::{Glyph, Seg, glyph};
use crate::score::{
    Beam, ChordSymbol, Clef, Event, LeadSheet, Measure, Part, Pitch, TabStaff, Tuplet, Value,
};

/// From one string's line to the next on tablature (staff spaces).
pub const TAB_GAP: f32 = 1.5;
/// Fret numbers' size (staff spaces).
const FRET_SIZE: f32 = 1.7;

/// A part's staff from its top line to its bottom line (staff spaces).
pub fn staff_height(part: &Part) -> f32 {
    match &part.tab {
        Some(t) => t.strings.len().saturating_sub(1) as f32 * TAB_GAP,
        None => 4.0,
    }
}

/// A fret number as written (a tied note's in brackets).
fn fret_text(fret: u8, tied: bool) -> String {
    if tied {
        format!("({fret})")
    } else {
        fret.to_string()
    }
}

/// The PDF's text fonts (the order of [`crate::assets::FONT_NAMES`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    Sans,
    SansBold,
    Serif,
    SerifBold,
    SerifItalic,
}

impl Font {
    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

/// Something drawn on a page (points, y down).
#[derive(Clone, Debug, PartialEq)]
pub enum Ink {
    /// A filled outline.
    Fill(Vec<Seg>),
    Line {
        a: (f32, f32),
        b: (f32, f32),
        width: f32,
    },
    /// Text on its baseline at `x`, `y`.
    Text {
        text: String,
        x: f32,
        y: f32,
        size: f32,
        font: Font,
        anchor: Anchor,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    pub ink: Vec<Ink>,
}

/// Where an engraved sheet put its systems, bars and notes (points on its
/// pages): what an editor points at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Map {
    /// The staff space (points).
    pub space: f32,
    pub systems: Vec<SystemMap>,
    /// Each part's events: by bar, then event (the first voice's, then
    /// the second's, as [`Measure::all_events`]); `None`: not drawn
    /// (tablature).
    pub events: Vec<Vec<Vec<Option<EventBox>>>>,
}

/// One system: its page, its bars and where its staves are.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemMap {
    pub page: usize,
    pub bars: std::ops::Range<usize>,
    /// Each part's staff: its top line's y.
    pub staves: Vec<f32>,
    /// Each part's staff height (from its top line to its bottom line).
    pub heights: Vec<f32>,
    pub spans: Vec<BarSpan>,
    /// From the top of the first staff's room to the bottom of the last's.
    pub top: f32,
    pub bottom: f32,
    /// Where the bars begin (after the clef, key and time) and end.
    pub left: f32,
    pub right: f32,
}

/// A bar of a system: where it is and where its times are drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct BarSpan {
    pub bar: usize,
    pub x: f32,
    pub width: f32,
    /// The times something starts in it (divisions) and their x (a
    /// notehead's middle there).
    pub points: Vec<(u32, f32)>,
    /// Its length (divisions).
    pub length: u32,
}

impl BarSpan {
    /// Where the bar's notes end: a little before its bar line.
    fn end_x(&self, space: f32) -> f32 {
        let last = self.points.last().map_or(self.x, |p| p.1);
        (self.x + self.width - 0.8 * space).max(last)
    }

    /// The time (divisions) at `x`: between the drawn times in proportion.
    pub fn time_at(&self, x: f32, space: f32) -> f32 {
        let mut pts: Vec<(f32, f32)> = self.points.iter().map(|(t, px)| (*t as f32, *px)).collect();
        pts.push((self.length as f32, self.end_x(space)));
        if x <= pts[0].1 {
            return pts[0].0;
        }
        for w in pts.windows(2) {
            let ((t0, x0), (t1, x1)) = (w[0], w[1]);
            if x < x1 {
                let f = if x1 > x0 { (x - x0) / (x1 - x0) } else { 0.0 };
                return t0 + f * (t1 - t0);
            }
        }
        self.length as f32
    }

    /// The x of time `at` (divisions): between the drawn times in
    /// proportion.
    pub fn x_at(&self, at: f32, space: f32) -> f32 {
        let mut pts: Vec<(f32, f32)> = self.points.iter().map(|(t, px)| (*t as f32, *px)).collect();
        pts.push((self.length as f32, self.end_x(space)));
        if at <= pts[0].0 {
            return pts[0].1;
        }
        for w in pts.windows(2) {
            let ((t0, x0), (t1, x1)) = (w[0], w[1]);
            if at < t1 {
                let f = if t1 > t0 { (at - t0) / (t1 - t0) } else { 0.0 };
                return x0 + f * (x1 - x0);
            }
        }
        self.end_x(space)
    }
}

/// A note (or chord) or a rest drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct EventBox {
    pub page: usize,
    pub system: usize,
    /// Its noteheads' column (a rest's glyph): left edge and width.
    pub x: f32,
    pub w: f32,
    /// Each pitch's notehead (as [`Event::pitches`]); a rest's middle.
    pub heads: Vec<Head>,
    /// The ink drawing it on its page (heads, accidentals, dots and
    /// ledger lines; not stems or beams): from, to.
    pub ink: (usize, usize),
    /// 0: the first voice, 1: the second.
    pub voice: u8,
    pub rest: bool,
}

impl EventBox {
    fn rest((x, w): (f32, f32), y: f32, from: usize, to: usize, voice: usize) -> Self {
        Self {
            page: 0,
            system: 0,
            x,
            w,
            heads: vec![Head {
                x: x + w / 2.0,
                y,
                ink: from,
                accidental: None,
            }],
            ink: (from, to),
            voice: voice as u8,
            rest: true,
        }
    }
}

/// A notehead drawn: its centre and its ink (and its accidental's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Head {
    pub x: f32,
    pub y: f32,
    pub ink: usize,
    pub accidental: Option<usize>,
}

/// What is pointed at: a part's event (by bar, then index as
/// [`Measure::all_events`]) and which of its pitches (`None`: a rest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointed {
    pub part: usize,
    pub bar: usize,
    pub event: usize,
    pub pitch: Option<usize>,
}

/// A place on a staff: its system, part and bar, the time there (divisions
/// into the bar, unrounded) and the staff step (half spaces above the
/// bottom line).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub system: usize,
    pub part: usize,
    pub bar: usize,
    pub at: f32,
    pub step: i32,
}

impl Map {
    /// The notehead or rest nearest `(x, y)` on `page`, within a staff
    /// space and a half.
    pub fn event_at(&self, page: usize, x: f32, y: f32) -> Option<Pointed> {
        let reach = 1.5 * self.space;
        let mut best: Option<(f32, Pointed)> = None;
        for (part, bars) in self.events.iter().enumerate() {
            for (bar, events) in bars.iter().enumerate() {
                for (event, b) in events.iter().enumerate() {
                    let Some(b) = b else { continue };
                    if b.page != page {
                        continue;
                    }
                    for (i, h) in b.heads.iter().enumerate() {
                        let d = (h.x - x).hypot(h.y - y);
                        if d <= reach && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((
                                d,
                                Pointed {
                                    part,
                                    bar,
                                    event,
                                    pitch: (!b.rest).then_some(i),
                                },
                            ));
                        }
                    }
                }
            }
        }
        best.map(|(_, p)| p)
    }

    /// The staff and time under `(x, y)` on `page`: the system there, its
    /// staff nearest, the bar under `x` (the first or last beyond them).
    pub fn place_at(&self, page: usize, x: f32, y: f32) -> Option<Place> {
        let sp = self.space;
        let (system, sys) = self
            .systems
            .iter()
            .enumerate()
            .find(|(_, s)| s.page == page && y >= s.top && y < s.bottom)?;
        let part = (0..sys.staves.len()).min_by(|a, b| {
            let d = |i: usize| (sys.staves[i] + sys.heights[i] / 2.0 - y).abs();
            d(*a).total_cmp(&d(*b))
        })?;
        let span = sys
            .spans
            .iter()
            .find(|s| x < s.x + s.width)
            .or(sys.spans.last())?;
        let at = span.time_at(x.max(span.x), sp);
        let step = ((sys.staves[part] + 4.0 * sp - y) / (0.5 * sp)).round() as i32;
        Some(Place {
            system,
            part,
            bar: span.bar,
            at,
            step,
        })
    }

    /// Where time `at` (divisions) of bar `bar` is drawn on staff `part`:
    /// its page, x, and the staff's top line and height.
    pub fn x_of(&self, part: usize, bar: usize, at: f32) -> Option<(usize, f32, f32, f32)> {
        let sys = self.systems.iter().find(|s| s.bars.contains(&bar))?;
        let span = sys.spans.iter().find(|s| s.bar == bar)?;
        Some((
            sys.page,
            span.x_at(at, self.space),
            *sys.staves.get(part)?,
            *sys.heights.get(part)?,
        ))
    }
}

/// The diatonic step (from C0, as [`Pitch::diatonic`]) on staff step
/// `step` (half spaces above the bottom line) under `clef`.
pub fn diatonic_at(step: i32, clef: Clef) -> i32 {
    step - staff_step(0, clef)
}

/// The page and the staff's size.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub margin: f32,
    /// A staff space (points).
    pub space: f32,
    pub bars_per_system: usize,
    /// One system of every bar, as wide as they need, on one page as
    /// wide and as tall as it (a strip an editor scrolls along; no
    /// heading).
    pub continuous: bool,
}

impl Default for Layout {
    /// A4, a 6.5 pt staff space (a 26 pt staff), four bars a line.
    fn default() -> Self {
        Self {
            width: 595.28,
            height: 841.89,
            margin: 54.0,
            space: 6.5,
            bars_per_system: 4,
            continuous: false,
        }
    }
}

impl Layout {
    /// The default page with staves the smaller the more of them a system
    /// has, so a score still gets a few systems to a page and a few bars
    /// to a system.
    pub fn for_staves(staves: usize) -> Self {
        let space = match staves {
            0 | 1 => 6.5,
            2 => 6.0,
            3 => 5.5,
            4 | 5 => 5.0,
            _ => 4.5,
        };
        Self {
            space,
            ..Self::default()
        }
    }
}

/// The width of `text` in `font` at `size` (points), from the fonts'
/// metrics (characters outside WinAnsi count as a question mark).
pub fn text_width(text: &str, font: Font, size: f32) -> f32 {
    let row = &WIDTHS[font.index()];
    let w: u32 = text
        .chars()
        .map(|c| {
            let b = cp1252(c).unwrap_or(b'?');
            u32::from(row[usize::from(b.max(32)) - 32])
        })
        .sum();
    w as f32 * size / 1000.0
}

/// A character's WinAnsi (Windows-1252) byte.
pub fn cp1252(c: char) -> Option<u8> {
    const HIGH: [(char, u8); 27] = [
        ('€', 0x80),
        ('‚', 0x82),
        ('ƒ', 0x83),
        ('„', 0x84),
        ('…', 0x85),
        ('†', 0x86),
        ('‡', 0x87),
        ('ˆ', 0x88),
        ('‰', 0x89),
        ('Š', 0x8A),
        ('‹', 0x8B),
        ('Œ', 0x8C),
        ('Ž', 0x8E),
        ('‘', 0x91),
        ('’', 0x92),
        ('“', 0x93),
        ('”', 0x94),
        ('•', 0x95),
        ('–', 0x96),
        ('—', 0x97),
        ('˜', 0x98),
        ('™', 0x99),
        ('š', 0x9A),
        ('›', 0x9B),
        ('œ', 0x9C),
        ('ž', 0x9E),
        ('Ÿ', 0x9F),
    ];
    let u = c as u32;
    if (0x20..0x7F).contains(&u) || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    HIGH.iter().find(|(h, _)| *h == c).map(|(_, b)| *b)
}

/// Draw glyph `name` with its origin at `x`, `y`, `sp` points a staff space.
fn put(ink: &mut Vec<Ink>, name: &str, x: f32, y: f32, sp: f32) -> Option<&'static Glyph> {
    let g = glyph(name)?;
    let t = |gx: f32, gy: f32| (x + gx * sp, y - gy * sp);
    let path = g
        .path
        .iter()
        .map(|s| match *s {
            Seg::M(a, b) => {
                let (a, b) = t(a, b);
                Seg::M(a, b)
            }
            Seg::L(a, b) => {
                let (a, b) = t(a, b);
                Seg::L(a, b)
            }
            Seg::C(a, b, c, d, e, f) => {
                let (a, b) = t(a, b);
                let (c, d) = t(c, d);
                let (e, f) = t(e, f);
                Seg::C(a, b, c, d, e, f)
            }
            Seg::Z => Seg::Z,
        })
        .collect();
    ink.push(Ink::Fill(path));
    Some(g)
}

fn advance(name: &str) -> f32 {
    glyph(name).map_or(1.0, |g| g.advance)
}

fn line(ink: &mut Vec<Ink>, a: (f32, f32), b: (f32, f32), width: f32) {
    ink.push(Ink::Line { a, b, width });
}

/// A filled quadrilateral (a beam).
fn quad(ink: &mut Vec<Ink>, p: [(f32, f32); 4]) {
    ink.push(Ink::Fill(vec![
        Seg::M(p[0].0, p[0].1),
        Seg::L(p[1].0, p[1].1),
        Seg::L(p[2].0, p[2].1),
        Seg::L(p[3].0, p[3].1),
        Seg::Z,
    ]));
}

/// A tie from `a` to `b` bowing `up` (or down), thin at its ends.
fn tie(ink: &mut Vec<Ink>, a: (f32, f32), b: (f32, f32), up: bool, sp: f32) {
    let len = (b.0 - a.0).max(1.0);
    let h = (0.5 * sp + 0.06 * len).min(1.6 * sp) * if up { -1.0 } else { 1.0 };
    let thick = E::TIE_MIDPOINT_THICKNESS * sp * if up { -1.0 } else { 1.0 };
    let (c1, c2) = ((a.0 + len * 0.25, a.1 + h), (b.0 - len * 0.25, b.1 + h));
    let (d1, d2) = (
        (a.0 + len * 0.25, a.1 + h + thick),
        (b.0 - len * 0.25, b.1 + h + thick),
    );
    ink.push(Ink::Fill(vec![
        Seg::M(a.0, a.1),
        Seg::C(c1.0, c1.1, c2.0, c2.1, b.0, b.1),
        Seg::C(d2.0, d2.1, d1.0, d1.1, a.0, a.1),
        Seg::Z,
    ]));
}

/// Where a pitch sits on the staff: steps (half spaces) above the bottom
/// line.
fn staff_step(diatonic: i32, clef: Clef) -> i32 {
    diatonic
        - match clef {
            Clef::Treble => 30,
            Clef::Treble8vb => 23,
            Clef::Bass => 18,
            Clef::Bass8vb => 11,
        }
}

fn notehead(e: &Event) -> &'static str {
    match e.duration.value {
        Value::Whole => "noteheadWhole",
        Value::Half => "noteheadHalf",
        _ => "noteheadBlack",
    }
}

fn rest_glyph(v: Value) -> &'static str {
    match v {
        Value::Whole => "restWhole",
        Value::Half => "restHalf",
        Value::Quarter => "restQuarter",
        Value::Eighth => "rest8th",
        Value::Sixteenth => "rest16th",
    }
}

fn accidental_glyph(a: i8) -> &'static str {
    match a {
        -2 => "accidentalDoubleFlat",
        -1 => "accidentalFlat",
        1 => "accidentalSharp",
        2 => "accidentalDoubleSharp",
        _ => "accidentalNatural",
    }
}

const LYRIC_SIZE: f32 = 10.5;
const CHORD_SIZE: f32 = 12.0;

/// A chord symbol's pieces: text and accidental glyphs in turn.
enum Piece {
    Text(String, f32),
    Glyph(&'static str),
}

fn chord_pieces(c: &ChordSymbol) -> Vec<Piece> {
    // The root letter large, the rest a little smaller, accidentals as
    // glyphs.
    let mut out: Vec<Piece> = Vec::new();
    let mut text = String::new();
    for ch in c.text().chars() {
        let g = match ch {
            '♭' => Some("accidentalFlat"),
            '♯' => Some("accidentalSharp"),
            '𝄫' => Some("accidentalDoubleFlat"),
            '𝄪' => Some("accidentalDoubleSharp"),
            _ => None,
        };
        let size = if out.is_empty() {
            CHORD_SIZE
        } else {
            CHORD_SIZE * 0.85
        };
        match g {
            Some(g) => {
                if !text.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut text), size));
                }
                out.push(Piece::Glyph(g));
            }
            None if out.is_empty() && !text.is_empty() => {
                out.push(Piece::Text(std::mem::take(&mut text), CHORD_SIZE));
                text.push(ch);
            }
            None => text.push(ch),
        }
    }
    if !text.is_empty() {
        let size = if out.is_empty() {
            CHORD_SIZE
        } else {
            CHORD_SIZE * 0.85
        };
        out.push(Piece::Text(text, size));
    }
    out
}

/// The accidental glyphs in chord symbols, a staff space being this
/// fraction of the symbol's size.
const CHORD_GLYPH: f32 = 0.42;

fn chord_width(c: &ChordSymbol) -> f32 {
    chord_pieces(c)
        .iter()
        .map(|p| match p {
            Piece::Text(t, s) => text_width(t, Font::SerifBold, *s),
            Piece::Glyph(g) => advance(g) * CHORD_SIZE * CHORD_GLYPH + 1.0,
        })
        .sum()
}

fn draw_chord(ink: &mut Vec<Ink>, c: &ChordSymbol, x: f32, y: f32) {
    let mut x = x;
    for p in chord_pieces(c) {
        match p {
            Piece::Text(t, s) => {
                let w = text_width(&t, Font::SerifBold, s);
                ink.push(Ink::Text {
                    text: t,
                    x,
                    y,
                    size: s,
                    font: Font::SerifBold,
                    anchor: Anchor::Start,
                });
                x += w;
            }
            Piece::Glyph(g) => {
                let sp = CHORD_SIZE * CHORD_GLYPH;
                put(ink, g, x + 0.5, y - CHORD_SIZE * 0.42, sp);
                x += advance(g) * sp + 1.0;
            }
        }
    }
}

/// The room (staff spaces) an event wants: by its value, more for an
/// accidental, a dot, its word.
fn room(e: &Event, sp: f32) -> f32 {
    if e.bar_rest {
        return 8.0;
    }
    let d = e.duration.divisions().max(1) as f32;
    let mut w = 1.8 + 1.0 * (d / 3.0).log2();
    let columns = accidental_columns(&e.pitches().copied().collect::<Vec<_>>(), Clef::Treble).1;
    w += 1.3 * columns as f32;
    // A second's note beside the stem.
    if e.chord
        .iter()
        .zip(e.pitches())
        .any(|(a, b)| a.diatonic() - b.diatonic() == 1)
    {
        w += 1.2;
    }
    w += 0.6 * f32::from(e.duration.dots);
    // Fret numbers.
    if let Some(widest) = e
        .frets
        .iter()
        .map(|f| text_width(&fret_text(f.fret, e.tied_from), Font::Sans, FRET_SIZE * sp))
        .reduce(f32::max)
    {
        w = w.max(widest / sp + 0.8);
    }
    if let Some(l) = &e.lyric {
        w = w.max(text_width(&l.text, Font::Serif, LYRIC_SIZE) / sp + 1.0);
    }
    w
}

/// Most noteheads a chord's ties are drawn for.
const MOST_HEADS: usize = 8;

/// Where each note was drawn (for ties, beams and words).
#[derive(Clone, Copy, Debug)]
struct Placed {
    page: usize,
    system: usize,
    /// The notehead's left edge, its width, its centre line (a chord's:
    /// the stem's column, the lowest note's line).
    x: f32,
    w: f32,
    y: f32,
    /// A chord's highest note's line (`y` for a single note).
    top: f32,
    /// Every notehead's line, lowest first (ties).
    heads: [f32; MOST_HEADS],
    count: usize,
    up: bool,
}

/// Each pitch's accidental column (0 nearest the notes; None: no
/// accidental), lowest pitch first, and how many columns: from the top
/// down, an accidental takes the first column where none is within a
/// sixth of it.
fn accidental_columns(pitches: &[Pitch], clef: Clef) -> (Vec<Option<usize>>, usize) {
    let steps: Vec<i32> = pitches
        .iter()
        .map(|p| staff_step(p.diatonic(), clef))
        .collect();
    let mut col = vec![None; pitches.len()];
    let mut taken: Vec<(i32, usize)> = Vec::new();
    for i in (0..pitches.len()).rev() {
        if pitches[i].accidental.is_none() {
            continue;
        }
        let mut c = 0;
        while taken
            .iter()
            .any(|(s, k)| *k == c && (s - steps[i]).abs() < 6)
        {
            c += 1;
        }
        taken.push((steps[i], c));
        col[i] = Some(c);
    }
    let n = taken.iter().map(|(_, c)| c + 1).max().unwrap_or(0);
    (col, n)
}

/// One system's bars and its horizontal plan.
struct System {
    bars: std::ops::Range<usize>,
    /// Where its staves begin (after a score's part names and bracket).
    left: f32,
    /// Each bar's left edge and width (points).
    spans: Vec<(f32, f32)>,
    prefix: f32,
}

/// A bar's horizontal plan, shared by its staves: the times something
/// starts in it (any staff's notes and rests, the chord symbols) and the
/// room (staff spaces) from each to the next, so what sounds together
/// stands one above the other. Each note wants the room of its value,
/// accidental, dot and word from its start to its end, a chord symbol its
/// width up to the next; the narrowest wants are met first, a want over
/// several steps shares out what it lacks by their lengths.
struct BarPlan {
    points: Vec<u32>,
    rooms: Vec<f32>,
}

impl BarPlan {
    /// The plan of a bar on several staves (the chords are the first's).
    fn new(staves: &[&Measure], sp: f32) -> Self {
        let len = staves.first().map_or(1, |m| m.length().max(1));
        let mut points: Vec<u32> = staves
            .iter()
            .flat_map(|m| {
                m.all_events()
                    .filter(|e| !e.bar_rest)
                    .map(|e| e.at)
                    .chain(m.chords.iter().map(|c| c.at))
            })
            .chain([0])
            .filter(|p| *p < len)
            .collect();
        points.sort_unstable();
        points.dedup();
        let n = points.len();
        let index = |at: u32| points.partition_point(|p| *p < at);
        let length = |k: usize| points.get(k + 1).copied().unwrap_or(len) - points[k];
        // What each wants: its first step, the step after its last, room.
        let mut wants: Vec<(usize, usize, f32)> = Vec::new();
        for m in staves {
            for e in m.all_events() {
                let a = if e.bar_rest { 0 } else { index(e.at) };
                let b = if e.bar_rest { n } else { index(e.end()) };
                wants.push((a, b.max(a + 1), room(e, sp)));
            }
        }
        if let Some(top) = staves.first() {
            for (i, c) in top.chords.iter().enumerate() {
                let a = index(c.at);
                let b = top.chords.get(i + 1).map_or(n, |d| index(d.at));
                wants.push((a, b.max(a + 1), chord_width(c) / sp + 1.2));
            }
        }
        wants.sort_by_key(|(a, b, _)| b - a);
        let mut rooms = vec![0.0f32; n];
        for (a, b, need) in wants {
            let b = b.min(n);
            if a >= b {
                continue;
            }
            let lack = need - rooms[a..b].iter().sum::<f32>();
            if lack <= 0.0 {
                continue;
            }
            let span: u32 = (a..b).map(length).sum();
            for (k, r) in rooms.iter_mut().enumerate().take(b).skip(a) {
                *r += lack * length(k) as f32 / span.max(1) as f32;
            }
        }
        Self { points, rooms }
    }

    /// The bar's least width (points).
    fn width(&self, sp: f32) -> f32 {
        (self.rooms.iter().sum::<f32>() + 2.0) * sp
    }

    /// Each point's x in a bar from `bx`, `bw` wide.
    fn xs(&self, bx: f32, bw: f32, sp: f32) -> Vec<f32> {
        let total = self.rooms.iter().sum::<f32>().max(1.0);
        let inner = bw - 2.0 * sp;
        let mut x = bx + 1.2 * sp;
        self.rooms
            .iter()
            .map(|r| {
                let at = x;
                x += inner * r / total;
                at
            })
            .collect()
    }

    /// The x of the point at `at` (or the last before it).
    fn x(&self, xs: &[f32], at: u32) -> f32 {
        let i = self.points.partition_point(|p| *p <= at).saturating_sub(1);
        xs.get(i).copied().unwrap_or(0.0)
    }
}

/// One staff of a system, drawn with its top line at y = 0.
struct Drawn {
    ink: Vec<Ink>,
    /// How far its notes reach above its top line and below its bottom
    /// line (points).
    top_extra: f32,
    bottom_extra: f32,
}

/// The highest and the lowest y `ink` reaches (curves by their control
/// points, text by its fonts' usual ascent and descent).
fn extent(ink: &[Ink]) -> (f32, f32) {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    let mut see = |y: f32| {
        lo = lo.min(y);
        hi = hi.max(y);
    };
    for i in ink {
        match i {
            Ink::Fill(path) => {
                for s in path {
                    match *s {
                        Seg::M(_, y) | Seg::L(_, y) => see(y),
                        Seg::C(_, a, _, b, _, c) => {
                            see(a);
                            see(b);
                            see(c);
                        }
                        Seg::Z => {}
                    }
                }
            }
            Ink::Line { a, b, width } => {
                for y in [a.1, b.1] {
                    see(y - width / 2.0);
                    see(y + width / 2.0);
                }
            }
            Ink::Text { y, size, .. } => {
                see(y - size * 0.75);
                see(y + size * 0.25);
            }
        }
    }
    if lo > hi { (0.0, 0.0) } else { (lo, hi) }
}

/// `ink` moved down by `dy`.
fn shifted(ink: Vec<Ink>, dy: f32) -> impl Iterator<Item = Ink> {
    ink.into_iter().map(move |i| match i {
        Ink::Fill(path) => Ink::Fill(
            path.into_iter()
                .map(|s| match s {
                    Seg::M(x, y) => Seg::M(x, y + dy),
                    Seg::L(x, y) => Seg::L(x, y + dy),
                    Seg::C(a, b, c, d, e, f) => Seg::C(a, b + dy, c, d + dy, e, f + dy),
                    Seg::Z => Seg::Z,
                })
                .collect(),
        ),
        Ink::Line { a, b, width } => Ink::Line {
            a: (a.0, a.1 + dy),
            b: (b.0, b.1 + dy),
            width,
        },
        Ink::Text {
            text,
            x,
            y,
            size,
            font,
            anchor,
        } => Ink::Text {
            text,
            x,
            y: y + dy,
            size,
            font,
            anchor,
        },
    })
}

/// A score's part names.
const NAME_SIZE: f32 = 10.0;
/// From a part's name to its staves (staff spaces): the bracket between.
const BRACKET_ROOM: f32 = 2.2;
/// The bracket's thick line (staff spaces; Bravura's bracketThickness).
const BRACKET_THICKNESS: f32 = 0.5;
/// A grand staff's brace: its width, its swell and the room it takes
/// left of the staves (staff spaces).
const BRACE_WIDTH: f32 = 1.2;
const BRACE_SWELL: f32 = 0.36;
const BRACE_ROOM: f32 = 1.7;

fn clef_glyph(clef: Clef) -> &'static str {
    match clef {
        Clef::Treble => "gClef",
        Clef::Treble8vb => "gClef8vb",
        Clef::Bass => "fClef",
        Clef::Bass8vb => "fClef8vb",
    }
}

/// `text` cut to `width` points (an ellipsis where cut).
fn fit(text: &str, font: Font, size: f32, width: f32) -> String {
    if text_width(text, font, size) <= width {
        return text.to_string();
    }
    let mut s = text.to_string();
    while !s.is_empty() && text_width(&format!("{s}…"), font, size) > width {
        s.pop();
    }
    format!("{}…", s.trim_end())
}

/// A score's bracket by its staves from `top` to `bottom` (their outer
/// lines), its thick line's left edge at `x`, a wing curling out at each
/// end.
fn bracket(ink: &mut Vec<Ink>, x: f32, top: f32, bottom: f32, sp: f32) {
    let th = BRACKET_THICKNESS * sp;
    let (t, b) = (top - 0.5 * sp, bottom + 0.5 * sp);
    quad(ink, [(x, t), (x + th, t), (x + th, b), (x, b)]);
    for (y, out) in [(t, 1.0), (b, -1.0)] {
        // `dy` staff spaces outwards (up at the top, down at the bottom).
        let p = |dx: f32, dy: f32| (x + dx * sp, y - out * dy * sp);
        let (c1, c2, tip) = (p(0.8, 0.1), p(1.4, 0.5), p(1.8, 1.0));
        let (d1, d2, back) = (p(1.3, 0.2), p(0.8, -0.35), p(th / sp, -0.4));
        let foot = p(0.0, -0.4);
        ink.push(Ink::Fill(vec![
            Seg::M(x, y),
            Seg::C(c1.0, c1.1, c2.0, c2.1, tip.0, tip.1),
            Seg::C(d1.0, d1.1, d2.0, d2.1, back.0, back.1),
            Seg::L(foot.0, foot.1),
            Seg::Z,
        ]));
    }
}

/// A grand staff's brace by its staves from `top` to `bottom` (their outer
/// lines), its tips at `x`: two halves meeting in a point at the middle,
/// each swelling from its tip and thinning to the point.
fn brace(ink: &mut Vec<Ink>, x: f32, top: f32, bottom: f32, sp: f32) {
    let h = (bottom - top) / 2.0;
    let (w, th) = (BRACE_WIDTH * sp, BRACE_SWELL * sp);
    for (tip, dir) in [(top, 1.0f32), (bottom, -1.0)] {
        // `f` of the way from the tip to the middle.
        let y = |f: f32| tip + dir * f * h;
        let shank = x - 0.5 * w;
        ink.push(Ink::Fill(vec![
            Seg::M(x, y(0.0)),
            Seg::C(
                x - 0.55 * w - 0.2 * th,
                y(0.02),
                shank - th,
                y(0.25),
                shank - th,
                y(0.45),
            ),
            Seg::C(
                shank - th,
                y(0.7),
                x - 0.55 * w - 0.3 * th,
                y(0.97),
                x - w,
                y(1.0),
            ),
            Seg::C(
                x - 0.55 * w + 0.3 * th,
                y(0.97),
                shank + th,
                y(0.7),
                shank + th,
                y(0.45),
            ),
            Seg::C(
                shank + th,
                y(0.25),
                x - 0.55 * w + 0.2 * th,
                y(0.02),
                x,
                y(0.0),
            ),
            Seg::Z,
        ]));
    }
}

/// A bar line at `x` from `a` down to `b` (the last bar's thin and thick).
fn bar_line(ink: &mut Vec<Ink>, x: f32, (a, b): (f32, f32), last: bool, sp: f32) {
    if last {
        let thick = E::THICK_BARLINE_THICKNESS * sp;
        let tx = x - thick / 2.0;
        line(ink, (tx, a), (tx, b), thick);
        let thin = tx - thick / 2.0 - E::THIN_THICK_BARLINE_SEPARATION * sp;
        line(ink, (thin, a), (thin, b), E::THIN_BARLINE_THICKNESS * sp);
    } else {
        line(ink, (x, a), (x, b), E::THIN_BARLINE_THICKNESS * sp);
    }
}

/// What every staff's drawing needs.
struct Engraver<'a> {
    sheet: &'a LeadSheet,
    sp: f32,
    /// The widest clef's advance (staff spaces).
    clef_w: f32,
    /// Each bar's plan.
    plans: Vec<BarPlan>,
}

impl Engraver<'_> {
    /// A time signature's width (staff spaces).
    fn time_w(m: &Measure) -> f32 {
        let digits = |n: u8| n.to_string().len() as f32;
        2.2 * digits(m.time.0).max(digits(m.time.1)) + 0.8
    }

    /// The room before a system's first bar (points): clef, key and time.
    fn prefix(&self, first: bool, m: &Measure) -> f32 {
        let key_w = self.sheet.key.fifths.unsigned_abs() as f32 * 1.05;
        (1.0 + self.clef_w
            + 0.8
            + key_w
            + if first || m.show_time {
                Self::time_w(m)
            } else {
                0.0
            }
            + 1.0)
            * self.sp
    }

    /// Staff `pi` of system `si` drawn with its top line at y = 0 (its
    /// notes noted in `placed` and everything drawn in `boxes`, by bar and
    /// event, on page 0).
    fn staff(
        &self,
        pi: usize,
        sys: &System,
        si: usize,
        placed: &mut [Vec<Option<Placed>>],
        boxes: &mut [Vec<Option<EventBox>>],
    ) -> Drawn {
        let sp = self.sp;
        let part = &self.sheet.parts[pi];
        if let Some(tab) = &part.tab {
            return self.tab_staff(part, tab, sys, si, placed);
        }
        let clef = part.clef;
        let mut ink = Vec::new();
        // Room for notes high or low on the staff.
        let (mut hi, mut lo) = (8i32, 0i32);
        for m in &part.measures[sys.bars.clone()] {
            for e in m.all_events() {
                for p in e.pitches() {
                    let s = staff_step(p.diatonic(), clef);
                    hi = hi.max(s);
                    lo = lo.min(s);
                }
            }
        }
        let top_extra = ((hi - 8) as f32 * 0.5 * sp).max(0.0);
        let bottom_extra = ((-lo) as f32 * 0.5 * sp).max(0.0);
        let step_y = |s: i32| 4.0 * sp - s as f32 * 0.5 * sp;
        let x_end = sys.spans.last().map_or(sys.left, |(x, w)| x + w);
        // The staff.
        for k in 0..5 {
            let ly = k as f32 * sp;
            line(
                &mut ink,
                (sys.left, ly),
                (x_end, ly),
                E::STAFF_LINE_THICKNESS * sp,
            );
        }
        // Clef, key, time.
        let mut x = sys.left + sp;
        let clef_step = match clef {
            Clef::Treble | Clef::Treble8vb => 2,
            Clef::Bass | Clef::Bass8vb => 6,
        };
        put(&mut ink, clef_glyph(clef), x, step_y(clef_step), sp);
        x += (self.clef_w + 0.8) * sp;
        let (sharps, flats) = ([8, 5, 9, 6, 3, 7, 4], [4, 7, 3, 6, 2, 5, 1]);
        let shift = if matches!(clef, Clef::Bass | Clef::Bass8vb) {
            -2
        } else {
            0
        };
        let key = self.sheet.key;
        for k in 0..key.fifths.unsigned_abs().min(7) as usize {
            let (g, s) = if key.fifths > 0 {
                ("accidentalSharp", sharps[k])
            } else {
                ("accidentalFlat", flats[k])
            };
            put(&mut ink, g, x, step_y(s + shift), sp);
            x += 1.05 * sp;
        }
        let first = &part.measures[sys.bars.start];
        if si == 0 || first.show_time {
            time_signature(&mut ink, first.time, x + 0.3 * sp, &step_y, sp);
        }
        let chord_y = -3.2 * sp - top_extra;
        // Dynamics under the staff, the words under them.
        let dynamics = part.measures[sys.bars.clone()]
            .iter()
            .any(|m| m.all_events().any(|e| e.dynamic.is_some()));
        let dynamic_y = 4.0 * sp + bottom_extra + 2.6 * sp;
        let lyric_y = 4.0 * sp
            + bottom_extra
            + 3.0 * sp
            + LYRIC_SIZE * 0.7
            + if dynamics { 2.6 * sp } else { 0.0 };
        for (k, mi) in sys.bars.clone().enumerate() {
            let m = &part.measures[mi];
            let plan = &self.plans[mi];
            // A time change inside the system.
            if k > 0 && m.show_time {
                let (bx, _) = sys.spans[k];
                time_signature(&mut ink, m.time, bx + 0.4 * sp, &step_y, sp);
            }
            let (bx, bw) = self.frame(sys, k, mi);
            let xs = plan.xs(bx, bw, sp);
            let n1 = m.events.len();
            let two = m.two_voices();
            // Notes and rests, the first voice's then the second's (stems
            // up and down while there are two).
            for (voice, list) in [(0usize, &m.events), (1, &m.under)] {
                let forced = two.then_some(voice == 0);
                let base = if voice == 0 { 0 } else { n1 };
                for (k, e) in list.iter().enumerate() {
                    let ei = base + k;
                    let x0 = plan.x(&xs, e.at);
                    let from = ink.len();
                    match e.pitch {
                        None if e.bar_rest => {
                            let w = advance("restWhole") * sp;
                            let gx = bx + bw / 2.0 - w / 2.0;
                            let s = match forced {
                                Some(true) => 8,
                                Some(false) => 2,
                                None => 6,
                            };
                            put(&mut ink, "restWhole", gx, step_y(s), sp);
                            boxes[mi][ei] = Some(EventBox::rest(
                                (gx, w),
                                step_y(s) + 0.5 * sp,
                                from,
                                ink.len(),
                                voice,
                            ));
                        }
                        None => {
                            let g = rest_glyph(e.duration.value);
                            let whole = e.duration.value == Value::Whole;
                            let s = if whole { 6 } else { 4 }
                                + match forced {
                                    Some(true) if whole => 2,
                                    Some(true) => 4,
                                    Some(false) => -4,
                                    None => 0,
                                };
                            put(&mut ink, g, x0, step_y(s), sp);
                            if e.duration.dots > 0 {
                                put(
                                    &mut ink,
                                    "augmentationDot",
                                    x0 + (advance(g) + 0.4) * sp,
                                    step_y(s + 1),
                                    sp,
                                );
                            }
                            let mid = if whole {
                                step_y(s) + 0.5 * sp
                            } else {
                                step_y(s)
                            };
                            boxes[mi][ei] = Some(EventBox::rest(
                                (x0, advance(g) * sp),
                                mid,
                                from,
                                ink.len(),
                                voice,
                            ));
                        }
                        Some(_) => {
                            let pitches: Vec<Pitch> = e.pitches().copied().collect();
                            let steps: Vec<i32> = pitches
                                .iter()
                                .map(|p| staff_step(p.diatonic(), clef))
                                .collect();
                            let (lo_s, hi_s) = (steps[0], steps[steps.len() - 1]);
                            let up = forced.unwrap_or(lo_s + hi_s < 8);
                            let head = notehead(e);
                            let hw = advance(head) * sp;
                            // A second's other note beside the stem: right
                            // of it when the stem goes up, left when it
                            // goes down.
                            let mut beside = vec![false; steps.len()];
                            if up {
                                for i in 1..steps.len() {
                                    if steps[i] - steps[i - 1] == 1 && !beside[i - 1] {
                                        beside[i] = true;
                                    }
                                }
                            } else {
                                for i in (0..steps.len().saturating_sub(1)).rev() {
                                    if steps[i + 1] - steps[i] == 1 && !beside[i + 1] {
                                        beside[i] = true;
                                    }
                                }
                            }
                            // Accidentals in columns before the noteheads.
                            let (cols, ncols) = accidental_columns(&pitches, clef);
                            let col_w = 1.15 * sp;
                            let left_heads = !up && beside.iter().any(|b| *b);
                            let mut nx = x0 + ncols as f32 * col_w;
                            if left_heads {
                                nx += hw;
                            }
                            let mut accidental_ink = vec![None; pitches.len()];
                            for (i, p) in pitches.iter().enumerate() {
                                if let (Some(c), Some(a)) = (cols[i], p.accidental) {
                                    let ax = nx
                                        - if left_heads { hw } else { 0.0 }
                                        - (c + 1) as f32 * col_w;
                                    accidental_ink[i] = Some(ink.len());
                                    put(&mut ink, accidental_glyph(a), ax, step_y(steps[i]), sp);
                                }
                            }
                            let ext = E::LEGER_LINE_EXTENSION * sp;
                            let right_heads = up && beside.iter().any(|b| *b);
                            let mut heads_box = Vec::with_capacity(steps.len());
                            for (i, s) in steps.iter().enumerate() {
                                let hx = match (beside[i], up) {
                                    (true, true) => nx + hw,
                                    (true, false) => nx - hw,
                                    _ => nx,
                                };
                                heads_box.push(Head {
                                    x: hx + hw / 2.0,
                                    y: step_y(*s),
                                    ink: ink.len(),
                                    accidental: accidental_ink[i],
                                });
                                put(&mut ink, head, hx, step_y(*s), sp);
                                // Ledger lines.
                                let mut l = -2;
                                while l >= *s {
                                    line(
                                        &mut ink,
                                        (hx - ext, step_y(l)),
                                        (hx + hw + ext, step_y(l)),
                                        E::LEGER_LINE_THICKNESS * sp,
                                    );
                                    l -= 2;
                                }
                                let mut l = 10;
                                while l <= *s {
                                    line(
                                        &mut ink,
                                        (hx - ext, step_y(l)),
                                        (hx + hw + ext, step_y(l)),
                                        E::LEGER_LINE_THICKNESS * sp,
                                    );
                                    l += 2;
                                }
                                let dot_x = nx + hw * if right_heads { 2.0 } else { 1.0 };
                                for d in 0..e.duration.dots {
                                    let dy = if s % 2 == 0 {
                                        step_y(s + 1)
                                    } else {
                                        step_y(*s)
                                    };
                                    put(
                                        &mut ink,
                                        "augmentationDot",
                                        dot_x + (0.35 + 0.5 * f32::from(d)) * sp,
                                        dy,
                                        sp,
                                    );
                                }
                            }
                            let mut heads = [0.0f32; MOST_HEADS];
                            for (h, s) in heads.iter_mut().zip(&steps) {
                                *h = step_y(*s);
                            }
                            placed[mi][ei] = Some(Placed {
                                page: 0,
                                system: si,
                                x: nx,
                                w: hw,
                                y: step_y(lo_s),
                                top: step_y(hi_s),
                                heads,
                                count: steps.len().min(MOST_HEADS),
                                up,
                            });
                            boxes[mi][ei] = Some(EventBox {
                                page: 0,
                                system: si,
                                x: nx,
                                w: hw,
                                heads: heads_box,
                                ink: (from, ink.len()),
                                voice: voice as u8,
                                rest: false,
                            });
                        }
                    }
                }
            }
            let (one, two_list) = placed[mi].split_at_mut(n1);
            stems_and_beams(&mut ink, &m.events, one, two.then_some(true), &step_y, sp);
            stems_and_beams(&mut ink, &m.under, two_list, Some(false), &step_y, sp);
            // Articulations and dynamics.
            for (voice, list) in [(0usize, &m.events), (1, &m.under)] {
                let base = if voice == 0 { 0 } else { n1 };
                for (k, e) in list.iter().enumerate() {
                    if e.tied_from {
                        continue;
                    }
                    if let Some(d) = e.dynamic {
                        let mut gx = plan.x(&xs, e.at);
                        for g in d.glyphs() {
                            put(&mut ink, g, gx, dynamic_y, sp);
                            gx += advance(g) * sp * 0.9;
                        }
                    }
                    let Some(pl) = placed[mi][base + k] else {
                        continue;
                    };
                    if e.marks.is_empty() {
                        continue;
                    }
                    // Opposite the stem; with two voices on the voice's
                    // side, clear of the stems and the staff.
                    let below = if two { voice == 1 } else { pl.up };
                    let cx = pl.x + pl.w / 2.0;
                    let mut edge = match (two, below) {
                        (true, true) => (pl.y + 4.0 * sp).max(4.6 * sp),
                        (true, false) => (pl.top - 4.0 * sp).min(-0.6 * sp),
                        (false, true) => pl.y + 0.9 * sp,
                        (false, false) => pl.top - 0.9 * sp,
                    };
                    for (above, _) in e.marks.glyphs() {
                        let name = if below {
                            above.replace("Above", "Below")
                        } else {
                            above.to_string()
                        };
                        let Some(g) = glyph(&name) else { continue };
                        let gx = cx - (g.bbox[0] + g.bbox[2]) / 2.0 * sp;
                        let height = (g.bbox[3] - g.bbox[1]) * sp;
                        if below {
                            put(&mut ink, &name, gx, edge + g.bbox[3] * sp, sp);
                            edge += height + 0.3 * sp;
                        } else {
                            put(&mut ink, &name, gx, edge + g.bbox[1] * sp, sp);
                            edge -= height + 0.3 * sp;
                        }
                    }
                }
            }
            // Chords over the bar (the top staff's).
            for c in &m.chords {
                draw_chord(&mut ink, c, plan.x(&xs, c.at), chord_y);
            }
        }
        draw_words(&mut ink, part, sys, placed, lyric_y, sp);
        Drawn {
            ink,
            top_extra,
            bottom_extra,
        }
    }

    /// Where bar `mi` (the `k`th of `sys`) puts its notes: its left edge
    /// and width, less a time signature changing inside the system.
    fn frame(&self, sys: &System, k: usize, mi: usize) -> (f32, f32) {
        let (mut bx, mut bw) = sys.spans[k];
        let m = &self.sheet.parts[0].measures[mi];
        if k > 0 && m.show_time {
            let tw = Self::time_w(m) * self.sp;
            bx += tw;
            bw -= tw;
        }
        (bx, bw)
    }

    /// A tablature staff of system `si` with its top line at y = 0: a line
    /// a string, broken where a fret number stands, its clef (no key: the
    /// key's room stays so its bars line up with the staves above), its
    /// metre and frets; standing alone, its rhythm under it.
    fn tab_staff(
        &self,
        part: &Part,
        tab: &TabStaff,
        sys: &System,
        si: usize,
        placed: &mut [Vec<Option<Placed>>],
    ) -> Drawn {
        let sp = self.sp;
        let gap = TAB_GAP * sp;
        let strings = tab.strings.len().max(1);
        let h = (strings - 1) as f32 * gap;
        let string_y = |s: u8| (f32::from(s.max(1)) - 1.0) * gap;
        let mut ink = Vec::new();
        let x_end = sys.spans.last().map_or(sys.left, |(x, w)| x + w);
        // Where the lines give way to fret numbers: (string, from, to).
        let mut breaks: Vec<(u8, f32, f32)> = Vec::new();
        let size = FRET_SIZE * sp;
        let head_w = advance("noteheadBlack") * sp;
        let mut x = sys.left + sp;
        put(&mut ink, tab_clef(strings), x, h / 2.0, sp);
        x += (self.clef_w + 0.8) * sp;
        x += self.sheet.key.fifths.unsigned_abs() as f32 * 1.05 * sp;
        // The metre's digits either side of the middle.
        let time_y = |s: i32| h / 2.0 + (4 - s) as f32 * 0.5 * sp;
        let first = &part.measures[sys.bars.start];
        if si == 0 || first.show_time {
            time_signature(&mut ink, first.time, x + 0.3 * sp, &time_y, sp);
        }
        let (rhythm_top, rhythm_end) = (h + 1.0 * sp, h + 3.4 * sp);
        let lyric_y = if tab.rhythm {
            rhythm_end + 1.0 * sp
        } else {
            h + 1.5 * sp
        } + 1.5 * sp
            + LYRIC_SIZE * 0.7;
        for (k, mi) in sys.bars.clone().enumerate() {
            let m = &part.measures[mi];
            let plan = &self.plans[mi];
            let (mut bx, mut bw) = sys.spans[k];
            if k > 0 && m.show_time {
                time_signature(&mut ink, m.time, bx + 0.4 * sp, &time_y, sp);
                let tw = Self::time_w(m) * sp;
                bx += tw;
                bw -= tw;
            }
            let xs = plan.xs(bx, bw, sp);
            // Each event's middle (where a notehead's would be).
            let centre: Vec<f32> = m
                .events
                .iter()
                .map(|e| plan.x(&xs, e.at) + head_w / 2.0)
                .collect();
            for (ei, e) in m.events.iter().enumerate() {
                let cx = centre[ei];
                if e.is_rest() {
                    if tab.rhythm {
                        let (g, rx) = if e.bar_rest {
                            ("restWhole", bx + bw / 2.0)
                        } else {
                            (rest_glyph(e.duration.value), cx)
                        };
                        let y = (rhythm_top + rhythm_end) / 2.0;
                        put(&mut ink, g, rx - advance(g) * sp / 2.0, y, sp);
                    }
                    continue;
                }
                for f in &e.frets {
                    let text = fret_text(f.fret, e.tied_from);
                    let w = text_width(&text, Font::Sans, size);
                    let y = string_y(f.string);
                    breaks.push((f.string, cx - w / 2.0 - 0.15 * sp, cx + w / 2.0 + 0.15 * sp));
                    ink.push(Ink::Text {
                        text,
                        x: cx,
                        y: y + size * 0.35,
                        size,
                        font: Font::Sans,
                        anchor: Anchor::Middle,
                    });
                }
                placed[mi][ei] = Some(Placed {
                    page: 0,
                    system: si,
                    x: cx - head_w / 2.0,
                    w: head_w,
                    y: h / 2.0,
                    top: h / 2.0,
                    heads: [h / 2.0; MOST_HEADS],
                    count: 1,
                    up: false,
                });
            }
            if tab.rhythm {
                tab_rhythm(&mut ink, m, &centre, (rhythm_top, rhythm_end), sp);
            }
            // Chords over the bar (the top staff's).
            for c in &m.chords {
                draw_chord(&mut ink, c, plan.x(&xs, c.at), -3.2 * sp);
            }
        }
        draw_words(&mut ink, part, sys, placed, lyric_y, sp);
        // The lines, broken around the numbers.
        for s in 1..=strings as u8 {
            let y = string_y(s);
            let mut cuts: Vec<(f32, f32)> = breaks
                .iter()
                .filter(|b| b.0 == s)
                .map(|b| (b.1, b.2))
                .collect();
            cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut from = sys.left;
            for (a, b) in cuts {
                if a > from {
                    line(&mut ink, (from, y), (a, y), E::STAFF_LINE_THICKNESS * sp);
                }
                from = from.max(b);
            }
            if x_end > from {
                line(
                    &mut ink,
                    (from, y),
                    (x_end, y),
                    E::STAFF_LINE_THICKNESS * sp,
                );
            }
        }
        Drawn {
            ink,
            top_extra: 0.0,
            bottom_extra: 0.0,
        }
    }
}

/// A system's words under its notes (each centred under its note): a
/// hyphen between a word's syllables (after the last on a line when the
/// word goes on in the next system), a line under the notes a word's last
/// syllable is held over.
fn draw_words(
    ink: &mut Vec<Ink>,
    part: &Part,
    sys: &System,
    placed: &[Vec<Option<Placed>>],
    lyric_y: f32,
    sp: f32,
) {
    let mut open: Option<(f32, f32)> = None;
    let close = |ink: &mut Vec<Ink>, open: Option<(f32, f32)>| {
        if let Some((a, b)) = open
            && b > a
        {
            line(
                ink,
                (a, lyric_y + 1.0),
                (b, lyric_y + 1.0),
                E::LYRIC_LINE_THICKNESS * sp,
            );
        }
    };
    let hyphen = |ink: &mut Vec<Ink>, x: f32| {
        ink.push(Ink::Text {
            text: "-".into(),
            x,
            y: lyric_y,
            size: LYRIC_SIZE,
            font: Font::Serif,
            anchor: Anchor::Middle,
        });
    };
    // A syllable waiting for the rest of its word: where its text ends.
    let mut hyphen_from: Option<f32> = None;
    for mi in sys.bars.clone() {
        for (ei, e) in part.measures[mi].events.iter().enumerate() {
            let Some(pl) = placed[mi][ei] else { continue };
            let cx = pl.x + pl.w / 2.0;
            if let Some(l) = &e.lyric {
                close(ink, open.take());
                let half = text_width(&l.text, Font::Serif, LYRIC_SIZE) / 2.0;
                if let Some(from) = hyphen_from.take() {
                    let to = cx - half;
                    hyphen(
                        ink,
                        if to > from {
                            (from + to) / 2.0
                        } else {
                            from + 2.0
                        },
                    );
                }
                ink.push(Ink::Text {
                    text: l.text.clone(),
                    x: cx,
                    y: lyric_y,
                    size: LYRIC_SIZE,
                    font: Font::Serif,
                    anchor: Anchor::Middle,
                });
                if l.syllabic.continues() {
                    hyphen_from = Some(cx + half);
                } else if l.extend {
                    let start = cx + half + 1.5;
                    open = Some((start, start));
                }
            } else if (e.held || e.tied_from) && open.is_some() {
                if let Some(o) = &mut open {
                    o.1 = pl.x + pl.w;
                }
            } else {
                close(ink, open.take());
            }
        }
    }
    close(ink, open.take());
    // The word goes on in the next system.
    if let Some(from) = hyphen_from {
        hyphen(ink, from + 0.6 * sp);
    }
}

/// A tablature clef for so many strings.
fn tab_clef(strings: usize) -> &'static str {
    if strings >= 6 {
        "6stringTabClef"
    } else {
        "4stringTabClef"
    }
}

/// A tablature's rhythm under it: stems down from `top` to `end` (a half's
/// short, a whole's none), beams along the stems' ends, flags, dots and
/// triplets' threes.
fn tab_rhythm(ink: &mut Vec<Ink>, m: &Measure, centre: &[f32], (top, end): (f32, f32), sp: f32) {
    let stem_w = E::STEM_THICKNESS * sp;
    let n = m.events.len();
    for (i, e) in m.events.iter().enumerate() {
        if e.is_rest() || e.duration.value == Value::Whole {
            continue;
        }
        let cx = centre[i];
        let from = if e.duration.value == Value::Half {
            (top + end) / 2.0
        } else {
            top
        };
        line(ink, (cx, from), (cx, end), stem_w);
        for d in 0..e.duration.dots {
            put(
                ink,
                "augmentationDot",
                cx + (0.45 + 0.5 * f32::from(d)) * sp,
                end - 0.8 * sp,
                sp,
            );
        }
        if e.beams[0].is_none() {
            let flag = match e.duration.value {
                Value::Eighth => Some("flag8thDown"),
                Value::Sixteenth => Some("flag16thDown"),
                _ => None,
            };
            if let Some(f) = flag {
                put(ink, f, cx - stem_w / 2.0, end, sp);
            }
        }
    }
    let thick = E::BEAM_THICKNESS * sp;
    let level = (E::BEAM_THICKNESS + E::BEAM_SPACING) * sp;
    let band = |ink: &mut Vec<Ink>, x0: f32, x1: f32, l: f32| {
        let y = end - l * level;
        quad(ink, [(x0, y - thick), (x1, y - thick), (x1, y), (x0, y)]);
    };
    let mut i = 0;
    while i < n {
        if m.events[i].beams[0] != Some(Beam::Begin) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && m.events[j].beams[0].is_some() {
            j += 1;
            if m.events[j - 1].beams[0] == Some(Beam::End) {
                break;
            }
        }
        band(
            ink,
            centre[i] - stem_w / 2.0,
            centre[j - 1] + stem_w / 2.0,
            0.0,
        );
        for k in i..j {
            let x = centre[k];
            match m.events[k].beams[1] {
                Some(Beam::Begin) => {
                    if let Some(z) = (k + 1..j).find(|z| m.events[*z].beams[1] == Some(Beam::End)) {
                        band(ink, x - stem_w / 2.0, centre[z] + stem_w / 2.0, 1.0);
                    }
                }
                Some(Beam::ForwardHook) => band(ink, x - stem_w / 2.0, x + 1.1 * sp, 1.0),
                Some(Beam::BackwardHook) => band(ink, x - 1.1 * sp, x + stem_w / 2.0, 1.0),
                _ => {}
            }
        }
        if m.events[i].tuplet == Some(Tuplet::Start) && m.events[i].duration.triplet {
            let mid = (centre[i] + centre[j - 1]) / 2.0;
            put(
                ink,
                "tuplet3",
                mid - advance("tuplet3") * sp * 0.4,
                end + 1.4 * sp,
                sp * 0.8,
            );
        }
        i = j;
    }
}

pub fn engrave(sheet: &LeadSheet, layout: &Layout) -> Vec<Page> {
    engrave_mapped(sheet, layout).0
}

/// [`engrave`], and where everything went ([`Map`]).
pub fn engrave_mapped(sheet: &LeadSheet, layout: &Layout) -> (Vec<Page>, Map) {
    let sp = layout.space;
    let left = layout.margin;
    let right = layout.width - layout.margin;
    let parts = &sheet.parts;
    let n = parts.len();
    let bars = parts.iter().map(|p| p.measures.len()).min().unwrap_or(0);
    // Instruments (a grand staff's two staves are one): a score has
    // several, named and bracketed; grand staves are braced.
    let instruments = sheet.instruments();
    let score = instruments.len() > 1;
    let braced = instruments.iter().any(|(_, k)| *k == 2);
    let brace_room = if braced { BRACE_ROOM } else { 0.0 };
    let en = Engraver {
        sheet,
        sp,
        clef_w: parts
            .iter()
            .map(|p| match &p.tab {
                Some(t) => advance(tab_clef(t.strings.len())),
                None => advance(clef_glyph(p.clef)),
            })
            .fold(0.0, f32::max),
        plans: (0..bars)
            .map(|mi| {
                let staves: Vec<&Measure> = parts.iter().map(|p| &p.measures[mi]).collect();
                BarPlan::new(&staves, sp)
            })
            .collect(),
    };
    // A score's part names: in full before the first system, short before
    // the others.
    let name_room = (right - left) * 0.22;
    let label = |p: &Part, first: bool| {
        let name = if first {
            p.name.clone()
        } else {
            p.abbreviation()
        };
        fit(&name, Font::Serif, NAME_SIZE, name_room)
    };
    let indent = |first: bool| {
        if !score {
            return brace_room * sp;
        }
        parts
            .iter()
            .map(|p| text_width(&label(p, first), Font::Serif, NAME_SIZE))
            .fold(0.0, f32::max)
            + (BRACKET_ROOM + brace_room) * sp
    };
    // Each bar's least width.
    let widths: Vec<f32> = en.plans.iter().map(|p| p.width(sp)).collect();
    // Systems: up to four bars, as many as fit.
    let time_of = |k: usize| &parts[0].measures[k];
    let mut systems: Vec<System> = Vec::new();
    let mut i = 0;
    if layout.continuous && bars > 0 {
        // One system of every bar at its own width (a little roomier),
        // a time change's signature in the bar it starts.
        let sys_left = left + indent(true);
        let pre = en.prefix(true, time_of(0));
        let mut x = sys_left + pre;
        let spans = (0..bars)
            .map(|k| {
                let extra = if k > 0 && time_of(k).show_time {
                    Engraver::time_w(time_of(k)) * sp
                } else {
                    0.0
                };
                let w = widths[k] * 1.25 + extra;
                let s = (x, w);
                x += w;
                s
            })
            .collect();
        systems.push(System {
            bars: 0..bars,
            left: sys_left,
            spans,
            prefix: pre,
        });
        i = bars;
    }
    while i < bars {
        let first = systems.is_empty();
        let sys_left = left + indent(first);
        let usable = right - sys_left;
        let pre = en.prefix(first, time_of(i));
        let mut j = i;
        let mut sum = 0.0;
        // Bars may close up to 80 % of their room to share a line.
        const SQUEEZE: f32 = 0.8;
        while j < bars
            && j - i < layout.bars_per_system
            && (j == i || pre + (sum + widths[j]) * SQUEEZE <= usable)
        {
            // A time change inside a system takes room for its signature.
            let extra = if j > i && time_of(j).show_time {
                Engraver::time_w(time_of(j)) * sp
            } else {
                0.0
            };
            if j > i && pre + (sum + widths[j] + extra) * SQUEEZE > usable {
                break;
            }
            sum += widths[j] + extra;
            j += 1;
        }
        let last = j == bars;
        let room_left = usable - pre;
        let scale = (room_left / sum.max(1.0)).max(0.1);
        // The last system is not stretched beyond half again.
        let scale = if last && j - i < layout.bars_per_system {
            scale.min(1.5)
        } else {
            scale
        };
        let mut x = sys_left + pre;
        let spans = (i..j)
            .map(|k| {
                let w = widths[k] * scale;
                let s = (x, w);
                x += w;
                s
            })
            .collect();
        systems.push(System {
            bars: i..j,
            left: sys_left,
            spans,
            prefix: pre,
        });
        i = j;
    }
    // Pages: systems stacked with room for chords above and words below.
    let mut pages = vec![Page {
        width: layout.width,
        height: layout.height,
        ink: Vec::new(),
    }];
    let mut y = layout.margin;
    // The heading (none on a strip).
    if !layout.continuous {
        let ink = &mut pages[0].ink;
        ink.push(Ink::Text {
            text: sheet.title.clone(),
            x: layout.width / 2.0,
            y: y + 20.0,
            size: 20.0,
            font: Font::SerifBold,
            anchor: Anchor::Middle,
        });
        y += 30.0;
        if !sheet.composer.is_empty() {
            ink.push(Ink::Text {
                text: sheet.composer.clone(),
                x: right,
                y: y + 8.0,
                size: 10.5,
                font: Font::SerifItalic,
                anchor: Anchor::End,
            });
        }
        if let Some(t) = sheet.tempo {
            put(ink, "metNoteQuarterUp", left, y + 8.0, sp * 0.9);
            ink.push(Ink::Text {
                text: format!(" = {}", t.round()),
                x: left + advance("metNoteQuarterUp") * sp * 0.9,
                y: y + 8.0,
                size: 10.5,
                font: Font::Serif,
                anchor: Anchor::Start,
            });
        }
        y += 18.0;
    }
    let mut placed: Vec<Vec<Vec<Option<Placed>>>> = parts
        .iter()
        .map(|p| {
            p.measures
                .iter()
                .map(|m| vec![None; m.events.len() + m.under.len()])
                .collect()
        })
        .collect();
    let mut boxes: Vec<Vec<Vec<Option<EventBox>>>> = parts
        .iter()
        .map(|p| {
            p.measures
                .iter()
                .map(|m| vec![None; m.events.len() + m.under.len()])
                .collect()
        })
        .collect();
    let mut system_maps: Vec<SystemMap> = Vec::new();
    let joined = sheet.bar_lines_joined();
    // Where each system's bars' notes begin and where it ends.
    let mut system_edges: Vec<(f32, f32)> = Vec::new();
    for (si, sys) in systems.iter().enumerate() {
        let drawn: Vec<Drawn> = (0..n)
            .map(|pi| en.staff(pi, sys, si, &mut placed[pi], &mut boxes[pi]))
            .collect();
        // Each staff's room above and below its lines: what it reaches,
        // and at least room for the chords over the top staff, for the
        // words under a staff that has them and a gap to the next system
        // under the last.
        let heights: Vec<f32> = parts.iter().map(|p| staff_height(p) * sp).collect();
        let rooms: Vec<(f32, f32)> = drawn
            .iter()
            .enumerate()
            .map(|(pi, d)| {
                let (lo, hi) = extent(&d.ink);
                let h = heights[pi];
                let above = d.top_extra
                    + if pi == 0 {
                        4.2 * sp + CHORD_SIZE
                    } else {
                        3.0 * sp
                    };
                let below = d.bottom_extra
                    + if parts[pi].has_lyrics() {
                        4.0 * sp + LYRIC_SIZE + 6.0
                    } else if pi + 1 == n {
                        5.0 * sp
                    } else {
                        3.0 * sp
                    };
                (above.max(0.6 * sp - lo), below.max(hi - h + 0.6 * sp))
            })
            .collect();
        let mut tops = Vec::with_capacity(n);
        let mut height = 0.0;
        for (pi, (above, below)) in rooms.iter().enumerate() {
            tops.push(height + above);
            height += above + heights[pi] + below;
        }
        // A system that does not fit goes on the next page (whole).
        if !layout.continuous
            && y + height > layout.height - layout.margin
            && !pages.last().is_some_and(|p| p.ink.is_empty())
        {
            pages.push(Page {
                width: layout.width,
                height: layout.height,
                ink: Vec::new(),
            });
            y = layout.margin;
        }
        let page = pages.len() - 1;
        for t in &mut tops {
            *t += y;
        }
        let ink = &mut pages[page].ink;
        for (pi, d) in drawn.into_iter().enumerate() {
            let base = ink.len();
            ink.extend(shifted(d.ink, tops[pi]));
            for mi in sys.bars.clone() {
                for pl in placed[pi][mi].iter_mut().flatten() {
                    pl.y += tops[pi];
                    pl.top += tops[pi];
                    for h in &mut pl.heads {
                        *h += tops[pi];
                    }
                    pl.page = page;
                }
                for b in boxes[pi][mi].iter_mut().flatten() {
                    b.page = page;
                    b.ink = (b.ink.0 + base, b.ink.1 + base);
                    for h in &mut b.heads {
                        h.y += tops[pi];
                        h.ink += base;
                        h.accidental = h.accidental.map(|a| a + base);
                    }
                }
            }
        }
        system_maps.push(SystemMap {
            page,
            bars: sys.bars.clone(),
            staves: tops.clone(),
            heights: heights.clone(),
            spans: sys
                .bars
                .clone()
                .enumerate()
                .map(|(k, mi)| {
                    let (bx, bw) = en.frame(sys, k, mi);
                    let plan = &en.plans[mi];
                    // Times are where their noteheads' middles are.
                    let half = advance("noteheadBlack") * sp / 2.0;
                    let xs = plan.xs(bx, bw, sp).into_iter().map(|x| x + half);
                    BarSpan {
                        bar: mi,
                        x: sys.spans[k].0,
                        width: sys.spans[k].1,
                        points: plan.points.iter().copied().zip(xs).collect(),
                        length: time_of(mi).length(),
                    }
                })
                .collect(),
            top: y,
            bottom: y + height,
            left: sys.left + sys.prefix,
            right: sys.spans.last().map_or(sys.left, |(x, w)| x + w),
        });
        let (top, bottom) = (tops[0], tops[n - 1] + heights[n - 1]);
        // The line that joins the staves at the left; a score's bracket and
        // its parts' names.
        line(
            ink,
            (sys.left, top),
            (sys.left, bottom),
            E::THIN_BARLINE_THICKNESS * sp,
        );
        // A grand staff's brace, by its staves.
        for &(pi, k) in &instruments {
            if k == 2 {
                brace(
                    ink,
                    sys.left - 0.35 * sp,
                    tops[pi],
                    tops[pi + 1] + heights[pi + 1],
                    sp,
                );
            }
        }
        if score {
            let bx = sys.left - (1.1 + brace_room) * sp;
            bracket(ink, bx, top, bottom, sp);
            for &(pi, k) in &instruments {
                let (a, z) = (tops[pi], tops[pi + k - 1] + heights[pi + k - 1]);
                ink.push(Ink::Text {
                    text: label(&parts[pi], si == 0),
                    x: sys.left - (BRACKET_ROOM + brace_room) * sp,
                    y: (a + z) / 2.0 + NAME_SIZE * 0.35,
                    size: NAME_SIZE,
                    font: Font::Serif,
                    anchor: Anchor::End,
                });
            }
        }
        // The bar lines: through the system when its parts are joined,
        // else instrument by instrument (a grand staff's through both its
        // staves unless words are sung under the upper one).
        let runs: Vec<(f32, f32)> = if joined {
            vec![(top, bottom)]
        } else {
            let mut runs = Vec::new();
            for &(pi, k) in &instruments {
                if k == 2 && !parts[pi].has_lyrics() {
                    runs.push((tops[pi], tops[pi + 1] + heights[pi + 1]));
                } else {
                    for i in pi..pi + k {
                        runs.push((tops[i], tops[i] + heights[i]));
                    }
                }
            }
            runs
        };
        for (k, mi) in sys.bars.clone().enumerate() {
            let (bx, bw) = sys.spans[k];
            for run in &runs {
                bar_line(ink, bx + bw, *run, mi + 1 == bars, sp);
            }
        }
        // The bar number.
        if si > 0 {
            ink.push(Ink::Text {
                text: (sys.bars.start + 1).to_string(),
                x: sys.left,
                y: top - 1.2 * sp,
                size: 8.0,
                font: Font::SerifItalic,
                anchor: Anchor::Start,
            });
        }
        let x_end = sys.spans.last().map_or(sys.left, |(x, w)| x + w);
        system_edges.push((sys.left + sys.prefix - 1.5 * sp, x_end));
        y += height;
    }
    // Ties (within a system, or to its end and from the next one's start).
    for (pi, part) in parts.iter().enumerate() {
        // Tablature brackets a tied note's fret instead.
        if part.tab.is_some() {
            continue;
        }
        let placed = &placed[pi];
        // Each voice's events in order: (bar, index in its events, the
        // event; with two voices in its bar, which way its ties bow).
        let voice = |second: bool| -> Vec<(usize, usize, &Event, Option<bool>)> {
            part.measures
                .iter()
                .enumerate()
                .take(bars)
                .flat_map(move |(mi, m)| {
                    let (list, base) = if second {
                        (&m.under, m.events.len())
                    } else {
                        (&m.events, 0)
                    };
                    let bow = m.two_voices().then_some(!second);
                    list.iter()
                        .enumerate()
                        .map(move |(k, e)| (mi, base + k, e, bow))
                })
                .collect()
        };
        for flat in [voice(false), voice(true)] {
            for (k, &(mi, ei, e, bow)) in flat.iter().enumerate() {
                if !e.tie {
                    continue;
                }
                let Some(a) = placed[mi][ei] else { continue };
                let Some(&(nm, ne, _, _)) = flat.get(k + 1) else {
                    continue;
                };
                let Some(b) = placed[nm][ne] else { continue };
                // A tie a note: a single note's against its stem, a chord's
                // upper half up and lower half down; with two voices the first
                // voice's up, the second's down.
                for h in 0..a.count.min(b.count).max(1) {
                    let up = match bow {
                        Some(up) => up,
                        None if a.count <= 1 => !a.up,
                        None => h >= a.count / 2,
                    };
                    let (ay, by) = (a.heads[h], b.heads[h]);
                    let dy = if up { -0.55 * sp } else { 0.55 * sp };
                    let from = (a.x + a.w + 0.15 * sp, ay + dy);
                    if a.system == b.system {
                        tie(
                            &mut pages[a.page].ink,
                            from,
                            (b.x - 0.15 * sp, by + dy),
                            up,
                            sp,
                        );
                    } else {
                        let (_, end) = system_edges[a.system];
                        tie(
                            &mut pages[a.page].ink,
                            from,
                            (end - 0.3 * sp, ay + dy),
                            up,
                            sp,
                        );
                        let (start, _) = system_edges[b.system];
                        tie(
                            &mut pages[b.page].ink,
                            (start, by + dy),
                            (b.x - 0.15 * sp, by + dy),
                            up,
                            sp,
                        );
                    }
                }
            }
        }
    }
    // A strip's page is as wide and as tall as its system.
    if layout.continuous
        && let (Some(page), Some(last)) = (pages.first_mut(), system_maps.last())
    {
        page.width = last.right + layout.margin;
        page.height = y + layout.margin;
    }
    // Page numbers after the first.
    let count = pages.len();
    for (i, p) in pages.iter_mut().enumerate().skip(1) {
        p.ink.push(Ink::Text {
            text: format!("{} / {count}", i + 1),
            x: layout.width / 2.0,
            y: layout.height - layout.margin / 2.0,
            size: 8.0,
            font: Font::Serif,
            anchor: Anchor::Middle,
        });
    }
    (
        pages,
        Map {
            space: sp,
            systems: system_maps,
            events: boxes,
        },
    )
}

fn time_signature(
    ink: &mut Vec<Ink>,
    (num, den): (u8, u8),
    x: f32,
    step_y: &impl Fn(i32) -> f32,
    sp: f32,
) {
    let digits = |n: u8| -> Vec<&'static str> {
        n.to_string()
            .chars()
            .map(|c| match c {
                '0' => "timeSig0",
                '1' => "timeSig1",
                '2' => "timeSig2",
                '3' => "timeSig3",
                '4' => "timeSig4",
                '5' => "timeSig5",
                '6' => "timeSig6",
                '7' => "timeSig7",
                '8' => "timeSig8",
                _ => "timeSig9",
            })
            .collect()
    };
    let width = |d: &[&str]| d.iter().map(|g| advance(g)).sum::<f32>() * sp;
    let (top, bottom) = (digits(num), digits(den));
    let w = width(&top).max(width(&bottom));
    for (ds, s) in [(top, 6), (bottom, 2)] {
        let mut gx = x + (w - width(&ds)) / 2.0;
        for g in ds {
            put(ink, g, gx, step_y(s), sp);
            gx += advance(g) * sp;
        }
    }
}

/// Stems, flags, beams and triplet numbers for a bar's notes.
/// A voice's stems, flags and beams (`forced`: its stems' direction while
/// there are two voices).
fn stems_and_beams(
    ink: &mut Vec<Ink>,
    events: &[Event],
    placed: &mut [Option<Placed>],
    forced: Option<bool>,
    step_y: &impl Fn(i32) -> f32,
    sp: f32,
) {
    let stem_w = E::STEM_THICKNESS * sp;
    let middle = step_y(4);
    let n = events.len();
    // Beamed runs share a direction (away from the notes furthest out).
    let mut i = 0;
    while i < n {
        let e = &events[i];
        if e.beams[0] == Some(Beam::Begin) {
            let mut j = i;
            while j < n && events[j].beams[0].is_some() {
                j += 1;
                if events[j - 1].beams[0] == Some(Beam::End) {
                    break;
                }
            }
            let run: Vec<usize> = (i..j).filter(|k| placed[*k].is_some()).collect();
            if run.len() >= 2 {
                beam_run(ink, events, placed, &run, forced, middle, sp, stem_w);
            }
            i = j;
            continue;
        }
        // A lone note: its stem and flag.
        if let Some(pl) = placed[i]
            && e.duration.value != Value::Whole
        {
            // (Chosen where the notes were drawn: away from the chord's
            // middle.)
            let up = pl.up;
            let head = glyph(notehead(e));
            let (ax, ay) = match (head, up) {
                (Some(g), true) => (g.stem_up[0], g.stem_up[1]),
                (Some(g), false) => (g.stem_down[0], g.stem_down[1]),
                (None, _) => (1.18, 0.0),
            };
            let x = pl.x + ax * sp + if up { -stem_w / 2.0 } else { stem_w / 2.0 };
            // From the far note through the near one.
            let (from, to) = if up { (pl.y, pl.top) } else { (pl.top, pl.y) };
            let y0 = from - ay * sp;
            let mut len = 3.5 * sp
                + if e.duration.value == Value::Sixteenth {
                    0.5 * sp
                } else {
                    0.0
                };
            // Reach the middle line from far away.
            if up && to - len > middle {
                len = to - middle;
            }
            if !up && to + len < middle {
                len = middle - to;
            }
            let y1 = if up {
                to - ay * sp - len
            } else {
                to - ay * sp + len
            };
            line(ink, (x, y0), (x, y1), stem_w);
            let flag = match (e.duration.value, up) {
                (Value::Eighth, true) => Some("flag8thUp"),
                (Value::Eighth, false) => Some("flag8thDown"),
                (Value::Sixteenth, true) => Some("flag16thUp"),
                (Value::Sixteenth, false) => Some("flag16thDown"),
                _ => None,
            };
            if let Some(f) = flag {
                put(ink, f, x - stem_w / 2.0, y1, sp);
            }
            placed[i] = Some(Placed { up, ..pl });
            // An unbeamed triplet's bracket and number.
            if e.tuplet == Some(Tuplet::Start) && e.duration.triplet {
                let mut k = i + 1;
                while k < n && events[k].tuplet == Some(Tuplet::Middle) {
                    k += 1;
                }
                if let (Some(a), Some(b)) = (placed[i], placed.get(k).copied().flatten()) {
                    let y = a.y.min(b.y) - 4.5 * sp;
                    let (x0, x1) = (a.x, b.x + b.w);
                    let mid = (x0 + x1) / 2.0;
                    let t = E::TUPLET_BRACKET_THICKNESS * sp;
                    line(ink, (x0, y + 0.6 * sp), (x0, y), t);
                    line(ink, (x0, y), (mid - 0.8 * sp, y), t);
                    line(ink, (mid + 0.8 * sp, y), (x1, y), t);
                    line(ink, (x1, y), (x1, y + 0.6 * sp), t);
                    put(
                        ink,
                        "tuplet3",
                        mid - advance("tuplet3") * sp * 0.4,
                        y + 0.4 * sp,
                        sp * 0.8,
                    );
                }
            }
        }
        i += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn beam_run(
    ink: &mut Vec<Ink>,
    events: &[Event],
    placed: &mut [Option<Placed>],
    run: &[usize],
    forced: Option<bool>,
    middle: f32,
    sp: f32,
    stem_w: f32,
) {
    let notes: Vec<Placed> = run.iter().filter_map(|k| placed[*k]).collect();
    // Up when the notes sit lower on average.
    let below: f32 = notes.iter().map(|p| (p.y + p.top) / 2.0 - middle).sum();
    let up = forced.unwrap_or(below > 0.0);
    // A chord's stem runs from its far note to the beam past its near one.
    let near = |p: &Placed| if up { p.top } else { p.y };
    let far = |p: &Placed| if up { p.y } else { p.top };
    let stem_x = |p: &Placed, e: &Event| {
        let g = glyph(notehead(e));
        if up {
            p.x + g.map_or(1.18, |g| g.stem_up[0]) * sp - stem_w / 2.0
        } else {
            p.x + g.map_or(0.0, |g| g.stem_down[0]) * sp + stem_w / 2.0
        }
    };
    let first = (stem_x(&notes[0], &events[run[0]]), near(&notes[0]));
    let last_i = notes.len() - 1;
    let last = (
        stem_x(&notes[last_i], &events[run[last_i]]),
        near(&notes[last_i]),
    );
    // The beam's slope follows the notes, gently.
    let dx = (last.0 - first.0).max(1.0);
    let slope = ((last.1 - first.1) / dx).clamp(-0.25, 0.25) * 0.6;
    let dir = if up { -1.0 } else { 1.0 };
    let levels = run
        .iter()
        .map(|k| events[*k].duration.value.beams())
        .max()
        .unwrap_or(1);
    let reach = (3.25 + 0.5 * f32::from(levels.saturating_sub(1))) * sp;
    // Start so every stem is long enough.
    let mut y0 = first.1 + dir * reach;
    for (p, k) in notes.iter().zip(run) {
        let x = stem_x(p, &events[*k]);
        let need = near(p) + dir * reach;
        let at = y0 + slope * (x - first.0);
        if up && at > need {
            y0 -= at - need;
        }
        if !up && at < need {
            y0 += need - at;
        }
    }
    let beam_y = |x: f32| y0 + slope * (x - first.0);
    let thick = E::BEAM_THICKNESS * sp;
    let gap = (E::BEAM_THICKNESS + E::BEAM_SPACING) * sp;
    // Stems to the beam.
    for (idx, (p, k)) in notes.iter().zip(run).enumerate() {
        let e = &events[*k];
        let x = stem_x(p, e);
        let head = glyph(notehead(e));
        let ay = head.map_or(0.0, |g| if up { g.stem_up[1] } else { g.stem_down[1] });
        line(ink, (x, far(p) - ay * sp), (x, beam_y(x)), stem_w);
        placed[run[idx]] = Some(Placed { up, ..*p });
    }
    // The beams: the first along the run, the second where sixteenths meet
    // (and hooks).
    let inward = -dir;
    let band = |ink: &mut Vec<Ink>, x0: f32, x1: f32, level: u8| {
        let off = f32::from(level) * gap * inward;
        let (a, b) = (beam_y(x0) + off, beam_y(x1) + off);
        quad(
            ink,
            [
                (x0, a),
                (x1, b),
                (x1, b + thick * inward),
                (x0, a + thick * inward),
            ],
        );
    };
    band(ink, first.0 - stem_w / 2.0, last.0 + stem_w / 2.0, 0);
    for (idx, k) in run.iter().enumerate() {
        let e = &events[*k];
        let x = stem_x(&notes[idx], e);
        match e.beams[1] {
            Some(Beam::Begin) => {
                if let Some(end) =
                    (idx + 1..run.len()).find(|j| events[run[*j]].beams[1] == Some(Beam::End))
                {
                    let xe = stem_x(&notes[end], &events[run[end]]);
                    band(ink, x - stem_w / 2.0, xe + stem_w / 2.0, 1);
                }
            }
            Some(Beam::ForwardHook) => band(ink, x - stem_w / 2.0, x + 1.1 * sp, 1),
            Some(Beam::BackwardHook) => band(ink, x - 1.1 * sp, x + stem_w / 2.0, 1),
            _ => {}
        }
    }
    // A beamed triplet's number: over the beam when the stems go up, over
    // the noteheads when they go down (the words are under the staff).
    if events[run[0]].tuplet == Some(Tuplet::Start) && events[run[0]].duration.triplet {
        let mid = (first.0 + last.0) / 2.0;
        let y = if up {
            beam_y(mid) - 1.0 * sp
        } else {
            notes.iter().map(|p| p.top).fold(f32::MAX, f32::min) - 1.6 * sp
        };
        put(
            ink,
            "tuplet3",
            mid - advance("tuplet3") * sp * 0.4,
            y,
            sp * 0.8,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_come_from_the_metrics() {
        // Helvetica's "A" is 667/1000 em, Times' 722.
        assert!((text_width("A", Font::Sans, 10.0) - 6.67).abs() < 1e-4);
        assert!((text_width("A", Font::Serif, 10.0) - 7.22).abs() < 1e-4);
        assert_eq!(cp1252('é'), Some(0xE9));
        assert_eq!(cp1252('€'), Some(0x80));
        assert_eq!(cp1252('♭'), None);
    }

    #[test]
    fn a_lead_sheet_fills_pages_with_its_staves() {
        let sheet = crate::musicxml::tests::sample();
        let pages = engrave(&sheet, &Layout::default());
        assert_eq!(pages.len(), 1);
        let ink = &pages[0].ink;
        let texts: Vec<&str> = ink
            .iter()
            .filter_map(|i| match i {
                Ink::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"A & B <test>"));
        assert!(texts.contains(&"Sing"));
        assert!(texts.contains(&"D"), "the chord symbol's root");
        // Five staff lines, everything inside the page.
        let lines = ink.iter().filter(|i| matches!(i, Ink::Line { .. })).count();
        assert!(lines >= 5);
        for i in ink {
            if let Ink::Fill(p) = i {
                for s in p {
                    if let Seg::M(x, y) | Seg::L(x, y) = *s {
                        assert!(x > 0.0 && x < 595.3 && y > 0.0 && y < 842.0, "{x} {y}");
                    }
                }
            }
        }
        // One staff: no names, no bracket.
        assert!(!texts.contains(&"Voice"));
    }

    /// A grand staff is braced, its bar lines run through both staves, and
    /// alone it is not named; in a score it is named once, between its
    /// staves.
    #[test]
    fn a_grand_staff_is_braced_and_barred_through() {
        let sheet = crate::musicxml::tests::piano_sample(false);
        let layout = Layout::for_staves(sheet.parts.len());
        let sp = layout.space;
        let pages = engrave(&sheet, &layout);
        let staves = staves_of(&pages[0], sp);
        assert_eq!(staves.len() % 2, 0);
        let (top, bottom, left) = (staves[0].0, staves[1].1, staves[0].2);
        let texts: Vec<&str> = pages[0]
            .ink
            .iter()
            .filter_map(|i| match i {
                Ink::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(!texts.contains(&"Piano"), "one instrument: no names");
        // The brace's halves: left of the staves, from the top line to the
        // middle and from the middle to the bottom line.
        let halves: Vec<(f32, f32, f32, f32)> = pages[0]
            .ink
            .iter()
            .filter_map(|i| match i {
                Ink::Fill(p) => {
                    let pts: Vec<(f32, f32)> = p
                        .iter()
                        .flat_map(|s| match *s {
                            Seg::M(x, y) | Seg::L(x, y) => vec![(x, y)],
                            Seg::C(a, b, c, d, e, f) => vec![(a, b), (c, d), (e, f)],
                            Seg::Z => vec![],
                        })
                        .collect();
                    let (x0, x1) = pts
                        .iter()
                        .fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                    let (y0, y1) = pts
                        .iter()
                        .fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                    Some((x0, x1, y0, y1))
                }
                _ => None,
            })
            .filter(|(x0, x1, _, _)| *x1 < left && *x0 > left - 2.0 * sp)
            .collect();
        let mid = (top + bottom) / 2.0;
        assert!(
            halves
                .iter()
                .any(|h| (h.2 - top).abs() < 0.5 && (h.3 - mid).abs() < 0.5),
            "{halves:?}"
        );
        assert!(
            halves
                .iter()
                .any(|h| (h.2 - mid).abs() < 0.5 && (h.3 - bottom).abs() < 0.5)
        );
        // Bar lines from the upper staff's top to the lower's bottom.
        let through = pages[0]
            .ink
            .iter()
            .filter(|i| {
                matches!(i, Ink::Line { a, b, .. }
                    if (a.0 - b.0).abs() < 1e-3 && (a.1 - top).abs() < 0.01 && (b.1 - bottom).abs() < 0.01)
            })
            .count();
        assert!(through >= 4, "{through}");
        // With a voice above: named, the piano once a system.
        let score = crate::musicxml::tests::piano_sample(true);
        let pages = engrave(&score, &Layout::for_staves(score.parts.len()));
        let texts: Vec<&str> = pages[0]
            .ink
            .iter()
            .filter_map(|i| match i {
                Ink::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts.iter().filter(|t| **t == "Piano").count(), 1);
        assert!(texts.contains(&"Voice"));
        if let Ok(dir) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
            let _ = std::fs::write(
                std::path::Path::new(&dir).join("grand.pdf"),
                crate::pdf::write(&pages, &score.title),
            );
        }
    }

    #[test]
    fn notes_sounding_together_share_their_place() {
        let sheet = crate::musicxml::tests::score_sample(4);
        let sp = 6.5;
        for mi in 0..4 {
            let staves: Vec<&Measure> = sheet.parts.iter().map(|p| &p.measures[mi]).collect();
            let plan = BarPlan::new(&staves, sp);
            // Every start of every staff is a point, and each note has its
            // room from its start to its end.
            for m in &staves {
                for e in m.events.iter().filter(|e| !e.bar_rest) {
                    let a = plan.points.iter().position(|p| *p == e.at).unwrap();
                    let b = plan.points.partition_point(|p| *p < e.end());
                    let have: f32 = plan.rooms[a..b].iter().sum();
                    assert!(have + 1e-3 >= room(e, sp), "bar {mi}: {e:?}");
                }
            }
            // The guitar's eighths take a point each (the tune's sixteenth
            // one more in the second bar).
            assert_eq!(plan.points.len(), if mi == 1 { 9 } else { 8 }, "bar {mi}");
            let xs = plan.xs(100.0, 200.0, sp);
            assert!(xs.windows(2).all(|w| w[1] > w[0]));
            assert_eq!(plan.x(&xs, 13), plan.x(&xs, 12));
        }
    }

    /// The staves on a page (top and bottom line, left and right end), by
    /// their lines: five long level lines evenly spaced.
    fn staves_of(page: &Page, sp: f32) -> Vec<(f32, f32, f32, f32)> {
        let mut lines: Vec<(f32, f32, f32)> = page
            .ink
            .iter()
            .filter_map(|i| match i {
                Ink::Line { a, b, width }
                    if (a.1 - b.1).abs() < 1e-3
                        && b.0 - a.0 > 100.0
                        && (width - E::STAFF_LINE_THICKNESS * sp).abs() < 1e-4 =>
                {
                    Some((a.1, a.0, b.0))
                }
                _ => None,
            })
            .collect();
        lines.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(lines.len() % 5, 0);
        lines
            .chunks(5)
            .map(|c| {
                assert!((c[4].0 - c[0].0 - 4.0 * sp).abs() < 0.01, "a staff's lines");
                (c[0].0, c[4].0, c[0].1, c[0].2)
            })
            .collect()
    }

    #[test]
    fn a_score_stacks_its_staves_and_starts_a_page_between_systems() {
        let sheet = crate::musicxml::tests::score_sample(40);
        let layout = Layout::for_staves(3);
        let sp = layout.space;
        let pages = engrave(&sheet, &layout);
        assert!(pages.len() >= 3, "{} pages", pages.len());
        let mut systems = 0;
        let mut names: Vec<String> = Vec::new();
        for (n, page) in pages.iter().enumerate() {
            let staves = staves_of(page, sp);
            // Whole systems of three staves on each page.
            assert!(
                !staves.is_empty() && staves.len().is_multiple_of(3),
                "page {n}"
            );
            systems += staves.len() / 3;
            // Each item drawn: the staff it is nearest; items running over
            // several staves (bar lines, the bracket) left out.
            let mut reach = vec![(f32::MAX, f32::MIN); staves.len()];
            for i in &page.ink {
                let (lo, hi) = extent(std::slice::from_ref(i));
                // Inside the page's margins (the page number below them).
                let is_number = matches!(i, Ink::Text { text, .. } if text.contains(" / "));
                assert!(lo >= layout.margin - 1.0, "page {n}: {i:?}");
                assert!(
                    is_number || hi <= layout.height - layout.margin + 1.0,
                    "page {n}: {i:?}"
                );
                if hi - lo > 5.0 * sp || is_number {
                    continue;
                }
                let mid = (lo + hi) / 2.0;
                let near = staves
                    .iter()
                    .enumerate()
                    .min_by(|a, b| {
                        let d = |s: &(f32, f32, f32, f32)| (s.0 - mid).max(mid - s.1).max(0.0);
                        d(a.1).total_cmp(&d(b.1))
                    })
                    .map_or(0, |(k, _)| k);
                reach[near].0 = reach[near].0.min(lo);
                reach[near].1 = reach[near].1.max(hi);
                if let Ink::Text {
                    text,
                    x,
                    size,
                    font,
                    anchor,
                    ..
                } = i
                {
                    // Words, chords, names and numbers stay off the staves.
                    let w = text_width(text, *font, *size);
                    let x0 = match anchor {
                        Anchor::Start => *x,
                        Anchor::Middle => x - w / 2.0,
                        Anchor::End => x - w,
                    };
                    for s in &staves {
                        let across = x0 < s.3 && x0 + w > s.2;
                        assert!(
                            !(across && lo < s.1 && hi > s.0),
                            "page {n}: '{text}' on a staff"
                        );
                    }
                    if anchor == &Anchor::End && *size == NAME_SIZE {
                        names.push(text.clone());
                    }
                }
            }
            // What belongs to a staff stays clear of the next one's.
            for k in 1..staves.len() {
                assert!(
                    reach[k - 1].1 < reach[k].0,
                    "page {n}: staff {k} meets the one above ({:?} {:?})",
                    reach[k - 1],
                    reach[k]
                );
            }
        }
        assert!(systems >= 10, "{systems} systems");
        // The names in full first, then short.
        assert_eq!(names[..3], ["Lead Vocal", "Guitar", "Bass"]);
        assert_eq!(names[3..6], ["Lead Voc.", "Gui.", "Bass"]);
        // The bracket: a thick line by each system's staves.
        let thick = pages[0]
            .ink
            .iter()
            .filter(|i| {
                matches!(i, Ink::Fill(p) if p.len() == 5
                    && extent(std::slice::from_ref(i)).1 - extent(std::slice::from_ref(i)).0
                        > 10.0 * sp)
            })
            .count();
        assert!(thick >= 1);
        if let Ok(dir) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
            let pdf = crate::pdf::write(&pages, &sheet.title);
            let _ = std::fs::write(std::path::Path::new(&dir).join("score.pdf"), pdf);
        }
    }

    fn notated(notes: Vec<crate::notate::Written>, bars: usize) -> LeadSheet {
        use crate::notate::{ScoreInput, ScoreStaff, notate};
        notate(&ScoreInput {
            title: "Map".into(),
            bars: (0..bars)
                .map(|i| crate::build::Bar {
                    start: 4.0 * i as f64,
                    time: (4, 4),
                })
                .collect(),
            key: Some(faderframe_midi::theory::Key::new(
                0,
                faderframe_midi::theory::Scale::Major,
            )),
            staves: vec![ScoreStaff {
                notes,
                ..ScoreStaff::default()
            }],
            ..ScoreInput::default()
        })
        .sheet
    }

    /// The map says where each note was drawn: pointing at a notehead
    /// finds its event and pitch, pointing at the staff its bar, time and
    /// line, and a time's x lies between its neighbours'.
    #[test]
    fn the_map_finds_what_was_drawn() {
        use crate::notate::Written;
        let notes = vec![
            Written::new(1, 0.0, 1.0, 60),
            Written::new(2, 1.0, 2.0, 64),
            Written::new(3, 2.0, 4.0, 67),
            Written::new(4, 4.0, 8.0, 72),
        ];
        let sheet = notated(notes, 2);
        let (pages, map) = engrave_mapped(&sheet, &Layout::default());
        assert_eq!(pages.len(), 1);
        assert_eq!(map.systems.len(), 1);
        let boxes = &map.events[0];
        for (bar, events) in boxes.iter().enumerate() {
            for (event, b) in events.iter().enumerate() {
                let b = b.as_ref().unwrap();
                let h = b.heads[0];
                let hit = map.event_at(0, h.x + 0.5, h.y - 0.5).unwrap();
                assert_eq!((hit.bar, hit.event), (bar, event));
                // Its notehead is ink of its page.
                assert!(matches!(pages[0].ink[h.ink], Ink::Fill(_)));
                assert!(b.ink.0 <= h.ink && h.ink < b.ink.1);
            }
        }
        // Middle C sits a step below the treble staff's first ledger line.
        let c = boxes[0][0].as_ref().unwrap().heads[0];
        let place = map.place_at(0, c.x, c.y).unwrap();
        assert_eq!((place.bar, place.part), (0, 0));
        assert_eq!(diatonic_at(place.step, Clef::Treble), 28);
        assert!(place.at < 1.0, "{}", place.at);
        let e = boxes[0][1].as_ref().unwrap().heads[0];
        let place = map.place_at(0, e.x, e.y).unwrap();
        assert!((place.at - 12.0).abs() < 1.0, "{}", place.at);
        let (_, x6, _, _) = map.x_of(0, 0, 6.0).unwrap();
        assert!(x6 > c.x && x6 < e.x);
        // The second bar.
        let w = boxes[1][0].as_ref().unwrap().heads[0];
        assert_eq!(map.place_at(0, w.x, w.y).unwrap().bar, 1);
    }

    /// A strip is one system on a page as wide and as tall as it, without
    /// a heading.
    #[test]
    fn a_strip_is_one_system_as_wide_as_its_bars() {
        use crate::notate::Written;
        let notes: Vec<Written> = (0..64)
            .map(|i| Written::new(i, i as f64 * 0.5, i as f64 * 0.5 + 0.5, 60 + (i % 12) as u8))
            .collect();
        let sheet = notated(notes, 8);
        let strip = Layout {
            continuous: true,
            ..Layout::default()
        };
        let (pages, map) = engrave_mapped(&sheet, &strip);
        assert_eq!(pages.len(), 1);
        assert_eq!(map.systems.len(), 1);
        assert_eq!(map.systems[0].bars, 0..8);
        assert!(pages[0].width > Layout::default().width);
        assert!(
            pages[0].height < Layout::default().height / 3.0,
            "{}",
            pages[0].height
        );
        let texts = pages[0]
            .ink
            .iter()
            .filter(|i| matches!(i, Ink::Text { text, .. } if text == "Map"))
            .count();
        assert_eq!(texts, 0, "no heading on a strip");
    }

    /// Two voices: the first voice's stems up, the second's down, and the
    /// second voice's notes are in the map as voice 1.
    #[test]
    fn two_voices_share_a_staff() {
        use crate::notate::Written;
        let mut notes = vec![Written::new(1, 0.0, 4.0, 60)];
        notes.extend((0..4).map(|i| Written::new(2 + i, i as f64, i as f64 + 1.0, 67 + i as u8)));
        let sheet = notated(notes, 1);
        let (pages, map) = engrave_mapped(&sheet, &Layout::default());
        let events = &map.events[0][0];
        assert_eq!(events.len(), 5);
        assert!(events[..4].iter().all(|b| b.as_ref().unwrap().voice == 0));
        assert_eq!(events[4].as_ref().unwrap().voice, 1);
        // Stems: each voice-1 note's stem rises from its head, the whole
        // note has none.
        let sp = Layout::default().space;
        let up_stems = pages[0]
            .ink
            .iter()
            .filter(|i| {
                matches!(i, Ink::Line { a, b, .. } if (a.0 - b.0).abs() < 0.01
                    && (a.1 - b.1).abs() > 2.5 * sp)
            })
            .count();
        assert!(up_stems >= 4, "{up_stems}");
    }
}

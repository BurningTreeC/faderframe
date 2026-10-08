//! Engraving: a lead sheet laid out on pages as a display list (filled
//! outlines, lines and text in points, y down) that the PDF writer and
//! the app's view both draw.
//!
//! Systems hold up to four bars (fewer when they are crowded) and are
//! justified; each note's room grows with its value, its accidental, its
//! word and the chord over it, so words and symbols never collide. The
//! glyphs are Bravura's ([`crate::assets`]), placed by SMuFL's rules
//! (stems at the noteheads' anchors, staff spaces as the unit).

use crate::assets::{WIDTHS, engraving as E};
use crate::glyph::{Glyph, Seg, glyph};
use crate::score::{Beam, ChordSymbol, Clef, Event, LeadSheet, Measure, Tuplet, Value};

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

/// The page and the staff's size.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub margin: f32,
    /// A staff space (points).
    pub space: f32,
    pub bars_per_system: usize,
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
    if e.pitch.and_then(|p| p.accidental).is_some() {
        w += 1.3;
    }
    w += 0.6 * f32::from(e.duration.dots);
    if let Some(l) = &e.lyric {
        w = w.max(text_width(&l.text, Font::Serif, LYRIC_SIZE) / sp + 1.0);
    }
    w
}

/// Each event's room in a bar, a chord symbol's included (the event a
/// chord starts in leaves room for it).
fn rooms(m: &Measure, sp: f32) -> Vec<f32> {
    let mut r: Vec<f32> = m.events.iter().map(|e| room(e, sp)).collect();
    for c in &m.chords {
        if let Some(i) = m.events.iter().position(|e| e.at <= c.at && c.at < e.end()) {
            r[i] = r[i].max(chord_width(c) / sp + 1.2);
        }
    }
    r
}

/// Where each note was drawn (for ties, beams and words).
#[derive(Clone, Copy, Debug)]
struct Placed {
    page: usize,
    system: usize,
    /// The notehead's left edge, its width, its centre line.
    x: f32,
    w: f32,
    y: f32,
    up: bool,
}

/// One system's bars and its horizontal plan.
struct System {
    bars: std::ops::Range<usize>,
    /// Each bar's left edge and width (points).
    spans: Vec<(f32, f32)>,
    prefix: f32,
}

pub fn engrave(sheet: &LeadSheet, layout: &Layout) -> Vec<Page> {
    let sp = layout.space;
    let left = layout.margin;
    let right = layout.width - layout.margin;
    let usable = right - left;
    let clef_glyph = match sheet.clef {
        Clef::Treble => "gClef",
        Clef::Treble8vb => "gClef8vb",
        Clef::Bass => "fClef",
    };
    let key_count = sheet.key.fifths.unsigned_abs() as f32;
    let key_w = key_count * 1.05;
    let time_w = |m: &Measure| {
        let digits = |n: u8| n.to_string().len() as f32;
        2.2 * digits(m.time.0).max(digits(m.time.1)) + 0.8
    };
    let prefix = |first: bool, m: &Measure| {
        (1.0 + advance(clef_glyph)
            + 0.8
            + key_w
            + if first || m.show_time { time_w(m) } else { 0.0 }
            + 1.0)
            * sp
    };
    // Each bar's least width.
    let widths: Vec<f32> = sheet
        .measures
        .iter()
        .map(|m| (rooms(m, sp).iter().sum::<f32>() + 2.0) * sp)
        .collect();
    // Systems: up to four bars, as many as fit.
    let mut systems = Vec::new();
    let mut i = 0;
    while i < sheet.measures.len() {
        let pre = prefix(systems.is_empty(), &sheet.measures[i]);
        let mut j = i;
        let mut sum = 0.0;
        // Bars may close up to 80 % of their room to share a line.
        const SQUEEZE: f32 = 0.8;
        while j < sheet.measures.len()
            && j - i < layout.bars_per_system
            && (j == i || pre + (sum + widths[j]) * SQUEEZE <= usable)
        {
            // A time change inside a system takes room for its signature.
            let extra = if j > i && sheet.measures[j].show_time {
                time_w(&sheet.measures[j]) * sp
            } else {
                0.0
            };
            if j > i && pre + (sum + widths[j] + extra) * SQUEEZE > usable {
                break;
            }
            sum += widths[j] + extra;
            j += 1;
        }
        let last = j == sheet.measures.len();
        let room_left = usable - pre;
        let scale = (room_left / sum.max(1.0)).max(0.1);
        // The last system is not stretched beyond half again.
        let scale = if last && j - i < layout.bars_per_system {
            scale.min(1.5)
        } else {
            scale
        };
        let mut x = left + pre;
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
    // The heading.
    {
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
    let above = 4.2 * sp + CHORD_SIZE;
    let below = 4.0 * sp + LYRIC_SIZE + 6.0;
    let mut placed: Vec<Vec<Option<Placed>>> = sheet
        .measures
        .iter()
        .map(|m| vec![None; m.events.len()])
        .collect();
    let mut system_edges: Vec<(usize, f32, f32, f32)> = Vec::new();
    for (si, sys) in systems.iter().enumerate() {
        // Room for notes high or low on the staff.
        let (mut hi, mut lo) = (8i32, 0i32);
        for m in &sheet.measures[sys.bars.clone()] {
            for e in &m.events {
                if let Some(p) = e.pitch {
                    let s = staff_step(p.diatonic(), sheet.clef);
                    hi = hi.max(s);
                    lo = lo.min(s);
                }
            }
        }
        let top_extra = ((hi - 8) as f32 * 0.5 * sp).max(0.0);
        let bottom_extra = ((-lo) as f32 * 0.5 * sp).max(0.0);
        let height = above + top_extra + 4.0 * sp + bottom_extra + below;
        if y + height > layout.height - layout.margin
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
        let top = y + above + top_extra;
        let ink = &mut pages[page].ink;
        let step_y = |s: i32| top + 4.0 * sp - s as f32 * 0.5 * sp;
        let x_end = sys.spans.last().map_or(right, |(x, w)| x + w);
        // The staff.
        for k in 0..5 {
            let ly = top + k as f32 * sp;
            line(ink, (left, ly), (x_end, ly), E::STAFF_LINE_THICKNESS * sp);
        }
        line(
            ink,
            (left, top),
            (left, top + 4.0 * sp),
            E::THIN_BARLINE_THICKNESS * sp,
        );
        // Clef, key, time.
        let mut x = left + sp;
        let clef_step = match sheet.clef {
            Clef::Treble | Clef::Treble8vb => 2,
            Clef::Bass => 6,
        };
        put(ink, clef_glyph, x, step_y(clef_step), sp);
        x += (advance(clef_glyph) + 0.8) * sp;
        let (sharps, flats) = ([8, 5, 9, 6, 3, 7, 4], [4, 7, 3, 6, 2, 5, 1]);
        let shift = if sheet.clef == Clef::Bass { -2 } else { 0 };
        for k in 0..sheet.key.fifths.unsigned_abs().min(7) as usize {
            let (g, s) = if sheet.key.fifths > 0 {
                ("accidentalSharp", sharps[k])
            } else {
                ("accidentalFlat", flats[k])
            };
            put(ink, g, x, step_y(s + shift), sp);
            x += 1.05 * sp;
        }
        let first = &sheet.measures[sys.bars.start];
        if si == 0 || first.show_time {
            time_signature(ink, first.time, x + 0.3 * sp, &step_y, sp);
        }
        // The bar number.
        if si > 0 {
            ink.push(Ink::Text {
                text: (sys.bars.start + 1).to_string(),
                x: left,
                y: top - 1.2 * sp,
                size: 8.0,
                font: Font::SerifItalic,
                anchor: Anchor::Start,
            });
        }
        system_edges.push((page, left + sys.prefix - 1.5 * sp, x_end, top));
        let chord_y = top - 3.2 * sp - top_extra;
        let lyric_y = top + 4.0 * sp + bottom_extra + 3.0 * sp + LYRIC_SIZE * 0.7;
        for (k, mi) in sys.bars.clone().enumerate() {
            let m = &sheet.measures[mi];
            let (bx, bw) = sys.spans[k];
            let mut bx = bx;
            let mut bw = bw;
            // A time change inside the system.
            if k > 0 && m.show_time {
                time_signature(ink, m.time, bx + 0.4 * sp, &step_y, sp);
                let tw = time_w(m) * sp;
                bx += tw;
                bw -= tw;
            }
            let rooms = rooms(m, sp);
            let total: f32 = rooms.iter().sum::<f32>().max(1.0);
            let inner = bw - 2.0 * sp;
            let mut ex = bx + 1.2 * sp;
            let mut xs = Vec::with_capacity(m.events.len());
            for r in &rooms {
                xs.push(ex);
                ex += inner * r / total;
            }
            // Notes and rests.
            for (ei, e) in m.events.iter().enumerate() {
                let x0 = xs[ei];
                match e.pitch {
                    None if e.bar_rest => {
                        let gx = bx + bw / 2.0 - advance("restWhole") * sp / 2.0;
                        put(ink, "restWhole", gx, step_y(6), sp);
                    }
                    None => {
                        let g = rest_glyph(e.duration.value);
                        let s = if e.duration.value == Value::Whole {
                            6
                        } else {
                            4
                        };
                        put(ink, g, x0, step_y(s), sp);
                        if e.duration.dots > 0 {
                            put(
                                ink,
                                "augmentationDot",
                                x0 + (advance(g) + 0.4) * sp,
                                step_y(5),
                                sp,
                            );
                        }
                    }
                    Some(p) => {
                        let s = staff_step(p.diatonic(), sheet.clef);
                        let ny = step_y(s);
                        let mut nx = x0;
                        if let Some(a) = p.accidental {
                            let g = accidental_glyph(a);
                            put(ink, g, nx, ny, sp);
                            nx += (advance(g) + 0.25) * sp;
                        }
                        let head = notehead(e);
                        let hw = advance(head) * sp;
                        put(ink, head, nx, ny, sp);
                        // Ledger lines.
                        let ext = E::LEGER_LINE_EXTENSION * sp;
                        let mut l = -2;
                        while l >= s {
                            line(
                                ink,
                                (nx - ext, step_y(l)),
                                (nx + hw + ext, step_y(l)),
                                E::LEGER_LINE_THICKNESS * sp,
                            );
                            l -= 2;
                        }
                        let mut l = 10;
                        while l <= s {
                            line(
                                ink,
                                (nx - ext, step_y(l)),
                                (nx + hw + ext, step_y(l)),
                                E::LEGER_LINE_THICKNESS * sp,
                            );
                            l += 2;
                        }
                        for d in 0..e.duration.dots {
                            let dy = if s % 2 == 0 { step_y(s + 1) } else { ny };
                            put(
                                ink,
                                "augmentationDot",
                                nx + hw + (0.35 + 0.5 * f32::from(d)) * sp,
                                dy,
                                sp,
                            );
                        }
                        placed[mi][ei] = Some(Placed {
                            page,
                            system: si,
                            x: nx,
                            w: hw,
                            y: ny,
                            up: s < 4,
                        });
                    }
                }
            }
            stems_and_beams(ink, m, &mut placed[mi], &step_y, sheet.clef, sp);
            // Chords over the bar.
            for c in &m.chords {
                let ex = m
                    .events
                    .iter()
                    .position(|e| e.at <= c.at && c.at < e.end())
                    .map_or(bx + sp, |i| {
                        let e = &m.events[i];
                        let frac = (c.at - e.at) as f32 / e.duration.divisions().max(1) as f32;
                        let next = xs.get(i + 1).copied().unwrap_or(bx + bw);
                        xs[i] + frac * (next - xs[i])
                    });
                draw_chord(ink, c, ex, chord_y);
            }
            // Words.
            for (ei, e) in m.events.iter().enumerate() {
                if let (Some(l), Some(pl)) = (&e.lyric, placed[mi][ei]) {
                    ink.push(Ink::Text {
                        text: l.text.clone(),
                        x: pl.x + pl.w / 2.0,
                        y: lyric_y,
                        size: LYRIC_SIZE,
                        font: Font::Serif,
                        anchor: Anchor::Middle,
                    });
                }
            }
            // The bar line.
            let end = bx + bw;
            if mi + 1 == sheet.measures.len() {
                let thick = E::THICK_BARLINE_THICKNESS * sp;
                let tx = end - thick / 2.0;
                line(ink, (tx, top), (tx, top + 4.0 * sp), thick);
                let thin = tx - thick / 2.0 - E::THIN_THICK_BARLINE_SEPARATION * sp;
                line(
                    ink,
                    (thin, top),
                    (thin, top + 4.0 * sp),
                    E::THIN_BARLINE_THICKNESS * sp,
                );
            } else {
                line(
                    ink,
                    (end, top),
                    (end, top + 4.0 * sp),
                    E::THIN_BARLINE_THICKNESS * sp,
                );
            }
        }
        // Melisma lines: from a held word to the end of its last note.
        let mut open: Option<(f32, f32)> = None;
        for mi in sys.bars.clone() {
            for (ei, e) in sheet.measures[mi].events.iter().enumerate() {
                let Some(pl) = placed[mi][ei] else { continue };
                if let Some(l) = &e.lyric {
                    if let Some((a, b)) = open.take()
                        && b > a
                    {
                        line(
                            ink,
                            (a, lyric_y + 1.0),
                            (b, lyric_y + 1.0),
                            E::LYRIC_LINE_THICKNESS * sp,
                        );
                    }
                    if l.extend {
                        let half = text_width(&l.text, Font::Serif, LYRIC_SIZE) / 2.0;
                        let start = pl.x + pl.w / 2.0 + half + 1.5;
                        open = Some((start, start));
                    }
                } else if (e.held || e.tied_from) && open.is_some() {
                    if let Some(o) = &mut open {
                        o.1 = pl.x + pl.w;
                    }
                } else if let Some((a, b)) = open.take()
                    && b > a
                {
                    line(
                        ink,
                        (a, lyric_y + 1.0),
                        (b, lyric_y + 1.0),
                        E::LYRIC_LINE_THICKNESS * sp,
                    );
                }
            }
        }
        if let Some((a, b)) = open.take()
            && b > a
        {
            line(
                ink,
                (a, lyric_y + 1.0),
                (b, lyric_y + 1.0),
                E::LYRIC_LINE_THICKNESS * sp,
            );
        }
        y += height;
    }
    // Ties (within a system, or to its end and from the next one's start).
    let flat: Vec<(usize, usize)> = sheet
        .measures
        .iter()
        .enumerate()
        .flat_map(|(mi, m)| (0..m.events.len()).map(move |ei| (mi, ei)))
        .collect();
    for (k, &(mi, ei)) in flat.iter().enumerate() {
        let e = &sheet.measures[mi].events[ei];
        if !e.tie {
            continue;
        }
        let Some(a) = placed[mi][ei] else { continue };
        let Some(&(nm, ne)) = flat.get(k + 1) else {
            continue;
        };
        let Some(b) = placed[nm][ne] else { continue };
        let up = !a.up;
        let dy = if up { -0.55 * sp } else { 0.55 * sp };
        let from = (a.x + a.w + 0.15 * sp, a.y + dy);
        if a.system == b.system {
            tie(
                &mut pages[a.page].ink,
                from,
                (b.x - 0.15 * sp, b.y + dy),
                up,
                sp,
            );
        } else {
            let (_, _, end, _) = system_edges[a.system];
            tie(
                &mut pages[a.page].ink,
                from,
                (end - 0.3 * sp, a.y + dy),
                up,
                sp,
            );
            let (_, start, _, _) = system_edges[b.system];
            tie(
                &mut pages[b.page].ink,
                (start, b.y + dy),
                (b.x - 0.15 * sp, b.y + dy),
                up,
                sp,
            );
        }
    }
    // Page numbers after the first.
    let n = pages.len();
    for (i, p) in pages.iter_mut().enumerate().skip(1) {
        p.ink.push(Ink::Text {
            text: format!("{} / {n}", i + 1),
            x: layout.width / 2.0,
            y: layout.height - layout.margin / 2.0,
            size: 8.0,
            font: Font::Serif,
            anchor: Anchor::Middle,
        });
    }
    pages
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
fn stems_and_beams(
    ink: &mut Vec<Ink>,
    m: &Measure,
    placed: &mut [Option<Placed>],
    step_y: &impl Fn(i32) -> f32,
    clef: Clef,
    sp: f32,
) {
    let stem_w = E::STEM_THICKNESS * sp;
    let middle = step_y(4);
    let n = m.events.len();
    // Beamed runs share a direction (away from the notes furthest out).
    let mut i = 0;
    while i < n {
        let e = &m.events[i];
        if e.beams[0] == Some(Beam::Begin) {
            let mut j = i;
            while j < n && m.events[j].beams[0].is_some() {
                j += 1;
                if m.events[j - 1].beams[0] == Some(Beam::End) {
                    break;
                }
            }
            let run: Vec<usize> = (i..j).filter(|k| placed[*k].is_some()).collect();
            if run.len() >= 2 {
                beam_run(ink, m, placed, &run, middle, sp, stem_w);
            }
            i = j;
            continue;
        }
        // A lone note: its stem and flag.
        if let Some(pl) = placed[i]
            && e.duration.value != Value::Whole
        {
            let s = staff_step(e.pitch.map_or(0, |p| p.diatonic()), clef);
            let up = s < 4;
            let head = glyph(notehead(e));
            let (ax, ay) = match (head, up) {
                (Some(g), true) => (g.stem_up[0], g.stem_up[1]),
                (Some(g), false) => (g.stem_down[0], g.stem_down[1]),
                (None, _) => (1.18, 0.0),
            };
            let x = pl.x + ax * sp + if up { -stem_w / 2.0 } else { stem_w / 2.0 };
            let y0 = pl.y - ay * sp;
            let mut len = 3.5 * sp
                + if e.duration.value == Value::Sixteenth {
                    0.5 * sp
                } else {
                    0.0
                };
            // Reach the middle line from far away.
            if up && pl.y - len > middle && s < -1 {
                len = pl.y - middle;
            }
            if !up && pl.y + len < middle && s > 9 {
                len = middle - pl.y;
            }
            let y1 = if up { y0 - len } else { y0 + len };
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
                while k < n && m.events[k].tuplet == Some(Tuplet::Middle) {
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

fn beam_run(
    ink: &mut Vec<Ink>,
    m: &Measure,
    placed: &mut [Option<Placed>],
    run: &[usize],
    middle: f32,
    sp: f32,
    stem_w: f32,
) {
    let notes: Vec<Placed> = run.iter().filter_map(|k| placed[*k]).collect();
    // Up when the notes sit lower on average.
    let below: f32 = notes.iter().map(|p| p.y - middle).sum();
    let up = below > 0.0;
    let stem_x = |p: &Placed, e: &Event| {
        let g = glyph(notehead(e));
        if up {
            p.x + g.map_or(1.18, |g| g.stem_up[0]) * sp - stem_w / 2.0
        } else {
            p.x + g.map_or(0.0, |g| g.stem_down[0]) * sp + stem_w / 2.0
        }
    };
    let first = (stem_x(&notes[0], &m.events[run[0]]), notes[0].y);
    let last_i = notes.len() - 1;
    let last = (
        stem_x(&notes[last_i], &m.events[run[last_i]]),
        notes[last_i].y,
    );
    // The beam's slope follows the notes, gently.
    let dx = (last.0 - first.0).max(1.0);
    let slope = ((last.1 - first.1) / dx).clamp(-0.25, 0.25) * 0.6;
    let dir = if up { -1.0 } else { 1.0 };
    let levels = run
        .iter()
        .map(|k| m.events[*k].duration.value.beams())
        .max()
        .unwrap_or(1);
    let reach = (3.25 + 0.5 * f32::from(levels.saturating_sub(1))) * sp;
    // Start so every stem is long enough.
    let mut y0 = first.1 + dir * reach;
    for (p, k) in notes.iter().zip(run) {
        let x = stem_x(p, &m.events[*k]);
        let need = p.y + dir * reach;
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
        let e = &m.events[*k];
        let x = stem_x(p, e);
        let head = glyph(notehead(e));
        let ay = head.map_or(0.0, |g| if up { g.stem_up[1] } else { g.stem_down[1] });
        line(ink, (x, p.y - ay * sp), (x, beam_y(x)), stem_w);
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
        let e = &m.events[*k];
        let x = stem_x(&notes[idx], e);
        match e.beams[1] {
            Some(Beam::Begin) => {
                if let Some(end) =
                    (idx + 1..run.len()).find(|j| m.events[run[*j]].beams[1] == Some(Beam::End))
                {
                    let xe = stem_x(&notes[end], &m.events[run[end]]);
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
    if m.events[run[0]].tuplet == Some(Tuplet::Start) && m.events[run[0]].duration.triplet {
        let mid = (first.0 + last.0) / 2.0;
        let y = if up {
            beam_y(mid) - 1.0 * sp
        } else {
            notes.iter().map(|p| p.y).fold(f32::MAX, f32::min) - 1.6 * sp
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
    }
}

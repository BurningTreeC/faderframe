//! Where things are: the editor's layout, the frequency and gain axes, and
//! musical notes (the piano display, typed values like "A4" or "C#2+13").

use faderframe_ui_canvas::{Rect, Size};

pub(crate) const TOP_H: f32 = 30.0;
pub(crate) const BOTTOM_H: f32 = 34.0;
/// The curves' gain scale (left).
pub(crate) const LEFT: f32 = 36.0;
/// The analyser's scale and the output meter (right).
pub(crate) const RIGHT: f32 = 46.0;
pub(crate) const AXIS_H: f32 = 18.0;
pub(crate) const PIANO_H: f32 = 30.0;
pub(crate) const NODE_R: f32 = 7.5;
/// The whole frequency range the display can show.
pub(crate) const F_MIN: f64 = 10.0;
pub(crate) const F_MAX: f64 = 30_000.0;

/// The editor's layout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub top: Rect,
    /// The response display.
    pub graph: Rect,
    /// The frequency scale (or the piano) under it.
    pub axis: Rect,
    pub bottom: Rect,
    /// The output meter at the right.
    pub meter: Rect,
}

impl Layout {
    pub fn new(size: Size, piano: bool) -> Self {
        let top = Rect::new(0.0, 0.0, size.w, TOP_H);
        let bottom = Rect::new(0.0, size.h - BOTTOM_H, size.w, BOTTOM_H);
        let axis_h = if piano { PIANO_H } else { AXIS_H };
        let graph = Rect::new(
            LEFT,
            TOP_H + 6.0,
            (size.w - LEFT - RIGHT).max(10.0),
            (size.h - TOP_H - BOTTOM_H - axis_h - 6.0).max(10.0),
        );
        let axis = Rect::new(graph.x, graph.bottom(), graph.w, axis_h);
        let meter = Rect::new(size.w - 12.0, graph.y, 7.0, graph.h);
        Self {
            top,
            graph,
            axis,
            bottom,
            meter,
        }
    }
}

/// The visible frequency range (zoomable) on a logarithmic axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FreqAxis {
    pub lo: f64,
    pub hi: f64,
}

impl FreqAxis {
    pub fn x(&self, g: &Rect, f: f64) -> f32 {
        g.x + g.w * ((f / self.lo).ln() / (self.hi / self.lo).ln()) as f32
    }

    pub fn f(&self, g: &Rect, x: f32) -> f64 {
        let t = f64::from((x - g.x) / g.w);
        self.lo * (self.hi / self.lo).powf(t)
    }

    /// Zoom by `factor` (> 1 in) round frequency `at`, inside the whole
    /// range.
    pub fn zoomed(&self, at: f64, factor: f64, top: f64) -> Self {
        let span = (self.hi / self.lo).ln() / factor;
        let t = (at / self.lo).ln() / (self.hi / self.lo).ln();
        let lo = at.ln() - t * span;
        let mut out = Self {
            lo: lo.exp(),
            hi: (lo + span).exp(),
        };
        out.clamp(top);
        out
    }

    /// Move by `octaves` (positive: up).
    pub fn panned(&self, octaves: f64, top: f64) -> Self {
        let k = 2f64.powf(octaves);
        let mut out = Self {
            lo: self.lo * k,
            hi: self.hi * k,
        };
        out.clamp(top);
        out
    }

    fn clamp(&mut self, top: f64) {
        let span = (self.hi / self.lo).clamp(2.0, top / F_MIN);
        if self.lo < F_MIN {
            self.lo = F_MIN;
            self.hi = F_MIN * span;
        }
        if self.hi > top {
            self.hi = top;
            self.lo = top / span;
        }
    }
}

/// The gain axis of the curves: ±`range` dB over most of the height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GainAxis {
    pub range: f32,
}

impl GainAxis {
    pub fn y(&self, g: &Rect, db: f32) -> f32 {
        g.y + g.h * 0.5 - db / self.range * g.h * 0.46
    }

    pub fn db(&self, g: &Rect, y: f32) -> f32 {
        (g.y + g.h * 0.5 - y) / (g.h * 0.46) * self.range
    }

    /// dB per pixel.
    pub fn per_px(&self, g: &Rect) -> f32 {
        self.range / (g.h * 0.46)
    }
}

/// The analyser's level axis: 0 dBFS at the top, `-range` at the bottom.
pub(crate) fn analyser_y(g: &Rect, dbfs: f32, range: f32) -> f32 {
    g.y + g.h * (-dbfs / range).clamp(0.0, 1.0)
}

/// Points at `n` frequencies across an axis.
pub(crate) fn sweep(axis: &FreqAxis, n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| axis.lo * (axis.hi / axis.lo).powf(i as f64 / (n - 1).max(1) as f64))
        .collect()
}

// --- notes -----------------------------------------------------------------

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The MIDI note (fractional) of a frequency (A4 = 440 Hz = 69).
pub(crate) fn note_of(freq: f64) -> f64 {
    69.0 + 12.0 * (freq.max(1e-3) / 440.0).log2()
}

pub(crate) fn freq_of(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

/// A note's name with its octave (middle C is C4).
pub(crate) fn note_name(note: i32) -> String {
    let n = note.rem_euclid(12) as usize;
    format!("{}{}", NAMES[n], note.div_euclid(12) - 1)
}

/// Whether a note is a black key.
pub(crate) fn is_black(note: i32) -> bool {
    matches!(note.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// A frequency as a note and cents ("A4 +12").
pub(crate) fn note_label(freq: f64) -> String {
    let n = note_of(freq);
    let near = n.round();
    let cents = ((n - near) * 100.0).round() as i32;
    if cents == 0 {
        note_name(near as i32)
    } else {
        format!("{} {cents:+}", note_name(near as i32)).replace('-', "−")
    }
}

/// A typed frequency: "1000", "1k", "2.5 kHz", "A4", "C#3+13", "Db2 -20".
pub(crate) fn parse_freq(text: &str) -> Option<f64> {
    let t = text.trim().replace('−', "-").replace(' ', "");
    if t.is_empty() {
        return None;
    }
    let first = t.chars().next()?.to_ascii_uppercase();
    if ('A'..='G').contains(&first) {
        return parse_note(&t).map(freq_of);
    }
    let lower = t.to_ascii_lowercase();
    let lower = lower.trim_end_matches("hz");
    let (num, k) = match lower.strip_suffix('k') {
        Some(n) => (n, 1000.0),
        None => (lower, 1.0),
    };
    let v: f64 = num.parse().ok()?;
    (v > 0.0).then_some(v * k)
}

/// "C#3+13" → MIDI note (fractional).
fn parse_note(t: &str) -> Option<f64> {
    let mut chars = t.chars().peekable();
    let letter = chars.next()?.to_ascii_uppercase();
    let base = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut semi = base;
    match chars.peek() {
        Some('#') => {
            semi += 1;
            chars.next();
        }
        Some('b') => {
            semi -= 1;
            chars.next();
        }
        _ => {}
    }
    let rest: String = chars.collect();
    // The octave (possibly negative), then optional cents.
    let split = rest
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == '+' || *c == '-')
        .map_or(rest.len(), |(i, _)| i);
    let (octave, cents) = rest.split_at(split);
    let octave: i32 = octave.parse().ok()?;
    let cents: f64 = if cents.is_empty() {
        0.0
    } else {
        cents.parse().ok()?
    };
    Some(f64::from((octave + 1) * 12 + semi) + cents / 100.0)
}

/// A level with its sign ("+4.5 dB", "−3.0 dB", never "−0.0").
pub(crate) fn db_text(v: f64) -> String {
    let v = if v.abs() < 0.05 { 0.0 } else { v };
    format!("{v:+.1} dB").replace('-', "−")
}

/// A typed level: "-3", "+4.5 dB", "2x" (twice as loud: +6 dB), "0.5x".
pub(crate) fn parse_db(text: &str) -> Option<f64> {
    let t = text.trim().replace('−', "-").to_ascii_lowercase();
    let t = t.trim_end_matches("db").trim();
    if let Some(x) = t.strip_suffix('x') {
        let k: f64 = x.trim().parse().ok()?;
        return (k > 0.0).then(|| 20.0 * k.log10());
    }
    t.parse().ok()
}

/// A typed plain number or percentage ("50%" of the span `lo..hi`).
pub(crate) fn parse_value(text: &str, lo: f64, hi: f64) -> Option<f64> {
    let t = text.trim();
    if let Some(p) = t.strip_suffix('%') {
        let p: f64 = p.trim().parse().ok()?;
        return Some(lo + (hi - lo) * p / 100.0);
    }
    t.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_frequencies_and_notes() {
        assert_eq!(parse_freq("1k"), Some(1000.0));
        assert_eq!(parse_freq("2.5 kHz"), Some(2500.0));
        assert_eq!(parse_freq("100"), Some(100.0));
        assert!((parse_freq("A4").unwrap() - 440.0).abs() < 1e-9);
        assert!((parse_freq("a4").unwrap() - 440.0).abs() < 1e-9);
        let c3 = parse_freq("C#3+13").unwrap();
        assert!((note_of(c3) - (49.13)).abs() < 1e-9, "{}", note_of(c3));
        assert!((note_of(parse_freq("Db2-20").unwrap()) - 36.8).abs() < 1e-9);
        assert!((note_of(parse_freq("C-1").unwrap())).abs() < 1e-9);
        assert_eq!(parse_freq("nonsense"), None);
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(21), "A0");
        assert_eq!(note_label(440.0), "A4");
        assert_eq!(note_label(freq_of(69.12)), "A4 +12");
        assert!(is_black(61) && !is_black(60));
    }

    #[test]
    fn typed_levels() {
        assert!((parse_db("2x").unwrap() - 6.0206).abs() < 1e-3);
        assert_eq!(parse_db("-3"), Some(-3.0));
        assert_eq!(parse_db("+4.5 dB"), Some(4.5));
        assert_eq!(parse_db("−6"), Some(-6.0));
        assert_eq!(parse_value("50%", -30.0, 30.0), Some(0.0));
    }

    #[test]
    fn the_axis_zooms_and_pans_inside_its_range() {
        let a = FreqAxis {
            lo: F_MIN,
            hi: F_MAX,
        };
        let z = a.zoomed(1_000.0, 4.0, F_MAX);
        assert!(z.lo > 100.0 && z.hi < 10_000.0, "{z:?}");
        let g = Rect::new(0.0, 0.0, 600.0, 100.0);
        // The frequency under the pointer stays put.
        let x = a.x(&g, 1_000.0);
        assert!((z.f(&g, x) - 1_000.0).abs() < 1.0);
        let p = z.panned(20.0, F_MAX);
        assert_eq!(p.hi, F_MAX);
        let back = z.zoomed(1_000.0, 1e-6, F_MAX);
        assert_eq!((back.lo, back.hi), (F_MIN, F_MAX));
    }
}

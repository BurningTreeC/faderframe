//! Values as people type and read them: frequencies ("1k", "A4",
//! "C#2+13"), levels ("-3", "2x"), times ("12 ms", "1.5 s"), percentages
//! and ratios; musical notes for the piano displays and the tuner.

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

/// A typed key range: "C2-B3", "C2 B3", "C2–B3" (a minus before an
/// octave stays one: "C-1-G9"); one key alone is a range of one.
pub(crate) fn parse_key_range(text: &str) -> Option<(u8, u8)> {
    let t = text.trim().replace('–', " ").replace("..", " ");
    let key = |s: &str| {
        let n = parse_note(s.trim())?.round();
        (0.0..=127.0).contains(&n).then_some(n as u8)
    };
    let chars: Vec<char> = t.chars().collect();
    // A dash that a note name follows splits the two keys.
    let dash = (1..chars.len())
        .find(|&i| chars[i] == '-' && chars.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()));
    let (a, b) = match dash {
        Some(i) => (
            chars[..i].iter().collect::<String>(),
            chars[i + 1..].iter().collect::<String>(),
        ),
        None => match t.split_once(' ') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (t.clone(), t.clone()),
        },
    };
    let (a, b) = (key(&a)?, key(&b)?);
    Some((a.min(b), a.max(b)))
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

/// A typed time in milliseconds: "12", "12 ms", "1.5 s".
pub(crate) fn parse_ms(text: &str) -> Option<f64> {
    let t = text.trim().to_ascii_lowercase().replace(' ', "");
    if let Some(ms) = t.strip_suffix("ms") {
        return ms.parse().ok();
    }
    if let Some(s) = t.strip_suffix('s') {
        return s.parse::<f64>().ok().map(|s| s * 1000.0);
    }
    t.parse().ok()
}

/// A typed ratio: "4", "4:1", "inf".
pub(crate) fn parse_ratio(text: &str) -> Option<f64> {
    let t = text.trim().to_ascii_lowercase();
    if t.starts_with("inf") || t == "∞" {
        return Some(f64::INFINITY);
    }
    t.split(':').next()?.trim().parse().ok()
}

/// A time as people read it: "0.50 ms", "12.0 ms", "1.20 s".
pub(crate) fn ms_text(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms >= 100.0 {
        format!("{ms:.0} ms")
    } else if ms >= 10.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn key_ranges_are_typed_as_people_write_them() {
        assert_eq!(parse_key_range("C2-B3"), Some((36, 59)));
        assert_eq!(parse_key_range("C2 – B3"), Some((36, 59)));
        assert_eq!(parse_key_range("C-1-G9"), Some((0, 127)));
        assert_eq!(parse_key_range("B3 C2"), Some((36, 59)));
        assert_eq!(parse_key_range("A4"), Some((69, 69)));
        assert_eq!(parse_key_range("H2"), None);
    }

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
    fn typed_times_and_ratios() {
        assert_eq!(parse_ms("12"), Some(12.0));
        assert_eq!(parse_ms("12 ms"), Some(12.0));
        assert_eq!(parse_ms("1.5s"), Some(1500.0));
        assert_eq!(parse_ratio("4:1"), Some(4.0));
        assert_eq!(parse_ratio("inf"), Some(f64::INFINITY));
        assert_eq!(ms_text(0.5), "0.50 ms");
        assert_eq!(ms_text(1200.0), "1.20 s");
    }
}

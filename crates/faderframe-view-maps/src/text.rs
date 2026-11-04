//! What the map editor's cells read as, and what typing into them means:
//! keys by name ("C1", "F#2", middle C = C4 = 60) or number, keyswitch
//! lists ("C0, D0@90": a key and its velocity when not 100), controller
//! lists ("1=64, 68=127"), signed semitones, percents.

use faderframe_project::articulation::{ControlValue, Keyswitch, key_name};

/// A key as the piano roll names it ("C4" = 60).
pub fn note_name(key: u8) -> String {
    key_name(key.min(127))
}

/// A typed key: a MIDI number, or a name like "C4", "F#3", "Bb-1".
pub fn parse_key(text: &str) -> Option<u8> {
    let t = text.trim().replace('−', "-");
    if let Ok(n) = t.parse::<i64>() {
        return u8::try_from(n).ok().filter(|k| *k <= 127);
    }
    let mut chars = t.chars();
    let base: i64 = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave) = if let Some(r) = rest.strip_prefix(['#', '♯']) {
        (1, r)
    } else if let Some(r) = rest.strip_prefix(['b', '♭']) {
        (-1, r)
    } else {
        (0, rest)
    };
    let octave: i64 = octave.trim().parse().ok()?;
    u8::try_from((octave + 1) * 12 + base + accidental)
        .ok()
        .filter(|k| *k <= 127)
}

/// "C0, D0@90" (a velocity only when it is not 100).
pub fn keyswitches_text(keys: &[Keyswitch]) -> String {
    keys.iter()
        .map(|k| {
            if k.velocity == 100 {
                note_name(k.key)
            } else {
                format!("{}@{}", note_name(k.key), k.velocity)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Typed keyswitches: keys by name or number, each with an optional
/// velocity ("C0@90" or "C0 90"), separated by commas or semicolons;
/// empty = none.
pub fn parse_keyswitches(text: &str) -> Option<Vec<Keyswitch>> {
    let mut out = Vec::new();
    for part in text.split([',', ';']) {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let split = p
            .split_once('@')
            .or_else(|| p.split_once(char::is_whitespace));
        let (key, velocity) = match split {
            Some((k, v)) => {
                let v: u8 = v.trim().parse().ok()?;
                (k, v)
            }
            None => (p, 100),
        };
        if !(1..=127).contains(&velocity) {
            return None;
        }
        out.push(Keyswitch {
            key: parse_key(key)?,
            velocity,
        });
    }
    Some(out)
}

/// "1=64, 68=127".
pub fn controls_text(controls: &[ControlValue]) -> String {
    controls
        .iter()
        .map(|c| format!("{}={}", c.controller, c.value))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Typed controller values: "controller=value" (or "CC 1=64", "1 64"),
/// separated by commas or semicolons; empty = none.
pub fn parse_controls(text: &str) -> Option<Vec<ControlValue>> {
    let mut out = Vec::new();
    for part in text.split([',', ';']) {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let p = p
            .strip_prefix("CC")
            .or_else(|| p.strip_prefix("cc"))
            .unwrap_or(p)
            .trim();
        let (c, v) = p
            .split_once('=')
            .or_else(|| p.split_once(char::is_whitespace))?;
        let controller: u8 = c.trim().parse().ok().filter(|c| *c <= 127)?;
        let value: u8 = v.trim().parse().ok().filter(|v| *v <= 127)?;
        out.push(ControlValue { controller, value });
    }
    Some(out)
}

/// A typed whole number ("−3", "+12", "80%").
pub fn parse_int(text: &str) -> Option<i32> {
    let t = text.trim().replace('−', "-");
    let t = t.trim_end_matches('%').trim();
    let t = t.strip_prefix('+').unwrap_or(t);
    t.parse().ok()
}

/// Semitones with their sign ("+12", "−3", "0").
pub fn semitones(v: i32) -> String {
    match v {
        0 => "0".into(),
        v if v > 0 => format!("+{v}"),
        v => format!("−{}", -v),
    }
}

/// An empty answer (none, as the track, as written).
pub fn is_blank(text: &str) -> bool {
    matches!(text.trim(), "" | "-" | "–" | "—")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_keyswitches_and_controllers_read_back_as_written() {
        assert_eq!(parse_key("C4"), Some(60));
        assert_eq!(parse_key("c-1"), Some(0));
        assert_eq!(parse_key("F#2"), Some(42));
        assert_eq!(parse_key("Bb0"), Some(22));
        assert_eq!(parse_key("127"), Some(127));
        assert_eq!(parse_key("128"), None);
        assert_eq!(parse_key("H2"), None);
        let ks = parse_keyswitches("C0, D0@90; 26 64").unwrap();
        assert_eq!(
            ks,
            vec![
                Keyswitch {
                    key: 12,
                    velocity: 100
                },
                Keyswitch {
                    key: 14,
                    velocity: 90
                },
                Keyswitch {
                    key: 26,
                    velocity: 64
                },
            ]
        );
        assert_eq!(keyswitches_text(&ks), "C0, D0@90, D1@64");
        assert_eq!(parse_keyswitches(&keyswitches_text(&ks)).unwrap(), ks);
        assert_eq!(parse_keyswitches("").unwrap(), vec![]);
        assert!(parse_keyswitches("C0@0").is_none());
        assert!(parse_keyswitches("X9").is_none());
        let cc = parse_controls("1=64, CC 68 = 127; 11 100").unwrap();
        assert_eq!(controls_text(&cc), "1=64, 68=127, 11=100");
        assert!(parse_controls("1").is_none());
        assert!(parse_controls("1=200").is_none());
        assert_eq!(parse_int("−12"), Some(-12));
        assert_eq!(parse_int("+3"), Some(3));
        assert_eq!(parse_int("80 %"), Some(80));
        assert_eq!(semitones(-5), "−5");
        assert!(is_blank(" – "));
    }
}

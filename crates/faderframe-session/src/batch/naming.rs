//! Naming written files by a pattern: text with parts in braces, e.g.
//! `{n:2} {name} ({rate})` → "03 Kick (48k)". `{{` and `}}` are braces
//! themselves. The result is made a file name (characters a file system
//! may refuse become `_`), and the format's extension is added.

use super::{BatchOutput, Stamp, depth_text};
use std::path::Path;

/// The parts a pattern may use, with what each gives.
pub const TOKENS: &[(&str, &str)] = &[
    ("{name}", "the file's name without its extension"),
    ("{ext}", "its extension (wav, mp3 …)"),
    ("{folder}", "the name of the folder it is in"),
    ("{n}", "its place in the list from 1 ({n:3} gives 001)"),
    ("{preset}", "the preset's name"),
    ("{date}", "today (2026-11-03)"),
    ("{time}", "the time the batch started (14-03-12)"),
    ("{rate}", "the sample rate written (44.1k, 48k)"),
    ("{bits}", "the depth written (16, 24, 32f)"),
    ("{channels}", "mono, stereo or the channel count (6ch)"),
    ("{format}", "wav or flac"),
];

/// What a name is made of.
pub(super) struct NameContext<'a> {
    pub input: &'a Path,
    /// From 0.
    pub index: usize,
    pub preset: &'a str,
    pub stamp: &'a Stamp,
    pub rate: u32,
    pub channels: usize,
    pub output: &'a BatchOutput,
}

/// One piece of a parsed pattern.
#[derive(Debug, PartialEq)]
enum Piece<'a> {
    Text(String),
    Token { name: &'a str, width: usize },
}

fn parse(pattern: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = pattern;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("{{") || rest.starts_with("}}") {
            text.push(c);
            rest = &rest[2..];
            continue;
        }
        if c == '}' {
            return Err("a '}' without its '{' (write '}}' for a brace)".into());
        }
        if c != '{' {
            text.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let Some(end) = rest.find('}') else {
            return Err(format!("'{rest}' is not closed with '}}'"));
        };
        let inner = &rest[1..end];
        let (name, width) = match inner.split_once(':') {
            Some((name, w)) => {
                let width = w
                    .parse::<usize>()
                    .ok()
                    .filter(|w| (1..=9).contains(w))
                    .ok_or_else(|| format!("'{{{inner}}}': the width is a digit from 1 to 9"))?;
                if name != "n" {
                    return Err(format!("'{{{inner}}}': only {{n}} takes a width"));
                }
                (name, width)
            }
            None => (inner, 0),
        };
        if !TOKENS.iter().any(|(t, _)| t[1..t.len() - 1] == *name) {
            return Err(format!(
                "'{{{inner}}}' is not a name part (they are {})",
                TOKENS.iter().map(|(t, _)| *t).collect::<Vec<_>>().join(" ")
            ));
        }
        if !text.is_empty() {
            out.push(Piece::Text(std::mem::take(&mut text)));
        }
        out.push(Piece::Token { name, width });
        rest = &rest[end + 1..];
    }
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    Ok(out)
}

/// Is `pattern` one names can be made from?
pub fn check_pattern(pattern: &str) -> Result<(), String> {
    let pieces = parse(pattern)?;
    if pieces.iter().all(|p| match p {
        Piece::Text(t) => t.trim().is_empty(),
        Piece::Token { .. } => false,
    }) {
        return Err("the name pattern is empty".into());
    }
    Ok(())
}

/// "44.1k", "48k", "22.05k".
pub fn rate_text(rate: u32) -> String {
    let k = format!("{:.3}", f64::from(rate) / 1000.0);
    let k = k.trim_end_matches('0').trim_end_matches('.');
    format!("{k}k")
}

/// The file name (without extension) `pattern` gives in `cx`.
pub(super) fn file_stem(pattern: &str, cx: &NameContext<'_>) -> Result<String, String> {
    let mut out = String::new();
    for piece in parse(pattern)? {
        match piece {
            Piece::Text(t) => out.push_str(&t),
            Piece::Token { name, width } => out.push_str(&value(name, width, cx)),
        }
    }
    let stem = faderframe_audio_files::import::clean_stem(&out);
    Ok(stem)
}

fn value(name: &str, width: usize, cx: &NameContext<'_>) -> String {
    let os = |s: Option<&std::ffi::OsStr>| s.map(|s| s.to_string_lossy().to_string());
    match name {
        "name" => os(cx.input.file_stem()).unwrap_or_default(),
        "ext" => os(cx.input.extension()).unwrap_or_default(),
        "folder" => os(cx.input.parent().and_then(Path::file_name)).unwrap_or_default(),
        "n" => format!("{:0width$}", cx.index + 1),
        "preset" => {
            if cx.preset.trim().is_empty() {
                "Batch".into()
            } else {
                cx.preset.trim().to_string()
            }
        }
        "date" => cx.stamp.date.clone(),
        "time" => cx.stamp.time.clone(),
        "rate" => rate_text(cx.rate),
        "bits" => depth_text(cx.output.depth).to_string(),
        "channels" => match cx.channels {
            1 => "mono".into(),
            2 => "stereo".into(),
            n => format!("{n}ch"),
        },
        "format" => cx.output.format.extension().to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::batch::FileFormat;

    fn cx<'a>(output: &'a BatchOutput, stamp: &'a Stamp) -> NameContext<'a> {
        NameContext {
            input: Path::new("/takes/Drums/Kick In.aiff"),
            index: 2,
            preset: "Podcast",
            stamp,
            rate: 44_100,
            channels: 2,
            output,
        }
    }

    #[test]
    fn every_part_is_filled_in() {
        let stamp = Stamp {
            date: "2026-11-03".into(),
            time: "14-03-12".into(),
        };
        let mut output = BatchOutput::default();
        let name = |p: &str, o: &BatchOutput| file_stem(p, &cx(o, &stamp)).unwrap();
        assert_eq!(name("{name}", &output), "Kick In");
        assert_eq!(name("{n:3} {name}", &output), "003 Kick In");
        assert_eq!(name("{n}-{folder}-{ext}", &output), "3-Drums-aiff");
        assert_eq!(
            name("{name} ({rate} {bits} {channels})", &output),
            "Kick In (44.1k 24 stereo)"
        );
        assert_eq!(
            name("{preset} {date} {time}", &output),
            "Podcast 2026-11-03 14-03-12"
        );
        output.format = FileFormat::Flac;
        assert_eq!(name("{name}.{format}", &output), "Kick In.flac");
        // Braces, and characters a file name cannot hold.
        assert_eq!(name("{{{name}}}", &output), "_Kick In_");
        assert_eq!(name("a/b: {name}", &output), "a_b_ Kick In");
    }

    #[test]
    fn mistakes_are_named() {
        assert!(check_pattern("{name}").is_ok());
        assert!(check_pattern("take {n:2}").is_ok());
        for bad in ["{nmae}", "{name", "name}", "{name:2}", "{n:0}", "", "   "] {
            assert!(check_pattern(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rates_read_as_kilohertz() {
        assert_eq!(rate_text(44_100), "44.1k");
        assert_eq!(rate_text(48_000), "48k");
        assert_eq!(rate_text(22_050), "22.05k");
        assert_eq!(rate_text(88_200), "88.2k");
    }
}

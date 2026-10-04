//! Cue sheets for a disc's audio file: `CATALOG`, CD-Text commands,
//! `ISRC`, `FLAGS` and indexes, as burning programs and cue2ddp read them.

use crate::{CdText, Disc, msf_colon};

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "'"))
}

fn text_lines(out: &mut String, t: &CdText, indent: &str) {
    for (cmd, v) in [
        ("TITLE", &t.title),
        ("PERFORMER", &t.performer),
        ("SONGWRITER", &t.songwriter),
        ("COMPOSER", &t.composer),
        ("ARRANGER", &t.arranger),
        ("MESSAGE", &t.message),
    ] {
        if !v.is_empty() {
            *out += &format!("{indent}{cmd} {}\n", quoted(v));
        }
    }
}

/// A cue sheet for `disc`'s audio in `file` (of type `kind`: `WAVE`,
/// `BINARY`). The file starts at sector `offset` of the disc: index times
/// are relative to it, and indexes before it (track 1's pregap when the
/// file leaves it out) are dropped.
pub fn cue_sheet(disc: &Disc, file: &str, kind: &str, offset: u32) -> String {
    let mut out = String::new();
    if let Some(upc) = &disc.upc {
        out += &format!("CATALOG {upc}\n");
    }
    text_lines(&mut out, &disc.text, "");
    out += &format!("FILE {} {kind}\n", quoted(file));
    for (i, t) in disc.tracks.iter().enumerate() {
        out += &format!("  TRACK {:02} AUDIO\n", i + 1);
        text_lines(&mut out, &t.text, "    ");
        if let Some(isrc) = &t.isrc {
            out += &format!("    ISRC {isrc}\n");
        }
        let flags: Vec<&str> = [
            (t.flags.pre_emphasis, "PRE"),
            (t.flags.copy_permitted, "DCP"),
            (t.flags.four_channel, "4CH"),
            (t.flags.scms, "SCMS"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        if !flags.is_empty() {
            out += &format!("    FLAGS {}\n", flags.join(" "));
        }
        let indexes = t
            .pregap
            .map(|p| (0usize, p))
            .into_iter()
            .chain(t.indexes.iter().enumerate().map(|(k, &x)| (k + 1, x)));
        for (n, at) in indexes {
            if at < offset {
                continue;
            }
            out += &format!("    INDEX {n:02} {}\n", msf_colon(at - offset));
        }
    }
    out
}

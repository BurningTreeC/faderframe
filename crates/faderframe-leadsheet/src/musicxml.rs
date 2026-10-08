//! A lead sheet as MusicXML 4.0 (`score-partwise`, one part): what
//! notation programs (MuseScore, Dorico, Sibelius, Finale) open. Elements
//! go in the order the schema asks for.

use crate::score::{Clef, DIVISIONS, Event, LeadSheet, Measure, Pitch, Tuplet};
use std::fmt::Write;

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn accidental_name(alter: i8) -> &'static str {
    match alter {
        -2 => "flat-flat",
        -1 => "flat",
        1 => "sharp",
        2 => "double-sharp",
        _ => "natural",
    }
}

/// The document. `date` (YYYY-MM-DD) goes into the encoding.
pub fn write(sheet: &LeadSheet, part_name: &str, date: &str) -> String {
    let mut x = String::new();
    // Writing to a String does not fail.
    let _ = write_into(&mut x, sheet, part_name, date);
    x
}

fn write_into(x: &mut String, s: &LeadSheet, part_name: &str, date: &str) -> std::fmt::Result {
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n");
    x.push_str("<!DOCTYPE score-partwise PUBLIC \"-//Recordare//DTD MusicXML 4.0 Partwise//EN\" \"http://www.musicxml.org/dtds/partwise.dtd\">\n");
    x.push_str("<score-partwise version=\"4.0\">\n");
    writeln!(
        x,
        "  <work><work-title>{}</work-title></work>",
        esc(&s.title)
    )?;
    x.push_str("  <identification>\n");
    if !s.composer.is_empty() {
        writeln!(
            x,
            "    <creator type=\"composer\">{}</creator>",
            esc(&s.composer)
        )?;
    }
    writeln!(
        x,
        "    <encoding><software>FaderFrame</software><encoding-date>{date}</encoding-date></encoding>"
    )?;
    x.push_str("  </identification>\n");
    x.push_str("  <part-list>\n");
    writeln!(
        x,
        "    <score-part id=\"P1\"><part-name>{}</part-name></score-part>",
        esc(part_name)
    )?;
    x.push_str("  </part-list>\n");
    x.push_str("  <part id=\"P1\">\n");
    let last = s.measures.len().saturating_sub(1);
    for (i, m) in s.measures.iter().enumerate() {
        writeln!(x, "    <measure number=\"{}\">", i + 1)?;
        if i == 0 || m.show_time {
            x.push_str("      <attributes>\n");
            if i == 0 {
                writeln!(x, "        <divisions>{DIVISIONS}</divisions>")?;
                writeln!(
                    x,
                    "        <key><fifths>{}</fifths><mode>{}</mode></key>",
                    s.key.fifths,
                    if s.key.minor { "minor" } else { "major" }
                )?;
            }
            if m.show_time {
                writeln!(
                    x,
                    "        <time><beats>{}</beats><beat-type>{}</beat-type></time>",
                    m.time.0, m.time.1
                )?;
            }
            if i == 0 {
                x.push_str(match s.clef {
                    Clef::Treble => "        <clef><sign>G</sign><line>2</line></clef>\n",
                    Clef::Treble8vb => "        <clef><sign>G</sign><line>2</line><clef-octave-change>-1</clef-octave-change></clef>\n",
                    Clef::Bass => "        <clef><sign>F</sign><line>4</line></clef>\n",
                });
            }
            x.push_str("      </attributes>\n");
        }
        if i == 0
            && let Some(t) = s.tempo
        {
            writeln!(
                x,
                "      <direction placement=\"above\"><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>{}</per-minute></metronome></direction-type><sound tempo=\"{}\"/></direction>",
                t.round(),
                (t * 100.0).round() / 100.0
            )?;
        }
        measure(x, m)?;
        if i == last {
            x.push_str(
                "      <barline location=\"right\"><bar-style>light-heavy</bar-style></barline>\n",
            );
        }
        x.push_str("    </measure>\n");
    }
    x.push_str("  </part>\n");
    x.push_str("</score-partwise>\n");
    Ok(())
}

fn measure(x: &mut String, m: &Measure) -> std::fmt::Result {
    let mut chords = m.chords.iter().peekable();
    for e in &m.events {
        // Chords starting while this note sounds go before it (with an
        // offset when inside it).
        while let Some(c) = chords.peek() {
            if c.at >= e.end() {
                break;
            }
            let (root, alter) = c.root;
            x.push_str("      <harmony>");
            write!(
                x,
                "<root><root-step>{}</root-step>",
                Pitch::STEPS[usize::from(root)]
            )?;
            if alter != 0 {
                write!(x, "<root-alter>{alter}</root-alter>")?;
            }
            x.push_str("</root>");
            write!(x, "<kind text=\"{}\">{}</kind>", esc(&c.suffix), c.kind)?;
            if let Some((b, a)) = c.bass {
                write!(
                    x,
                    "<bass><bass-step>{}</bass-step>",
                    Pitch::STEPS[usize::from(b)]
                )?;
                if a != 0 {
                    write!(x, "<bass-alter>{a}</bass-alter>")?;
                }
                x.push_str("</bass>");
            }
            for (value, alter, kind) in &c.degrees {
                write!(
                    x,
                    "<degree><degree-value>{value}</degree-value><degree-alter>{alter}</degree-alter><degree-type>{kind}</degree-type></degree>"
                )?;
            }
            if c.at > e.at {
                write!(x, "<offset>{}</offset>", c.at - e.at)?;
            }
            x.push_str("</harmony>\n");
            chords.next();
        }
        note(x, e, m.length())?;
    }
    Ok(())
}

fn note(x: &mut String, e: &Event, bar: u32) -> std::fmt::Result {
    x.push_str("      <note>");
    match &e.pitch {
        Some(p) => {
            write!(
                x,
                "<pitch><step>{}</step>",
                Pitch::STEPS[usize::from(p.step)]
            )?;
            if p.alter != 0 {
                write!(x, "<alter>{}</alter>", p.alter)?;
            }
            write!(x, "<octave>{}</octave></pitch>", p.octave)?;
        }
        None if e.bar_rest => x.push_str("<rest measure=\"yes\"/>"),
        None => x.push_str("<rest/>"),
    }
    let duration = if e.bar_rest {
        bar
    } else {
        e.duration.divisions()
    };
    write!(x, "<duration>{duration}</duration>")?;
    if e.tied_from {
        x.push_str("<tie type=\"stop\"/>");
    }
    if e.tie {
        x.push_str("<tie type=\"start\"/>");
    }
    x.push_str("<voice>1</voice>");
    if !e.bar_rest {
        write!(x, "<type>{}</type>", e.duration.value.name())?;
        for _ in 0..e.duration.dots {
            x.push_str("<dot/>");
        }
    }
    if let Some(a) = e.pitch.and_then(|p| p.accidental) {
        write!(x, "<accidental>{}</accidental>", accidental_name(a))?;
    }
    if e.duration.triplet {
        x.push_str("<time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification>");
    }
    for (level, b) in e.beams.iter().enumerate() {
        if let Some(b) = b {
            write!(x, "<beam number=\"{}\">{}</beam>", level + 1, b.name())?;
        }
    }
    let tuplet = match e.tuplet {
        Some(Tuplet::Start) if e.duration.triplet => Some("start"),
        Some(Tuplet::Stop) => Some("stop"),
        _ => None,
    };
    if e.tie || e.tied_from || tuplet.is_some() {
        x.push_str("<notations>");
        if e.tied_from {
            x.push_str("<tied type=\"stop\"/>");
        }
        if e.tie {
            x.push_str("<tied type=\"start\"/>");
        }
        if let Some(t) = tuplet {
            write!(x, "<tuplet type=\"{t}\"/>")?;
        }
        x.push_str("</notations>");
    }
    if let Some(l) = &e.lyric {
        write!(
            x,
            "<lyric number=\"1\"><syllabic>single</syllabic><text>{}</text>",
            esc(&l.text)
        )?;
        if l.extend {
            x.push_str("<extend/>");
        }
        x.push_str("</lyric>");
    }
    x.push_str("</note>\n");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::build::{Bar, ChordAt, Input, Line, Note, build};
    use faderframe_midi::theory::{Chord, Key, Quality, Scale};

    pub(crate) fn sample() -> LeadSheet {
        let bars = (0..4)
            .map(|i| Bar {
                start: f64::from(i) * 4.0,
                time: (4, 4),
            })
            .collect();
        let tune = [
            (0.0, 1.0, 69),
            (1.0, 1.5, 71),
            (1.5, 2.0, 72),
            (2.0, 4.0, 76),
            (4.0, 4.75, 74),
            (4.75, 5.0, 72),
            (5.0, 6.0, 71),
            (6.0, 6.333, 72),
            (6.333, 6.667, 71),
            (6.667, 7.0, 69),
            (7.0, 8.0, 68),
            (9.0, 12.0, 69),
        ];
        build(&Input {
            title: "A & B <test>".into(),
            composer: "FaderFrame".into(),
            key: Some(Key::new(9, Scale::Minor)),
            bars,
            tempo: Some(96.0),
            notes: tune
                .iter()
                .map(|&(start, end, key)| Note { start, end, key })
                .collect(),
            chords: vec![
                ChordAt {
                    start: 0.0,
                    chord: Chord::new(9, Quality::Minor7),
                },
                ChordAt {
                    start: 4.0,
                    chord: Chord::new(2, Quality::Minor),
                },
                ChordAt {
                    start: 6.0,
                    chord: Chord::new(4, Quality::Dominant7),
                },
                ChordAt {
                    start: 8.0,
                    chord: Chord::new(9, Quality::Minor).with_bass(Some(0)),
                },
            ],
            lines: vec![
                Line {
                    start: 0.0,
                    end: 4.0,
                    text: "Sing a song".into(),
                },
                Line {
                    start: 4.0,
                    end: 11.0,
                    text: "of the night".into(),
                },
            ],
            ..Input::default()
        })
    }

    #[test]
    fn it_writes_a_partwise_score() {
        let x = write(&sample(), "Voice", "2026-10-08");
        assert!(x.starts_with("<?xml"));
        assert!(x.contains("<work-title>A &amp; B &lt;test&gt;</work-title>"));
        assert!(x.contains("<fifths>0</fifths><mode>minor</mode>"));
        assert!(x.contains("<beats>4</beats><beat-type>4</beat-type>"));
        assert!(x.contains("<kind text=\"m7\">minor-seventh</kind>"));
        assert!(x.contains("<bass><bass-step>C</bass-step></bass>"));
        assert!(x.contains("<per-minute>96</per-minute>"));
        assert!(x.contains("<text>Sing</text>"));
        assert!(x.contains("<accidental>sharp</accidental>"), "G♯");
        assert!(x.contains("<actual-notes>3</actual-notes>"));
        assert!(x.contains("<tie type=\"start\"/>"));
        assert!(x.contains("light-heavy"));
        // Every measure adds up to its length.
        for m in x.split("<measure ").skip(1) {
            let d: u32 = m
                .split("<duration>")
                .skip(1)
                .filter_map(|p| p.split('<').next()?.parse::<u32>().ok())
                .sum();
            assert_eq!(d, 48, "{m}");
        }
        // Well formed: tags open and close in order.
        let mut stack: Vec<String> = Vec::new();
        for tag in x.split('<').skip(1) {
            let t = tag.split('>').next().unwrap_or("");
            if t.starts_with('?') || t.starts_with('!') || t.ends_with('/') {
                continue;
            }
            let name = t.split_whitespace().next().unwrap_or("");
            if let Some(close) = name.strip_prefix('/') {
                assert_eq!(stack.pop().as_deref(), Some(close));
            } else {
                stack.push(name.to_string());
            }
        }
        assert!(stack.is_empty(), "{stack:?}");
        if let Ok(dir) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
            let _ = std::fs::write(std::path::Path::new(&dir).join("sample.musicxml"), &x);
        }
    }
}

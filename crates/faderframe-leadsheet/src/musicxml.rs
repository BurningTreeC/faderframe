//! A lead sheet as MusicXML 4.0 (`score-partwise`, a part a staff; a
//! score's parts bracketed in one group): what notation programs
//! (MuseScore, Dorico, Sibelius, Finale) open. Elements go in the order
//! the schema asks for. Chords are notes with `<chord/>`; tablature is a
//! part with a TAB clef, its strings' tuning in `staff-details` and each
//! note's string and fret.

use crate::score::{Clef, DIVISIONS, Event, LeadSheet, Measure, Part, Pitch, TabStaff, Tuplet};
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
pub fn write(sheet: &LeadSheet, date: &str) -> String {
    let mut x = String::new();
    // Writing to a String does not fail.
    let _ = write_into(&mut x, sheet, date);
    x
}

fn write_into(x: &mut String, s: &LeadSheet, date: &str) -> std::fmt::Result {
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
    let score = s.parts.len() > 1;
    if score {
        writeln!(
            x,
            "    <part-group type=\"start\" number=\"1\"><group-symbol>bracket</group-symbol><group-barline>{}</group-barline></part-group>",
            if s.bar_lines_joined() { "yes" } else { "no" }
        )?;
    }
    for (n, part) in s.parts.iter().enumerate() {
        // Tablature under a part's notes is named after it.
        let name = if part.name.is_empty() && part.tab.is_some() {
            let above = n.checked_sub(1).map_or("", |m| s.parts[m].name.as_str());
            if above.is_empty() {
                "Tablature".to_string()
            } else {
                format!("{above} (Tab)")
            }
        } else {
            part.name.clone()
        };
        write!(
            x,
            "    <score-part id=\"P{}\"><part-name>{}</part-name>",
            n + 1,
            esc(&name)
        )?;
        if score {
            write!(
                x,
                "<part-abbreviation>{}</part-abbreviation>",
                esc(&part.abbreviation())
            )?;
        }
        x.push_str("</score-part>\n");
    }
    if score {
        x.push_str("    <part-group type=\"stop\" number=\"1\"/>\n");
    }
    x.push_str("  </part-list>\n");
    for (n, part) in s.parts.iter().enumerate() {
        write_part(x, s, part, n)?;
    }
    x.push_str("</score-partwise>\n");
    Ok(())
}

/// Part `n`'s bars (the tempo marked in the first part's).
fn write_part(x: &mut String, s: &LeadSheet, part: &Part, n: usize) -> std::fmt::Result {
    writeln!(x, "  <part id=\"P{}\">", n + 1)?;
    let last = part.measures.len().saturating_sub(1);
    for (i, m) in part.measures.iter().enumerate() {
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
                match &part.tab {
                    Some(t) => tab_attributes(x, t)?,
                    None => x.push_str(match part.clef {
                        Clef::Treble => "        <clef><sign>G</sign><line>2</line></clef>\n",
                        Clef::Treble8vb => "        <clef><sign>G</sign><line>2</line><clef-octave-change>-1</clef-octave-change></clef>\n",
                        Clef::Bass => "        <clef><sign>F</sign><line>4</line></clef>\n",
                        Clef::Bass8vb => "        <clef><sign>F</sign><line>4</line><clef-octave-change>-1</clef-octave-change></clef>\n",
                    }),
                }
            }
            x.push_str("      </attributes>\n");
        }
        if i == 0
            && n == 0
            && let Some(t) = s.tempo
        {
            writeln!(
                x,
                "      <direction placement=\"above\"><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>{}</per-minute></metronome></direction-type><sound tempo=\"{}\"/></direction>",
                t.round(),
                (t * 100.0).round() / 100.0
            )?;
        }
        measure(x, m, part.tab.is_some())?;
        if i == last {
            x.push_str(
                "      <barline location=\"right\"><bar-style>light-heavy</bar-style></barline>\n",
            );
        }
        x.push_str("    </measure>\n");
    }
    x.push_str("  </part>\n");
    Ok(())
}

/// A tablature's clef and strings (MusicXML counts lines from the bottom:
/// the lowest string first).
fn tab_attributes(x: &mut String, t: &TabStaff) -> std::fmt::Result {
    x.push_str("        <clef><sign>TAB</sign><line>5</line></clef>\n");
    writeln!(
        x,
        "        <staff-details><staff-lines>{}</staff-lines>",
        t.strings.len()
    )?;
    for (line, open) in t.strings.iter().rev().enumerate() {
        let (step, alter, octave) = crate::build::spell(*open, crate::score::KeySig::C);
        write!(
            x,
            "          <staff-tuning line=\"{}\"><tuning-step>{}</tuning-step>",
            line + 1,
            Pitch::STEPS[usize::from(step)]
        )?;
        if alter != 0 {
            write!(x, "<tuning-alter>{alter}</tuning-alter>")?;
        }
        writeln!(x, "<tuning-octave>{octave}</tuning-octave></staff-tuning>")?;
    }
    if t.capo > 0 {
        writeln!(x, "          <capo>{}</capo>", t.capo)?;
    }
    x.push_str("        </staff-details>\n");
    Ok(())
}

fn measure(x: &mut String, m: &Measure, tab: bool) -> std::fmt::Result {
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
        // The chord's notes: the first with the beams and the words.
        let n = 1 + e.chord.len();
        for k in 0..n {
            note(x, e, m.length(), k, tab)?;
        }
    }
    Ok(())
}

/// The `k`th note of event `e` (0: its pitch, then its chord's).
fn note(x: &mut String, e: &Event, bar: u32, k: usize, tab: bool) -> std::fmt::Result {
    x.push_str("      <note>");
    if k > 0 {
        x.push_str("<chord/>");
    }
    let pitch = if k == 0 {
        e.pitch
    } else {
        e.chord.get(k - 1).copied()
    };
    match &pitch {
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
    if let Some(a) = pitch.and_then(|p| p.accidental) {
        write!(x, "<accidental>{}</accidental>", accidental_name(a))?;
    }
    if e.duration.triplet {
        x.push_str("<time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification>");
    }
    if k == 0 {
        for (level, b) in e.beams.iter().enumerate() {
            if let Some(b) = b {
                write!(x, "<beam number=\"{}\">{}</beam>", level + 1, b.name())?;
            }
        }
    }
    let tuplet = match e.tuplet {
        Some(Tuplet::Start) if e.duration.triplet && k == 0 => Some("start"),
        Some(Tuplet::Stop) if k == 0 => Some("stop"),
        _ => None,
    };
    let fret = e.frets.get(k).filter(|_| tab);
    if e.tie || e.tied_from || tuplet.is_some() || fret.is_some() {
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
        if let Some(f) = fret {
            write!(
                x,
                "<technical><string>{}</string><fret>{}</fret></technical>",
                f.string, f.fret
            )?;
        }
        x.push_str("</notations>");
    }
    if let Some(l) = e.lyric.as_ref().filter(|_| k == 0) {
        write!(
            x,
            "<lyric number=\"1\"><syllabic>{}</syllabic><text>{}</text>",
            l.syllabic.name(),
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
    use crate::build::{Bar, ChordAt, Input, Line, Note, Staff, build};
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
            staves: vec![Staff {
                name: "Voice".into(),
                notes: tune
                    .iter()
                    .map(|&(start, end, key)| Note { start, end, key })
                    .collect(),
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
                tab: None,
            }],
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
            ..Input::default()
        })
    }

    /// A score of `bars` bars in A minor: a sung tune with its words, a
    /// guitar's eighths (treble 8vb) and a bass line (bass clef), the
    /// chords Am Dm E7 Am a bar each.
    pub(crate) fn score_sample(bars: usize) -> LeadSheet {
        let q = |b: usize, beat: f64| b as f64 * 4.0 + beat;
        let (mut voice, mut guitar, mut bass) = (Vec::new(), Vec::new(), Vec::new());
        let mut lines = Vec::new();
        let mut chords = Vec::new();
        // Each chord's root (pitch class), tones for the guitar and the
        // tune's notes over the bar.
        type Change = (u8, Quality, [u8; 4], &'static [(f64, f64, u8)]);
        let changes: [Change; 4] = [
            (
                9,
                Quality::Minor,
                [52, 57, 60, 57],
                &[
                    (0.0, 1.0, 69),
                    (1.0, 1.5, 71),
                    (1.5, 2.0, 72),
                    (2.0, 4.0, 76),
                ],
            ),
            (
                2,
                Quality::Minor,
                [53, 57, 62, 57],
                &[
                    (0.0, 0.75, 74),
                    (0.75, 1.0, 72),
                    (1.0, 2.0, 71),
                    (2.0, 4.0, 69),
                ],
            ),
            (
                4,
                Quality::Dominant7,
                [52, 56, 59, 62],
                &[
                    (0.0, 1.0, 68),
                    (1.0, 2.0, 71),
                    (2.0, 3.0, 74),
                    (3.0, 4.0, 76),
                ],
            ),
            (9, Quality::Minor, [52, 57, 60, 64], &[(0.0, 4.0, 69)]),
        ];
        for b in 0..bars {
            let (root, quality, tones, tune) = changes[b % 4];
            chords.push(ChordAt {
                start: q(b, 0.0),
                chord: Chord::new(root, quality),
            });
            for &(a, z, key) in tune {
                voice.push(Note {
                    start: q(b, a),
                    end: q(b, z),
                    key,
                });
            }
            for i in 0..8 {
                guitar.push(Note {
                    start: q(b, i as f64 * 0.5),
                    end: q(b, i as f64 * 0.5 + 0.5),
                    key: tones[i % 4],
                });
            }
            let low = 36 + root;
            bass.push(Note {
                start: q(b, 0.0),
                end: q(b, 2.0),
                key: low,
            });
            bass.push(Note {
                start: q(b, 2.0),
                end: q(b, 4.0),
                key: low + 7,
            });
            if b % 2 == 0 {
                lines.push(Line {
                    start: q(b, 0.0),
                    end: q(b + 2, 0.0),
                    text: if b % 4 == 0 {
                        "Sing me a song of the night"
                    } else {
                        "and the morning light"
                    }
                    .into(),
                });
            }
        }
        build(&Input {
            title: "Night & Morning".into(),
            composer: "FaderFrame".into(),
            key: Some(Key::new(9, Scale::Minor)),
            bars: (0..bars)
                .map(|i| Bar {
                    start: i as f64 * 4.0,
                    time: (4, 4),
                })
                .collect(),
            tempo: Some(84.0),
            staves: vec![
                Staff {
                    name: "Lead Vocal".into(),
                    notes: voice,
                    lines,
                    tab: None,
                },
                Staff {
                    name: "Guitar".into(),
                    notes: guitar,
                    lines: vec![],
                    tab: None,
                },
                Staff {
                    name: "Bass".into(),
                    notes: bass,
                    lines: vec![],
                    tab: None,
                },
            ],
            chords,
            ..Input::default()
        })
    }

    #[test]
    fn a_score_writes_a_part_a_staff() {
        let s = score_sample(8);
        assert_eq!(s.parts[1].clef, Clef::Treble8vb);
        assert_eq!(s.parts[2].clef, Clef::Bass);
        let x = write(&s, "2026-10-13");
        // The parts listed, bracketed, words under the top one so its bar
        // lines stand alone.
        assert!(x.contains("<part-group type=\"start\" number=\"1\"><group-symbol>bracket</group-symbol><group-barline>no</group-barline></part-group>"));
        assert!(x.contains("<score-part id=\"P1\"><part-name>Lead Vocal</part-name><part-abbreviation>Lead Voc.</part-abbreviation></score-part>"));
        assert!(x.contains("<score-part id=\"P3\"><part-name>Bass</part-name>"));
        assert!(x.contains("<part-group type=\"stop\" number=\"1\"/>"));
        let parts: Vec<&str> = x.split("  <part id=").skip(1).collect();
        assert_eq!(parts.len(), 3);
        for (i, p) in parts.iter().enumerate() {
            assert!(p.starts_with(&format!("\"P{}\"", i + 1)));
            assert_eq!(p.matches("<measure ").count(), 8);
            assert_eq!(p.matches("light-heavy").count(), 1);
            // The key in each; the tempo and the chords in the first only.
            assert!(p.contains("<fifths>0</fifths><mode>minor</mode>"));
            assert_eq!(p.contains("<per-minute>84</per-minute>"), i == 0);
            assert_eq!(p.contains("<harmony>"), i == 0);
            assert_eq!(p.contains("<lyric "), i == 0);
        }
        assert!(parts[1].contains("<clef-octave-change>-1</clef-octave-change>"));
        assert!(parts[2].contains("<sign>F</sign><line>4</line>"));
        // Every measure adds up to its length.
        for m in x.split("<measure ").skip(1) {
            let d: u32 = m
                .split("<duration>")
                .skip(1)
                .filter_map(|p| p.split('<').next()?.parse::<u32>().ok())
                .sum();
            assert_eq!(d, 48, "{m}");
        }
        well_formed(&x);
        if let Ok(dir) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
            let _ = std::fs::write(std::path::Path::new(&dir).join("score.musicxml"), &x);
        }
    }

    /// Tags open and close in order.
    fn well_formed(x: &str) {
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
    }

    #[test]
    fn it_writes_a_partwise_score() {
        let x = write(&sample(), "2026-10-08");
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
        // One part, no group.
        assert!(x.contains("<score-part id=\"P1\"><part-name>Voice</part-name></score-part>"));
        assert!(!x.contains("part-group"));
        well_formed(&x);
        if let Ok(dir) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
            let _ = std::fs::write(std::path::Path::new(&dir).join("sample.musicxml"), &x);
        }
    }
}

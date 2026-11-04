//! Reading a MusicXML document: each part's measures parsed into events
//! at their places in the bar, the bars laid out in the order they are
//! played (repeats and endings), then velocities from dynamics, tied notes
//! joined and words held over their notes.

use super::*;
use crate::zip::ZipReader;
use crate::{InterchangeError, Result};
use faderframe_midi::theory::{Quality, Scale};
use roxmltree::Node;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

const EPS: f64 = 1e-6;

/// Read a score file: compressed (`.mxl`) or plain.
pub fn read_file(path: &Path) -> Result<Score> {
    let bytes = std::fs::read(path).map_err(|e| InterchangeError::io(path.display(), e))?;
    read_bytes(&bytes)
}

/// Read a score from its bytes: a zip is an `.mxl`; text may be UTF-8 or
/// UTF-16 (with its byte order mark).
pub fn read_bytes(bytes: &[u8]) -> Result<Score> {
    if bytes.starts_with(b"PK\x03\x04") {
        return read_mxl(bytes);
    }
    read_xml(&decode(bytes)?)
}

fn decode(bytes: &[u8]) -> Result<String> {
    let utf16 = |be: bool| -> Result<String> {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| {
                if be {
                    u16::from_be_bytes(c)
                } else {
                    u16::from_le_bytes(c)
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|e| InterchangeError::Xml(e.to_string()))
    };
    match bytes {
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        [0xEF, 0xBB, 0xBF, rest @ ..] => Ok(String::from_utf8_lossy(rest).into_owned()),
        _ => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// Read a compressed score: the file `META-INF/container.xml` names
/// (else the first score in it).
pub fn read_mxl(bytes: &[u8]) -> Result<Score> {
    let mut zip = ZipReader::new(std::io::Cursor::new(bytes), "compressed MusicXML")?;
    let mut root: Option<String> = None;
    if let Ok(c) = zip.read("META-INF/container.xml") {
        let text = decode(&c)?;
        let doc = parse(&text)?;
        root = doc
            .descendants()
            .filter(|n| n.has_tag_name("rootfile"))
            .filter(|n| {
                n.attribute("media-type").is_none_or(|m| {
                    m.contains("musicxml") || m == "application/xml" || m == "text/xml"
                })
            })
            .find_map(|n| n.attribute("full-path").map(str::to_string));
    }
    let name = match root {
        Some(r) => r,
        None => zip
            .entries()
            .iter()
            .map(|e| e.name.clone())
            .find(|n| {
                !n.starts_with("META-INF") && (n.ends_with(".xml") || n.ends_with(".musicxml"))
            })
            .ok_or_else(|| InterchangeError::Format("the .mxl file holds no score".into()))?,
    };
    let score = zip.read(&name)?;
    read_xml(&decode(&score)?)
}

fn parse(text: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            nodes_limit: u32::MAX,
            ..Default::default()
        },
    )
    .map_err(|e| InterchangeError::Xml(e.to_string()))
}

fn elements<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    elements(n).find(|c| c.has_tag_name(name))
}

fn text_of<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(n, name).and_then(|c| c.text()).map(str::trim)
}

fn number_of(n: Node, name: &str) -> Option<f64> {
    text_of(n, name)?.parse::<f64>().ok()
}

/// A MIDI instrument of a part (`<midi-instrument>`).
#[derive(Clone, Copy, Debug, Default)]
struct Instrument {
    channel: Option<u8>,
    program: Option<u8>,
    unpitched: Option<u8>,
}

#[derive(Clone, Debug, Default)]
struct LyricBit {
    text: String,
    syllabic: Syllabic,
    extend: bool,
    end_line: bool,
}

#[derive(Clone, Debug, Default)]
struct RawNote {
    at: f64,
    dur: f64,
    key: u8,
    voice: u8,
    staff: u8,
    tie_start: bool,
    tie_stop: bool,
    /// Sounding share of the written length.
    share: f64,
    accent: i32,
    /// The note's own `dynamics` (a velocity).
    velocity: Option<u8>,
    /// Dynamic marks in its notations (`sfz` on the note).
    marks: Vec<String>,
    lyrics: Vec<(u32, LyricBit)>,
    channel: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wedge {
    Crescendo,
    Diminuendo,
    Stop,
}

#[derive(Clone, Debug)]
enum Mark {
    Dynamic(String),
    Velocity(u8),
    Wedge(Wedge),
}

#[derive(Clone, Debug, Default)]
struct RawMeasure {
    number: String,
    length: f64,
    notes: Vec<RawNote>,
    marks: Vec<(f64, Mark)>,
    tempos: Vec<(f64, f64)>,
    harmonies: Vec<(f64, Option<Chord>)>,
    keys: Vec<(f64, Key)>,
    time: Option<(u8, u8)>,
    forward: bool,
    /// A backward repeat (and its `times`).
    backward: Option<Option<u32>>,
    ending_start: Option<Vec<u32>>,
    ending_stop: bool,
}

/// What a part carries from measure to measure while it is read.
struct PartReader<'a> {
    divisions: f64,
    transpose: i32,
    instruments: &'a HashMap<String, Instrument>,
    default: Instrument,
    staves: u8,
    tab: bool,
    percussion: bool,
    grace: usize,
    cue: usize,
    jumps: BTreeSet<&'static str>,
    notes: BTreeSet<String>,
}

fn step_semitone(step: &str) -> Option<i32> {
    Some(match step.trim() {
        "C" => 0,
        "D" => 2,
        "E" => 4,
        "F" => 5,
        "G" => 7,
        "A" => 9,
        "B" => 11,
        _ => return None,
    })
}

/// A time signature from `<time>` (composite ones summed).
fn time_of(n: Node) -> Option<(u8, u8)> {
    if child(n, "senza-misura").is_some() {
        return None;
    }
    let beats: Vec<f64> = elements(n)
        .filter(|c| c.has_tag_name("beats"))
        .map(|c| {
            c.text()
                .unwrap_or("")
                .split('+')
                .filter_map(|b| b.trim().parse::<f64>().ok())
                .sum()
        })
        .collect();
    let types: Vec<f64> = elements(n)
        .filter(|c| c.has_tag_name("beat-type"))
        .filter_map(|c| c.text()?.trim().parse::<f64>().ok())
        .collect();
    if beats.is_empty() || types.len() != beats.len() {
        return None;
    }
    let quarters: f64 = beats.iter().zip(&types).map(|(b, t)| b * 4.0 / t).sum();
    let den = types.iter().copied().fold(0.0, f64::max);
    let num = (quarters * den / 4.0).round();
    fitted(num, den)
}

/// A signature (numerator 1–64, denominator a power of two up to 64).
fn fitted(num: f64, den: f64) -> Option<(u8, u8)> {
    let (n, d) = (num as i64, den as i64);
    ((1..=64).contains(&n) && (1..=64).contains(&d) && (d as u64).is_power_of_two())
        .then_some((n as u8, d as u8))
}

fn bar_length((num, den): (u8, u8)) -> f64 {
    f64::from(num) * 4.0 / f64::from(den)
}

/// A signature counting a bar of `length` quarters (the written one's
/// beat first).
fn signature_for(length: f64, written: (u8, u8)) -> Option<(u8, u8)> {
    for den in [written.1, 4, 8, 16, 2, 32] {
        let num = length * f64::from(den) / 4.0;
        if (num - num.round()).abs() < 1e-6
            && let Some(s) = fitted(num.round(), f64::from(den))
        {
            return Some(s);
        }
    }
    None
}

/// The key of `<key>`: its fifths and mode.
fn key_of(n: Node) -> Option<Key> {
    let fifths: i32 = text_of(n, "fifths")?.parse().ok()?;
    let mode = text_of(n, "mode").unwrap_or("major");
    let major = (fifths * 7).rem_euclid(12);
    let (offset, scale) = match mode {
        "minor" | "aeolian" => (9, Scale::Minor),
        "dorian" => (2, Scale::Dorian),
        "phrygian" => (4, Scale::Phrygian),
        "lydian" => (5, Scale::Lydian),
        "mixolydian" => (7, Scale::Mixolydian),
        "locrian" => (11, Scale::Locrian),
        _ => (0, Scale::Major),
    };
    Some(Key::new(((major + offset) % 12) as u8, scale))
}

/// A pitch class from a step and its alteration.
fn pitch_class(step: Option<&str>, alter: Option<&str>) -> Option<u8> {
    let s = step_semitone(step?)?;
    let a = alter
        .and_then(|a| a.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
        .round() as i32;
    Some((s + a).rem_euclid(12) as u8)
}

/// A chord symbol (`None`: N.C.; `Err`: not read).
fn harmony_of(n: Node) -> std::result::Result<Option<Chord>, ()> {
    let root = child(n, "root").ok_or(())?;
    let root = pitch_class(text_of(root, "root-step"), text_of(root, "root-alter")).ok_or(())?;
    let kind_node = child(n, "kind");
    let kind = kind_node.and_then(|k| k.text()).map_or("major", str::trim);
    let degrees: Vec<(i32, i32, String)> = elements(n)
        .filter(|c| c.has_tag_name("degree"))
        .filter_map(|d| {
            Some((
                text_of(d, "degree-value")?.parse().ok()?,
                text_of(d, "degree-alter")
                    .and_then(|a| a.parse::<f64>().ok())
                    .unwrap_or(0.0)
                    .round() as i32,
                text_of(d, "degree-type").unwrap_or("add").to_string(),
            ))
        })
        .collect();
    let adds = |value: i32, alter: i32| {
        degrees
            .iter()
            .any(|(v, a, t)| *v == value && *a == alter && t == "add")
    };
    let quality = match kind {
        "none" => return Ok(None),
        "major" if adds(9, 0) => Quality::Add9,
        "major" => Quality::Major,
        "minor" if adds(9, 0) => Quality::MinorAdd9,
        "minor" => Quality::Minor,
        "augmented" => Quality::Augmented,
        "diminished" => Quality::Diminished,
        "dominant" if adds(9, 0) => Quality::Dominant9,
        "dominant" => Quality::Dominant7,
        "major-seventh" => Quality::Major7,
        "minor-seventh" => Quality::Minor7,
        "diminished-seventh" => Quality::Diminished7,
        "augmented-seventh" => Quality::Augmented7,
        "half-diminished" => Quality::HalfDiminished7,
        "major-minor" => Quality::MinorMajor7,
        "major-sixth" => Quality::Major6,
        "minor-sixth" => Quality::Minor6,
        "dominant-ninth" => Quality::Dominant9,
        "major-ninth" | "major-11th" | "major-13th" => Quality::Major9,
        "minor-ninth" | "minor-11th" | "minor-13th" => Quality::Minor9,
        "dominant-11th" => Quality::Dominant11,
        "dominant-13th" => Quality::Dominant13,
        "suspended-second" => Quality::Sus2,
        "suspended-fourth" if adds(7, -1) || adds(7, 0) => Quality::Seven4,
        "suspended-fourth" => Quality::Sus4,
        "power" | "pedal" => Quality::Power,
        "Neapolitan" => Quality::Major,
        "Italian" | "French" | "German" => Quality::Dominant7,
        "Tristan" => Quality::HalfDiminished7,
        _ => {
            // "other": the symbol as written.
            let text = kind_node.and_then(|k| k.attribute("text")).unwrap_or("");
            let name = format!("{}{text}", faderframe_midi::theory::pc_name(root, false));
            return Ok(Some(
                Chord::parse(&name)
                    .map(|c| Chord::new(root, c.quality))
                    .unwrap_or(Chord::new(root, Quality::Major))
                    .with_bass(bass_of(n)),
            ));
        }
    };
    Ok(Some(Chord::new(root, quality).with_bass(bass_of(n))))
}

fn bass_of(n: Node) -> Option<u8> {
    let b = child(n, "bass")?;
    pitch_class(text_of(b, "bass-step"), text_of(b, "bass-alter"))
}

impl PartReader<'_> {
    fn note(&mut self, text: impl Into<String>) {
        self.notes.insert(text.into());
    }

    fn quarters(&self, n: Node, name: &str) -> f64 {
        number_of(n, name).unwrap_or(0.0) / self.divisions.max(1e-9)
    }

    /// One measure's events (`m` holds its number and bar lines in a
    /// partwise score; `content` its music).
    fn measure(&mut self, m: Node, content: Node) -> RawMeasure {
        let mut out = RawMeasure {
            number: m.attribute("number").unwrap_or("").to_string(),
            ..RawMeasure::default()
        };
        let mut cursor = 0.0f64;
        let mut chord_at = 0.0f64;
        let mut chord_main_is_grace = false;
        for e in elements(content) {
            match e.tag_name().name() {
                "attributes" => self.attributes(e, cursor, &mut out),
                "backup" => cursor = (cursor - self.quarters(e, "duration")).max(0.0),
                "forward" => {
                    cursor += self.quarters(e, "duration");
                    out.length = out.length.max(cursor);
                }
                "note" => {
                    let in_chord = child(e, "chord").is_some();
                    let grace = child(e, "grace").is_some();
                    if grace || (in_chord && chord_main_is_grace) {
                        chord_main_is_grace = grace || chord_main_is_grace;
                        if !in_chord {
                            self.grace += 1;
                        }
                        continue;
                    }
                    chord_main_is_grace = false;
                    let dur = self.quarters(e, "duration");
                    let at = if in_chord { chord_at } else { cursor };
                    if !in_chord {
                        chord_at = cursor;
                        cursor += dur;
                        out.length = out.length.max(cursor);
                    }
                    if child(e, "cue").is_some() {
                        self.cue += 1;
                        continue;
                    }
                    if child(e, "rest").is_some() {
                        continue;
                    }
                    if let Some(n) = self.pitched(e, at, dur) {
                        out.notes.push(n);
                    }
                }
                "direction" => self.direction(e, cursor, &mut out),
                "sound" => self.sound(e, cursor, &mut out),
                "harmony" => {
                    let offset = self.quarters(e, "offset");
                    match harmony_of(e) {
                        Ok(c) => out.harmonies.push((cursor + offset, c)),
                        Err(()) => self.note("a chord symbol without a root was left out"),
                    }
                }
                "barline" => self.barline(e, &mut out),
                _ => {}
            }
        }
        out
    }

    fn attributes(&mut self, e: Node, cursor: f64, out: &mut RawMeasure) {
        if let Some(d) = number_of(e, "divisions").filter(|d| *d > 0.0) {
            self.divisions = d;
        }
        if let Some(k) = child(e, "key").and_then(key_of) {
            out.keys.push((cursor, k));
        }
        if let Some(t) = child(e, "time") {
            match time_of(t) {
                Some(sig) => out.time = Some(sig),
                None if child(t, "senza-misura").is_some() => {
                    self.note(
                        "a bar without metre (senza misura) is counted in the signature before it",
                    );
                }
                None => self.note("a time signature that does not read was left out"),
            }
        }
        if let Some(s) = text_of(e, "staves").and_then(|s| s.parse::<u8>().ok()) {
            self.staves = self.staves.max(s);
        }
        for c in elements(e).filter(|c| c.has_tag_name("clef")) {
            match text_of(c, "sign") {
                Some("TAB") => self.tab = true,
                Some("percussion") => self.percussion = true,
                _ => {}
            }
        }
        if let Some(t) = child(e, "transpose") {
            let chromatic = number_of(t, "chromatic").unwrap_or(0.0).round() as i32;
            let octaves = number_of(t, "octave-change").unwrap_or(0.0).round() as i32;
            let double = if child(t, "double").is_some() { -12 } else { 0 };
            self.transpose = chromatic + 12 * octaves + double;
        }
    }

    /// A sounding note (`None`: a pitch that does not read or sound).
    fn pitched(&mut self, e: Node, at: f64, dur: f64) -> Option<RawNote> {
        let instrument = child(e, "instrument")
            .and_then(|i| i.attribute("id"))
            .and_then(|id| self.instruments.get(id))
            .copied()
            .unwrap_or(self.default);
        let key = if let Some(p) = child(e, "pitch") {
            let step = step_semitone(text_of(p, "step")?)?;
            let alter = number_of(p, "alter").unwrap_or(0.0);
            if alter.fract().abs() > 1e-6 {
                self.note("microtonal alterations were rounded to semitones");
            }
            let octave = number_of(p, "octave")? as i32;
            (octave + 1) * 12 + step + alter.round() as i32 + self.transpose
        } else {
            let u = child(e, "unpitched")?;
            self.percussion = true;
            match instrument.unpitched {
                Some(k) => i32::from(k),
                None => {
                    let step = step_semitone(text_of(u, "display-step")?)?;
                    let octave = number_of(u, "display-octave")? as i32;
                    (octave + 1) * 12 + step
                }
            }
        };
        if !(0..=127).contains(&key) {
            self.note("notes beyond MIDI's keys were left out");
            return None;
        }
        let mut n = RawNote {
            at,
            dur,
            key: key as u8,
            voice: text_of(e, "voice")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            staff: text_of(e, "staff")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            share: 1.0,
            channel: instrument.channel,
            velocity: e
                .attribute("dynamics")
                .and_then(|d| d.trim().parse::<f64>().ok())
                .map(|d| (d / 100.0 * 90.0).round().clamp(1.0, 127.0) as u8),
            ..RawNote::default()
        };
        // Ties sound by <tie>; files with only the drawn <tied> too.
        let mut tie = |kind: Option<&str>| match kind {
            Some("start") => n.tie_start = true,
            Some("stop") => n.tie_stop = true,
            _ => {}
        };
        let ties: Vec<Node> = elements(e).filter(|c| c.has_tag_name("tie")).collect();
        for t in &ties {
            tie(t.attribute("type"));
        }
        for notations in elements(e).filter(|c| c.has_tag_name("notations")) {
            for x in elements(notations) {
                match x.tag_name().name() {
                    "tied" if ties.is_empty() => tie(x.attribute("type")),
                    "articulations" => {
                        for a in elements(x) {
                            let name = a.tag_name().name();
                            if let Some((_, art)) = ARTICULATIONS.iter().find(|(k, _)| *k == name) {
                                n.share = n.share.min(art.length);
                                n.accent += art.velocity;
                            }
                        }
                    }
                    "dynamics" => {
                        n.marks
                            .extend(elements(x).map(|d| d.tag_name().name().to_string()));
                    }
                    _ => {}
                }
            }
        }
        for l in elements(e).filter(|c| c.has_tag_name("lyric")) {
            if l.attribute("print-object") == Some("no") && child(l, "text").is_none() {
                continue;
            }
            let verse = l
                .attribute("number")
                .map(|s| {
                    s.chars()
                        .filter(char::is_ascii_digit)
                        .collect::<String>()
                        .parse::<u32>()
                        .unwrap_or(1)
                })
                .unwrap_or(1);
            // Elided syllables (two on one note) are sung as one.
            let text: Vec<&str> = elements(l)
                .filter(|c| c.has_tag_name("text"))
                .filter_map(|c| c.text())
                .collect();
            let syllabics: Vec<Syllabic> = elements(l)
                .filter(|c| c.has_tag_name("syllabic"))
                .filter_map(|c| c.text())
                .map(Syllabic::parse)
                .collect();
            let syllabic = match (syllabics.first(), syllabics.last()) {
                (Some(a), Some(z)) => {
                    let starts_word = matches!(a, Syllabic::Single | Syllabic::Begin);
                    let ends_word = matches!(z, Syllabic::Single | Syllabic::End);
                    match (starts_word, ends_word) {
                        (true, true) => Syllabic::Single,
                        (true, false) => Syllabic::Begin,
                        (false, true) => Syllabic::End,
                        (false, false) => Syllabic::Middle,
                    }
                }
                _ => Syllabic::Single,
            };
            n.lyrics.push((
                verse,
                LyricBit {
                    text: text.join(" "),
                    syllabic,
                    extend: child(l, "extend").is_some_and(|x| x.attribute("type") != Some("stop")),
                    end_line: child(l, "end-line").is_some() || child(l, "end-paragraph").is_some(),
                },
            ));
        }
        Some(n)
    }

    fn direction(&mut self, e: Node, cursor: f64, out: &mut RawMeasure) {
        let offset = child(e, "offset")
            .filter(|o| o.attribute("sound") == Some("yes"))
            .and_then(|o| o.text()?.trim().parse::<f64>().ok())
            .map_or(0.0, |d| d / self.divisions.max(1e-9));
        let at = (cursor + offset).max(0.0);
        let mut metronome = None;
        for dt in elements(e).filter(|c| c.has_tag_name("direction-type")) {
            for x in elements(dt) {
                match x.tag_name().name() {
                    "dynamics" => {
                        for d in elements(x) {
                            out.marks
                                .push((at, Mark::Dynamic(d.tag_name().name().to_string())));
                        }
                    }
                    "wedge" => {
                        let w = match x.attribute("type") {
                            Some("crescendo") => Wedge::Crescendo,
                            Some("diminuendo") => Wedge::Diminuendo,
                            _ => Wedge::Stop,
                        };
                        out.marks.push((at, Mark::Wedge(w)));
                    }
                    "metronome" => metronome = metronome_bpm(x),
                    "segno" | "coda" => {
                        self.jumps.insert("segni and codas");
                    }
                    _ => {}
                }
            }
        }
        let had_tempo = child(e, "sound").is_some_and(|s| s.attribute("tempo").is_some());
        if let Some(s) = child(e, "sound") {
            self.sound(s, at, out);
        }
        if !had_tempo && let Some(bpm) = metronome {
            out.tempos.push((at, bpm));
        }
    }

    fn sound(&mut self, s: Node, at: f64, out: &mut RawMeasure) {
        if let Some(t) = s
            .attribute("tempo")
            .and_then(|t| t.trim().parse::<f64>().ok())
            .filter(|t| *t > 0.0)
        {
            out.tempos.push((at, t));
        }
        if let Some(d) = s
            .attribute("dynamics")
            .and_then(|d| d.trim().parse::<f64>().ok())
        {
            out.marks.push((
                at,
                Mark::Velocity((d / 100.0 * 90.0).round().clamp(1.0, 127.0) as u8),
            ));
        }
        for (attr, what) in [
            ("dacapo", "da capo"),
            ("dalsegno", "dal segno"),
            ("tocoda", "to coda"),
            ("fine", "fine"),
        ] {
            if s.attribute(attr).is_some() {
                self.jumps.insert(what);
            }
        }
    }

    fn barline(&mut self, e: Node, out: &mut RawMeasure) {
        if let Some(r) = child(e, "repeat") {
            match r.attribute("direction") {
                Some("forward") => out.forward = true,
                Some("backward") => {
                    out.backward = Some(r.attribute("times").and_then(|t| t.trim().parse().ok()));
                }
                _ => {}
            }
        }
        if let Some(end) = child(e, "ending") {
            match end.attribute("type") {
                Some("start") => {
                    let numbers: Vec<u32> = end
                        .attribute("number")
                        .unwrap_or("1")
                        .split([',', ' '])
                        .filter_map(|n| n.trim().parse().ok())
                        .collect();
                    out.ending_start = Some(if numbers.is_empty() { vec![1] } else { numbers });
                }
                Some("stop") | Some("discontinue") => out.ending_stop = true,
                _ => {}
            }
        }
    }
}

/// A metronome mark's tempo in quarters a minute.
fn metronome_bpm(m: Node) -> Option<f64> {
    let per_minute: f64 = text_of(m, "per-minute")?
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| !s.is_empty())?
        .parse()
        .ok()?;
    let unit = match text_of(m, "beat-unit")? {
        "long" => 16.0,
        "breve" => 8.0,
        "whole" => 4.0,
        "half" => 2.0,
        "quarter" => 1.0,
        "eighth" => 0.5,
        "16th" => 0.25,
        "32nd" => 0.125,
        _ => return None,
    };
    let dots = elements(m)
        .take_while(|c| !c.has_tag_name("per-minute"))
        .filter(|c| c.has_tag_name("beat-unit-dot"))
        .count();
    let mut length = unit;
    let mut add = unit;
    for _ in 0..dots {
        add /= 2.0;
        length += add;
    }
    Some(per_minute * length)
}

/// The order bars are played in: (measure index, pass), repeats and
/// endings followed.
fn play_order(measures: &[RawMeasure]) -> Vec<(usize, u32)> {
    let n = measures.len();
    // The ending each bar is in (its numbers), and where endings stop.
    let mut in_ending: Vec<Option<Vec<u32>>> = vec![None; n];
    let mut current: Option<Vec<u32>> = None;
    for (i, m) in measures.iter().enumerate() {
        if let Some(nums) = &m.ending_start {
            current = Some(nums.clone());
        }
        in_ending[i] = current.clone();
        if m.ending_stop {
            current = None;
        }
    }
    // Passes a backward repeat makes: its `times`, else enough for the
    // endings around it.
    let times_of = |i: usize| -> u32 {
        if let Some(Some(t)) = measures[i].backward {
            return t.max(1);
        }
        let mut most = 2;
        let mut j = i;
        while j < n && in_ending[j].is_some() {
            most = most.max(
                in_ending[j]
                    .as_ref()
                    .map_or(0, |v| v.iter().copied().max().unwrap_or(0)),
            );
            j += 1;
        }
        most
    };
    let mut order = Vec::new();
    let mut i = 0;
    let mut start = 0;
    let mut pass = 1u32;
    let mut taken: HashMap<usize, u32> = HashMap::new();
    let mut jumped = false;
    let limit = n * 32 + 64;
    while i < n && order.len() < limit {
        let m = &measures[i];
        if m.forward && !jumped {
            start = i;
            pass = 1;
        }
        jumped = false;
        // An ending not for this pass: past it.
        if let Some(nums) = &m.ending_start
            && !nums.contains(&pass)
        {
            let mut j = i;
            while j < n && !measures[j].ending_stop {
                j += 1;
            }
            i = j + 1;
            continue;
        }
        order.push((i, pass));
        if m.backward.is_some() {
            let done = taken.entry(i).or_insert(0);
            if *done + 1 < times_of(i) {
                *done += 1;
                pass += 1;
                i = start;
                jumped = true;
                continue;
            }
            pass = 1;
            start = i + 1;
        }
        i += 1;
    }
    order
}

/// The velocity of a mark, and the level after it (accents return).
fn mark_level(mark: &str) -> Option<(u8, Option<u8>)> {
    let v = DYNAMICS.iter().find(|(k, _)| *k == mark)?.1;
    let after = match mark {
        "fp" | "sfp" | "sfzp" => Some(49),
        "sfpp" => Some(33),
        m if ACCENT_MARKS.contains(&m) => None,
        _ => Some(v),
    };
    Some((v, after))
}

/// A part's velocities over time: levels set by marks, accents on their
/// notes, ramps along wedges.
struct Levels {
    /// (time, level from then on)
    steps: Vec<(f64, u8)>,
    /// (time, velocity of notes starting then)
    accents: Vec<(f64, u8)>,
    /// (from, to, level at from, level at to)
    ramps: Vec<(f64, f64, f64, f64)>,
}

impl Levels {
    fn new(mut marks: Vec<(f64, Mark)>) -> Self {
        marks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut steps = vec![(f64::NEG_INFINITY, DEFAULT_VELOCITY)];
        let mut accents = Vec::new();
        let mut wedges: Vec<(f64, Wedge)> = Vec::new();
        for (t, m) in &marks {
            match m {
                Mark::Dynamic(name) => {
                    if let Some((v, after)) = mark_level(name) {
                        match after {
                            Some(a) if a == v => steps.push((*t, v)),
                            Some(a) => {
                                accents.push((*t, v));
                                steps.push((*t, a));
                            }
                            None => accents.push((*t, v)),
                        }
                    }
                }
                Mark::Velocity(v) => steps.push((*t, *v)),
                Mark::Wedge(w) => wedges.push((*t, *w)),
            }
        }
        let level_at = |steps: &[(f64, u8)], t: f64| {
            steps
                .iter()
                .rev()
                .find(|(s, _)| *s <= t + EPS)
                .map_or(DEFAULT_VELOCITY, |(_, v)| *v)
        };
        let mut ramps = Vec::new();
        let mut i = 0;
        while i < wedges.len() {
            let (t0, w) = wedges[i];
            if w == Wedge::Stop {
                i += 1;
                continue;
            }
            let t1 = wedges[i + 1..]
                .iter()
                .find(|(_, x)| *x == Wedge::Stop)
                .map_or(t0 + 4.0, |(t, _)| *t);
            let from = f64::from(level_at(&steps, t0));
            let up = w == Wedge::Crescendo;
            // To the next mark that goes the wedge's way, else a step.
            let next = steps
                .iter()
                .find(|(s, v)| {
                    *s > t0 + EPS && (f64::from(*v) > from) == up && f64::from(*v) != from
                })
                .map(|(_, v)| f64::from(*v));
            let to = next
                .unwrap_or(if up { from + 16.0 } else { from - 16.0 })
                .clamp(1.0, 127.0);
            if t1 > t0 {
                ramps.push((t0, t1, from, to));
                if next.is_none() {
                    steps.push((t1, to.round() as u8));
                    steps.sort_by(|a, b| a.0.total_cmp(&b.0));
                }
            }
            i += 1;
        }
        Self {
            steps,
            accents,
            ramps,
        }
    }

    fn velocity(&self, t: f64) -> u8 {
        if let Some(&(_, v)) = self.accents.iter().find(|(a, _)| (a - t).abs() < EPS) {
            return v;
        }
        if let Some(&(a, b, from, to)) = self.ramps.iter().find(|(a, b, ..)| t >= a - EPS && t < *b)
        {
            return (from + (to - from) * (t - a) / (b - a))
                .round()
                .clamp(1.0, 127.0) as u8;
        }
        self.steps
            .iter()
            .rev()
            .find(|(s, _)| *s <= t + EPS)
            .map_or(DEFAULT_VELOCITY, |(_, v)| *v)
    }
}

/// A note laid out, before ties are joined.
struct Placed {
    raw: RawNote,
    start: f64,
    velocity: u8,
    lyric: Option<LyricBit>,
}

/// Join tied notes; returns the notes and the syllables on them.
fn join_ties(mut placed: Vec<Placed>) -> (Vec<Note>, Vec<Syllable>) {
    placed.sort_by(|a, b| a.start.total_cmp(&b.start));
    // (note, written end, open for a tie)
    let mut out: Vec<(Note, f64, bool)> = Vec::new();
    let mut words: Vec<(usize, LyricBit)> = Vec::new();
    for p in placed {
        let written_end = p.start + p.raw.dur;
        if p.raw.tie_stop
            && let Some((n, end, open)) = out
                .iter_mut()
                .rev()
                .find(|(n, end, open)| *open && n.key == p.raw.key && (end - p.start).abs() < 1e-4)
        {
            n.length = (p.start - n.start) + p.raw.dur * p.raw.share;
            *end = written_end;
            *open = p.raw.tie_start;
            continue;
        }
        if let Some(l) = p.lyric {
            words.push((out.len(), l));
        }
        out.push((
            Note {
                start: p.start,
                length: p.raw.dur * p.raw.share,
                key: p.raw.key,
                velocity: p.velocity,
                channel: p.raw.channel,
                voice: p.raw.voice,
                staff: p.raw.staff,
            },
            written_end,
            p.raw.tie_start,
        ));
    }
    let notes: Vec<Note> = out
        .into_iter()
        .map(|(n, _, _)| n)
        .filter(|n| n.length > EPS)
        .collect();
    // Words: each to the end of its note, a held one (extend) over the
    // notes up to the next word.
    let mut syllables: Vec<Syllable> = Vec::with_capacity(words.len());
    let at: Vec<f64> = words
        .iter()
        .map(|(i, _)| notes.get(*i).map_or(0.0, |n| n.start))
        .collect();
    for (k, (i, l)) in words.iter().enumerate() {
        let Some(n) = notes.get(*i) else { continue };
        let mut end = n.start + n.length;
        if l.extend {
            let next = at.get(k + 1).copied().unwrap_or(f64::INFINITY);
            for m in notes
                .iter()
                .filter(|m| m.start > n.start && m.start < next - EPS)
            {
                end = end.max(m.start + m.length);
            }
        }
        syllables.push(Syllable {
            at: n.start,
            end,
            text: l.text.clone(),
            syllabic: l.syllabic,
            extend: l.extend,
            end_line: l.end_line,
        });
    }
    (notes, syllables)
}

/// Read a MusicXML document (partwise or timewise).
pub fn read_xml(text: &str) -> Result<Score> {
    let doc = parse(text)?;
    let root = doc.root_element();
    let timewise = match root.tag_name().name() {
        "score-partwise" => false,
        "score-timewise" => true,
        other => {
            return Err(InterchangeError::Format(format!(
                "<{other}> is not a MusicXML score"
            )));
        }
    };
    let mut score = Score {
        title: child(root, "work")
            .and_then(|w| text_of(w, "work-title"))
            .or_else(|| text_of(root, "movement-title"))
            .unwrap_or("")
            .to_string(),
        composer: child(root, "identification")
            .and_then(|i| {
                elements(i)
                    .find(|c| c.has_tag_name("creator") && c.attribute("type") == Some("composer"))
            })
            .and_then(|c| c.text())
            .unwrap_or("")
            .trim()
            .to_string(),
        ..Score::default()
    };
    // The parts listed and their instruments.
    struct Listed {
        id: String,
        name: String,
        abbreviation: String,
        instruments: HashMap<String, Instrument>,
        default: Instrument,
    }
    let mut listed: Vec<Listed> = Vec::new();
    if let Some(list) = child(root, "part-list") {
        for sp in elements(list).filter(|c| c.has_tag_name("score-part")) {
            let mut instruments = HashMap::new();
            let mut default = None;
            for mi in elements(sp).filter(|c| c.has_tag_name("midi-instrument")) {
                let one_based = |name| {
                    number_of(mi, name)
                        .filter(|v| (1.0..=128.0).contains(v))
                        .map(|v| v as u8 - 1)
                };
                let ins = Instrument {
                    channel: one_based("midi-channel").filter(|c| *c < 16),
                    program: one_based("midi-program"),
                    unpitched: one_based("midi-unpitched"),
                };
                default.get_or_insert(ins);
                if let Some(id) = mi.attribute("id") {
                    instruments.insert(id.to_string(), ins);
                }
            }
            listed.push(Listed {
                id: sp.attribute("id").unwrap_or("").to_string(),
                name: text_of(sp, "part-name").unwrap_or("").to_string(),
                abbreviation: text_of(sp, "part-abbreviation").unwrap_or("").to_string(),
                instruments,
                default: default.unwrap_or_default(),
            });
        }
    }
    // Each part's measures: (measure element, its content).
    let mut by_part: Vec<(String, Vec<(Node, Node)>)> = Vec::new();
    if timewise {
        for m in elements(root).filter(|c| c.has_tag_name("measure")) {
            for p in elements(m).filter(|c| c.has_tag_name("part")) {
                let id = p.attribute("id").unwrap_or("").to_string();
                match by_part.iter_mut().find(|(i, _)| *i == id) {
                    Some((_, v)) => v.push((m, p)),
                    None => by_part.push((id, vec![(m, p)])),
                }
            }
        }
    } else {
        for p in elements(root).filter(|c| c.has_tag_name("part")) {
            let id = p.attribute("id").unwrap_or("").to_string();
            let ms: Vec<(Node, Node)> = elements(p)
                .filter(|c| c.has_tag_name("measure"))
                .map(|m| (m, m))
                .collect();
            by_part.push((id, ms));
        }
    }
    if by_part.is_empty() {
        return Err(InterchangeError::Format("the score has no parts".into()));
    }
    let empty = HashMap::new();
    let mut notes: BTreeSet<String> = BTreeSet::new();
    let mut grace = 0;
    let mut cue = 0;
    let mut jumps: BTreeSet<&'static str> = BTreeSet::new();
    struct Read {
        part: Part,
        measures: Vec<RawMeasure>,
    }
    let mut read: Vec<Read> = Vec::new();
    for (id, measures) in &by_part {
        let info = listed.iter().find(|l| l.id == *id);
        let mut r = PartReader {
            divisions: 1.0,
            transpose: 0,
            instruments: info.map_or(&empty, |l| &l.instruments),
            default: info.map(|l| l.default).unwrap_or_default(),
            staves: 1,
            tab: false,
            percussion: false,
            grace: 0,
            cue: 0,
            jumps: BTreeSet::new(),
            notes: BTreeSet::new(),
        };
        let raw: Vec<RawMeasure> = measures.iter().map(|(m, c)| r.measure(*m, *c)).collect();
        grace += r.grace;
        cue += r.cue;
        jumps.extend(r.jumps.iter());
        notes.extend(r.notes.iter().cloned());
        let default = r.default;
        read.push(Read {
            part: Part {
                id: id.clone(),
                name: info.map_or_else(String::new, |l| l.name.clone()),
                abbreviation: info.map_or_else(String::new, |l| l.abbreviation.clone()),
                channel: default.channel,
                program: default.program,
                percussion: r.percussion || default.channel == Some(9),
                staves: r.staves,
                tab: r.tab,
                ..Part::default()
            },
            measures: raw,
        });
    }
    if grace > 0 {
        notes.insert(format!("{grace} grace note(s) were left out"));
    }
    if cue > 0 {
        notes.insert(format!("{cue} cue note(s) were left out"));
    }
    if !jumps.is_empty() {
        notes.insert(format!(
            "{} are not followed (the bars play as written, repeats aside)",
            jumps.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    // Bars: the longest part's length each, the first part's signatures
    // and repeats.
    let count = read.iter().map(|r| r.measures.len()).max().unwrap_or(0);
    let lead = &read[0].measures;
    let mut written = Vec::with_capacity(count);
    let mut sig = (4u8, 4u8);
    for i in 0..count {
        if let Some(t) = read
            .iter()
            .find_map(|r| r.measures.get(i).and_then(|m| m.time))
        {
            sig = t;
        }
        written.push(sig);
    }
    let lengths: Vec<f64> = (0..count)
        .map(|i| {
            let l = read
                .iter()
                .filter_map(|r| r.measures.get(i))
                .map(|m| m.length)
                .fold(0.0, f64::max);
            if l > EPS { l } else { bar_length(written[i]) }
        })
        .collect();
    let mut template: Vec<RawMeasure> = (0..count)
        .map(|i| {
            let mut m = lead.get(i).cloned().unwrap_or_default();
            // Repeats and endings of any part (some files mark them once).
            for r in &read {
                if let Some(x) = r.measures.get(i) {
                    m.forward |= x.forward;
                    m.ending_stop |= x.ending_stop;
                    if m.backward.is_none() {
                        m.backward = x.backward;
                    }
                    if m.ending_start.is_none() {
                        m.ending_start = x.ending_start.clone();
                    }
                }
            }
            m
        })
        .collect();
    for m in &mut template {
        m.notes.clear();
    }
    let order = play_order(&template);
    // Lay the bars out: a short first bar is a pickup, late in a whole
    // bar; other bars get a signature that fits them.
    let mut starts = Vec::with_capacity(order.len());
    let mut t = 0.0;
    for (k, &(i, _)) in order.iter().enumerate() {
        let len = lengths[i];
        let bar = bar_length(written[i]);
        if k == 0 && len + EPS < bar {
            t = bar - len;
        }
        let last = k + 1 == order.len();
        let time = if (len - bar).abs() < EPS || k == 0 || (last && len < bar) {
            written[i]
        } else {
            match signature_for(len, written[i]) {
                Some(s) => s,
                None => {
                    notes.insert(format!(
                        "bar {} is {len:.3} quarters long: no time signature counts it",
                        template[i].number
                    ));
                    written[i]
                }
            }
        };
        starts.push(t);
        score.measures.push(Measure {
            number: template[i].number.clone(),
            start: t,
            length: len,
            time,
            written: written[i],
        });
        t += len;
    }
    // Tempo and key changes (any part's tempo, the first part's keys).
    let mut tempos: Vec<TempoAt> = Vec::new();
    for (k, &(i, _)) in order.iter().enumerate() {
        for r in &read {
            if let Some(m) = r.measures.get(i) {
                tempos.extend(m.tempos.iter().map(|&(at, bpm)| TempoAt {
                    at: starts[k] + at,
                    bpm,
                }));
            }
        }
        if let Some(m) = read[0].measures.get(i) {
            for &(at, key) in &m.keys {
                if score.keys.last().is_none_or(|l| l.key != key) {
                    score.keys.push(KeyAt {
                        at: starts[k] + at,
                        key,
                    });
                }
            }
        }
    }
    tempos.sort_by(|a, b| a.at.total_cmp(&b.at));
    tempos.dedup_by(|b, a| (a.at - b.at).abs() < EPS);
    tempos.dedup_by(|b, a| (a.bpm - b.bpm).abs() < 1e-9);
    score.tempos = tempos;
    // Each part's notes, words and chords as played.
    for r in read {
        let mut part = r.part;
        let mut marks: Vec<(f64, Mark)> = Vec::new();
        let mut placed_raw: Vec<(f64, RawNote, u32)> = Vec::new();
        for (k, &(i, pass)) in order.iter().enumerate() {
            let Some(m) = r.measures.get(i) else { continue };
            let s = starts[k];
            marks.extend(m.marks.iter().map(|(at, x)| (s + at, x.clone())));
            for n in &m.notes {
                placed_raw.push((s + n.at, n.clone(), pass));
            }
            for &(at, chord) in &m.harmonies {
                part.chords.push(ChordAt { at: s + at, chord });
            }
        }
        // Marks on notes count from the note's place.
        for (t, n, _) in &placed_raw {
            for mark in &n.marks {
                marks.push((*t, Mark::Dynamic(mark.clone())));
            }
        }
        let levels = Levels::new(marks);
        let placed: Vec<Placed> = placed_raw
            .into_iter()
            .map(|(start, raw, pass)| {
                let base = raw.velocity.unwrap_or_else(|| levels.velocity(start));
                let velocity = (i32::from(base) + raw.accent).clamp(1, 127) as u8;
                // The pass's verse, else the note's first.
                let lyric = raw
                    .lyrics
                    .iter()
                    .find(|(v, _)| *v == pass)
                    .or_else(|| raw.lyrics.iter().min_by_key(|(v, _)| *v))
                    .map(|(_, l)| l.clone());
                Placed {
                    raw,
                    start,
                    velocity,
                    lyric,
                }
            })
            .collect();
        let (notes_out, syllables) = join_ties(placed);
        part.notes = notes_out;
        part.lyrics = syllables;
        part.chords.sort_by(|a, b| a.at.total_cmp(&b.at));
        score.parts.push(part);
    }
    score.notes = notes.into_iter().collect();
    Ok(score)
}

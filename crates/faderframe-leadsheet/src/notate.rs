//! The score editor's reading of MIDI notes: every note of the staves'
//! clips written out, as the lead sheet's builder writes a line
//! ([`crate::build`]) but keeping what an editor needs — each event knows
//! the notes it was written from ([`Event::sources`]), notes sounding
//! against each other go to two voices, and the editor's choices per note
//! (voice, spelling, syllable, articulations, dynamic) are honoured.
//!
//! 1. Each beat's grid is chosen by cost from its onsets, as the lead
//!    sheet's ([`Display::grid`] per stretch of the staff); starts and ends
//!    land on it, no note shorter than its beat's grid. A staccato note
//!    (played half its value) is read at its written value.
//! 2. Voices: notes starting together and ending together are one chord;
//!    a chord sounding while another starts (more than a little: legato
//!    playing overlaps) goes to the second voice. Where the two voices
//!    sound against each other the higher line is the first (stems up), the
//!    lower the second (stems down); a note given a voice keeps it. A third
//!    line is cut short where the next note of its voice starts.
//! 3. In each voice, notes starting together are one chord; a played
//!    note's short gap to the next is closed when [`Display::legato`]
//!    (only notes whose end was off the grid: a rest written on the grid
//!    stays).
//! 4. Bars, values, ties across bar lines, beams, triplets and accidentals
//!    by key as the lead sheet's; the second voice only in the bars where
//!    it has notes (its gaps there rests).

use crate::build::{
    Bar, ChordAt, Grid, Grid2, accidentals, beam, beat_of, grid_for, grids, key_signature,
    lay_chords, rest, spell, split, tolerance, triplets,
};
use crate::score::{
    Clef, DIVISIONS, Dynamic, Event, KeySig, LeadSheet, Lyric, Marks, Measure, Part, Pitch,
    Syllabic,
};
use faderframe_midi::theory::{Key, Scale};

/// How much of its written value a staccato note is played.
pub const STACCATO_LENGTH: f64 = 0.5;

/// A note to write: its id (what an editor selects), where it is (quarters
/// on the bars' clock) and how the score shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub id: u64,
    pub start: f64,
    pub end: f64,
    pub key: u8,
    /// The voice it is written in (1 or 2; `None`: by the notes around
    /// it).
    pub voice: Option<u8>,
    /// The letter it is spelt on (0 = C … 6 = B; `None`, or one more than
    /// a double sharp or flat away: as the key spells it).
    pub step: Option<u8>,
    /// The syllable sung on it: a trailing `-` means the word goes on on
    /// the next note, a trailing `_` that it is held over the notes after
    /// it.
    pub lyric: Option<String>,
    pub marks: Marks,
    pub dynamic: Option<Dynamic>,
}

impl Written {
    /// A plain note.
    pub fn new(id: u64, start: f64, end: f64, key: u8) -> Self {
        Self {
            id,
            start,
            end,
            key,
            voice: None,
            step: None,
            lyric: None,
            marks: Marks::NONE,
            dynamic: None,
        }
    }
}

/// How a stretch of a staff is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    /// The rhythms' grid (as the lead sheet's: sixteenths or triplets by
    /// cost, or one of them).
    pub grid: Grid,
    /// A played note's short gap to the next is closed (a legato line
    /// reads as one); rests written on the grid stay either way.
    pub legato: bool,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            grid: Grid::Auto,
            legato: true,
        }
    }
}

/// A staff to write.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScoreStaff {
    pub name: String,
    pub notes: Vec<Written>,
    /// Its clef (`None`: by its notes).
    pub clef: Option<Clef>,
    /// The upper staff of a grand staff: the next staff is its lower one.
    pub brace: bool,
    /// How stretches of it are shown (start, end in quarters); a beat
    /// takes the stretch it starts in (none: [`Display::default`]).
    pub display: Vec<(f64, f64, Display)>,
}

#[derive(Clone, Debug, Default)]
pub struct ScoreInput {
    pub title: String,
    pub composer: String,
    /// `None`: found from the notes and the chords.
    pub key: Option<Key>,
    pub bars: Vec<Bar>,
    pub tempo: Option<f64>,
    pub staves: Vec<ScoreStaff>,
    /// Written over the top staff.
    pub chords: Vec<ChordAt>,
}

/// Where a note was written: its id, start and end on the grid (quarters
/// on the bars' clock; what quantising to the score gives it), its part
/// and voice (1 or 2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WrittenAt {
    pub id: u64,
    pub start: f64,
    pub end: f64,
    pub part: usize,
    pub voice: u8,
}

/// The score and where each note went.
#[derive(Clone, Debug, PartialEq)]
pub struct Notated {
    pub sheet: LeadSheet,
    pub written: Vec<WrittenAt>,
}

/// A note (or chord) on the grid (divisions from the first bar).
#[derive(Clone, Debug)]
struct Item {
    s: u32,
    e: u32,
    /// Its notes, lowest first.
    notes: Vec<Written>,
    /// Its end was off the grid (played): a short gap after it may close.
    loose: bool,
    voice: u8,
    fixed: bool,
}

impl Item {
    fn top(&self) -> u8 {
        self.notes.last().map_or(0, |n| n.key)
    }

    fn mean(&self) -> f64 {
        self.notes.iter().map(|n| f64::from(n.key)).sum::<f64>() / self.notes.len().max(1) as f64
    }
}

/// Write the score.
pub fn notate(input: &ScoreInput) -> Notated {
    let grid = Grid2::new(&input.bars);
    let tol = tolerance(input.tempo);
    let empty = [ScoreStaff::default()];
    let staves: &[ScoreStaff] = if input.staves.is_empty() {
        &empty
    } else {
        &input.staves
    };
    let placed: Vec<(Vec<Vec<bool>>, Vec<Item>)> =
        staves.iter().map(|s| place(&grid, s, tol)).collect();
    // The key, from every staff's notes and the chords.
    let key = input.key.unwrap_or_else(|| {
        let mut h = [0.0f64; 12];
        for item in placed.iter().flat_map(|(_, p)| p) {
            for n in &item.notes {
                h[usize::from(n.key % 12)] += f64::from(item.e - item.s);
            }
        }
        let end = input.bars.last().map_or(0.0, |b| {
            b.start + f64::from(crate::score::bar_length(b.time)) / f64::from(DIVISIONS)
        });
        for (i, c) in input.chords.iter().enumerate() {
            let until = input.chords.get(i + 1).map_or(end, |n| n.start);
            let span = (until - c.start).max(0.0) * f64::from(DIVISIONS);
            for pc in c.chord.pitch_classes() {
                h[usize::from(pc % 12)] += span;
            }
            h[usize::from(c.chord.root % 12)] += span;
        }
        faderframe_midi::theory::detect_key(&h).unwrap_or(Key::new(0, Scale::Major))
    });
    let sig = key_signature(key);
    let origin = input.bars.first().map_or(0.0, |b| b.start);
    let mut parts = Vec::new();
    let mut written = Vec::new();
    for (pi, (staff, (swung, items))) in staves.iter().zip(&placed).enumerate() {
        let mut part = write_staff(&grid, swung, items, sig, staff.clef);
        part.name.clone_from(&staff.name);
        part.brace = staff.brace;
        parts.push(part);
        for item in items {
            for n in &item.notes {
                written.push(WrittenAt {
                    id: n.id,
                    start: origin + f64::from(item.s) / f64::from(DIVISIONS),
                    end: origin + f64::from(item.e) / f64::from(DIVISIONS),
                    part: pi,
                    voice: item.voice,
                });
            }
        }
    }
    if let Some(top) = parts.first_mut() {
        lay_chords(&mut top.measures, &grid, &input.chords, sig);
    }
    Notated {
        sheet: LeadSheet {
            title: input.title.clone(),
            composer: input.composer.clone(),
            key: sig,
            tempo: input.tempo,
            parts,
        },
        written,
    }
}

/// Where a note's written value ends: a staccato note is played short of
/// it.
fn written_end(n: &Written) -> f64 {
    if n.marks.staccato {
        n.start + (n.end - n.start) / STACCATO_LENGTH
    } else {
        n.end
    }
}

/// The display of the beat starting at `q` (quarters).
fn display_at(staff: &ScoreStaff, q: f64) -> Display {
    staff
        .display
        .iter()
        .find(|(a, b, _)| q >= *a - 1e-9 && q < *b - 1e-9)
        .map_or_else(Display::default, |(_, _, d)| *d)
}

/// Steps 1 to 3 for a staff: each bar's beats (straight or triplets) and
/// its notes on the grid, in voices.
fn place(grid: &Grid2, staff: &ScoreStaff, tol: f64) -> (Vec<Vec<bool>>, Vec<Item>) {
    let mut notes: Vec<Written> = staff
        .notes
        .iter()
        .filter(|n| n.end > n.start)
        .cloned()
        .collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    // 1. Each beat's grid, by the onsets in it.
    let mut offsets: Vec<Vec<Vec<f64>>> = grid
        .bars
        .iter()
        .map(|(b, _, len)| {
            let (beat, _) = beat_of(b.time);
            vec![Vec::new(); (*len).div_ceil(beat) as usize]
        })
        .collect();
    if grid.bars.is_empty() {
        return (Vec::new(), Vec::new());
    }
    for n in &notes {
        let (b, p) = grid.place(n.start);
        let (beat, _) = beat_of(grid.bars[b].0.time);
        let i = ((p / f64::from(beat)) as usize).min(offsets[b].len().saturating_sub(1));
        offsets[b][i].push(p - f64::from(i as u32 * beat));
        // An end written on a grid (entered, or quantised before) counts
        // as an onset: the rest after it is written as it is. A played
        // end, off every grid, does not.
        let (b, p) = grid.place(written_end(n));
        let (beat, _) = beat_of(grid.bars[b].0.time);
        let on = |u: f64| (p - (p / u).round() * u).abs() < 0.05;
        let into = p - (p / f64::from(beat)).floor() * f64::from(beat);
        if (on(3.0) || on(4.0)) && into > 0.05 && f64::from(beat) - into > 0.05 {
            let i = ((p / f64::from(beat)) as usize).min(offsets[b].len().saturating_sub(1));
            offsets[b][i].push(p - f64::from(i as u32 * beat));
        }
    }
    let units: Vec<Vec<u32>> = offsets
        .iter()
        .zip(&grid.bars)
        .map(|(beats, (bar, _, _))| {
            let (beat, triplets) = beat_of(bar.time);
            beats
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let at = bar.start + f64::from(i as u32 * beat) / f64::from(DIVISIONS);
                    let how = display_at(staff, at).grid;
                    grid_for(o, grids(beat, triplets, how), tol)
                })
                .collect()
        })
        .collect();
    let swung: Vec<Vec<bool>> = units
        .iter()
        .map(|beats| beats.iter().map(|u| *u == 4).collect())
        .collect();
    let unit_at = |q: f64| -> (u32, u32, f64) {
        let (b, p) = grid.place(q);
        let (bar, off, len) = grid.bars[b];
        let (beat, _) = beat_of(bar.time);
        let i = ((p / f64::from(beat)) as usize).min(units[b].len().saturating_sub(1));
        let unit = units[b][i];
        let start = f64::from(i as u32 * beat);
        let snapped = start + ((p - start) / f64::from(unit)).round() * f64::from(unit);
        let at = off + (snapped.max(0.0) as u32).min(len);
        (unit, at, f64::from(off) + p)
    };
    let total = grid.total();
    let mut items: Vec<Item> = Vec::new();
    for n in notes {
        let (unit, s, _) = unit_at(n.start);
        let (_, e, raw) = unit_at(written_end(&n));
        let e = e.max(s + unit).min(total.max(s + 1));
        let loose = (raw - f64::from(e)).abs() > 0.05;
        let fixed = n.voice.is_some();
        let voice = n.voice.unwrap_or(1).clamp(1, 2);
        // Notes starting and ending together (in one voice) are a chord.
        match items
            .iter_mut()
            .rev()
            .take_while(|i| i.s == s)
            .find(|i| i.e == e && i.fixed == fixed && (!fixed || i.voice == voice))
        {
            Some(chord) => {
                chord.notes.push(n);
                chord.notes.sort_by_key(|n| n.key);
                chord.loose |= loose;
            }
            None => items.push(Item {
                s,
                e,
                notes: vec![n],
                loose,
                voice,
                fixed,
            }),
        }
    }
    items.retain(|i| i.s < total);
    // 2. Voices: by start, the higher first.
    items.sort_by(|a, b| a.s.cmp(&b.s).then(b.top().cmp(&a.top())));
    let slack = |i: &Item| (DIVISIONS / 2).min((i.e - i.s) / 2);
    // Where each voice is busy until (and what it is), for the automatic.
    let mut busy: [Option<usize>; 2] = [None, None];
    for k in 0..items.len() {
        let s = items[k].s;
        let free = |v: usize, items: &[Item]| {
            busy[v].is_none_or(|j| items[j].e <= s || items[j].e - s <= slack(&items[j]))
        };
        if !items[k].fixed {
            items[k].voice = if free(0, &items) {
                1
            } else if free(1, &items) {
                2
            } else {
                // A third line: the voice whose note ends first.
                let end = |v: usize| busy[v].map_or(0, |j| items[j].e);
                if end(0) <= end(1) { 1 } else { 2 }
            };
        }
        let v = usize::from(items[k].voice - 1);
        if busy[v].is_none_or(|j| items[k].e >= items[j].e) {
            busy[v] = Some(k);
        }
    }
    // Where the voices sound against each other, the higher line is the
    // first: a run of overlapping notes whose second voice sits higher is
    // turned over (unless a note of it was given its voice).
    let overlaps = |a: &Item, b: &Item| {
        a.s < b.e && b.s < a.e && a.e.min(b.e) - a.s.max(b.s) > slack(a).min(slack(b))
    };
    let mut group: Vec<usize> = (0..items.len()).collect();
    fn root(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    for i in 0..items.len() {
        for j in i + 1..items.len() {
            if items[j].s >= items[i].e {
                break;
            }
            if items[i].voice != items[j].voice && overlaps(&items[i], &items[j]) {
                let (a, b) = (root(&mut group, i), root(&mut group, j));
                group[a] = b;
            }
        }
    }
    let mut runs: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..items.len() {
        let r = root(&mut group, i);
        runs.entry(r).or_default().push(i);
    }
    for members in runs.values() {
        if members.len() < 2 || members.iter().any(|i| items[*i].fixed) {
            continue;
        }
        let mean = |v: u8| {
            let (sum, n) = members
                .iter()
                .filter(|i| items[**i].voice == v)
                .fold((0.0, 0usize), |(s, n), i| (s + items[*i].mean(), n + 1));
            (n > 0).then(|| sum / n as f64)
        };
        if let (Some(one), Some(two)) = (mean(1), mean(2))
            && two > one
        {
            for i in members {
                items[*i].voice = 3 - items[*i].voice;
            }
        }
    }
    // 3. Each voice: chords merged, overlaps cut, played gaps closed.
    let legato_at = |s: u32| {
        let q = grid
            .bars
            .iter()
            .rev()
            .find(|(_, off, _)| *off <= s)
            .map_or(0.0, |(bar, off, _)| {
                bar.start + f64::from(s - off) / f64::from(DIVISIONS)
            });
        display_at(staff, q).legato
    };
    let mut out: Vec<Item> = Vec::new();
    for voice in [1u8, 2] {
        let mut line: Vec<Item> = items.iter().filter(|i| i.voice == voice).cloned().collect();
        line.sort_by_key(|i| i.s);
        let mut merged: Vec<Item> = Vec::new();
        for item in line {
            match merged.last_mut() {
                Some(last) if last.s == item.s => {
                    last.e = last.e.max(item.e);
                    last.loose |= item.loose;
                    last.fixed |= item.fixed;
                    last.notes.extend(item.notes);
                    last.notes.sort_by_key(|n| n.key);
                    last.notes.dedup_by_key(|n| n.key);
                }
                _ => merged.push(item),
            }
        }
        for i in 0..merged.len() {
            let next = merged.get(i + 1).map_or(total, |n| n.s);
            let (s, e) = (merged[i].s, merged[i].e.min(next));
            let gap = next.saturating_sub(e);
            let close = i + 1 < merged.len()
                && merged[i].loose
                && legato_at(s)
                && gap > 0
                && gap < DIVISIONS / 2
                && gap < e - s;
            merged[i].e = if close { next } else { e }.max(s + 1).min(total);
        }
        out.extend(merged);
    }
    out.sort_by(|a, b| a.s.cmp(&b.s).then(a.voice.cmp(&b.voice)));
    (swung, out)
}

/// The letter's natural pitch class (0 = C … 6 = B).
const NATURAL: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];

/// `midi` spelt on `step` when it is at most a double sharp or flat from
/// it, else as `key` spells it.
pub fn spell_on(midi: u8, step: Option<u8>, key: KeySig) -> (u8, i8, i8) {
    if let Some(step) = step.filter(|s| *s < 7) {
        let natural = i32::from(NATURAL[usize::from(step)]);
        let alter = (i32::from(midi % 12) - natural + 6).rem_euclid(12) - 6;
        if alter.abs() <= 2 {
            let octave = (i32::from(midi) - alter - natural).div_euclid(12) - 1;
            return (step, alter as i8, octave as i8);
        }
    }
    spell(midi, key)
}

/// The other spellings of `midi` (letters a double sharp or flat away at
/// most), the key's own first.
pub fn spellings(midi: u8, key: KeySig) -> Vec<(u8, i8)> {
    let (first, alter, _) = spell(midi, key);
    let mut out = vec![(first, alter)];
    for d in [1i32, -1, 2, -2] {
        let step = (i32::from(first) + d).rem_euclid(7) as u8;
        let (s, a, _) = spell_on(midi, Some(step), key);
        if s == step && !out.contains(&(s, a)) {
            out.push((s, a));
        }
    }
    out
}

/// What a note's text says: its syllable, whether the word goes on, and
/// whether it is held.
fn syllable(text: &str) -> (String, bool, bool) {
    let t = text.trim();
    if let Some(t) = t.strip_suffix('-') {
        (t.to_string(), true, false)
    } else if let Some(t) = t.strip_suffix('_') {
        (t.to_string(), false, true)
    } else {
        (t.to_string(), false, false)
    }
}

/// Step 4 for one staff: its clef and bars of both voices.
fn write_staff(
    grid: &Grid2,
    swung: &[Vec<bool>],
    items: &[Item],
    sig: KeySig,
    clef: Option<Clef>,
) -> Part {
    let mut keys: Vec<u8> = items
        .iter()
        .flat_map(|i| i.notes.iter().map(|n| n.key))
        .collect();
    keys.sort_unstable();
    let median = keys.get(keys.len() / 2).copied().unwrap_or(67);
    let clef = clef.unwrap_or(if median >= 59 {
        Clef::Treble
    } else if median >= 52 {
        Clef::Treble8vb
    } else {
        Clef::Bass
    });
    let pitch = |n: &Written| {
        let (step, alter, octave) = spell_on(n.key, n.step, sig);
        Pitch {
            step,
            alter,
            octave,
            accidental: None,
            midi: n.key,
        }
    };
    let mut measures: Vec<Measure> = Vec::new();
    let mut last_time = None;
    for (b, (bar, off, len)) in grid.bars.iter().enumerate() {
        let end = off + len;
        let voice = |v: u8| -> Vec<Event> {
            let mut events: Vec<Event> = Vec::new();
            let mut cursor = *off;
            let push_rest = |events: &mut Vec<Event>, from: u32, to: u32| {
                for d in split(from - off, to - from, bar.time, &swung[b]) {
                    let at = events.last().map_or(from - off, |e: &Event| e.end());
                    events.push(rest(at.max(from - off), d));
                }
            };
            for item in items.iter().filter(|i| i.voice == v) {
                let (s, e) = (item.s, item.e);
                if e <= *off || s >= end {
                    continue;
                }
                let (a, z) = (s.max(*off).max(cursor), e.min(end));
                if z <= a {
                    continue;
                }
                if a > cursor {
                    push_rest(&mut events, cursor, a);
                }
                let pieces = split(a - off, z - a, bar.time, &swung[b]);
                let n = pieces.len();
                let mut at = a - off;
                let first_piece = s >= *off;
                for (j, d) in pieces.into_iter().enumerate() {
                    let opening = j == 0 && first_piece;
                    let lead = &item.notes[0];
                    let lyric = item
                        .notes
                        .iter()
                        .rev()
                        .find_map(|n| n.lyric.as_deref())
                        .filter(|_| opening && v == 1)
                        .map(|text| {
                            let (text, more, held) = syllable(text);
                            (text, more, held)
                        });
                    events.push(Event {
                        at,
                        duration: d,
                        pitch: Some(pitch(lead)),
                        tie: j + 1 < n || e > end,
                        tied_from: j > 0 || s < *off,
                        bar_rest: false,
                        beams: [None; 2],
                        tuplet: None,
                        lyric: lyric.map(|(text, more, held)| Lyric {
                            text,
                            extend: held,
                            // Joined to the next by its own hyphen; what
                            // came before is set below.
                            syllabic: if more {
                                Syllabic::Begin
                            } else {
                                Syllabic::Single
                            },
                            end_line: false,
                        }),
                        held: false,
                        chord: item.notes[1..].iter().map(pitch).collect(),
                        frets: Vec::new(),
                        sources: item.notes.iter().map(|n| n.id).collect(),
                        marks: if opening {
                            item.notes
                                .iter()
                                .fold(Marks::NONE, |m, n| merge_marks(m, n.marks))
                        } else {
                            Marks::NONE
                        },
                        dynamic: item
                            .notes
                            .iter()
                            .find_map(|n| n.dynamic)
                            .filter(|_| opening),
                    });
                    at += d.divisions();
                }
                cursor = z;
            }
            if cursor < end && !events.is_empty() {
                push_rest(&mut events, cursor, end);
            }
            events
        };
        let mut events = voice(1);
        let under = voice(2);
        if events.is_empty() {
            events.push(Event {
                bar_rest: true,
                ..rest(0, crate::score::Duration::plain(crate::score::Value::Whole))
            });
        }
        let time = bar.time;
        measures.push(Measure {
            time,
            show_time: last_time != Some(time),
            events,
            chords: Vec::new(),
            under,
        });
        last_time = Some(time);
    }
    // A bar of the second voice with only its rests is no second voice;
    // one beginning with a note held from the bar before keeps its rests.
    for m in &mut measures {
        if m.under.iter().all(Event::is_rest) {
            m.under.clear();
        }
    }
    for m in &mut measures {
        accidentals(m, sig);
        beam(m);
        triplets(m);
    }
    words(&mut measures);
    Part {
        name: String::new(),
        clef,
        measures,
        tab: None,
        brace: false,
    }
}

/// Marks of a chord: any note's.
fn merge_marks(a: Marks, b: Marks) -> Marks {
    Marks {
        staccato: a.staccato || b.staccato,
        accent: a.accent || b.accent,
        tenuto: a.tenuto || b.tenuto,
        marcato: a.marcato || b.marcato,
    }
}

/// The words' syllables joined (a syllable after one whose word goes on
/// is its middle or end) and held syllables' lines drawn under the notes
/// after them until the next word.
fn words(measures: &mut [Measure]) {
    let mut continues = false;
    let mut holding = false;
    for m in measures.iter_mut() {
        for e in &mut m.events {
            if e.is_rest() {
                holding = false;
                continue;
            }
            match &mut e.lyric {
                Some(l) => {
                    let more = l.syllabic == Syllabic::Begin;
                    l.syllabic = match (continues, more) {
                        (false, false) => Syllabic::Single,
                        (false, true) => Syllabic::Begin,
                        (true, true) => Syllabic::Middle,
                        (true, false) => Syllabic::End,
                    };
                    continues = more;
                    holding = l.extend;
                }
                None if holding && !e.tied_from => e.held = true,
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;

//! Tablature: where on a fretted instrument each note is played.
//!
//! A note has a place on every string whose open note (with the capo) is at
//! or below it and close enough for the frets; a chord takes one string a
//! note. Of all the ways to play a passage the one chosen costs least
//! (a Viterbi search over each chord's candidate shapes): a shape costs its
//! stretch beyond four frets, its height up the neck and strings skipped
//! inside a chord, less for open strings; moving between shapes costs the
//! distance the hand travels. Notes the instrument cannot reach are moved
//! by octaves until it can; more notes at once than strings keep the
//! highest and the lowest.

use crate::score::Fret;

/// An instrument's strings: their open notes (MIDI), string 1 (the top line
/// of the tablature, usually the highest) first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    pub name: &'static str,
    pub strings: &'static [u8],
    /// Frets on the neck.
    pub frets: u8,
    /// Written an octave above it sounds (guitars on a treble clef with an
    /// 8 below, basses on the bass clef).
    pub bass: bool,
}

/// The tunings offered (the first is the default).
pub const TUNINGS: [Tuning; 12] = [
    guitar("Guitar · Standard", &[64, 59, 55, 50, 45, 40]),
    guitar("Guitar · Drop D", &[64, 59, 55, 50, 45, 38]),
    guitar("Guitar · Half Step Down", &[63, 58, 54, 49, 44, 39]),
    guitar("Guitar · Whole Step Down", &[62, 57, 53, 48, 43, 38]),
    guitar("Guitar · DADGAD", &[62, 57, 55, 50, 45, 38]),
    guitar("Guitar · Open G", &[62, 59, 55, 50, 43, 38]),
    guitar("Guitar · Open D", &[62, 57, 54, 50, 45, 38]),
    guitar("Guitar · Open E", &[64, 59, 56, 52, 47, 40]),
    guitar("Guitar · 7 Strings", &[64, 59, 55, 50, 45, 40, 35]),
    Tuning {
        name: "Bass · 4 Strings",
        strings: &[43, 38, 33, 28],
        frets: 22,
        bass: true,
    },
    Tuning {
        name: "Bass · 5 Strings",
        strings: &[43, 38, 33, 28, 23],
        frets: 22,
        bass: true,
    },
    Tuning {
        name: "Ukulele · GCEA",
        strings: &[69, 64, 60, 67],
        frets: 15,
        bass: false,
    },
];

const fn guitar(name: &'static str, strings: &'static [u8]) -> Tuning {
    Tuning {
        name,
        strings,
        frets: 22,
        bass: false,
    }
}

impl Tuning {
    /// The tuning for a part called `name` when `chosen` is the guitar
    /// tuning asked for: a bass part plays on a bass (unless a bass tuning
    /// was chosen).
    pub fn for_part(chosen: Tuning, name: &str) -> Tuning {
        let lower = name.to_lowercase();
        if !chosen.bass && lower.contains("bass") && !lower.contains("drum") {
            TUNINGS[9]
        } else {
            chosen
        }
    }

    /// The string names, string 1 first ("E", "B", "G", …).
    pub fn names(&self) -> Vec<&'static str> {
        const PC: [&str; 12] = [
            "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
        ];
        self.strings
            .iter()
            .map(|s| PC[usize::from(s % 12)])
            .collect()
    }
}

/// A note as played: its (possibly moved) pitch and its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Played {
    pub key: u8,
    pub fret: Fret,
}

/// One way to play a chord.
#[derive(Clone, Debug)]
struct Shape {
    notes: Vec<Played>,
    cost: f32,
    /// Where the hand is: the lowest fretted fret (None: open strings only).
    hand: Option<f32>,
}

/// Fret positions above the capo where a stretch starts to cost.
const EASY_SPAN: u8 = 3;
/// The most frets a shape may spread over.
const MOST_SPAN: u8 = 5;
/// Shapes kept per chord.
const KEPT: usize = 48;

/// The pitches moved by octaves into what the strings reach, duplicates
/// gone, at most a note a string (the highest and lowest kept), ascending.
fn playable(keys: &[u8], open: &[u8], top: u8) -> Vec<u8> {
    let lowest = open.iter().copied().min().unwrap_or(40);
    let highest = open.iter().copied().max().unwrap_or(64).saturating_add(top);
    let mut out: Vec<u8> = keys
        .iter()
        .map(|&k| {
            let mut k = i32::from(k);
            while k < i32::from(lowest) {
                k += 12;
            }
            while k > i32::from(highest) {
                k -= 12;
            }
            k.clamp(0, 127) as u8
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    while out.len() > open.len() && out.len() > 2 {
        // Drop from the middle: the outer notes say most.
        out.remove(out.len() / 2);
    }
    out
}

/// Every way to play `keys` (ascending) on strings tuned `open` (capo
/// included) with `top` frets above the capo, cheapest first, at most
/// [`KEPT`].
fn shapes(keys: &[u8], open: &[u8], top: u8) -> Vec<Shape> {
    let mut out = Vec::new();
    let mut taken = vec![false; open.len()];
    let mut notes: Vec<Played> = Vec::with_capacity(keys.len());
    fn walk(
        keys: &[u8],
        open: &[u8],
        top: u8,
        span_limit: u8,
        taken: &mut [bool],
        notes: &mut Vec<Played>,
        out: &mut Vec<Shape>,
    ) {
        if out.len() >= 4096 {
            return;
        }
        let Some((&k, rest)) = keys.split_last() else {
            out.push(score(notes, open));
            return;
        };
        for (s, &o) in open.iter().enumerate() {
            if taken[s] || k < o || k - o > top {
                continue;
            }
            let fret = k - o;
            let fretted = notes.iter().map(|n| n.fret.fret).filter(|f| *f > 0);
            if fret > 0 {
                let (lo, hi) = fretted.fold((fret, fret), |(a, b), f| (a.min(f), b.max(f)));
                if hi - lo > span_limit {
                    continue;
                }
            }
            taken[s] = true;
            notes.push(Played {
                key: k,
                fret: Fret {
                    string: s as u8 + 1,
                    fret,
                },
            });
            walk(rest, open, top, span_limit, taken, notes, out);
            notes.pop();
            taken[s] = false;
        }
    }
    walk(keys, open, top, MOST_SPAN, &mut taken, &mut notes, &mut out);
    if out.is_empty() {
        // Nothing within a hand's reach: any shape (it will cost).
        walk(keys, open, top, u8::MAX, &mut taken, &mut notes, &mut out);
    }
    out.sort_by(|a, b| a.cost.total_cmp(&b.cost));
    out.truncate(KEPT);
    out
}

/// A shape's own cost (what playing it asks of the hand).
fn score(notes: &[Played], open: &[u8]) -> Shape {
    let fretted: Vec<u8> = notes
        .iter()
        .map(|n| n.fret.fret)
        .filter(|f| *f > 0)
        .collect();
    let (lo, hi) = fretted
        .iter()
        .fold((u8::MAX, 0u8), |(a, b), f| (a.min(*f), b.max(*f)));
    let mut cost = 0.0f32;
    if !fretted.is_empty() {
        let span = hi - lo;
        if span > EASY_SPAN {
            cost += f32::from(span - EASY_SPAN) * 1.5;
        }
        // Low on the neck is easier to read and to reach.
        cost += f32::from(lo) * 0.06;
    }
    let opens = notes.iter().filter(|n| n.fret.fret == 0).count() as f32;
    cost -= 0.15 * opens;
    // A chord's strings next to each other (none skipped in between).
    if notes.len() >= 2 {
        let mut strings: Vec<u8> = notes.iter().map(|n| n.fret.string).collect();
        strings.sort_unstable();
        let gaps = strings
            .windows(2)
            .map(|w| u32::from(w[1] - w[0] - 1))
            .sum::<u32>();
        cost += gaps as f32 * 0.6;
        // Higher notes on higher strings (crossed voicings are awkward).
        let crossed = notes.iter().any(|a| {
            notes.iter().any(|b| {
                a.key > b.key
                    && open[usize::from(a.fret.string - 1)] < open[usize::from(b.fret.string - 1)]
            })
        });
        if crossed {
            cost += 1.0;
        }
    }
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| n.key);
    Shape {
        notes: sorted,
        cost,
        hand: (!fretted.is_empty()).then_some(f32::from(lo)),
    }
}

/// The cost of going from one shape to the next.
fn travel(a: &Shape, b: &Shape) -> f32 {
    let mut cost = match (a.hand, b.hand) {
        (Some(x), Some(y)) => {
            let d = (x - y).abs();
            // Within a position the fingers reach; beyond it the hand moves.
            if d <= 2.0 { d * 0.25 } else { 0.5 + d * 0.45 }
        }
        _ => 0.0,
    };
    // The same note again on another string sounds and reads oddly.
    for n in &b.notes {
        if a.notes
            .iter()
            .any(|m| m.key == n.key && m.fret.string != n.fret.string)
        {
            cost += 0.4;
        }
    }
    cost
}

/// Where to play each chord (each a set of MIDI keys sounding together, in
/// time order) on `tuning` with a capo at `capo`: each chord's notes
/// ascending, as played (moved by octaves where the strings cannot reach
/// them). Frets count from the capo.
pub fn finger(chords: &[Vec<u8>], tuning: &Tuning, capo: u8) -> Vec<Vec<Played>> {
    let open: Vec<u8> = tuning
        .strings
        .iter()
        .map(|s| s.saturating_add(capo))
        .collect();
    let top = tuning.frets.saturating_sub(capo);
    let options: Vec<Vec<Shape>> = chords
        .iter()
        .map(|keys| shapes(&playable(keys, &open, top), &open, top))
        .collect();
    // Viterbi: the cheapest way to each shape of each chord (every chord
    // has a shape: `playable` leaves only what the strings reach, and no
    // notes at all is the empty shape).
    let mut best: Vec<Vec<(f32, usize)>> = Vec::with_capacity(options.len());
    for (i, shapes) in options.iter().enumerate() {
        let row = shapes
            .iter()
            .map(|s| match i.checked_sub(1) {
                None => (s.cost, 0),
                Some(prev) => options[prev]
                    .iter()
                    .enumerate()
                    .map(|(j, p)| (best[prev][j].0 + travel(p, s) + s.cost, j))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .unwrap_or((s.cost, 0)),
            })
            .collect();
        best.push(row);
    }
    // Back from the cheapest last shape.
    let mut out = vec![Vec::new(); options.len()];
    let mut pick = best.last().and_then(|r| {
        r.iter()
            .enumerate()
            .min_by(|a, b| a.1.0.total_cmp(&b.1.0))
            .map(|(k, _)| k)
    });
    for i in (0..options.len()).rev() {
        let Some(k) = pick.filter(|k| *k < options[i].len()) else {
            break;
        };
        out[i].clone_from(&options[i][k].notes);
        pick = Some(best[i][k].1);
    }
    out
}

/// The tablature as plain text, as tabs are shared: a row a string (named
/// at the left, string 1 on top), bars between `|`, each note's room by
/// its value, the words under the strings; four bars a row.
pub fn text(sheet: &crate::score::LeadSheet) -> Option<String> {
    use crate::score::DIVISIONS;
    let parts: Vec<(usize, &crate::score::Part)> = sheet
        .parts
        .iter()
        .enumerate()
        .filter(|(_, p)| p.tab.is_some())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut out = format!("{}\n", sheet.title);
    for (i, part) in parts {
        let tab = part.tab.as_ref()?;
        // The words: this part's, or (under notes) those of the notes above.
        let words_from = if part.has_lyrics() || i == 0 {
            part
        } else {
            &sheet.parts[i - 1]
        };
        const PC: [&str; 12] = [
            "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
        ];
        let names: Vec<String> = tab
            .strings
            .iter()
            .enumerate()
            .map(|(k, s)| {
                let n = PC[usize::from(s % 12)];
                // The top string's E lower-case, as tabs write it.
                if k == 0 && n == "E" {
                    "e".into()
                } else {
                    n.to_string()
                }
            })
            .collect();
        let width = names.iter().map(|n| n.len()).max().unwrap_or(1);
        let name = if part.name.is_empty() {
            &words_from.name
        } else {
            &part.name
        };
        out.push('\n');
        if !name.is_empty() {
            out.push_str(name);
            out.push('\n');
        }
        if tab.capo > 0 {
            out.push_str(&format!("Capo {}\n", tab.capo));
        }
        for (row, bars) in part.measures.chunks(4).enumerate() {
            let mut lines: Vec<String> = names.iter().map(|n| format!("{n:<width$}|")).collect();
            let mut words = " ".repeat(width + 1);
            for (b, m) in bars.iter().enumerate() {
                let mi = row * 4 + b;
                let sung = &words_from.measures.get(mi).map_or(&m.events, |w| &w.events);
                for (ei, e) in m.events.iter().enumerate() {
                    let cells: Vec<String> = (1..=names.len() as u8)
                        .map(|s| {
                            e.frets
                                .iter()
                                .find(|f| f.string == s)
                                .map(|f| {
                                    if e.tied_from {
                                        format!("({})", f.fret)
                                    } else {
                                        f.fret.to_string()
                                    }
                                })
                                .unwrap_or_default()
                        })
                        .collect();
                    let word = sung
                        .get(ei)
                        .filter(|w| w.at == e.at)
                        .and_then(|w| w.lyric.as_ref())
                        .map(|l| {
                            if l.syllabic.continues() {
                                format!("{}-", l.text)
                            } else {
                                l.text.clone()
                            }
                        })
                        .unwrap_or_default();
                    let room = if e.bar_rest {
                        8
                    } else {
                        (e.duration.divisions() / (DIVISIONS / 4)).clamp(1, 16) as usize
                    };
                    let col = cells
                        .iter()
                        .map(String::len)
                        .max()
                        .unwrap_or(0)
                        .max(word.chars().count())
                        + room;
                    for (line, cell) in lines.iter_mut().zip(&cells) {
                        line.push_str(cell);
                        line.push_str(&"-".repeat(col - cell.len()));
                    }
                    words.push_str(&word);
                    words.push_str(&" ".repeat(col - word.chars().count()));
                }
                for line in &mut lines {
                    line.push('|');
                }
                words.push(' ');
            }
            out.push('\n');
            for line in lines {
                out.push_str(&line);
                out.push('\n');
            }
            if !words.trim().is_empty() {
                out.push_str(words.trim_end());
                out.push('\n');
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standard() -> Tuning {
        TUNINGS[0]
    }

    fn frets(p: &[Played]) -> Vec<(u8, u8)> {
        let mut v: Vec<(u8, u8)> = p.iter().map(|n| (n.fret.string, n.fret.fret)).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn open_strings_and_the_open_c_chord() {
        // Low E: the sixth string open.
        let e = finger(&[vec![40]], &standard(), 0);
        assert_eq!(frets(&e[0]), [(6, 0)]);
        // C major, C3 E3 G3 C4 E4: the open position chord.
        let c = finger(&[vec![48, 52, 55, 60, 64]], &standard(), 0);
        assert_eq!(frets(&c[0]), [(1, 0), (2, 1), (3, 0), (4, 2), (5, 3)]);
        // G major with the low G: 3-2-0-0-0-3.
        let g = finger(&[vec![43, 47, 50, 55, 59, 67]], &standard(), 0);
        assert_eq!(
            frets(&g[0]),
            [(1, 3), (2, 0), (3, 0), (4, 0), (5, 2), (6, 3)]
        );
    }

    #[test]
    fn a_run_stays_in_one_position() {
        // A minor pentatonic from A2 up two octaves.
        let run: Vec<Vec<u8>> = [45, 48, 50, 52, 55, 57, 60, 62, 64, 67, 69]
            .iter()
            .map(|k| vec![*k])
            .collect();
        let played = finger(&run, &standard(), 0);
        let fretted: Vec<u8> = played
            .iter()
            .map(|p| p[0].fret.fret)
            .filter(|f| *f > 0)
            .collect();
        let (lo, hi) = (
            *fretted.iter().min().unwrap(),
            *fretted.iter().max().unwrap(),
        );
        assert!(hi - lo <= 4, "one hand position: {played:?}");
        // Every note where it sounds.
        for (keys, p) in run.iter().zip(&played) {
            let s = usize::from(p[0].fret.string - 1);
            assert_eq!(standard().strings[s] + p[0].fret.fret, keys[0]);
        }
    }

    #[test]
    fn a_capo_moves_the_open_shapes() {
        // The open C shape with a capo at 2 sounds D.
        let d = finger(&[vec![50, 54, 57, 62, 66]], &standard(), 2);
        assert_eq!(frets(&d[0]), [(1, 0), (2, 1), (3, 0), (4, 2), (5, 3)]);
    }

    #[test]
    fn notes_out_of_reach_move_by_octaves_and_crowds_thin_out() {
        // C1 is below a guitar: played an octave or two higher.
        let low = finger(&[vec![24]], &standard(), 0);
        assert_eq!(low[0].len(), 1);
        assert_eq!(low[0][0].key % 12, 0);
        assert!(low[0][0].key >= 40);
        // Eight notes at once on six strings: six, the outer ones kept.
        let many = finger(&[vec![40, 45, 50, 55, 59, 64, 67, 69]], &standard(), 0);
        assert_eq!(many[0].len(), 6);
        assert_eq!(many[0][0].key, 40);
        assert_eq!(many[0][5].key, 69);
        let strings: std::collections::BTreeSet<u8> =
            many[0].iter().map(|n| n.fret.string).collect();
        assert_eq!(strings.len(), 6, "a string a note");
    }

    #[test]
    fn basses_and_their_parts() {
        let bass = TUNINGS[9];
        let line = finger(&[vec![28], vec![33], vec![35]], &bass, 0);
        assert_eq!(frets(&line[0]), [(4, 0)]);
        assert_eq!(frets(&line[1]), [(3, 0)]);
        assert_eq!(Tuning::for_part(standard(), "Bass").name, bass.name);
        assert_eq!(
            Tuning::for_part(standard(), "Lead Guitar").name,
            standard().name
        );
        assert_eq!(standard().names(), ["E", "B", "G", "D", "A", "E"]);
    }
}

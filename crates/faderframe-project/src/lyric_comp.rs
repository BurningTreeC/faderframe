//! Comping by lyric: the words sung in each take of a take folder, lined
//! up across the takes, so the comp can be chosen word by word.
//!
//! The words heard in a source ([`SourceWords`]: kept by source in
//! `Project::source_words`, edited whole by `Command::SetSourceWords`) are
//! in source frames at the project rate, with the spans of the source
//! heard so far — passes of a loop recording share one source, and each
//! part is heard once. A folder's takes map them into folder frames
//! ([`take_words`]); [`align`] lines up the same line sung several times:
//! a sequence alignment on the words' text (Needleman–Wunsch) finds the
//! words every take agrees on, and between two of those, where the text
//! differs (a word misheard, sung differently or left out), the words are
//! paired by their onsets — a dynamic time warping of the onsets measured
//! from the last word agreed on, pairs only where both agree it is the
//! nearest and within [`PAIR_REACH`] seconds.
//!
//! Comp edits by word stay ordinary comp edits (`TakeFolder::set_comp`
//! through `Command::SetClipContent`, so the folder's invariants hold).
//! [`plan_comp`] says where a word's (or a phrase's) comp boundaries may
//! go — the gaps around it, where neither take's neighbouring word is
//! heard — and which take plays on the other side; the session picks the
//! quiet point inside each gap and the zero crossing next to it.

use crate::TakeFolder;
use faderframe_core::AudioSourceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Range;

/// How far apart (seconds) two takes' words may start, measured from the
/// last word they agree on, and still be the same word.
pub const PAIR_REACH: f64 = 0.5;
/// How far (seconds) before or after a word a comp boundary may go.
pub const EDGE_REACH: f64 = 0.5;

/// A word heard in a source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SungWord {
    /// Source frames (at [`SourceWords::rate`]) it is heard over.
    pub start: i64,
    pub end: i64,
    pub text: String,
    /// It starts a line (a stretch the model heard as one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub line: bool,
    /// How sure the model was (percent).
    #[serde(default)]
    pub confidence: u8,
}

/// The words heard in a source so far.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceWords {
    /// The rate its frames count at (the project's when heard).
    pub rate: u32,
    /// The source spans heard (sorted, apart, each `start < end`).
    pub heard: Vec<(i64, i64)>,
    /// By start.
    pub words: Vec<SungWord>,
    /// The language heard ("en", "de", …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl SourceWords {
    /// Empty, at `rate`.
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            ..Self::default()
        }
    }

    /// The parts of `a..b` not heard yet.
    pub fn missing(&self, a: i64, b: i64) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        let mut at = a;
        for &(s, e) in &self.heard {
            if e <= at {
                continue;
            }
            if s >= b {
                break;
            }
            if s > at {
                out.push((at, s.min(b)));
            }
            at = at.max(e);
            if at >= b {
                break;
            }
        }
        if at < b {
            out.push((at, b));
        }
        out
    }

    /// The words heard in `span`, in place of those starting inside it.
    pub fn add(&mut self, span: (i64, i64), words: Vec<SungWord>) {
        let (a, b) = span;
        if b <= a {
            return;
        }
        self.words.retain(|w| w.start < a || w.start >= b);
        self.words.extend(words);
        self.words
            .sort_by(|x, y| x.start.cmp(&y.start).then(x.end.cmp(&y.end)));
        self.heard.push((a, b));
        self.heard.sort();
        let mut merged: Vec<(i64, i64)> = Vec::with_capacity(self.heard.len());
        for &(s, e) in &self.heard {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.heard = merged;
    }
}

/// A take's word in its folder.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderWord {
    /// Folder frames (at the project rate).
    pub start: i64,
    pub end: i64,
    /// As heard (trimmed).
    pub text: String,
    pub line: bool,
    pub confidence: u8,
    /// Which of its source's words it is.
    pub index: usize,
}

/// Every take's words in the folder (frames at `rate`, the project's):
/// those whose middle lies in the part the take has material for, cut to
/// it.
pub fn take_words(
    folder: &TakeFolder,
    words: &BTreeMap<AudioSourceId, SourceWords>,
    rate: u32,
) -> Vec<Vec<FolderWord>> {
    folder
        .takes
        .iter()
        .map(|t| {
            let Some(sw) = words.get(&t.audio.source) else {
                return Vec::new();
            };
            let k = if sw.rate == 0 || sw.rate == rate {
                1.0
            } else {
                f64::from(rate) / f64::from(sw.rate)
            };
            let to_folder = |f: i64| t.folder_of((f as f64 * k).round() as i64);
            sw.words
                .iter()
                .enumerate()
                .filter_map(|(index, w)| {
                    let (a, b) = (to_folder(w.start), to_folder(w.end));
                    let mid = (a + b) / 2;
                    let (a, b) = (a.min(b), a.max(b));
                    (mid >= t.start && mid < t.end()).then(|| FolderWord {
                        start: a.max(t.start),
                        end: b.min(t.end()).max(a.max(t.start)),
                        text: w.text.trim().to_string(),
                        line: w.line,
                        confidence: w.confidence,
                        index,
                    })
                })
                .collect()
        })
        .collect()
}

/// A word as the takes have it: per take, which of its words it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LyricColumn {
    /// As most takes heard it (one take's spelling, punctuation kept).
    pub text: String,
    /// Per take: its word here (an index into that take's words).
    pub takes: Vec<Option<usize>>,
    /// It starts a line.
    pub line: bool,
}

/// The folder's line(s), word by word across the takes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LyricAlignment {
    /// Per take, its words ([`take_words`]).
    pub words: Vec<Vec<FolderWord>>,
    /// The words in the order sung.
    pub columns: Vec<LyricColumn>,
}

/// A word's text for comparing: lower case, letters and digits only.
pub fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Line up the takes' words (`words` per take, frames at `rate`).
pub fn align(words: Vec<Vec<FolderWord>>, rate: u32) -> LyricAlignment {
    let n = words.len();
    let secs = |f: i64| f as f64 / f64::from(rate.max(1));
    // The take with the most words first (ties: the first), then the
    // others in order.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&t| (std::cmp::Reverse(words[t].len()), t));
    let mut columns: Vec<LyricColumn> = Vec::new();
    for &t in &order {
        if columns.is_empty() {
            columns = words[t]
                .iter()
                .enumerate()
                .map(|(j, _)| {
                    let mut takes = vec![None; n];
                    takes[t] = Some(j);
                    LyricColumn {
                        takes,
                        ..LyricColumn::default()
                    }
                })
                .collect();
            continue;
        }
        let reps: Vec<(String, f64)> = columns
            .iter()
            .map(|c| (majority(c, &words), secs(onset(c, &words))))
            .collect();
        let mine: Vec<(String, f64)> = words[t]
            .iter()
            .map(|w| (normalized(&w.text), secs(w.start)))
            .collect();
        let ops = pair(&reps, &mine);
        let mut next = Vec::with_capacity(ops.len());
        for (c, w) in ops {
            let mut col = match c {
                Some(c) => std::mem::take(&mut columns[c]),
                None => LyricColumn {
                    takes: vec![None; n],
                    ..LyricColumn::default()
                },
            };
            col.takes[t] = w;
            next.push(col);
        }
        columns = next;
    }
    for c in &mut columns {
        let key = majority(c, &words);
        c.text = c
            .takes
            .iter()
            .enumerate()
            .filter_map(|(t, j)| j.map(|j| &words[t][j]))
            .find(|w| normalized(&w.text) == key)
            .map(|w| w.text.clone())
            .unwrap_or_default();
        let (lines, all) = c
            .takes
            .iter()
            .enumerate()
            .filter_map(|(t, j)| j.map(|j| words[t][j].line))
            .fold((0, 0), |(l, a), x| (l + usize::from(x), a + 1));
        c.line = lines > 0 && lines * 2 >= all;
    }
    if let Some(first) = columns.first_mut() {
        first.line = true;
    }
    LyricAlignment { words, columns }
}

/// The text most of a column's words have (ties: the earliest take's).
fn majority(c: &LyricColumn, words: &[Vec<FolderWord>]) -> String {
    let texts: Vec<String> = c
        .takes
        .iter()
        .enumerate()
        .filter_map(|(t, j)| j.map(|j| normalized(&words[t][j].text)))
        .collect();
    let mut best: Option<(&String, usize)> = None;
    for s in &texts {
        let count = texts.iter().filter(|x| *x == s).count();
        if best.is_none_or(|(_, n)| count > n) {
            best = Some((s, count));
        }
    }
    best.map(|(s, _)| s.clone()).unwrap_or_default()
}

/// A column's onset: the median of its words' starts.
fn onset(c: &LyricColumn, words: &[Vec<FolderWord>]) -> i64 {
    let mut s: Vec<i64> = c
        .takes
        .iter()
        .enumerate()
        .filter_map(|(t, j)| j.map(|j| words[t][j].start))
        .collect();
    s.sort_unstable();
    s.get(s.len() / 2).copied().unwrap_or(0)
}

/// One take's words (`b`: text, onset seconds) lined up with the columns
/// (`a`): in order, each a column, a word or both.
fn pair(a: &[(String, f64)], b: &[(String, f64)]) -> Vec<(Option<usize>, Option<usize>)> {
    // The text alignment.
    let ops = needleman_wunsch(a, b);
    // Its exact matches are the anchors; between them the onsets decide.
    let anchors: Vec<(usize, usize)> = ops
        .iter()
        .filter_map(|&(i, j)| match (i, j) {
            (Some(i), Some(j)) if a[i].0 == b[j].0 && !a[i].0.is_empty() => Some((i, j)),
            _ => None,
        })
        .collect();
    let mut out = Vec::with_capacity(a.len() + b.len());
    let mut from = (0usize, 0usize);
    for k in 0..=anchors.len() {
        let to = anchors.get(k).copied().unwrap_or((a.len(), b.len()));
        // Onsets measured from the anchor before (the one after for the
        // first stretch; none: as they are).
        let base = if k > 0 {
            Some(anchors[k - 1])
        } else {
            anchors.first().copied()
        };
        let (ra, rb) = base.map_or((0.0, 0.0), |(i, j)| (a[i].1, b[j].1));
        let xs: Vec<f64> = (from.0..to.0).map(|i| a[i].1 - ra).collect();
        let ys: Vec<f64> = (from.1..to.1).map(|j| b[j].1 - rb).collect();
        for (i, j) in by_onsets(&xs, &ys) {
            out.push((i.map(|i| i + from.0), j.map(|j| j + from.1)));
        }
        if let Some(&(i, j)) = anchors.get(k) {
            out.push((Some(i), Some(j)));
            from = (i + 1, j + 1);
        }
    }
    out
}

/// Global alignment of two word sequences by their text: equal +2,
/// different −1, left out −1.
fn needleman_wunsch(
    a: &[(String, f64)],
    b: &[(String, f64)],
) -> Vec<(Option<usize>, Option<usize>)> {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    let mut score = vec![0i32; (n + 1) * w];
    for i in 0..=n {
        score[i * w] = -(i as i32);
    }
    for (j, v) in score.iter_mut().take(w).enumerate() {
        *v = -(j as i32);
    }
    for i in 1..=n {
        for j in 1..=m {
            let same = if a[i - 1].0 == b[j - 1].0 && !a[i - 1].0.is_empty() {
                2
            } else {
                -1
            };
            score[i * w + j] = (score[(i - 1) * w + j - 1] + same)
                .max(score[(i - 1) * w + j] - 1)
                .max(score[i * w + j - 1] - 1);
        }
    }
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        let here = score[i * w + j];
        if i > 0 && j > 0 {
            let same = if a[i - 1].0 == b[j - 1].0 && !a[i - 1].0.is_empty() {
                2
            } else {
                -1
            };
            if here == score[(i - 1) * w + j - 1] + same {
                out.push((Some(i - 1), Some(j - 1)));
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && here == score[(i - 1) * w + j] - 1 {
            out.push((Some(i - 1), None));
            i -= 1;
        } else {
            out.push((None, Some(j - 1)));
            j -= 1;
        }
    }
    out.reverse();
    out
}

/// Two runs of onsets (seconds) paired by a dynamic time warping of their
/// differences: a pair where both are each other's nearest along the path
/// and within [`PAIR_REACH`]; the rest alone, all in order of onset.
fn by_onsets(xs: &[f64], ys: &[f64]) -> Vec<(Option<usize>, Option<usize>)> {
    let (n, m) = (xs.len(), ys.len());
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    if n > 0 && m > 0 {
        let cost = |i: usize, j: usize| (xs[i] - ys[j]).abs();
        let w = m + 1;
        let mut acc = vec![f64::INFINITY; (n + 1) * w];
        acc[0] = 0.0;
        for i in 1..=n {
            for j in 1..=m {
                let best = acc[(i - 1) * w + j - 1]
                    .min(acc[(i - 1) * w + j])
                    .min(acc[i * w + j - 1]);
                acc[i * w + j] = cost(i - 1, j - 1) + best;
            }
        }
        let mut path = Vec::new();
        let (mut i, mut j) = (n, m);
        while i > 0 && j > 0 {
            path.push((i - 1, j - 1));
            let d = acc[(i - 1) * w + j - 1];
            let u = acc[(i - 1) * w + j];
            let l = acc[i * w + j - 1];
            if d <= u && d <= l {
                i -= 1;
                j -= 1;
            } else if u <= l {
                i -= 1;
            } else {
                j -= 1;
            }
        }
        path.reverse();
        for &(i, j) in &path {
            let c = cost(i, j);
            let row_best = path
                .iter()
                .filter(|p| p.0 == i)
                .all(|&(_, jj)| cost(i, jj) >= c);
            let col_best = path
                .iter()
                .filter(|p| p.1 == j)
                .all(|&(ii, _)| cost(ii, j) >= c);
            let free = pairs.last().is_none_or(|&(pi, pj)| i > pi && j > pj);
            if row_best && col_best && free && c <= PAIR_REACH {
                pairs.push((i, j));
            }
        }
    }
    // Merge in order: pairs where they are, the rest by onset.
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    let mut k = 0;
    while i < n || j < m {
        if let Some(&(pi, pj)) = pairs.get(k)
            && i == pi
            && j == pj
        {
            out.push((Some(i), Some(j)));
            i += 1;
            j += 1;
            k += 1;
            continue;
        }
        let next_pair = pairs.get(k).copied().unwrap_or((n, m));
        let take_x = i < next_pair.0 && (j >= next_pair.1 || xs[i] <= ys[j]);
        if take_x {
            out.push((Some(i), None));
            i += 1;
        } else {
            out.push((None, Some(j)));
            j += 1;
        }
    }
    out
}

impl LyricAlignment {
    /// Take `take`'s word in column `c`.
    pub fn word(&self, c: usize, take: usize) -> Option<&FolderWord> {
        let j = (*self.columns.get(c)?.takes.get(take)?)?;
        self.words.get(take)?.get(j)
    }

    /// Where take `take` sings column `c` (folder frames).
    pub fn span(&self, c: usize, take: usize) -> Option<(i64, i64)> {
        self.word(c, take).map(|w| (w.start, w.end))
    }

    /// The take the comp plays for column `c`: one whose word plays whole;
    /// else the one playing at the middle of a word of its own there.
    pub fn chosen(&self, c: usize, folder: &TakeFolder) -> Option<usize> {
        let col = self.columns.get(c)?;
        let whole = col.takes.iter().enumerate().find_map(|(t, j)| {
            let w = self.words.get(t)?.get((*j)?)?;
            plays_whole(folder, t, w.start, w.end).then_some(t)
        });
        whole.or_else(|| {
            col.takes.iter().enumerate().find_map(|(t, j)| {
                let w = self.words.get(t)?.get((*j)?)?;
                (folder.take_at((w.start + w.end) / 2) == Some(t)).then_some(t)
            })
        })
    }

    /// Where column `c` is shown: the chosen take's word, else the first
    /// take's that has it.
    pub fn shown_span(&self, c: usize, folder: &TakeFolder) -> Option<(i64, i64)> {
        if let Some(t) = self.chosen(c, folder) {
            return self.span(c, t);
        }
        let col = self.columns.get(c)?;
        (0..col.takes.len()).find_map(|t| self.span(c, t))
    }

    /// The line column `c` is in (columns).
    pub fn line(&self, c: usize) -> Range<usize> {
        if self.columns.is_empty() {
            return 0..0;
        }
        let c = c.min(self.columns.len() - 1);
        let start = (0..=c).rev().find(|&i| self.columns[i].line).unwrap_or(0);
        let end = (c + 1..self.columns.len())
            .find(|&i| self.columns[i].line)
            .unwrap_or(self.columns.len());
        start..end
    }

    /// Every line (column ranges).
    pub fn lines(&self) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut c = 0;
        while c < self.columns.len() {
            let l = self.line(c);
            c = l.end;
            out.push(l);
        }
        out
    }
}

/// Does the comp play `take` over all of `a..b`?
pub fn plays_whole(folder: &TakeFolder, take: usize, a: i64, b: i64) -> bool {
    folder
        .segments()
        .filter(|&(s, e, _)| s < b.max(a + 1) && e > a)
        .all(|(_, _, t)| t == Some(take))
}

/// Where a comp boundary at one edge of a word may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    /// The word's edge (folder frames).
    pub word: i64,
    /// The gap the boundary may go in (`lo <= hi`).
    pub lo: i64,
    pub hi: i64,
    /// The take heard on the other side of the boundary (`None`:
    /// silence).
    pub other: Option<usize>,
    /// A comp boundary already in the gap: used again, so no sliver of
    /// a third take is left between two words.
    pub existing: Option<i64>,
}

impl Edge {
    /// The boundary when no audio says better: an existing one, else the
    /// middle of the gap.
    pub fn plain(&self) -> i64 {
        self.existing.unwrap_or((self.lo + self.hi) / 2)
    }
}

/// A comp edit by words: `take` from `start` to `end` (`None`: the take
/// plays on that side already — no boundary is made there).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordComp {
    pub take: usize,
    pub start: Option<Edge>,
    pub end: Option<Edge>,
    /// The words' span in the take.
    pub words: (i64, i64),
}

/// Where the comp boundaries go to play `take` for `columns` (its first
/// to last word in them): each edge in the gap between the words on
/// either side — after both takes' words before and before both takes'
/// words of the comped stretch at the start, mirrored at the end — at
/// most [`EDGE_REACH`] from the word, the folder's crossfade inside it
/// where it fits. `None`: the take sings none of those words.
pub fn plan_comp(
    al: &LyricAlignment,
    folder: &TakeFolder,
    columns: Range<usize>,
    take: usize,
    rate: u32,
) -> Option<WordComp> {
    let t = folder.takes.get(take)?;
    let first = columns.clone().find(|&c| al.span(c, take).is_some())?;
    let last = columns
        .clone()
        .rev()
        .find(|&c| al.span(c, take).is_some())?;
    let (a, _) = al.span(first, take)?;
    let (_, b) = al.span(last, take)?;
    let reach = (EDGE_REACH * f64::from(rate.max(1))) as i64;
    let half_fade = folder.crossfade.max(0) / 2;
    let len = folder.length;
    // The words of `take` and of another take `o` just outside the stretch.
    let before = |o: usize| (0..first).rev().find_map(|c| al.span(c, o)).map(|s| s.1);
    let after = |o: usize| {
        (last + 1..al.columns.len())
            .find_map(|c| al.span(c, o))
            .map(|s| s.0)
    };
    let own_start = |o: usize| columns.clone().find_map(|c| al.span(c, o)).map(|s| s.0);
    let own_end = |o: usize| {
        columns
            .clone()
            .rev()
            .find_map(|c| al.span(c, o))
            .map(|s| s.1)
    };
    let boundaries: Vec<i64> = folder
        .comp
        .iter()
        .map(|s| s.start)
        .filter(|&s| s > 0)
        .collect();
    let fit = |lo: i64, hi: i64, word: i64, other: Option<usize>, toward_word: bool| {
        let (mut lo, mut hi) = (lo.clamp(0, len), hi.clamp(0, len));
        if lo > hi {
            // Words run together here: right at the word's edge.
            let (l, h) = if toward_word {
                (word - half_fade, word)
            } else {
                (word, word + half_fade)
            };
            lo = l.clamp(0, len);
            hi = h.clamp(0, len);
        } else if hi - lo > 2 * half_fade {
            lo += half_fade;
            hi -= half_fade;
        } else {
            let mid = (lo + hi) / 2;
            lo = mid;
            hi = mid;
        }
        let existing = boundaries
            .iter()
            .copied()
            .filter(|&x| x >= lo && x <= hi)
            .min_by_key(|&x| (x - word).abs());
        Edge {
            word,
            lo,
            hi,
            other,
            existing,
        }
    };
    let start = (a > 0 && folder.take_at(a - 1) != Some(take)).then(|| {
        let other = folder.take_at(a - 1);
        let mut lo = (a - reach).max(t.start);
        let mut hi = a;
        if let Some(e) = before(take) {
            lo = lo.max(e);
        }
        if let Some(o) = other {
            if let Some(e) = before(o) {
                lo = lo.max(e);
            }
            if let Some(s) = own_start(o) {
                hi = hi.min(s);
            }
        }
        fit(lo, hi, a, other, true)
    });
    let end = (b < len && folder.take_at(b) != Some(take)).then(|| {
        let other = folder.take_at(b);
        let mut lo = b;
        let mut hi = (b + reach).min(t.end());
        if let Some(s) = after(take) {
            hi = hi.min(s);
        }
        if let Some(o) = other {
            if let Some(s) = after(o) {
                hi = hi.min(s);
            }
            if let Some(e) = own_end(o) {
                lo = lo.max(e);
            }
        }
        fit(lo, hi, b, other, false)
    });
    Some(WordComp {
        take,
        start,
        end,
        words: (a, b),
    })
}

/// Apply a planned comp with its boundaries chosen (`None`: the side
/// where the take plays already).
pub fn apply_comp(folder: &mut TakeFolder, plan: &WordComp, start: Option<i64>, end: Option<i64>) {
    let (a, b) = plan.words;
    // Where no boundary is made, run on into the segment playing it.
    let from = start.unwrap_or_else(|| {
        folder
            .segments()
            .find(|&(s, e, _)| a >= s && a < e)
            .map_or(a, |(s, _, _)| s)
    });
    let to = end.unwrap_or_else(|| {
        folder
            .segments()
            .find(|&(s, e, _)| b > s && b <= e)
            .map_or(b, |(_, e, _)| e)
    });
    folder.set_comp(from.min(a), to.max(b), Some(plan.take));
}

#[cfg(test)]
mod tests;

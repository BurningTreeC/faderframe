//! Word timings, as the reference implementation finds them: the decoder
//! reads the window's text again (forced, no timestamps) and the
//! cross-attention of the checkpoint's alignment heads — which look at
//! the audio a token is heard in — gives a token × time matrix. Each head
//! is a softmax over the window's audio, standardised along the tokens
//! and median-filtered along time (7 frames); their mean, negated, is the
//! cost of a dynamic time warping through the text, and the frame where
//! the path enters a token is where it starts (20 ms frames). A word's
//! tokens run from its first token's start to the next word's.
//!
//! Punctuation joins the word next to it (opening marks the following,
//! closing marks the preceding), as the reference does.

/// Encoder frames a second (each 20 ms).
pub const FRAMES_PER_SECOND: f64 = 50.0;
/// The median filter's width (frames).
const MEDIAN_WIDTH: usize = 7;
/// Marks that join the word after them.
const PREPENDED: &str = "\"'“¿([{-";
/// Marks that join the word before them.
const APPENDED: &str = "\"'.。,，!！?？:：”)]}、";

/// A word heard: seconds from the start of the audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    /// As the model wrote it (with its leading space, if any).
    pub text: String,
    /// How sure the model was of its tokens (their mean probability).
    pub probability: f32,
}

/// The median of each run of `width` values along `x` (reflected at the
/// ends; shorter rows than half the width stay as they are).
pub fn median_filter(x: &[f32], width: usize) -> Vec<f32> {
    let pad = width / 2;
    let n = x.len();
    if n <= pad || width < 2 {
        return x.to_vec();
    }
    let at = |i: isize| -> f32 {
        let last = n as isize - 1;
        let j = if i < 0 {
            -i
        } else if i > last {
            2 * last - i
        } else {
            i
        };
        x[j.clamp(0, last) as usize]
    };
    let mut window = vec![0.0f32; width];
    (0..n)
        .map(|i| {
            for (k, w) in window.iter_mut().enumerate() {
                *w = at(i as isize + k as isize - pad as isize);
            }
            window.sort_by(f32::total_cmp);
            window[width / 2]
        })
        .collect()
}

/// The cheapest monotonic path through `cost` (`rows × cols`, row-major)
/// from its first cell to its last, moving down, right or diagonally:
/// the (row, column) pairs along it.
pub fn dtw(cost: &[f32], rows: usize, cols: usize) -> Vec<(usize, usize)> {
    if rows == 0 || cols == 0 {
        return Vec::new();
    }
    let w = cols + 1;
    let mut acc = vec![f32::INFINITY; (rows + 1) * w];
    // 0: diagonal, 1: from above, 2: from the left.
    let mut trace = vec![0u8; (rows + 1) * w];
    acc[0] = 0.0;
    for j in 1..=cols {
        for i in 1..=rows {
            let c0 = acc[(i - 1) * w + j - 1];
            let c1 = acc[(i - 1) * w + j];
            let c2 = acc[i * w + j - 1];
            let (c, t) = if c0 < c1 && c0 < c2 {
                (c0, 0)
            } else if c1 < c0 && c1 < c2 {
                (c1, 1)
            } else {
                (c2, 2)
            };
            acc[i * w + j] = cost[(i - 1) * cols + j - 1] + c;
            trace[i * w + j] = t;
        }
    }
    let (mut i, mut j) = (rows, cols);
    let mut path = Vec::with_capacity(rows + cols);
    while i > 0 && j > 0 {
        path.push((i - 1, j - 1));
        match trace[i * w + j] {
            0 => {
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
    }
    // Off the edge (never with a finite cost): straight to the origin.
    while i > 0 {
        i -= 1;
        path.push((i, 0));
    }
    while j > 0 {
        j -= 1;
        path.push((0, j));
    }
    path.dedup();
    path.reverse();
    path
}

/// Where each row of the token × time matrix starts: from the heads'
/// scores (per head `rows × frames`, before the softmax), the frame at
/// which the warping path enters each of `rows[skip..rows - 1]`. A row's
/// query predicts the next token, so with the sequence `prompt (skip
/// tokens), no-timestamps, text…, end`, entry `i` is where text token `i`
/// starts and the last entry where the last one ends.
pub fn token_starts(scores: &[Vec<f32>], rows: usize, frames: usize, skip: usize) -> Vec<usize> {
    if rows <= skip + 1 || frames == 0 {
        return Vec::new();
    }
    let mut matrix = vec![0.0f32; rows * frames];
    let mut heads = 0;
    for s in scores.iter().filter(|s| s.len() == rows * frames) {
        heads += 1;
        // The softmax over the window's audio.
        let mut w = s.clone();
        for row in w.chunks_mut(frames) {
            let top = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let mut sum = 0.0;
            for v in row.iter_mut() {
                *v = (*v - top).exp();
                sum += *v;
            }
            for v in row.iter_mut() {
                *v /= sum.max(f32::MIN_POSITIVE);
            }
        }
        // Standardised along the tokens.
        for c in 0..frames {
            let mean = (0..rows).map(|r| w[r * frames + c]).sum::<f32>() / rows as f32;
            let var = (0..rows)
                .map(|r| (w[r * frames + c] - mean).powi(2))
                .sum::<f32>()
                / rows as f32;
            let sd = var.sqrt().max(1e-10);
            for r in 0..rows {
                w[r * frames + c] = (w[r * frames + c] - mean) / sd;
            }
        }
        // Median-filtered along time, summed.
        for (r, row) in w.chunks(frames).enumerate() {
            for (c, v) in median_filter(row, MEDIAN_WIDTH).into_iter().enumerate() {
                matrix[r * frames + c] += v;
            }
        }
    }
    if heads == 0 {
        return Vec::new();
    }
    // The rows timed (without the prompt and the last token), negated.
    let n = rows - 1 - skip;
    let cost: Vec<f32> = matrix[skip * frames..(rows - 1) * frames]
        .iter()
        .map(|v| -v / heads as f32)
        .collect();
    let path = dtw(&cost, n, frames);
    let mut starts = vec![frames; n];
    let mut seen = vec![false; n];
    for (r, c) in path {
        if !seen[r] {
            seen[r] = true;
            starts[r] = c;
        }
    }
    starts
}

/// Words from the text tokens' pieces (`pieces[i]`: the bytes of token
/// `i`), their start frames (`starts`, one more than tokens: the last is
/// where the last token ends) and probabilities, `spaced`: words are
/// split at spaces (else at every character, for languages without
/// them). Times are seconds from `offset`.
pub fn words(
    pieces: &[Vec<u8>],
    starts: &[usize],
    probabilities: &[f32],
    spaced: bool,
    offset: f64,
) -> Vec<Word> {
    let n = pieces.len();
    if n == 0 || starts.len() < n + 1 {
        return Vec::new();
    }
    // Pieces into whole characters (a character may span tokens).
    let mut subwords: Vec<(String, std::ops::Range<usize>)> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut from = 0;
    for (i, p) in pieces.iter().enumerate() {
        bytes.extend_from_slice(p);
        let done = match std::str::from_utf8(&bytes) {
            Ok(_) => true,
            // Never more than a character's worth waiting.
            Err(e) => e.error_len().is_some() || bytes.len() > 8,
        };
        if done || i + 1 == n {
            subwords.push((String::from_utf8_lossy(&bytes).into_owned(), from..i + 1));
            bytes.clear();
            from = i + 1;
        }
    }
    // Into words: a space, a punctuation mark or (unspaced) any character
    // starts one.
    let mut grouped: Vec<(String, std::ops::Range<usize>)> = Vec::new();
    for (text, range) in subwords {
        let punctuation =
            !text.trim().is_empty() && text.trim().chars().all(|c| c.is_ascii_punctuation());
        let starts_word = !spaced || text.starts_with(' ') || punctuation || grouped.is_empty();
        match grouped.last_mut() {
            Some((t, r)) if !starts_word => {
                t.push_str(&text);
                r.end = range.end;
            }
            _ => grouped.push((text, range)),
        }
    }
    let secs = |f: usize| offset + f as f64 / FRAMES_PER_SECOND;
    let mut out: Vec<Word> = grouped
        .into_iter()
        .map(|(text, r)| {
            let p =
                &probabilities[r.start.min(probabilities.len())..r.end.min(probabilities.len())];
            Word {
                start: secs(starts[r.start]),
                end: secs(starts[r.end]),
                text,
                probability: if p.is_empty() {
                    0.0
                } else {
                    p.iter().sum::<f32>() / p.len() as f32
                },
            }
        })
        .collect();
    merge_punctuation(&mut out);
    out.retain(|w| !w.text.trim().is_empty());
    out
}

/// Opening marks join the word after them, closing marks the one before
/// (the reference's `merge_punctuations`).
fn merge_punctuation(words: &mut [Word]) {
    if words.len() < 2 {
        return;
    }
    // Opening marks, from the back.
    let mut j = words.len() - 1;
    for i in (0..words.len() - 1).rev() {
        let w = &words[i];
        if w.text.starts_with(' ') && is_one_of(w.text.trim(), PREPENDED) {
            let (head, tail) = words.split_at_mut(j);
            let (prev, next) = (&mut head[i], &mut tail[0]);
            next.text = format!("{}{}", prev.text, next.text);
            next.start = next.start.min(prev.start);
            prev.text.clear();
        } else {
            j = i;
        }
    }
    // Closing marks, from the front.
    let mut i = 0;
    for j in 1..words.len() {
        let (head, tail) = words.split_at_mut(j);
        let (prev, next) = (&mut head[i], &mut tail[0]);
        if !prev.text.ends_with(' ') && is_one_of(&next.text, APPENDED) {
            prev.text.push_str(&next.text);
            prev.end = prev.end.max(next.end);
            next.text.clear();
        } else {
            i = j;
        }
    }
}

/// Is `text` one of the marks in `set` (each a single character)?
fn is_one_of(text: &str, set: &str) -> bool {
    let mut chars = text.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if set.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_median_filter_reflects_at_the_ends() {
        let x = [5.0, 1.0, 9.0, 2.0, 2.0, 8.0, 0.0];
        assert_eq!(median_filter(&x, 3), [1.0, 5.0, 2.0, 2.0, 2.0, 2.0, 8.0]);
        // Too short to filter.
        assert_eq!(median_filter(&[1.0, 3.0], 7), [1.0, 3.0]);
    }

    #[test]
    fn the_warping_path_follows_the_cheap_cells() {
        // Three tokens over eight frames: cheap where each is heard.
        let rows = 3;
        let cols = 8;
        let heard = [0..2, 2..5, 5..8];
        let mut cost = vec![1.0f32; rows * cols];
        for (r, span) in heard.iter().enumerate() {
            for c in span.clone() {
                cost[r * cols + c] = 0.0;
            }
        }
        let path = dtw(&cost, rows, cols);
        assert_eq!(path.first(), Some(&(0, 0)));
        assert_eq!(path.last(), Some(&(2, 7)));
        let enters: Vec<usize> = (0..rows)
            .map(|r| path.iter().find(|p| p.0 == r).unwrap().1)
            .collect();
        assert_eq!(enters, [0, 2, 5]);
        assert!(
            path.windows(2)
                .all(|w| w[1].0 >= w[0].0 && w[1].1 >= w[0].1)
        );
    }

    /// The heads attend where each token is heard: the starts come out
    /// there (the prompt rows skipped, the last row not timed).
    #[test]
    fn token_starts_come_from_the_attention() {
        let frames = 40;
        // Rows: one prompt token, four text tokens, the end.
        let rows = 6;
        let heard = [(0, 40), (2, 10), (10, 22), (22, 30), (30, 40), (35, 40)];
        let mut s = vec![-5.0f32; rows * frames];
        for (r, &(a, b)) in heard.iter().enumerate() {
            for c in a..b {
                s[r * frames + c] = 5.0;
            }
        }
        let starts = token_starts(&[s.clone(), s], rows, frames, 1);
        assert_eq!(starts.len(), 4);
        for (got, want) in starts.iter().zip([2, 10, 22, 30]) {
            assert!(got.abs_diff(want) <= 3, "{starts:?}");
        }
    }

    #[test]
    fn tokens_make_words_and_marks_join_them() {
        let pieces: Vec<Vec<u8>> = [" And", " so", ",", " my", " fel", "low", " Americans"]
            .iter()
            .map(|p| p.as_bytes().to_vec())
            .collect();
        let starts = [0, 25, 40, 45, 60, 70, 80, 120];
        let probs = [0.9; 7];
        let w = words(&pieces, &starts, &probs, true, 1.0);
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, [" And", " so,", " my", " fellow", " Americans"]);
        assert!((w[0].start - 1.0).abs() < 1e-9);
        assert!((w[3].start - (1.0 + 60.0 / 50.0)).abs() < 1e-9);
        assert!((w[3].end - (1.0 + 80.0 / 50.0)).abs() < 1e-9);
        assert!((w[4].end - (1.0 + 120.0 / 50.0)).abs() < 1e-9);
        // An opening quote joins the next word.
        let pieces: Vec<Vec<u8>> = [" \"", "Hi", "\""]
            .iter()
            .map(|p| p.as_bytes().to_vec())
            .collect();
        let w = words(&pieces, &[0, 5, 10, 12], &[0.5; 3], true, 0.0);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].text, " \"Hi\"");
    }

    /// A character split over tokens stays whole; unspaced languages make
    /// a word of every character.
    #[test]
    fn characters_over_tokens_stay_whole() {
        let ka = "か".as_bytes();
        let pieces = vec![ka[..1].to_vec(), ka[1..].to_vec(), "な".as_bytes().to_vec()];
        let w = words(&pieces, &[0, 3, 6, 9], &[1.0; 3], false, 0.0);
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["か", "な"]);
    }
}

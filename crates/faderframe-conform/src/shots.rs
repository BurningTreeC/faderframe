//! Changes between two pictures without cut lists: every frame is reduced
//! to a signature (a tiny luma picture), and the new picture is walked
//! frame by frame, each run of frames found in the old picture becoming a
//! [`Move`]. A run continues while the next frames match the next old
//! frames; where it breaks (a cut), the frame is looked for anew — first
//! right after the last old run (a trim), then anywhere, a candidate kept
//! only when the frames after it match too (so a black frame or a still
//! does not land just anywhere). New frames found nowhere are new
//! material.

use crate::changes::{Changes, Move};

/// A frame's signature: its luma at 16×9.
pub type Signature = [u8; SIGNATURE];
pub const SIGNATURE: usize = 16 * 9;

/// How alike two signatures are (mean absolute difference, 0–255).
pub fn distance(a: &Signature, b: &Signature) -> f32 {
    let sum: u32 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs())
        .sum();
    sum as f32 / SIGNATURE as f32
}

/// Frames this alike are the same picture (re-encoding and scaling
/// differences stay well under it).
const SAME: f32 = 6.0;
/// Frames after a candidate that must match too.
const CONFIRM: usize = 6;

/// The frames of one picture: their start times (seconds) and signatures.
pub struct Frames<'a> {
    pub times: &'a [f64],
    pub signatures: &'a [Signature],
    /// Where the picture ends (seconds).
    pub end: f64,
}

impl Frames<'_> {
    fn len(&self) -> usize {
        self.times.len().min(self.signatures.len())
    }

    fn end_of(&self, i: usize) -> f64 {
        self.times.get(i + 1).copied().unwrap_or(self.end)
    }
}

/// Whether old frames from `j` match new frames from `i` for `n` frames
/// (as many as both have), on average.
fn run_matches(old: &Frames, j: usize, new: &Frames, i: usize, n: usize) -> bool {
    let n = n.min(old.len() - j).min(new.len() - i);
    if n == 0 {
        return false;
    }
    let total: f32 = (0..n)
        .map(|k| distance(&old.signatures[j + k], &new.signatures[i + k]))
        .sum();
    total / n as f32 <= SAME
}

/// How alike old frames from `j` and new frames from `i` are over `n`
/// frames (as many as both have), on average.
fn score(old: &Frames, j: usize, new: &Frames, i: usize, n: usize) -> f32 {
    let n = n.min(old.len() - j).min(new.len() - i).max(1);
    (0..n)
        .map(|k| distance(&old.signatures[j + k], &new.signatures[i + k]))
        .sum::<f32>()
        / n as f32
}

/// Frames a candidate is judged on (a still or a slow shot has frames
/// alike one by one; a second of them tells them apart).
const WINDOW: usize = 24;

/// Where new frame `i` is in the old picture: right after `next` when it
/// is there, else the confirmed match that goes on matching best (nearest
/// `next` among equals).
fn find(
    old: &Frames,
    by_mean: &[(f32, usize)],
    new: &Frames,
    i: usize,
    next: Option<usize>,
) -> Option<usize> {
    if let Some(j) = next
        && j < old.len()
        && run_matches(old, j, new, i, CONFIRM)
        && score(old, j, new, i, WINDOW) <= SAME
    {
        return Some(j);
    }
    let sig = &new.signatures[i];
    // Only frames whose mean is close: the difference of the means is a
    // lower bound of the distance.
    let m = mean(sig);
    let from = by_mean.partition_point(|(v, _)| *v < m - SAME);
    let mut best: Option<(f32, usize)> = None;
    for &(v, j) in &by_mean[from..] {
        if v > m + SAME {
            break;
        }
        if distance(&old.signatures[j], sig) > SAME || !run_matches(old, j, new, i, CONFIRM) {
            continue;
        }
        let d = score(old, j, new, i, WINDOW);
        let away = |j: usize| next.map_or(j, |n| n.abs_diff(j));
        let better = match best {
            None => true,
            Some((bd, bj)) => d + 0.01 < bd || ((d - bd).abs() <= 0.01 && away(j) < away(bj)),
        };
        if better {
            best = Some((d, j));
        }
    }
    best.map(|(_, j)| j)
}

fn mean(s: &Signature) -> f32 {
    s.iter().map(|v| *v as u32).sum::<u32>() as f32 / SIGNATURE as f32
}

/// The changes from the `old` picture to the `new` one.
pub fn match_pictures(old: &Frames, new: &Frames) -> Changes {
    let mut by_mean: Vec<(f32, usize)> = (0..old.len())
        .map(|j| (mean(&old.signatures[j]), j))
        .collect();
    by_mean.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut moves: Vec<Move> = Vec::new();
    let mut added: Vec<(f64, f64)> = Vec::new();
    let mut i = 0;
    let mut next: Option<usize> = None;
    while i < new.len() {
        let Some(mut j) = find(old, &by_mean, new, i, next) else {
            let (a, b) = (new.times[i], new.end_of(i));
            match added.last_mut() {
                Some(last) if (last.1 - a).abs() < 1e-6 => last.1 = b,
                _ => added.push((a, b)),
            }
            i += 1;
            next = None;
            continue;
        };
        // The run: as long as frames go on matching one by one.
        let (i0, j0) = (i, j);
        while i < new.len()
            && j < old.len()
            && distance(&old.signatures[j], &new.signatures[i]) <= SAME * 2.0
        {
            i += 1;
            j += 1;
        }
        moves.push(Move {
            old_in: old.times[j0],
            old_out: old.end_of(j - 1),
            new_in: new.times[i0],
        });
        next = Some(j);
    }
    // What of the old picture no run took.
    let mut taken: Vec<(f64, f64)> = moves.iter().map(|m| (m.old_in, m.old_out)).collect();
    taken.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut removed = Vec::new();
    let mut at = old.times.first().copied().unwrap_or(0.0);
    for (a, b) in taken {
        if a > at + 1e-6 {
            removed.push((at, a));
        }
        at = at.max(b);
    }
    if old.end > at + 1e-6 {
        removed.push((at, old.end));
    }
    Changes {
        moves,
        added,
        removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture whose shots look nothing alike and whose frames change
    /// slowly within a shot: shot `s`, frame `k` of it.
    fn frame(shot: u32, k: u32) -> Signature {
        let mut s = [0u8; SIGNATURE];
        for (p, v) in s.iter_mut().enumerate() {
            let h = (shot.wrapping_mul(2_654_435_761) ^ (p as u32).wrapping_mul(40_503))
                .wrapping_mul(2_246_822_519);
            *v = (h >> 24) as u8 / 2 + 40 + (k / 2) as u8;
        }
        s
    }

    fn picture(shots: &[(u32, u32, u32)]) -> (Vec<f64>, Vec<Signature>) {
        // (shot, first frame, frames)
        let mut times = Vec::new();
        let mut sigs = Vec::new();
        for &(s, from, n) in shots {
            for k in from..from + n {
                times.push(times.len() as f64 / 25.0);
                sigs.push(frame(s, k));
            }
        }
        (times, sigs)
    }

    #[test]
    fn shots_reordered_trimmed_and_new() {
        // Old: shot 1 (50 frames), shot 2 (50), shot 3 (50).
        let (ot, os) = picture(&[(1, 0, 50), (2, 0, 50), (3, 0, 50)]);
        // New: shot 3, shot 1 without its first 10 frames, a new shot 9,
        // shot 2.
        let (nt, ns) = picture(&[(3, 0, 50), (1, 10, 40), (9, 0, 25), (2, 0, 50)]);
        let old = Frames {
            times: &ot,
            signatures: &os,
            end: ot.len() as f64 / 25.0,
        };
        let new = Frames {
            times: &nt,
            signatures: &ns,
            end: nt.len() as f64 / 25.0,
        };
        let c = match_pictures(&old, &new);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert_eq!(c.moves.len(), 3, "{:?}", c.moves);
        assert!(close(c.moves[0].old_in, 4.0) && close(c.moves[0].new_in, 0.0));
        assert!(close(c.moves[1].old_in, 0.4) && close(c.moves[1].old_out, 2.0));
        assert!(close(c.moves[1].new_in, 2.0));
        assert!(close(c.moves[2].old_in, 2.0) && close(c.moves[2].new_in, 4.6));
        assert_eq!(c.added.len(), 1);
        assert!(close(c.added[0].0, 3.6) && close(c.added[0].1, 4.6));
        assert_eq!(c.removed.len(), 1);
        assert!(close(c.removed[0].0, 0.0) && close(c.removed[0].1, 0.4));
    }
}

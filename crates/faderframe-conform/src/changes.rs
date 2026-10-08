//! The changes between two cuts of the picture: each span of the new cut
//! that shows material the old cut showed comes from where the old cut
//! showed it ([`Move`]); new material has no old place; what the old cut
//! showed and the new one does not is gone.
//!
//! Material is matched by source and source time: a new event's source
//! span overlapping an old event's (same source, both at normal speed)
//! maps record time to record time. Where the old cut used the same
//! material twice, the occurrence that keeps the new cut's order (nearest
//! after the last one taken) wins.

use crate::{CutList, Event, Kind};

/// A span of the old cut (`old_in`..`old_out`, record seconds) that is at
/// `new_in` in the new cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Move {
    pub old_in: f64,
    pub old_out: f64,
    pub new_in: f64,
}

impl Move {
    pub fn length(&self) -> f64 {
        self.old_out - self.old_in
    }

    /// How far it moved.
    pub fn shift(&self) -> f64 {
        self.new_in - self.old_in
    }
}

/// The changes from one cut to another.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// In new-cut order, neighbours that move together merged.
    pub moves: Vec<Move>,
    /// Spans of the new cut with no old material (record seconds).
    pub added: Vec<(f64, f64)>,
    /// Spans of the old cut the new one leaves out.
    pub removed: Vec<(f64, f64)>,
}

/// Times closer than this are the same (a hair under a frame at 120 fps).
const EPS: f64 = 1e-4;

/// The changes of picture track `track` from `old` to `new`.
pub fn changes(old: &CutList, new: &CutList, kind: Kind, track: usize) -> Changes {
    let olds = old.track(kind, track);
    let news = new.track(kind, track);
    let mut moves: Vec<Move> = Vec::new();
    let mut added = Vec::new();
    let mut last_old_out = f64::NEG_INFINITY;
    for e in &news {
        // The pieces of this new event that old events show.
        let mut pieces: Vec<Move> = Vec::new();
        let mut from = e.source_in;
        while from < e.source_out - EPS {
            let Some((o, start, end)) = best_old(&olds, e, from, last_old_out) else {
                // No old event shows it from here: new material up to the
                // next old material in this event (or its end).
                let next = olds
                    .iter()
                    .filter(|o| same_material(o, e) && o.source_in > from + EPS)
                    .map(|o| o.source_in)
                    .fold(e.source_out, f64::min);
                added.push((record_of(e, from), record_of(e, next)));
                from = next;
                continue;
            };
            let _ = start;
            let piece = Move {
                old_in: o.record_in + (from - o.source_in),
                old_out: o.record_in + (end - o.source_in),
                new_in: record_of(e, from),
            };
            last_old_out = piece.old_out;
            pieces.push(piece);
            from = end;
        }
        for p in pieces {
            match moves.last_mut() {
                Some(m)
                    if (m.old_out - p.old_in).abs() < EPS
                        && (m.new_in + m.length() - p.new_in).abs() < EPS =>
                {
                    m.old_out = p.old_out;
                }
                _ => moves.push(p),
            }
        }
    }
    added.retain(|(a, b)| b - a > EPS);
    // What of the old cut no move takes.
    let mut taken: Vec<(f64, f64)> = moves.iter().map(|m| (m.old_in, m.old_out)).collect();
    taken.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut removed = Vec::new();
    for o in &olds {
        let mut at = o.record_in;
        for &(a, b) in &taken {
            if b <= at + EPS || a >= o.record_out - EPS {
                continue;
            }
            if a > at + EPS {
                removed.push((at, a.min(o.record_out)));
            }
            at = at.max(b);
        }
        if o.record_out > at + EPS {
            removed.push((at, o.record_out));
        }
    }
    Changes {
        moves,
        added,
        removed,
    }
}

fn same_material(o: &Event, e: &Event) -> bool {
    o.source == e.source && (o.speed - 1.0).abs() < 1e-6 && (e.speed - 1.0).abs() < 1e-6
}

/// The new event's record time at source time `t`.
fn record_of(e: &Event, t: f64) -> f64 {
    e.record_in + (t - e.source_in)
}

/// The old event showing the new event's material from source time
/// `from`, and that material's span (`from`..`end`) in it: of several,
/// the one nearest after `after` on the old record (the cut's order).
fn best_old<'a>(
    olds: &[&'a Event],
    e: &Event,
    from: f64,
    after: f64,
) -> Option<(&'a Event, f64, f64)> {
    olds.iter()
        .filter(|o| same_material(o, e))
        .filter(|o| o.source_in <= from + EPS && o.source_out > from + EPS)
        .map(|o| (*o, from, o.source_out.min(e.source_out)))
        .min_by(|a, b| {
            let key = |o: &Event| {
                let at = o.record_in + (from - o.source_in);
                // After the last one taken first, nearest first.
                if at >= after - EPS {
                    (0, at - after)
                } else {
                    (1, after - at)
                }
            };
            let (ka, kb) = (key(a.0), key(b.0));
            ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(source: &str, src: f64, rec: f64, len: f64) -> Event {
        Event {
            kind: Kind::Video,
            track: 0,
            source: source.into(),
            name: None,
            source_in: src,
            source_out: src + len,
            record_in: rec,
            record_out: rec + len,
            speed: 1.0,
        }
    }

    fn list(events: Vec<Event>) -> CutList {
        CutList {
            title: None,
            events,
        }
    }

    #[test]
    fn a_trim_a_swap_a_new_shot_and_a_removal() {
        // Old: A 0–10, B 10–15, C 15–20.
        let old = list(vec![
            ev("A", 100.0, 0.0, 10.0),
            ev("B", 200.0, 10.0, 5.0),
            ev("C", 300.0, 15.0, 5.0),
        ]);
        // New: A trimmed by 2 s at its head, C before B, a new shot N,
        // B lost its last second.
        let new = list(vec![
            ev("A", 102.0, 0.0, 8.0),
            ev("C", 300.0, 8.0, 5.0),
            ev("N", 0.0, 13.0, 3.0),
            ev("B", 200.0, 16.0, 4.0),
        ]);
        let c = changes(&old, &new, Kind::Video, 0);
        assert_eq!(
            c.moves,
            vec![
                Move {
                    old_in: 2.0,
                    old_out: 10.0,
                    new_in: 0.0
                },
                Move {
                    old_in: 15.0,
                    old_out: 20.0,
                    new_in: 8.0
                },
                Move {
                    old_in: 10.0,
                    old_out: 14.0,
                    new_in: 16.0
                },
            ]
        );
        assert_eq!(c.added, vec![(13.0, 16.0)]);
        assert_eq!(c.removed, vec![(0.0, 2.0), (14.0, 15.0)]);
    }

    #[test]
    fn an_unchanged_cut_moves_nothing_and_merges() {
        let old = list(vec![ev("A", 0.0, 0.0, 5.0), ev("A", 5.0, 5.0, 5.0)]);
        let c = changes(&old, &old, Kind::Video, 0);
        assert_eq!(
            c.moves,
            vec![Move {
                old_in: 0.0,
                old_out: 10.0,
                new_in: 0.0
            }]
        );
        assert!(c.added.is_empty() && c.removed.is_empty());
    }

    #[test]
    fn material_used_twice_follows_the_cut() {
        // The old cut shows the same take twice; the new one keeps both.
        let old = list(vec![ev("A", 0.0, 0.0, 4.0), ev("A", 0.0, 10.0, 4.0)]);
        let new = list(vec![ev("A", 0.0, 2.0, 4.0), ev("A", 0.0, 12.0, 4.0)]);
        let c = changes(&old, &new, Kind::Video, 0);
        assert_eq!(c.moves.len(), 2);
        assert_eq!((c.moves[0].old_in, c.moves[1].old_in), (0.0, 10.0));
    }
}

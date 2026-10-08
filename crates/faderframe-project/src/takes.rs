//! Take folders: several takes of the same passage and the *comp* — which
//! take plays where.
//!
//! A [`TakeFolder`] is the content of one clip on an audio track. Every
//! [`Take`] references an audio source and the part of the folder it covers.
//! The comp is a sorted list of [`CompSegment`]s; each one plays a take (or
//! nothing) from its start until the next segment starts, so the comp can
//! never overlap or leave holes. All positions are frames at the project
//! rate relative to the folder (clip) start.
//!
//! Comp edits are ordinary clip edits (`Command::SetClipContent`), so they
//! are undoable and coalesce during a swipe. The engine expands the comp
//! into regions with short equal-power crossfades at segment boundaries.

use crate::ClipFades;
use faderframe_core::AudioSourceId;
use serde::{Deserialize, Serialize};

/// Default crossfade at comp boundaries: 10 ms at 48 kHz.
pub const DEFAULT_COMP_CROSSFADE: i64 = 480;

fn default_crossfade() -> i64 {
    DEFAULT_COMP_CROSSFADE
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Take {
    pub name: String,
    pub source: AudioSourceId,
    /// Source frame aligned with the folder start (may be negative when the
    /// take starts later than the folder).
    pub source_offset: i64,
    /// Folder-relative range this take has material for.
    pub start: i64,
    pub end: i64,
    #[serde(default)]
    pub gain_db: f32,
    /// How good it is: 0 (not rated) to 5 stars.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rating: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

impl Take {
    pub fn covers(&self, pos: i64) -> bool {
        pos >= self.start && pos < self.end
    }
}

/// From `start` until the next segment (or the folder end), play `take`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompSegment {
    pub start: i64,
    /// Index into [`TakeFolder::takes`]; `None` is silence.
    pub take: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TakeFolder {
    /// Length in frames (project rate).
    pub length: i64,
    pub takes: Vec<Take>,
    pub comp: Vec<CompSegment>,
    #[serde(default)]
    pub gain_db: f32,
    /// Fades at the folder edges.
    #[serde(default)]
    pub fades: ClipFades,
    /// Crossfade length at comp boundaries (frames).
    #[serde(default = "default_crossfade")]
    pub crossfade: i64,
}

/// One played piece of the comp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompPiece {
    pub start: i64,
    pub end: i64,
    pub take: usize,
}

impl TakeFolder {
    pub fn new(length: i64) -> Self {
        Self {
            length: length.max(0),
            takes: Vec::new(),
            comp: vec![CompSegment {
                start: 0,
                take: None,
            }],
            gain_db: 0.0,
            fades: ClipFades::default(),
            crossfade: DEFAULT_COMP_CROSSFADE,
        }
    }

    /// Add a take; returns its index. The comp is unchanged.
    pub fn add_take(&mut self, take: Take) -> usize {
        self.takes.push(take);
        self.takes.len() - 1
    }

    /// Restore the invariants: sorted, first segment at 0, no duplicates,
    /// no segment at/after the end, adjacent equal segments merged, valid
    /// take indices.
    pub fn normalize(&mut self) {
        let n = self.takes.len();
        let len = self.length;
        for s in &mut self.comp {
            if s.take.is_some_and(|t| t >= n) {
                s.take = None;
            }
        }
        // Stable sort; for equal starts the last one wins.
        self.comp.sort_by_key(|s| s.start);
        let mut out: Vec<CompSegment> = Vec::with_capacity(self.comp.len() + 1);
        for s in self.comp.drain(..) {
            if s.start >= len && len > 0 {
                continue;
            }
            let s = CompSegment {
                start: s.start.max(0),
                ..s
            };
            match out.last_mut() {
                Some(last) if last.start == s.start => *last = s,
                _ => out.push(s),
            }
        }
        if out.first().is_none_or(|s| s.start > 0) {
            out.insert(
                0,
                CompSegment {
                    start: 0,
                    take: None,
                },
            );
        }
        out.dedup_by(|b, a| a.take == b.take);
        self.comp = out;
    }

    /// `(start, end, take)` for every comp segment.
    pub fn segments(&self) -> impl Iterator<Item = (i64, i64, Option<usize>)> + '_ {
        self.comp.iter().enumerate().map(|(i, s)| {
            let end = self.comp.get(i + 1).map_or(self.length, |n| n.start);
            (s.start, end.min(self.length), s.take)
        })
    }

    /// The take playing at `pos`.
    pub fn take_at(&self, pos: i64) -> Option<usize> {
        let i = self.comp.partition_point(|s| s.start <= pos);
        self.comp.get(i.checked_sub(1)?)?.take
    }

    /// What actually sounds: comp segments clipped to the material each
    /// take has (gaps in a take are silent).
    pub fn pieces(&self) -> Vec<CompPiece> {
        let mut out = Vec::new();
        for (a, b, t) in self.segments() {
            let Some(t) = t else { continue };
            let Some(take) = self.takes.get(t) else {
                continue;
            };
            let (a, b) = (a.max(take.start), b.min(take.end));
            if b > a {
                out.push(CompPiece {
                    start: a,
                    end: b,
                    take: t,
                });
            }
        }
        out
    }

    /// Play `take` in `[a, b)` (swipe comping); the rest is unchanged.
    pub fn set_comp(&mut self, a: i64, b: i64, take: Option<usize>) {
        let (a, b) = (a.clamp(0, self.length), b.clamp(0, self.length));
        if b <= a {
            return;
        }
        let after = self.take_at(b);
        self.comp.retain(|s| s.start < a || s.start > b);
        self.comp.push(CompSegment { start: a, take });
        if b < self.length {
            self.comp.push(CompSegment {
                start: b,
                take: after,
            });
        }
        self.normalize();
    }

    /// Play `take` everywhere it has material (silence elsewhere).
    pub fn use_take(&mut self, take: usize) {
        let Some(t) = self.takes.get(take) else {
            return;
        };
        let (a, b) = (t.start, t.end);
        self.comp = vec![CompSegment {
            start: 0,
            take: None,
        }];
        self.set_comp(a, b, Some(take));
    }

    /// Remove a take; comp segments that used it go silent.
    pub fn remove_take(&mut self, take: usize) {
        if take >= self.takes.len() {
            return;
        }
        self.takes.remove(take);
        for s in &mut self.comp {
            s.take = match s.take {
                Some(t) if t == take => None,
                Some(t) if t > take => Some(t - 1),
                other => other,
            };
        }
        self.normalize();
    }

    /// Grow the folder by `before` frames at the start and `after` at the
    /// end (existing material keeps its timeline position).
    pub fn extend(&mut self, before: i64, after: i64) {
        let (before, after) = (before.max(0), after.max(0));
        for t in &mut self.takes {
            t.source_offset -= before;
            t.start += before;
            t.end += before;
        }
        for s in &mut self.comp {
            s.start += before;
        }
        self.length += before + after;
        if before > 0 {
            self.comp.insert(
                0,
                CompSegment {
                    start: 0,
                    take: None,
                },
            );
        }
        self.normalize();
    }

    /// Split at folder frame `at` into `(left, right)`; both keep every take.
    pub fn split(&self, at: i64) -> Option<(TakeFolder, TakeFolder)> {
        if at <= 0 || at >= self.length {
            return None;
        }
        let mut left = self.clone();
        left.length = at;
        left.fades.fade_out = 0;
        for t in &mut left.takes {
            t.end = t.end.min(at);
            t.start = t.start.min(at);
        }
        left.normalize();
        let mut right = self.clone();
        right.length = self.length - at;
        right.fades.fade_in = 0;
        let first = self.take_at(at);
        for t in &mut right.takes {
            t.source_offset += at;
            t.start = (t.start - at).max(0);
            t.end = (t.end - at).max(0);
        }
        right.comp = self
            .comp
            .iter()
            .filter(|s| s.start > at)
            .map(|s| CompSegment {
                start: s.start - at,
                take: s.take,
            })
            .collect();
        right.comp.insert(
            0,
            CompSegment {
                start: 0,
                take: first,
            },
        );
        right.normalize();
        Some((left, right))
    }

    pub fn sources(&self) -> impl Iterator<Item = AudioSourceId> + '_ {
        self.takes.iter().map(|t| t.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(takes: usize, len: i64) -> TakeFolder {
        let mut f = TakeFolder::new(len);
        for i in 0..takes {
            f.add_take(Take {
                name: format!("Take {}", i + 1),
                source: AudioSourceId(i as u64 + 1),
                source_offset: 0,
                start: 0,
                end: len,
                gain_db: 0.0,
                rating: 0,
            });
        }
        f
    }

    #[test]
    fn swipe_comping_keeps_segments_consistent() {
        let mut f = folder(3, 1000);
        f.use_take(2);
        assert_eq!(
            f.pieces(),
            vec![CompPiece {
                start: 0,
                end: 1000,
                take: 2
            }]
        );
        f.set_comp(200, 400, Some(0));
        f.set_comp(300, 600, Some(1));
        let segs: Vec<_> = f.segments().collect();
        assert_eq!(
            segs,
            vec![
                (0, 200, Some(2)),
                (200, 300, Some(0)),
                (300, 600, Some(1)),
                (600, 1000, Some(2))
            ]
        );
        // Re-selecting a neighbour's take merges segments.
        f.set_comp(200, 300, Some(1));
        assert_eq!(f.segments().count(), 3);
        assert_eq!(f.take_at(250), Some(1));
        // Swiping over the whole folder replaces everything.
        f.set_comp(-50, 5000, Some(0));
        assert_eq!(f.segments().collect::<Vec<_>>(), vec![(0, 1000, Some(0))]);
    }

    #[test]
    fn partial_takes_play_silence_where_they_have_no_material() {
        let mut f = folder(2, 1000);
        f.takes[1].start = 400;
        f.use_take(1);
        assert_eq!(f.take_at(100), None);
        assert_eq!(
            f.pieces(),
            vec![CompPiece {
                start: 400,
                end: 1000,
                take: 1
            }]
        );
        f.takes[0].end = 500;
        f.set_comp(0, 1000, Some(0));
        assert_eq!(
            f.pieces(),
            vec![CompPiece {
                start: 0,
                end: 500,
                take: 0
            }]
        );
    }

    #[test]
    fn remove_extend_and_split() {
        let mut f = folder(3, 1000);
        f.use_take(0);
        f.set_comp(500, 1000, Some(2));
        f.remove_take(0);
        assert_eq!(f.takes.len(), 2);
        assert_eq!(
            f.segments().collect::<Vec<_>>(),
            vec![(0, 500, None), (500, 1000, Some(1))]
        );

        f.extend(100, 50);
        assert_eq!(f.length, 1150);
        assert_eq!(f.takes[0].source_offset, -100);
        assert_eq!((f.takes[0].start, f.takes[0].end), (100, 1100));
        assert_eq!(f.take_at(650), Some(1));
        assert_eq!(f.take_at(50), None);

        let (l, r) = f.split(700).unwrap();
        assert_eq!((l.length, r.length), (700, 450));
        assert_eq!(l.take_at(650), Some(1));
        assert_eq!(r.take_at(0), Some(1));
        assert_eq!(r.takes[1].source_offset, f.takes[1].source_offset + 700);
        assert_eq!((r.takes[1].start, r.takes[1].end), (0, 400));
        assert!(f.split(0).is_none() && f.split(1150).is_none());
    }

    #[test]
    fn serde_round_trip() {
        let mut f = folder(2, 480);
        f.set_comp(100, 200, Some(1));
        let json = serde_json::to_string(&f).unwrap();
        let back: TakeFolder = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }
}

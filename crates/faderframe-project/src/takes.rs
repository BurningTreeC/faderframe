//! Take folders: several takes of the same passage and the *comp* — which
//! take plays where.
//!
//! A [`TakeFolder`] is the content of one clip on an audio track. Every
//! [`Take`] is an audio clip of its own — a source with every edit a clip
//! can carry (pitch, warp, transposition, reversal, clip effects, spectral
//! edits, envelope, fades) — placed in the folder. The comp is a sorted list
//! of [`CompSegment`]s; each one plays a take (or nothing) from its start
//! until the next segment starts, so the comp can never overlap or leave
//! holes. All positions are frames at the project rate relative to the
//! folder (clip) start.
//!
//! Comp edits are ordinary clip edits (`Command::SetClipContent`), so they
//! are undoable and coalesce during a swipe. What a comp plays is the takes'
//! clips cut at the comp's boundaries ([`TakeFolder::played`]): the engine
//! plays those pieces as it plays any clip, and Flatten Comp places them —
//! one function, so a flattened comp is what the comp played.

use crate::{AudioClip, ClipFades, FadeShape};
use faderframe_core::AudioSourceId;
use serde::{Deserialize, Serialize};

/// Default crossfade at comp boundaries: 10 ms at 48 kHz.
pub const DEFAULT_COMP_CROSSFADE: i64 = 480;

/// Frames of linear fade at comp edges inside the folder that touch no
/// other piece (a declick): what the engine plays and Flatten Comp keeps.
pub const COMP_DECLICK: i64 = 64;

fn default_crossfade() -> i64 {
    DEFAULT_COMP_CROSSFADE
}

/// A take's name in its folder: given once by the folder, never given to
/// another take of it, kept through comp edits, splits, undo and saving —
/// what an editor working on one take holds on to (0: not given yet;
/// [`TakeFolder::add_take`] and [`TakeFolder::normalize`] give one).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TakeId(pub u32);

impl TakeId {
    pub fn is_given(self) -> bool {
        self.0 != 0
    }
}

impl std::fmt::Display for TakeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Take {
    /// Its id in the folder.
    #[serde(default)]
    pub id: TakeId,
    pub name: String,
    /// Folder frame its clip starts at (its material lasts
    /// `audio.length` frames from here; a part outside the folder is
    /// never played).
    pub start: i64,
    /// What it plays: an audio clip with all of a clip's edits (`length`
    /// 0: no material in this folder, a lane kept after a split).
    pub audio: AudioClip,
    /// How good it is: 0 (not rated) to 5 stars.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rating: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

impl Take {
    /// A take of `audio` (a clip's audio, edits and all) starting at
    /// folder frame `start`.
    pub fn of_clip(name: impl Into<String>, start: i64, audio: AudioClip) -> Self {
        Self {
            id: TakeId::default(),
            name: name.into(),
            start,
            audio,
            rating: 0,
        }
    }

    /// A take of plain audio: `length` frames of `source` from source
    /// frame `source_offset`, starting at folder frame `start`.
    pub fn plain(
        name: impl Into<String>,
        source: AudioSourceId,
        source_offset: i64,
        start: i64,
        length: i64,
    ) -> Self {
        Self::of_clip(name, start, AudioClip::plain(source, source_offset, length))
    }

    /// Folder frame its material ends at.
    pub fn end(&self) -> i64 {
        self.start + self.audio.length
    }

    pub fn covers(&self, pos: i64) -> bool {
        pos >= self.start && pos < self.end()
    }

    /// The source frame played at folder frame `pos` (through its warp;
    /// a reversed take counts back from its end).
    pub fn source_at(&self, pos: i64) -> f64 {
        let out = (pos - self.start) as f64;
        if self.audio.reversed {
            (self.audio.source_offset + self.audio.length) as f64 - 1.0 - out
        } else {
            self.audio.source_at(out)
        }
    }

    /// The folder frame source frame `src` plays at (through its warp; a
    /// reversed take counts back from its end). Note lengths a live pitch
    /// edit changes are not followed.
    pub fn folder_of(&self, src: i64) -> i64 {
        let a = &self.audio;
        let out = match &a.warp {
            _ if a.reversed => a.source_offset + a.length - 1 - src,
            Some(w) => w.output_of(a.source_offset, a.length, src),
            None => src - a.source_offset,
        };
        self.start + out
    }

    /// Plays its source as it is, frame for frame: no warp that moves
    /// anything, no transposition, not reversed, no pitch edit played
    /// live. (Edits rendered into its source — clip effects, spectral
    /// edits, a polyphonic pitch edit — play their source frame for frame
    /// too.)
    pub fn plays_source_as_is(&self) -> bool {
        let a = &self.audio;
        a.warp
            .as_ref()
            .is_none_or(|w| w.is_identity(a.source_offset, a.length))
            && a.stretch.semitones() == 0.0
            && !a.reversed
            && a.pitch.as_ref().is_none_or(|p| !p.plays_live())
    }

    /// This take cut to folder frames `[from, to)` (`None` when it has no
    /// material there).
    pub fn part(&self, from: i64, to: i64) -> Option<Take> {
        let audio = self.audio.cut(from - self.start, to - self.start)?;
        Some(Take {
            id: self.id,
            name: self.name.clone(),
            start: from.max(self.start),
            audio,
            rating: self.rating,
        })
    }

    /// The take with no material left, placed at folder frame `at`.
    fn emptied_at(&self, at: i64) -> Take {
        Take {
            id: self.id,
            name: self.name.clone(),
            start: at,
            audio: self.audio.emptied(),
            rating: self.rating,
        }
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
    /// The id the next take gets (ids are never given twice in a folder).
    #[serde(default)]
    pub next_take: u32,
}

/// One played piece of the comp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompPiece {
    pub start: i64,
    pub end: i64,
    pub take: usize,
}

/// What a folder plays from folder frame `start`: take `take`'s clip cut
/// to the piece (with the comp's crossfade into its neighbours), the
/// comp's fades and the folder's gain in it — a clip as the engine plays
/// clips ([`TakeFolder::played`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayedPiece {
    pub start: i64,
    pub take: usize,
    pub audio: AudioClip,
}

/// A fade's length, shape and drawn bend.
type Fade = (i64, FadeShape, i16);

const DECLICK_FADE: Fade = (COMP_DECLICK, FadeShape::Linear, 0);

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
            next_take: 1,
        }
    }

    /// Add a take (it gets a new id here, whatever it had); returns its
    /// index. The comp is unchanged.
    pub fn add_take(&mut self, mut take: Take) -> usize {
        take.id = self.new_take_id();
        self.takes.push(take);
        self.takes.len() - 1
    }

    /// An id no take of the folder has had.
    fn new_take_id(&mut self) -> TakeId {
        let above = self.takes.iter().map(|t| t.id.0).max().unwrap_or(0) + 1;
        let id = self.next_take.max(above).max(1);
        self.next_take = id + 1;
        TakeId(id)
    }

    /// The index of take `id` now.
    pub fn index_of(&self, id: TakeId) -> Option<usize> {
        self.takes.iter().position(|t| t.id == id)
    }

    /// Take `id`.
    pub fn take(&self, id: TakeId) -> Option<&Take> {
        self.takes.iter().find(|t| t.id == id)
    }

    pub fn take_mut(&mut self, id: TakeId) -> Option<&mut Take> {
        self.takes.iter_mut().find(|t| t.id == id)
    }

    /// Restore the invariants: every take an id of its own (one not given
    /// yet, or given twice, gets a new one), sorted, first segment at 0,
    /// no duplicates, no segment at/after the end, adjacent equal segments
    /// merged, valid take indices.
    pub fn normalize(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        let mut fresh = Vec::new();
        for (i, t) in self.takes.iter().enumerate() {
            if !t.id.is_given() || !seen.insert(t.id) {
                fresh.push(i);
            }
        }
        for i in fresh {
            let id = self.new_take_id();
            self.takes[i].id = id;
        }
        if let Some(top) = self.takes.iter().map(|t| t.id.0).max() {
            self.next_take = self.next_take.max(top + 1);
        }
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
            let (a, b) = (a.max(take.start), b.min(take.end()));
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

    /// Play `take` everywhere it has material (silence elsewhere); a take
    /// with none here (a split left it outside this half) changes nothing.
    pub fn use_take(&mut self, take: usize) {
        let Some(t) = self.takes.get(take) else {
            return;
        };
        let (a, b) = (t.start.clamp(0, self.length), t.end().clamp(0, self.length));
        if b <= a {
            return;
        }
        self.comp = vec![CompSegment {
            start: 0,
            take: None,
        }];
        self.set_comp(a, b, Some(take));
    }

    /// Play `take` in the comp segment at `pos` (a click on its lane): the
    /// section between the comp boundaries around `pos` changes, the rest
    /// of the comp stays. A take with no material in that section changes
    /// nothing.
    pub fn use_take_at(&mut self, pos: i64, take: usize) {
        let Some(t) = self.takes.get(take) else {
            return;
        };
        let pos = pos.clamp(0, (self.length - 1).max(0));
        let Some((a, b, _)) = self.segments().find(|&(a, b, _)| pos >= a && pos < b) else {
            return;
        };
        if t.start.max(a) >= t.end().min(b) {
            return;
        }
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
            t.start += before;
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

    /// Split at folder frame `at` into `(left, right)`; both keep every
    /// take (a take is cut where the split is, edits and all; one with no
    /// material in a half keeps an empty lane there).
    pub fn split(&self, at: i64) -> Option<(TakeFolder, TakeFolder)> {
        if at <= 0 || at >= self.length {
            return None;
        }
        let mut left = self.clone();
        left.length = at;
        left.fades.fade_out = 0;
        left.takes = self
            .takes
            .iter()
            .map(|t| {
                t.part(t.start, at)
                    .unwrap_or_else(|| t.emptied_at(t.start.min(at)))
            })
            .collect();
        left.normalize();
        let mut right = self.clone();
        right.length = self.length - at;
        right.fades.fade_in = 0;
        right.takes = self
            .takes
            .iter()
            .map(|t| match t.part(at, t.end()) {
                Some(p) => Take {
                    start: p.start - at,
                    ..p
                },
                None => t.emptied_at(0),
            })
            .collect();
        let first = self.take_at(at);
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

    /// Its takes as the folder plays them, to be taken into another
    /// folder: the folder's gain in each, its edge fades in the takes at
    /// its edges (cut there; what lay beyond a faded edge goes).
    pub fn takes_as_played(&self) -> Vec<Take> {
        let fades = self.fades;
        self.takes
            .iter()
            .map(|t| {
                let mut t = t.clone();
                if fades.fade_in > 0
                    && t.start <= 0
                    && t.end() > 0
                    && let Some(p) = t.part(0, t.end())
                {
                    t = p;
                    if fades.fade_in > t.audio.fades.fade_in {
                        t.audio.fades.fade_in = fades.fade_in;
                        t.audio.fades.fade_in_shape = fades.fade_in_shape;
                        t.audio.fades.fade_in_bend = fades.fade_in_bend;
                    }
                }
                if fades.fade_out > 0
                    && t.start < self.length
                    && t.end() >= self.length
                    && let Some(p) = t.part(t.start, self.length)
                {
                    t = p;
                    if fades.fade_out > t.audio.fades.fade_out {
                        t.audio.fades.fade_out = fades.fade_out;
                        t.audio.fades.fade_out_shape = fades.fade_out_shape;
                        t.audio.fades.fade_out_bend = fades.fade_out_bend;
                    }
                }
                t.audio.gain_db += self.gain_db;
                t
            })
            .collect()
    }

    /// The sources its takes play.
    pub fn sources(&self) -> impl Iterator<Item = AudioSourceId> + '_ {
        self.takes.iter().map(|t| t.audio.source)
    }

    /// What the comp plays, in order: each piece its take's clip cut
    /// there. Where two pieces touch they overlap by the crossfade
    /// (centred on the boundary, as far as each take has material) with
    /// equal-power fades; an edge inside the folder that touches no other
    /// piece gets a short declick; the folder's fades are at its edges. A
    /// take's own fade stays where the take itself begins or ends, and the
    /// longer of it and the comp's fade plays there. The folder's gain is
    /// in each piece's.
    pub fn played(&self) -> Vec<PlayedPiece> {
        let pieces = self.pieces();
        let half = self.crossfade.max(0) / 2;
        let mut out = Vec::with_capacity(pieces.len());
        for (i, p) in pieces.iter().enumerate() {
            let take = &self.takes[p.take];
            let touches_prev = i > 0 && pieces[i - 1].end == p.start;
            let touches_next = pieces.get(i + 1).is_some_and(|n| n.start == p.end);
            let a = if touches_prev {
                (p.start - half).max(take.start)
            } else {
                p.start
            };
            let b = if touches_next {
                (p.end + half).min(take.end())
            } else {
                p.end
            };
            let fade_in = if touches_prev {
                (p.start + half - a, FadeShape::EqualPower, 0)
            } else if p.start == 0 {
                self.fade_in()
            } else {
                DECLICK_FADE
            };
            let fade_out = if touches_next {
                (b - (p.end - half), FadeShape::EqualPower, 0)
            } else if p.end == self.length {
                self.fade_out()
            } else {
                DECLICK_FADE
            };
            out.extend(self.piece(p.take, a, b, fade_in, fade_out));
        }
        out
    }

    /// Take `k` played on its own (auditioned): where it has material in
    /// the folder, with the folder's fades at the folder's edges and a
    /// declick elsewhere.
    pub fn played_take(&self, k: usize) -> Vec<PlayedPiece> {
        let Some(take) = self.takes.get(k) else {
            return Vec::new();
        };
        let (a, b) = (take.start.max(0), take.end().min(self.length));
        let fade_in = if a == 0 { self.fade_in() } else { DECLICK_FADE };
        let fade_out = if b == self.length {
            self.fade_out()
        } else {
            DECLICK_FADE
        };
        self.piece(k, a, b, fade_in, fade_out).into_iter().collect()
    }

    fn fade_in(&self) -> Fade {
        let f = &self.fades;
        (f.fade_in, f.fade_in_shape, f.fade_in_bend)
    }

    fn fade_out(&self) -> Fade {
        let f = &self.fades;
        (f.fade_out, f.fade_out_shape, f.fade_out_bend)
    }

    /// Take `k` cut to folder frames `[a, b)` with the comp's fades there
    /// (the take's own where it begins or ends, if longer).
    fn piece(
        &self,
        k: usize,
        a: i64,
        b: i64,
        fade_in: Fade,
        fade_out: Fade,
    ) -> Option<PlayedPiece> {
        let take = self.takes.get(k)?;
        let part = take.part(a, b)?;
        let mut audio = part.audio;
        let own = audio.fades;
        let longer = |comp: Fade, own: Fade| if own.0 > comp.0 { own } else { comp };
        let (fade_in, fade_out) = (
            longer(fade_in, (own.fade_in, own.fade_in_shape, own.fade_in_bend)),
            longer(
                fade_out,
                (own.fade_out, own.fade_out_shape, own.fade_out_bend),
            ),
        );
        let length = audio.length;
        let fade_in_len = fade_in.0.clamp(0, length);
        audio.fades = ClipFades {
            fade_in: fade_in_len,
            fade_out: fade_out.0.clamp(0, length - fade_in_len),
            fade_in_shape: fade_in.1,
            fade_out_shape: fade_out.1,
            fade_in_bend: fade_in.2,
            fade_out_bend: fade_out.2,
        };
        audio.gain_db += self.gain_db;
        Some(PlayedPiece {
            start: part.start,
            take: k,
            audio,
        })
    }
}

/// A take named across edits: the folder (clip) it is in and its id
/// there (an index moves when an earlier take goes; this does not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TakeRef {
    pub clip: faderframe_core::ClipId,
    pub take: TakeId,
}

/// What a clip editor (the pitch editor first) works on: an audio clip,
/// or one take of a take folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioRef {
    Clip(faderframe_core::ClipId),
    Take(TakeRef),
}

impl From<faderframe_core::ClipId> for AudioRef {
    fn from(c: faderframe_core::ClipId) -> Self {
        AudioRef::Clip(c)
    }
}

impl From<TakeRef> for AudioRef {
    fn from(t: TakeRef) -> Self {
        AudioRef::Take(t)
    }
}

impl AudioRef {
    /// The clip it is, or the folder it is in.
    pub fn clip(self) -> faderframe_core::ClipId {
        match self {
            AudioRef::Clip(c) => c,
            AudioRef::Take(t) => t.clip,
        }
    }

    pub fn take(self) -> Option<TakeId> {
        match self {
            AudioRef::Clip(_) => None,
            AudioRef::Take(t) => Some(t.take),
        }
    }
}

impl crate::Project {
    /// The audio `r` plays now (`None`: gone, or not audio).
    pub fn audio(&self, r: AudioRef) -> Option<&AudioClip> {
        match r {
            AudioRef::Clip(c) => self.clip(c)?.as_audio(),
            AudioRef::Take(t) => Some(&self.take(t)?.audio),
        }
    }

    /// The project frame its first frame plays at (a take's from its
    /// folder's start).
    pub fn audio_start(&self, r: AudioRef) -> Option<i64> {
        let c = self.clip(r.clip())?;
        let start = self
            .timeline
            .to_samples(c.start, f64::from(self.sample_rate));
        match r {
            AudioRef::Clip(_) => c.as_audio().map(|_| start),
            AudioRef::Take(t) => Some(start + self.take(t)?.start),
        }
    }

    /// What to call it: the clip's name, or "<folder> · <take>".
    pub fn audio_name(&self, r: AudioRef) -> Option<String> {
        let c = self.clip(r.clip())?;
        match r {
            AudioRef::Clip(_) => c.as_audio().map(|_| c.name.clone()),
            AudioRef::Take(t) => Some(format!("{} · {}", c.name, self.take(t)?.name)),
        }
    }

    /// Every audio clip and every take of every folder.
    pub fn audio_refs(&self) -> Vec<AudioRef> {
        let mut out = Vec::new();
        for c in self.clips.values() {
            match &c.content {
                crate::ClipContent::Audio(_) => out.push(AudioRef::Clip(c.id)),
                crate::ClipContent::Takes(f) => out.extend(f.takes.iter().map(|t| {
                    AudioRef::Take(TakeRef {
                        clip: c.id,
                        take: t.id,
                    })
                })),
                crate::ClipContent::Midi(_) => {}
            }
        }
        out
    }

    /// The edits giving each of `changes` its audio: a clip's content, or
    /// its folder's with the take changed — one command a clip, however
    /// many of its takes change (refs that name nothing are left out).
    pub fn set_audio(&self, changes: Vec<(AudioRef, AudioClip)>) -> Vec<crate::Command> {
        let mut out: Vec<(faderframe_core::ClipId, crate::ClipContent)> = Vec::new();
        for (r, audio) in changes {
            let Some(c) = self.clip(r.clip()) else {
                continue;
            };
            let at = out.iter().position(|(id, _)| *id == c.id);
            match r {
                AudioRef::Clip(_) if c.as_audio().is_some() => match at {
                    Some(i) => out[i].1 = crate::ClipContent::Audio(audio),
                    None => out.push((c.id, crate::ClipContent::Audio(audio))),
                },
                AudioRef::Take(t) => {
                    let i = match at {
                        Some(i) => i,
                        None => {
                            out.push((c.id, c.content.clone()));
                            out.len() - 1
                        }
                    };
                    if let crate::ClipContent::Takes(f) = &mut out[i].1
                        && let Some(take) = f.take_mut(t.take)
                    {
                        take.audio = audio;
                    }
                }
                AudioRef::Clip(_) => {}
            }
        }
        out.into_iter()
            .filter_map(|(id, content)| {
                Some(crate::Command::SetClipContent {
                    clip: id,
                    start: self.clip(id)?.start,
                    content: Box::new(content),
                })
            })
            .collect()
    }

    /// The folder `r` names and the take's index in it now.
    pub fn take_in(&self, r: TakeRef) -> Option<(&TakeFolder, usize)> {
        let f = self.clip(r.clip)?.as_takes()?;
        Some((f, f.index_of(r.take)?))
    }

    /// Take `r`.
    pub fn take(&self, r: TakeRef) -> Option<&Take> {
        let (f, i) = self.take_in(r)?;
        f.takes.get(i)
    }

    /// The take at `index` of folder `clip`, named so that edits keep
    /// naming it.
    pub fn take_ref(&self, clip: faderframe_core::ClipId, index: usize) -> Option<TakeRef> {
        let t = self.clip(clip)?.as_takes()?.takes.get(index)?;
        Some(TakeRef { clip, take: t.id })
    }

    /// The edit giving take `r` the audio `audio` (its clip: source, place
    /// in its material, edits): the folder's content with it changed,
    /// one undo step like a comp edit (`None`: no such take).
    pub fn set_take_audio(&self, r: TakeRef, audio: crate::AudioClip) -> Option<crate::Command> {
        let c = self.clip(r.clip)?;
        let mut f = c.as_takes()?.clone();
        f.take_mut(r.take)?.audio = audio;
        Some(crate::Command::SetClipContent {
            clip: r.clip,
            start: c.start,
            content: Box::new(crate::ClipContent::Takes(f)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(takes: usize, len: i64) -> TakeFolder {
        let mut f = TakeFolder::new(len);
        for i in 0..takes {
            f.add_take(Take::plain(
                format!("Take {}", i + 1),
                AudioSourceId(i as u64 + 1),
                0,
                0,
                len,
            ));
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
        f.takes[1] = f.takes[1].part(400, 1000).unwrap();
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
        f.takes[0].audio.length = 500;
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
        assert_eq!(f.takes[0].audio.source_offset, 0, "it plays what it did");
        assert_eq!((f.takes[0].start, f.takes[0].end()), (100, 1100));
        assert_eq!(f.take_at(650), Some(1));
        assert_eq!(f.take_at(50), None);

        let (l, r) = f.split(700).unwrap();
        assert_eq!((l.length, r.length), (700, 450));
        assert_eq!(l.take_at(650), Some(1));
        assert_eq!(r.take_at(0), Some(1));
        assert_eq!(
            r.takes[1].audio.source_offset,
            f.takes[1].audio.source_offset + 600,
            "from 700 on: 600 frames into a take starting at 100"
        );
        assert_eq!((r.takes[1].start, r.takes[1].end()), (0, 400));
        assert!(f.split(0).is_none() && f.split(1150).is_none());
    }

    /// After a split, a take with no material in one half keeps its lane
    /// there; using it changes nothing (it used to silence the half).
    #[test]
    fn using_a_take_with_no_material_here_changes_nothing() {
        let mut f = folder(2, 1000);
        f.takes[1].start = 600;
        f.use_take(0);
        let (left, _) = f.split(500).unwrap();
        assert_eq!(
            left.takes[1].start,
            left.takes[1].end(),
            "empty in the left half"
        );
        let mut used = left.clone();
        used.use_take(1);
        assert_eq!(used, left);
        assert_eq!(used.take_at(100), Some(0), "still plays");
    }

    /// A click on a lane swaps the section under the pointer, never the
    /// whole comp (issue #20).
    #[test]
    fn a_click_uses_the_take_in_one_section() {
        let mut f = folder(3, 1000);
        f.use_take(0);
        f.set_comp(200, 400, Some(1));
        f.set_comp(600, 800, Some(2));
        f.use_take_at(300, 2);
        assert_eq!(
            f.segments().collect::<Vec<_>>(),
            vec![
                (0, 200, Some(0)),
                (200, 400, Some(2)),
                (400, 600, Some(0)),
                (600, 800, Some(2)),
                (800, 1000, Some(0))
            ]
        );
        // The take already playing there: nothing changes.
        let before = f.clone();
        f.use_take_at(250, 2);
        assert_eq!(f, before);
        // A neighbour's take merges the sections.
        f.use_take_at(500, 2);
        assert_eq!(
            f.segments().collect::<Vec<_>>(),
            vec![(0, 200, Some(0)), (200, 800, Some(2)), (800, 1000, Some(0))]
        );
        // The folder's end belongs to the last section.
        f.use_take_at(1000, 1);
        assert_eq!(f.take_at(900), Some(1));
        assert_eq!(f.take_at(100), Some(0));
        // Nothing comped yet: the one section is the whole folder.
        let mut g = folder(2, 1000);
        g.use_take_at(10, 1);
        assert_eq!(g.segments().collect::<Vec<_>>(), vec![(0, 1000, Some(1))]);
    }

    #[test]
    fn a_click_where_the_take_has_no_material_changes_nothing() {
        let mut f = folder(2, 1000);
        f.takes[1].start = 600;
        f.use_take(0);
        f.set_comp(200, 500, None);
        let before = f.clone();
        f.use_take_at(300, 1);
        assert_eq!(f, before, "take 2 starts at 600");
        f.use_take_at(300, 7);
        assert_eq!(f, before, "no such take");
    }

    /// What the comp plays: the takes cut at its boundaries, crossfading
    /// over each, the folder's fades at its edges and its gain in all.
    #[test]
    fn the_comp_plays_its_takes_cut_at_its_boundaries() {
        let mut f = folder(2, 1000);
        f.crossfade = 100;
        f.gain_db = -3.0;
        f.fades.fade_in = 20;
        f.use_take(0);
        f.set_comp(500, 800, Some(1));
        f.takes[1].audio.source_offset = 7_000;
        let played = f.played();
        let shape: Vec<_> = played
            .iter()
            .map(|p| {
                let a = &p.audio;
                (
                    p.start,
                    p.take,
                    a.source_offset,
                    a.length,
                    a.fades.fade_in,
                    a.fades.fade_out,
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (0, 0, 0, 550, 20, 100),
                (450, 1, 7_450, 400, 100, 100),
                (750, 0, 750, 250, 100, 0),
            ]
        );
        assert!(played.iter().all(|p| p.audio.gain_db == -3.0));
        // Auditioned: take 1 alone, a declick where it meets nothing.
        let alone = f.played_take(1);
        assert_eq!(alone.len(), 1);
        assert_eq!((alone[0].start, alone[0].audio.length), (0, 1000));
        assert_eq!(alone[0].audio.fades.fade_in, 20, "the folder's fade");
    }

    /// A take is a clip of its own: where the comp plays it, it plays with
    /// its warp, envelope, transposition and fades.
    #[test]
    fn a_take_plays_its_edits_where_the_comp_plays_it() {
        use crate::clip_envelope::{ClipEnvelope, EnvelopePoint};
        let mut f = folder(2, 1000);
        f.crossfade = 100;
        let a = &mut f.takes[1].audio;
        a.stretch = crate::StretchSettings::pitch(-12.0);
        let mut w = crate::Warp::uniform(1000);
        w.markers.push(crate::WarpMarker {
            at: 600,
            source: 400,
        });
        a.warp = Some(w);
        a.envelope = Some(Box::new(ClipEnvelope {
            volume: vec![EnvelopePoint::new(0, 0.0), EnvelopePoint::new(1000, -12.0)],
            pan: Vec::new(),
        }));
        a.fades.fade_out = 300;
        f.use_take(0);
        f.set_comp(500, 1000, Some(1));
        let played = f.played();
        let take = &f.takes[1];
        let piece = played.iter().find(|p| p.take == 1).unwrap();
        assert_eq!((piece.start, piece.audio.length), (450, 550));
        assert_eq!(piece.audio.stretch.semitones(), -12.0);
        // The same source frame at every folder frame, through its warp.
        for x in [450, 600, 799, 999] {
            let cut = Take {
                start: piece.start,
                audio: piece.audio.clone(),
                ..take.clone()
            };
            assert!(
                (cut.source_at(x) - take.source_at(x)).abs() < 1.0,
                "{x}: {} vs {}",
                cut.source_at(x),
                take.source_at(x)
            );
        }
        // Its envelope from where the piece starts.
        let e = piece.audio.envelope.as_ref().unwrap();
        assert_eq!(e.volume.first().map(|p| p.at), Some(0));
        let v = e.volume.first().unwrap().value;
        assert!(v < -1.0 && v > -12.0, "{v}: on its way down");
        // Its own fade-out where it ends (longer than the folder's none),
        // the crossfade where the comp cuts it.
        assert_eq!(piece.audio.fades.fade_in, 100);
        assert_eq!(piece.audio.fades.fade_out, 300);
    }

    /// A reversed take cut anywhere plays the source frames it played:
    /// its earlier output is its later source.
    #[test]
    fn a_reversed_take_cut_plays_what_it_played() {
        let mut f = folder(1, 1000);
        f.takes[0].audio.source_offset = 2_000;
        f.takes[0].audio.reversed = true;
        let take = f.takes[0].clone();
        assert_eq!(take.source_at(0), 2_999.0);
        assert_eq!(take.source_at(999), 2_000.0);
        let (l, r) = f.split(300).unwrap();
        let (lt, rt) = (&l.takes[0], &r.takes[0]);
        assert_eq!(lt.source_at(0), take.source_at(0));
        assert_eq!(lt.source_at(299), take.source_at(299));
        assert_eq!(rt.source_at(0), take.source_at(300));
        assert_eq!(rt.source_at(699), take.source_at(999));
        for x in [0, 299, 512, 999] {
            assert_eq!(take.folder_of(take.source_at(x) as i64), x);
        }
    }

    /// Splitting a folder cuts a warped take where the split is: each half
    /// plays the source the whole did there.
    #[test]
    fn a_split_cuts_a_warped_take_where_it_is() {
        let mut f = folder(1, 1000);
        let mut w = crate::Warp::uniform(1000);
        w.markers.push(crate::WarpMarker {
            at: 250,
            source: 500,
        });
        f.takes[0].audio.warp = Some(w);
        let take = f.takes[0].clone();
        let (l, r) = f.split(600).unwrap();
        for x in [0, 100, 250, 599] {
            assert!((l.takes[0].source_at(x) - take.source_at(x)).abs() < 1.0);
        }
        for x in [600, 700, 999] {
            assert!((r.takes[0].source_at(x - 600) - take.source_at(x)).abs() < 1.0);
        }
    }

    /// Taken into another folder, a folder's takes carry its gain and the
    /// fades at its edges.
    #[test]
    fn a_folders_gain_and_edge_fades_go_into_its_takes() {
        let mut f = folder(2, 1000);
        f.gain_db = -2.0;
        f.fades.fade_in = 200;
        f.fades.fade_out = 50;
        f.takes[1] = f.takes[1].part(300, 1000).unwrap();
        let takes = f.takes_as_played();
        assert!(takes.iter().all(|t| t.audio.gain_db == -2.0));
        assert_eq!(takes[0].audio.fades.fade_in, 200);
        assert_eq!(takes[0].audio.fades.fade_out, 50);
        assert_eq!(takes[1].audio.fades.fade_in, 0, "it begins after the fade");
        assert_eq!(takes[1].audio.fades.fade_out, 50);
    }

    /// A take's id is given once by its folder and stays its own: removing
    /// an earlier take moves its index, not its id, and a removed take's id
    /// is never given again.
    #[test]
    fn take_ids_are_given_once_and_kept() {
        let mut f = folder(3, 1000);
        let ids: Vec<u32> = f.takes.iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        f.use_take(2);
        f.remove_take(2);
        f.remove_take(0);
        assert_eq!(f.index_of(TakeId(2)), Some(0), "take 2 moved up");
        assert_eq!(f.take(TakeId(1)), None);
        let k = f.add_take(Take::plain("New", AudioSourceId(9), 0, 0, 1000));
        assert_eq!(f.takes[k].id, TakeId(4), "never 1 or 3 again");
        // Splits, parts and extensions keep them.
        f.extend(100, 0);
        let (l, r) = f.split(500).unwrap();
        for half in [&l, &r] {
            let ids: Vec<u32> = half.takes.iter().map(|t| t.id.0).collect();
            assert_eq!(ids, vec![2, 4]);
            assert_eq!(half.next_take, 5);
        }
        // A take given no id (built by hand), or one given twice, gets
        // one of its own.
        let mut g = folder(2, 1000);
        g.takes
            .push(Take::plain("Pushed", AudioSourceId(7), 0, 0, 1000));
        g.takes[1].id = TakeId(1);
        g.normalize();
        let ids: Vec<u32> = g.takes.iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec![1, 3, 4]);
        assert_eq!(g.next_take, 5);
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

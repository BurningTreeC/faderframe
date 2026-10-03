//! Elastic audio edits: warp markers, transient warping, quantizing,
//! time-compression trims and separating at transients.
//!
//! Drags arrive as a stream of absolute edits inside one gesture; every
//! edit recomputes from the clip as it was when the gesture first touched
//! it ([`Session::gesture_clip`]), so dragging back restores exactly what
//! was there.

use crate::editing::{ClipEdge, EditMode};
use crate::{Result, Session, SessionError};
use faderframe_core::ClipId;
use faderframe_project::{AudioClip, Clip, ClipContent, Command, Warp, WarpAlgorithm, WarpMarker};
use faderframe_timeline::{MusicalTime, snap_nearest};

/// How a warp drag treats the audio around the dragged point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpDrag {
    /// Stretch up to the neighbouring warp markers (or the clip edges).
    Free,
    /// Pin the neighbouring transients first, so only the audio between
    /// them moves (moving one hit).
    Transients,
    /// Pin these source frames first (a range warp inside a selection).
    Range { from: i64, to: i64 },
    /// Telescoping: everything after the point moves along; the clip gets
    /// longer or shorter.
    Telescope,
}

/// Shortest clip a stretch or warp leaves (frames).
const MIN_FRAMES: i64 = 64;

fn audio(c: &Clip) -> Result<AudioClip> {
    match &c.content {
        ClipContent::Audio(a) => Ok(a.clone()),
        _ => Err(SessionError::Other("only audio clips can be warped".into())),
    }
}

/// The warp map of `a` (an identity map when it has none).
fn warp_of(a: &AudioClip) -> Warp {
    a.warp.clone().unwrap_or_else(|| Warp::uniform(a.length))
}

impl Session {
    /// The clip as the running gesture first saw it (recorded on first
    /// use), or the current clip outside gestures.
    pub(crate) fn gesture_clip(&mut self, clip: ClipId) -> Result<Clip> {
        let current = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        if !self.history.in_gesture() {
            self.gesture_base.clear();
            return Ok(current);
        }
        Ok(self.gesture_base.entry(clip).or_insert(current).clone())
    }

    fn set_audio(&mut self, label: &str, c: &Clip, start: MusicalTime, a: AudioClip) -> Result<()> {
        self.batch(
            label,
            vec![Command::SetClipContent {
                clip: c.id,
                start,
                content: Box::new(ClipContent::Audio(a)),
            }],
        )
    }

    /// Pin source frame `source` of `clip` at clip-relative output frame
    /// `to` (adding a warp marker there if none exists).
    pub fn warp_to(&mut self, clip: ClipId, source: i64, to: i64, drag: WarpDrag) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let mut a = audio(&c)?;
        let (off, mut len) = (a.source_offset, a.length);
        let mut w = warp_of(&a);
        if !w.source_range(off).contains(&source) {
            return Ok(());
        }
        let pin = |w: &mut Warp, s: i64, len: i64| {
            if w.source_range(off).contains(&s)
                && s != off
                && !w.markers.iter().any(|m| m.source == s)
            {
                let at = w.output_of(off, len, s);
                w.markers.push(WarpMarker { at, source: s });
                w.normalize(off, len);
            }
        };
        match drag {
            WarpDrag::Transients => {
                let hits = self.source_transients(a.source).unwrap_or_default();
                let margin = (self.project.sample_rate / 200) as i64;
                if let Some(&prev) = hits.iter().rev().find(|&&h| h < source - margin) {
                    pin(&mut w, prev, len);
                }
                if let Some(&next) = hits.iter().find(|&&h| h > source + margin) {
                    pin(&mut w, next, len);
                }
            }
            WarpDrag::Range { from, to } => {
                pin(&mut w, from, len);
                pin(&mut w, to, len);
            }
            WarpDrag::Free | WarpDrag::Telescope => {}
        }
        let existing = w.markers.iter().position(|m| m.source == source);
        let current = existing.map_or_else(|| w.output_of(off, len, source), |i| w.markers[i].at);
        if let Some(i) = existing {
            w.markers.remove(i);
        }
        let to = if drag == WarpDrag::Telescope {
            // Later markers and the clip end move along.
            let delta = (to - current).max(MIN_FRAMES - len);
            for m in &mut w.markers {
                if m.source > source {
                    m.at += delta;
                }
            }
            len += delta;
            (current + delta).max(1)
        } else {
            // Strictly between the neighbouring anchors.
            let prev = w
                .markers
                .iter()
                .filter(|m| m.source < source)
                .map(|m| m.at)
                .max()
                .unwrap_or(0);
            let next = w
                .markers
                .iter()
                .filter(|m| m.source > source)
                .map(|m| m.at)
                .min()
                .unwrap_or(len);
            if next - prev < 3 {
                return Ok(());
            }
            to.clamp(prev + 1, next - 1)
        };
        w.markers.push(WarpMarker { at: to, source });
        w.normalize(off, len);
        a.length = len;
        a.fades.fade_out = a.fades.fade_out.min(len);
        a.fades.fade_in = a.fades.fade_in.min(len - a.fades.fade_out);
        a.warp = Some(w);
        self.set_audio("Warp", &c, c.start, a)
    }

    /// Remove the warp marker pinning `source`.
    pub fn remove_warp_marker(&mut self, clip: ClipId, source: i64) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let mut a = audio(&c)?;
        let Some(mut w) = a.warp.clone() else {
            return Ok(());
        };
        w.markers.retain(|m| m.source != source);
        a.warp = Some(w);
        self.set_audio("Remove Warp Marker", &c, c.start, a)
    }

    /// Move the clip's transients to the nearest grid lines.
    pub fn quantize_warp(&mut self, clip: ClipId) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let mut a = audio(&c)?;
        let (off, len) = (a.source_offset, a.length);
        let w = warp_of(&a);
        let hits = self.source_transients(a.source).ok_or_else(|| {
            SessionError::Other("transients are still being detected — try again shortly".into())
        })?;
        let p = &self.project;
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(c.start, rate);
        let mut q = Warp {
            source_length: w.source_length,
            markers: Vec::new(),
            algorithm: w.algorithm,
        };
        let mut last = 0;
        for s in hits
            .into_iter()
            .filter(|s| w.source_range(off).contains(s) && *s > off)
        {
            let out = w.output_of(off, len, s);
            let t = p.timeline.to_musical(base + out, rate);
            let snapped = snap_nearest(t, self.editor.grid, &p.timeline.meter);
            let at = p.timeline.to_samples(snapped, rate) - base;
            if at > last && at < len {
                q.markers.push(WarpMarker { at, source: s });
                last = at;
            }
        }
        q.normalize(off, len);
        a.warp = Some(q);
        self.set_audio("Quantize Transients", &c, c.start, a)
    }

    /// Play the clip's audio unwarped again (at its original length).
    pub fn clear_warp(&mut self, clip: ClipId) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let mut a = audio(&c)?;
        let Some(w) = a.warp.take() else {
            return Ok(());
        };
        a.length = w.source_length.max(1);
        a.fades.fade_out = a.fades.fade_out.min(a.length);
        a.fades.fade_in = a.fades.fade_in.min(a.length - a.fades.fade_out);
        self.set_audio("Remove Warp", &c, c.start, a)
    }

    pub fn set_warp_algorithm(&mut self, clip: ClipId, algorithm: WarpAlgorithm) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let mut a = audio(&c)?;
        let mut w = warp_of(&a);
        w.algorithm = algorithm;
        a.warp = Some(w);
        self.set_audio("Warp Algorithm", &c, c.start, a)
    }

    /// Time-compress or expand a clip by moving one edge: the audio (or the
    /// notes) keep their content and change speed.
    pub fn stretch_clip(&mut self, clip: ClipId, edge: ClipEdge, to: MusicalTime) -> Result<()> {
        let cmds = self.stretch_commands(clip, edge, to)?;
        self.batch("Time Stretch", cmds)
    }

    pub(crate) fn stretch_commands(
        &mut self,
        clip: ClipId,
        edge: ClipEdge,
        to: MusicalTime,
    ) -> Result<Vec<Command>> {
        let c = self.gesture_clip(clip)?;
        let end = self.clip_end(&c);
        let min = MusicalTime::from_quarters(1.0 / 64.0);
        let (start, new_end) = match edge {
            ClipEdge::Start => (to.min(end - min).max(MusicalTime::ZERO), end),
            ClipEdge::End => (c.start, to.max(c.start + min)),
        };
        let content = match &c.content {
            ClipContent::Audio(a) => {
                let mut a = a.clone();
                let len = self.frames_between(start, new_end).max(MIN_FRAMES);
                a.warp = Some(warp_of(&a).scaled(a.length, len));
                // Fades keep their share of the clip.
                let f = len as f64 / a.length.max(1) as f64;
                a.fades.fade_in = (a.fades.fade_in as f64 * f) as i64;
                a.fades.fade_out = (a.fades.fade_out as f64 * f) as i64;
                a.length = len;
                ClipContent::Audio(a)
            }
            ClipContent::Midi(m) => {
                let f = (new_end - start).ticks() as f64 / m.length.ticks().max(1) as f64;
                ClipContent::Midi(m.scaled(f))
            }
            ClipContent::Takes(_) => {
                return Err(SessionError::Other(
                    "take folders cannot be stretched; flatten the comp first".into(),
                ));
            }
        };
        let mut cmds = vec![Command::SetClipContent {
            clip,
            start,
            content: Box::new(content),
        }];
        if self.editor.edit_mode == EditMode::Shuffle && edge == ClipEdge::End {
            cmds.extend(self.shuffle_later(&c, end, new_end)?);
        }
        Ok(cmds)
    }

    /// Moves of the clips after `c` on its track (as the gesture first saw
    /// them) when its end moves from `old_end` to `new_end`.
    pub(crate) fn shuffle_later(
        &mut self,
        c: &Clip,
        old_end: MusicalTime,
        new_end: MusicalTime,
    ) -> Result<Vec<Command>> {
        let ids: Vec<ClipId> = self
            .project
            .clips_of(c.track)
            .into_iter()
            .map(|o| o.id)
            .filter(|id| *id != c.id)
            .collect();
        let mut cmds = Vec::new();
        for id in ids {
            let o = self.gesture_clip(id)?;
            if o.start >= old_end {
                let start = if new_end >= old_end {
                    o.start + (new_end - old_end)
                } else {
                    o.start - (old_end - new_end)
                };
                cmds.push(Command::MoveClip {
                    clip: id,
                    track: o.track,
                    start: start.max(MusicalTime::ZERO),
                });
            }
        }
        Ok(cmds)
    }

    /// Split an audio clip at each of its transients.
    pub fn separate_at_transients(&mut self, clip: ClipId) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        audio(&c)?;
        if self
            .source_transients(match &c.content {
                ClipContent::Audio(a) => a.source,
                _ => return Ok(()),
            })
            .is_none()
        {
            return Err(SessionError::Other(
                "transients are still being detected — try again shortly".into(),
            ));
        }
        let p = &self.project;
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(c.start, rate);
        let len = self.frames_between(c.start, self.clip_end(&c));
        let margin = (p.sample_rate / 100) as i64;
        let cuts: Vec<MusicalTime> = self
            .clip_transient_frames(&c)
            .into_iter()
            .filter(|f| *f > margin && *f < len - margin)
            .map(|f| p.timeline.to_musical(base + f, rate))
            .collect();
        // Split right to left so the clip being split stays `clip`.
        let mut cmds = Vec::new();
        for at in cuts.into_iter().rev() {
            let new_clip: ClipId = self.project.ids.allocate();
            cmds.push(Command::SplitClip { clip, at, new_clip });
        }
        self.batch("Separate at Transients", cmds)
    }
}

/// Warp-aware trimming of an audio clip's start by `delta` output frames
/// (negative: extend to the left, at the original speed).
pub(crate) fn trim_audio_start(a: &mut AudioClip, delta: i64) {
    match a.warp.take() {
        Some(w) if delta >= 0 => {
            let delta = delta.min(a.length - 1);
            let (src, right) = w.after(a.source_offset, a.length, delta);
            a.source_offset = src;
            a.length -= delta;
            a.warp = Some(right);
        }
        Some(mut w) => {
            let e = (-delta).min(a.source_offset);
            for m in &mut w.markers {
                m.at += e;
            }
            w.markers.insert(
                0,
                WarpMarker {
                    at: e,
                    source: a.source_offset,
                },
            );
            a.source_offset -= e;
            w.source_length += e;
            a.length += e;
            w.normalize(a.source_offset, a.length);
            a.warp = Some(w);
        }
        None => {
            let delta = delta.max(-a.source_offset).min(a.length - 1);
            a.source_offset += delta;
            a.length -= delta;
        }
    }
}

/// Warp-aware trimming of an audio clip's end to `len` output frames
/// (longer: extend at the original speed, up to `source_frames`).
pub(crate) fn trim_audio_end(a: &mut AudioClip, len: i64, source_frames: i64) {
    let len = len.max(1);
    match a.warp.take() {
        Some(w) if len <= a.length => {
            a.warp = Some(w.before(a.source_offset, a.length, len));
            a.length = len;
        }
        Some(mut w) => {
            let room = source_frames - (a.source_offset + w.source_length);
            let e = (len - a.length).min(room.max(0));
            w.markers.push(WarpMarker {
                at: a.length,
                source: a.source_offset + w.source_length,
            });
            w.source_length += e;
            a.length += e;
            w.normalize(a.source_offset, a.length);
            a.warp = Some(w);
        }
        None => {
            a.length = len.min(source_frames - a.source_offset).max(1);
        }
    }
}

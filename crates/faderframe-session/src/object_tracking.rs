//! Panning that follows the picture: an object boxed on one frame of a
//! video clip is followed through the clip's frames, and its path becomes
//! a track's pan or surround position automation — one undo step.
//!
//! * **Keys.** The box the user draws ([`FollowOp::Key`]) is a key: the
//!   object followed from there to the next key (or the clip's last
//!   frame) on a job thread ([`faderframe_video::track`]: template
//!   tracking on the frames decoded in order at a small size, from the
//!   proxy when there is one). Another box is a correction: the stretch
//!   from the key before keeps what was followed up to it (bent onto the
//!   new box, [`faderframe_video::track::bend`], so the path is
//!   continuous), and the object is followed afresh from it. Frames where
//!   the match was poor are marked lost; a key there puts it right.
//! * **Nothing in the project** until the path is applied: keys and path
//!   are the session's (like a selection), gone with another project.
//! * **Applied** ([`FollowOp::Apply`]): every frame of the path within
//!   the clip, lightly smoothed (a centred mean over five frames, fewer
//!   towards the ends), becomes
//!   a point at the frame's start on the timeline; the picture's
//!   horizontal place (−1 at the left edge, 1 at the right) drives what
//!   places the track: its pan (a stereo or mono destination), both sides
//!   of a dual pan, its room position X in a surround bed or as an object
//!   ([`faderframe_automation::AutomationTarget::Surround`]), or its
//!   VBAP azimuth (the picture's width spanning the front speakers, ±30°).
//!   With depth, the box's size moves a room-panned track front to back:
//!   twice the size at the first key comes `depth` nearer the back. The
//!   points are thinned and replace the lanes' stretch the path covers
//!   (`AutomationCurve::replace_range`), new lanes made where there are
//!   none; all in one batch, "Follow Object".

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::{SurroundParam, TrackId, VideoClipId, VideoSourceId};
use faderframe_project::Command;
use faderframe_project::video::ns_to_samples;
use faderframe_video::track::{Area, FollowJob, Followed, bend, follow_in_file};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The label of the following jobs.
pub const JOB_LABEL: &str = "Following the object";
/// Frames are decoded for following at most this size.
pub const FOLLOW_SIZE: (u32, u32) = (480, 270);
/// The VBAP azimuth at the picture's edges (degrees).
pub const PICTURE_AZIMUTH: f32 = 30.0;
/// Depth when it is asked for without an amount.
pub const DEFAULT_DEPTH: f32 = 0.5;

/// What following an object does.
#[derive(Clone, Debug, PartialEq)]
pub enum FollowOp {
    /// The object is in `area` (shares of the picture) on frame `frame`
    /// of `clip`'s file: the first key starts following, later ones
    /// correct it.
    Key {
        clip: VideoClipId,
        frame: usize,
        area: Area,
    },
    /// Take a correction back (the stretch before it is followed on).
    RemoveKey { frame: usize },
    /// Write the path as automation of `track` (as soon as it is followed
    /// through); `depth`: the box's size moves a room-panned track front
    /// to back by this much per doubling.
    Apply { track: TrackId, depth: Option<f32> },
    /// Forget the object.
    Forget,
    /// The video window's box tool: a drag on the picture draws a key
    /// (not an edit; off again once a box is drawn).
    Draw(bool),
}

/// What a track's automation follows the object with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowTarget {
    /// Its pan (balance for a stereo source).
    Pan,
    /// Both sides of its dual pan, their spread kept.
    DualPan,
    /// Its room position (X, and Y for depth): surround bed or object.
    Room,
    /// Its VBAP azimuth.
    Azimuth,
}

impl FollowTarget {
    pub fn label(self) -> &'static str {
        match self {
            FollowTarget::Pan => "Pan",
            FollowTarget::DualPan => "Dual Pan",
            FollowTarget::Room => "Surround Position",
            FollowTarget::Azimuth => "Surround Azimuth",
        }
    }

    /// Whether the box's size can move it front to back.
    pub fn has_depth(self) -> bool {
        self == FollowTarget::Room
    }
}

/// How a frame's box was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// Drawn by the user.
    Key,
    /// Followed.
    Followed,
    /// The match was poor (guessed): a key here would put it right.
    Lost,
}

/// The object followed, as the views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct FollowStatus {
    pub clip: VideoClipId,
    pub keys: Vec<usize>,
    /// Frames with a place, and of them lost.
    pub followed: usize,
    pub lost: usize,
    /// The frames from the first key to the clip's last.
    pub span: (usize, usize),
    /// Still following.
    pub busy: bool,
    /// Applied when done to this track.
    pub applying: Option<TrackId>,
}

/// A stretch followed by a job: from key `key` on.
pub(crate) struct FollowedStretch {
    pub(crate) generation: u64,
    pub(crate) key: usize,
    pub(crate) path: Vec<(usize, Followed)>,
}

/// The session's following state (kept in the video state).
#[derive(Default)]
pub(crate) struct TrackingState {
    object: Option<Object>,
    /// Applied once followed through.
    apply: Option<(TrackId, Option<f32>)>,
    next_generation: u64,
    /// The box tool is on.
    drawing: bool,
}

struct Object {
    clip: VideoClipId,
    source: VideoSourceId,
    keys: BTreeMap<usize, Area>,
    /// What was followed from each key (frames from the key, the key's own
    /// first).
    stretches: BTreeMap<usize, Vec<(usize, Followed)>>,
    /// The jobs following from keys: their generation and cancel flag.
    jobs: BTreeMap<usize, (u64, Arc<AtomicBool>)>,
    /// The clip's frames.
    first: usize,
    last: usize,
}

impl Object {
    /// The key at or before `frame`, and the one after it.
    fn around(&self, frame: usize) -> (Option<usize>, Option<usize>) {
        (
            self.keys.range(..=frame).next_back().map(|(k, _)| *k),
            self.keys.range(frame + 1..).next().map(|(k, _)| *k),
        )
    }

    /// The stretch followed from key `k`, bent onto the next key when it
    /// reaches it: `(frame, where, how)`.
    fn stretch(&self, k: usize) -> Vec<(usize, Area, Seen)> {
        let Some(stretch) = self.stretches.get(&k) else {
            return Vec::new();
        };
        let next = self.keys.range(k + 1..).next();
        let through = next.is_some_and(|(n, _)| stretch.last().is_some_and(|(f, _)| f + 1 >= *n));
        let steps: Vec<Followed> = stretch.iter().map(|(_, f)| *f).collect();
        let bent = bend(&steps, next.filter(|_| through).map(|(_, a)| *a));
        stretch
            .iter()
            .zip(bent)
            .map(|((frame, f), area)| {
                let seen = if *frame == k {
                    Seen::Key
                } else if f.lost() {
                    Seen::Lost
                } else {
                    Seen::Followed
                };
                (*frame, area, seen)
            })
            .collect()
    }

    /// The path: every frame followed; `(frame, where, how)`.
    fn path(&self) -> BTreeMap<usize, (Area, Seen)> {
        let mut out = BTreeMap::new();
        for &k in self.stretches.keys() {
            for (frame, area, seen) in self.stretch(k) {
                out.insert(frame, (area, seen));
            }
        }
        for (&k, a) in &self.keys {
            out.insert(k, (*a, Seen::Key));
        }
        out
    }
}

/// The values a place gives (the picture's centre `x` in shares, the
/// box's size against the first key's).
fn pan_of(x: f32) -> f32 {
    (2.0 * x - 1.0).clamp(-1.0, 1.0)
}

impl Session {
    /// The object followed, if any.
    pub fn follow_status(&self) -> Option<FollowStatus> {
        let t = &self.video.tracking;
        let o = t.object.as_ref()?;
        let path = o.path();
        Some(FollowStatus {
            clip: o.clip,
            keys: o.keys.keys().copied().collect(),
            followed: path.len(),
            lost: path.values().filter(|(_, s)| *s == Seen::Lost).count(),
            span: (o.keys.keys().next().copied().unwrap_or(o.first), o.last),
            busy: !o.jobs.is_empty(),
            applying: t.apply.map(|(track, _)| track),
        })
    }

    /// Where the object is on frame `frame` of `clip`'s file.
    pub fn followed_box(&self, clip: VideoClipId, frame: usize) -> Option<(Area, Seen)> {
        let o = self.video.tracking.object.as_ref()?;
        if o.clip != clip {
            return None;
        }
        if let Some(a) = o.keys.get(&frame) {
            return Some((*a, Seen::Key));
        }
        let (key, _) = o.around(frame);
        let stretch = o.stretch(key?);
        let i = stretch.binary_search_by_key(&frame, |(f, _, _)| *f).ok()?;
        Some((stretch[i].1, stretch[i].2))
    }

    /// Whether the video window's box tool is on.
    pub fn follow_drawing(&self) -> bool {
        self.video.tracking.drawing
    }

    /// What `track`'s automation follows an object with (`None`: it
    /// cannot: no audio, or the master).
    pub fn follow_target(&self, track: TrackId) -> Option<FollowTarget> {
        let p = &self.project;
        let t = p.track(track)?;
        if !t.kind.has_audio()
            || t.kind == faderframe_project::TrackKind::Master
            || !crate::automation::has_automation(t.kind)
        {
            return None;
        }
        if p.surround_panned(t).is_some() && !p.balanced_into_bed(t) {
            return Some(match p.effective_surround(t).panner {
                faderframe_core::surround::SurroundPanner::Vbap => FollowTarget::Azimuth,
                faderframe_core::surround::SurroundPanner::Room => FollowTarget::Room,
            });
        }
        if p.dual_panned(t) {
            return Some(FollowTarget::DualPan);
        }
        Some(FollowTarget::Pan)
    }

    /// The tracks a path can be applied to, in the editors' order.
    pub fn follow_tracks(&self) -> Vec<(TrackId, String, FollowTarget)> {
        self.project
            .folder_order()
            .into_iter()
            .filter_map(|t| Some((t.id, t.name.clone(), self.follow_target(t.id)?)))
            .collect()
    }

    pub(crate) fn follow_op(&mut self, op: FollowOp) -> Result<()> {
        match op {
            FollowOp::Key { clip, frame, area } => {
                self.video.tracking.drawing = false;
                self.follow_key(clip, frame, area)
            }
            FollowOp::Draw(on) => {
                self.video.tracking.drawing = on;
                self.revision += 1;
                Ok(())
            }
            FollowOp::RemoveKey { frame } => {
                let o = self
                    .video
                    .tracking
                    .object
                    .as_mut()
                    .ok_or_else(|| SessionError::Other("no object is followed".into()))?;
                if o.keys.len() <= 1 || o.keys.remove(&frame).is_none() {
                    return Err(SessionError::Other(
                        "only a correction can be taken back (Forget the object instead)".into(),
                    ));
                }
                o.stretches.remove(&frame);
                if let Some((_, (_, c))) = o.jobs.remove_entry(&frame) {
                    c.store(true, Ordering::Relaxed);
                }
                // The stretch before follows on through where it stood.
                if let (Some(before), _) = o.around(frame) {
                    self.follow_from(before);
                }
                self.revision += 1;
                Ok(())
            }
            FollowOp::Apply { track, depth } => {
                let busy = match &self.video.tracking.object {
                    None => return Err(SessionError::Other("no object is followed".into())),
                    Some(o) => !o.jobs.is_empty(),
                };
                self.follow_target(track).ok_or_else(|| {
                    SessionError::Other("that track has no pan to follow an object with".into())
                })?;
                if busy {
                    self.video.tracking.apply = Some((track, depth));
                    self.revision += 1;
                    Ok(())
                } else {
                    self.apply_object_path(track, depth)
                }
            }
            FollowOp::Forget => {
                self.forget_following();
                self.revision += 1;
                Ok(())
            }
        }
    }

    fn follow_key(&mut self, clip: VideoClipId, frame: usize, area: Area) -> Result<()> {
        if !area.is_valid() {
            return Err(SessionError::Other("draw a box on the picture".into()));
        }
        let (c, source) = self
            .project
            .video
            .clip(clip)
            .map(|(_, c)| (c.clone(), c.source))
            .ok_or_else(|| SessionError::Other("no such video clip".into()))?;
        let index = self
            .video_index(source)
            .ok_or_else(|| SessionError::Other("the video is still being read".into()))?;
        let first = index.frame_at(c.offset).unwrap_or(0);
        let last = index
            .last_before(c.offset + c.length)
            .unwrap_or(index.len().saturating_sub(1));
        if frame < first || frame > last {
            return Err(SessionError::Other(format!(
                "frame {frame} is not in the clip (frames {first}–{last})"
            )));
        }
        let t = &mut self.video.tracking;
        // Another clip: a new object.
        if t.object.as_ref().is_some_and(|o| o.clip != clip) {
            self.forget_following();
        }
        let t = &mut self.video.tracking;
        let o = t.object.get_or_insert_with(|| Object {
            clip,
            source,
            keys: BTreeMap::new(),
            stretches: BTreeMap::new(),
            jobs: BTreeMap::new(),
            first,
            last,
        });
        o.keys.insert(frame, area);
        // The stretch it falls in keeps what came before it (a job still
        // following that stretch keeps what it finds before it too:
        // `followed`).
        if let (Some(before), _) = o.around(frame.saturating_sub(1))
            && before < frame
            && let Some(s) = o.stretches.get_mut(&before)
        {
            s.retain(|(f, _)| *f < frame);
        }
        self.follow_from(frame);
        self.revision += 1;
        Ok(())
    }

    /// Follow from key `key` to the next key (or the clip's end), afresh.
    fn follow_from(&mut self, key: usize) {
        let Some(o) = self.video.tracking.object.as_mut() else {
            return;
        };
        let Some(start) = o.keys.get(&key).copied() else {
            return;
        };
        if let Some((_, c)) = o.jobs.remove(&key) {
            c.store(true, Ordering::Relaxed);
        }
        o.stretches.remove(&key);
        let to = o.around(key).1.map_or(o.last, |n| n - 1);
        let source = o.source;
        let generation = self.video.tracking.next_generation;
        self.video.tracking.next_generation += 1;
        let Some(index) = self.video_index(source) else {
            return;
        };
        let Some(s) = self.project.video.sources.get(&source).cloned() else {
            return;
        };
        // From the proxy when there is one (all frames intra: fast).
        let (file, proxy, size) = match self.video_proxy(source) {
            Some((path, (w, h))) => {
                let fit = faderframe_video::fit(w, h, (1, 1), FOLLOW_SIZE.0, FOLLOW_SIZE.1);
                (path, true, fit)
            }
            None => (
                s.path.clone(),
                false,
                faderframe_video::fit(s.width, s.height, s.par, FOLLOW_SIZE.0, FOLLOW_SIZE.1),
            ),
        };
        let cancel = self.spawn_video(JOB_LABEL, Some(source), move |share, cancel| {
            let job = FollowJob {
                file: &file,
                proxy,
                index: &index,
                size,
                from: key,
                to,
                start,
            };
            let path = follow_in_file(&job, cancel, |x| share.set(x)).map_err(|e| e.to_string())?;
            Ok(crate::video::Done::Followed(Box::new(FollowedStretch {
                generation,
                key,
                path,
            })))
        });
        if let (Some(c), Some(o)) = (cancel, self.video.tracking.object.as_mut()) {
            o.jobs.insert(key, (generation, c));
        }
    }

    /// A stretch was followed (from a job).
    pub(crate) fn followed(&mut self, done: FollowedStretch) {
        let Some(o) = self.video.tracking.object.as_mut() else {
            return;
        };
        if o.jobs
            .get(&done.key)
            .is_none_or(|(g, _)| *g != done.generation)
        {
            return;
        }
        o.jobs.remove(&done.key);
        // A key added since cut it short.
        let end = o.around(done.key).1.unwrap_or(usize::MAX);
        let path: Vec<(usize, Followed)> =
            done.path.into_iter().filter(|(f, _)| *f < end).collect();
        o.stretches.insert(done.key, path);
        self.revision += 1;
    }

    /// A following job failed.
    pub(crate) fn follow_failed(&mut self, e: &str) {
        // Which stretch failed is not known: every job stops (the keys
        // stay; another key follows again).
        if let Some(o) = self.video.tracking.object.as_mut() {
            for (_, c) in o.jobs.values() {
                c.store(true, Ordering::Relaxed);
            }
            o.jobs.clear();
        }
        self.video.tracking.apply = None;
        self.notify(NoticeLevel::Error, format!("following the object: {e}"));
    }

    /// Apply a path asked for while it was followed (from the tick).
    pub(crate) fn tick_following(&mut self) {
        let t = &self.video.tracking;
        let ready = t.object.as_ref().is_some_and(|o| o.jobs.is_empty());
        if ready
            && let Some((track, depth)) = self.video.tracking.apply.take()
            && let Err(e) = self.apply_object_path(track, depth)
        {
            self.notify(NoticeLevel::Error, e.to_string());
        }
    }

    /// Forget the object (its jobs stop).
    pub(crate) fn forget_following(&mut self) {
        if let Some(o) = self.video.tracking.object.take() {
            for (_, c) in o.jobs.values() {
                c.store(true, Ordering::Relaxed);
            }
        }
        self.video.tracking.apply = None;
    }

    /// Write the path as `track`'s automation: one undo step.
    fn apply_object_path(&mut self, track: TrackId, depth: Option<f32>) -> Result<()> {
        let target = self
            .follow_target(track)
            .ok_or_else(|| SessionError::Other("that track has no pan to follow with".into()))?;
        let o = self
            .video
            .tracking
            .object
            .as_ref()
            .ok_or_else(|| SessionError::Other("no object is followed".into()))?;
        let (_, clip) = self
            .project
            .video
            .clip(o.clip)
            .ok_or_else(|| SessionError::Other("the video clip is gone".into()))?;
        let clip = clip.clone();
        let index = self
            .video_index(o.source)
            .ok_or_else(|| SessionError::Other("the video is still being read".into()))?;
        let path = o.path();
        if path.len() < 2 {
            return Err(SessionError::Other(
                "nothing followed yet: draw a box around the object".into(),
            ));
        }
        let rate = self.project.sample_rate;
        // Timeline places of the frames within the clip.
        let frames: Vec<(i64, Area)> = path
            .iter()
            .filter_map(|(&n, (a, _))| {
                let t = *index.times.get(n)?;
                (t >= clip.offset && t < clip.offset + clip.length)
                    .then(|| (clip.start + ns_to_samples(t - clip.offset, rate), *a))
            })
            .collect();
        if frames.len() < 2 {
            return Err(SessionError::Other("the path is not in the clip".into()));
        }
        let smooth = smoothed(&frames.iter().map(|(_, a)| *a).collect::<Vec<_>>());
        let reference = smooth
            .first()
            .map(|a| (a.w * a.h).sqrt())
            .unwrap_or(1.0)
            .max(1e-6);
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other("no such track".into()))?;
        let dual = t.dual_pan;
        let room = t.surround;
        let existing: Vec<AutomationLane> = t.automation.lanes.clone();
        let depth = depth.filter(|_| target.has_depth());
        // (target, value per frame, thinning tolerance)
        let mut lanes: Vec<(AutomationTarget, Vec<f64>, f64)> = Vec::new();
        let xs: Vec<f32> = smooth.iter().map(|a| pan_of(a.centre().0)).collect();
        match target {
            FollowTarget::Pan => {
                lanes.push((
                    AutomationTarget::TrackPan,
                    xs.iter().map(|v| f64::from(*v)).collect(),
                    0.004,
                ));
            }
            FollowTarget::DualPan => {
                let half = (dual.right - dual.left) / 2.0;
                lanes.push((
                    AutomationTarget::DualPanLeft,
                    xs.iter()
                        .map(|v| f64::from((v - half).clamp(-1.0, 1.0)))
                        .collect(),
                    0.004,
                ));
                lanes.push((
                    AutomationTarget::DualPanRight,
                    xs.iter()
                        .map(|v| f64::from((v + half).clamp(-1.0, 1.0)))
                        .collect(),
                    0.004,
                ));
            }
            FollowTarget::Room => {
                lanes.push((
                    AutomationTarget::Surround(SurroundParam::X),
                    xs.iter().map(|v| f64::from(*v)).collect(),
                    0.004,
                ));
                if let Some(amount) = depth {
                    let ys = smooth
                        .iter()
                        .map(|a| {
                            let grown = ((a.w * a.h).sqrt() / reference).max(1e-6).log2();
                            f64::from((room.y - amount * grown).clamp(-1.0, 1.0))
                        })
                        .collect();
                    lanes.push((AutomationTarget::Surround(SurroundParam::Y), ys, 0.004));
                }
            }
            FollowTarget::Azimuth => {
                lanes.push((
                    AutomationTarget::Surround(SurroundParam::Azimuth),
                    xs.iter().map(|v| f64::from(-v * PICTURE_AZIMUTH)).collect(),
                    0.15,
                ));
            }
        }
        let times: Vec<faderframe_timeline::MusicalTime> = frames
            .iter()
            .map(|(s, _)| self.engine.samples_to_musical(&self.project, *s))
            .collect();
        let (Some(&from), Some(&to)) = (times.first(), times.last()) else {
            return Ok(());
        };
        let mut commands = Vec::new();
        let mut shown = Vec::new();
        for (target, values, tolerance) in lanes {
            let points: Vec<AutomationPoint> = times
                .iter()
                .zip(&values)
                .map(|(t, v)| AutomationPoint {
                    time: *t,
                    value: *v,
                    shape: CurveShape::Linear,
                })
                .collect();
            let points = faderframe_automation::thin(&points, tolerance);
            match existing.iter().find(|l| l.target == target).cloned() {
                Some(mut lane) => {
                    lane.curve.replace_range(from, to, &points);
                    shown.push(lane.id);
                    commands.push(Command::SetAutomationLane {
                        track,
                        lane: Box::new(lane),
                    });
                }
                None => {
                    let id = self.project.ids.allocate();
                    shown.push(id);
                    commands.push(Command::AddAutomationLane {
                        track,
                        lane: Box::new(AutomationLane {
                            id,
                            target,
                            curve: AutomationCurve::from_points(points),
                            mode: AutomationMode::Read,
                            visible: true,
                        }),
                    });
                }
            }
        }
        let n = frames.len();
        self.edit(Command::Batch {
            label: "Follow Object".into(),
            commands,
        })?;
        for id in shown {
            self.workspace.automation_shown.insert(id);
        }
        let name = self
            .project
            .track(track)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        self.notify(
            NoticeLevel::Info,
            format!(
                "{name}'s {} follows the object ({n} frames)",
                target.label().to_lowercase()
            ),
        );
        Ok(())
    }

    /// Wait until the object is followed through (tests, scripts);
    /// whether it was.
    pub fn wait_for_following(&mut self, timeout: std::time::Duration) -> bool {
        let end = std::time::Instant::now() + timeout;
        loop {
            self.poll_video();
            let busy = self
                .video
                .tracking
                .object
                .as_ref()
                .is_some_and(|o| !o.jobs.is_empty())
                || self.video.tracking.apply.is_some();
            if !busy {
                return true;
            }
            if std::time::Instant::now() > end {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Boxes smoothed by a centred mean over five frames — fewer towards the
/// ends, always as many before as after (a steady move stays where it is,
/// to its last frame).
fn smoothed(boxes: &[Area]) -> Vec<Area> {
    let n = boxes.len();
    (0..n)
        .map(|i| {
            let r = 2.min(i).min(n - 1 - i);
            let window = &boxes[i - r..=i + r];
            let k = window.len() as f32;
            let sum = window.iter().fold((0.0, 0.0, 0.0, 0.0), |s, x| {
                let (cx, cy) = x.centre();
                (s.0 + cx, s.1 + cy, s.2 + x.w, s.3 + x.h)
            });
            Area::new(0.0, 0.0, sum.2 / k, sum.3 / k).at_centre(sum.0 / k, sum.1 / k)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_spans_the_pan() {
        assert_eq!(pan_of(0.0), -1.0);
        assert_eq!(pan_of(0.5), 0.0);
        assert_eq!(pan_of(1.0), 1.0);
        assert_eq!(pan_of(1.4), 1.0);
    }

    #[test]
    fn smoothing_keeps_a_steady_move() {
        let boxes: Vec<Area> = (0..10)
            .map(|i| Area::new(0.1 + 0.02 * i as f32, 0.3, 0.1, 0.1))
            .collect();
        let s = smoothed(&boxes);
        for (a, b) in s.iter().zip(&boxes) {
            assert!((a.x - b.x).abs() < 1e-5);
        }
        // A jitter is evened out.
        let mut jitter = boxes.clone();
        jitter[5].x += 0.05;
        let s = smoothed(&jitter);
        assert!((s[5].x - boxes[5].x).abs() < 0.011);
    }
}

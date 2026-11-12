//! Slicing, REX-style: audio cut where it hits (its transients), on the
//! grid, into equal parts or where the clips were cut by hand, and the
//! slices played by a sampler — a Drum Sampler pad each, or a Sampler in
//! slice mode (a key each) — with a MIDI clip that plays them in the
//! original rhythm.
//!
//! What is sliced is a span of an audio track rendered as its clips play it
//! (clip gain, fades, warp; no devices — [`Session::render_span`], shared
//! with [`crate::sampling`]), or the sample a Sampler or a Drum Sampler pad
//! plays. The render, the transient detection and writing the slices
//! (float WAVs in the media folder's `Samples`) run on a worker thread; the
//! session places the result in its tick as one undo step: the new track
//! (or the device's new state, [`Command::SetPluginState`]) and the MIDI
//! clip. With a MIDI clip the clips the span covers whole are muted (the
//! clip plays them now); undo brings everything back.
//!
//! Pads take one slice each (sixteen at most, or the kit's free pads):
//! slicing by transients keeps the strongest that fit, the other ways say
//! the slices do not fit. A Sampler takes up to [`MAX_SAMPLER_SLICES`].

use crate::samples;
use crate::sampling::{EDGE_FADE, SpanRender};
use crate::{NoticeLevel, Result, SelectMode, Session, SessionError, UiRequest};
use faderframe_audio_files::onsets;
use faderframe_audio_files::{Dither, WavFormat, write_wav_with};
use faderframe_core::{ClipId, ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_plugin_host::devices::samples::{Sample, SampleDoc};
use faderframe_plugin_host::devices::{drums, sampler};
use faderframe_project::{
    Clip, ClipContent, Command, MidiClip, MidiNote, PluginRef, PluginSlot, Track, TrackColor,
    TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

/// The most slices a Sampler plays (from its first slice key up).
pub const MAX_SAMPLER_SLICES: usize = 64;
/// The shortest slice (seconds): transients closer than this to the one
/// before (or to the start) are left out.
pub const MIN_SLICE_SECONDS: f64 = 0.02;
/// The key a new kit's first pad and a new Sampler's first slice play.
pub const FIRST_KEY: u8 = 36;
/// The velocity of the MIDI clip's notes (a slice plays at its own level).
const VELOCITY: u8 = 127;

/// Where the cuts go.
#[derive(Clone, Debug, PartialEq)]
pub enum SliceBy {
    /// At the transients (at the editor's transient sensitivity).
    Transients,
    /// On the grid: every `step` from the span's start (spans only).
    Grid(MusicalTime),
    /// Into this many equal parts.
    Parts(u32),
    /// Where the clips in the span start: cut by hand (spans only).
    Clips,
}

impl SliceBy {
    fn label(&self) -> &'static str {
        match self {
            SliceBy::Transients => "transients",
            SliceBy::Grid(_) => "the grid",
            SliceBy::Parts(_) => "equal parts",
            SliceBy::Clips => "the clips' cuts",
        }
    }
}

/// What is sliced.
#[derive(Clone, Debug, PartialEq)]
pub enum SliceSource {
    /// A span of an audio track, as its clips play it.
    Span {
        track: TrackId,
        start: MusicalTime,
        end: MusicalTime,
    },
    /// The sample a sampler plays: the Sampler's (slot 0), a Drum
    /// Sampler pad's (slot = pad).
    Sample {
        plugin: PluginInstanceId,
        slot: usize,
    },
}

/// Where the slices go.
#[derive(Clone, Debug, PartialEq)]
pub enum SliceTarget {
    /// A new instrument track with a Drum Sampler: a slice a pad.
    NewDrums,
    /// The free pads of this Drum Sampler (slicing one of its pads: that
    /// pad keeps the first slice).
    Pads(PluginInstanceId),
    /// A new instrument track with a Sampler in slice mode: the whole
    /// material as its sample, a key a slice.
    NewSampler,
    /// The Sampler's own sample (the source) in slice mode.
    InPlace,
}

/// A slicing under way.
pub(crate) struct PendingSlice {
    source: SliceSource,
    target: SliceTarget,
    by: SliceBy,
    midi: bool,
    /// What it is called ("Drums Slices").
    name: String,
    /// The material's start on the timeline (spans; samples: where the
    /// MIDI clip goes).
    at: MusicalTime,
    /// The pads the slices go to (`Pads`), in order.
    pads: Vec<usize>,
    handle: Option<JoinHandle<std::result::Result<Sliced, String>>>,
}

/// What the worker hands back.
struct Sliced {
    /// Each slice's file (pads), or the material's one file (a new
    /// Sampler; none in place).
    files: Vec<PathBuf>,
    /// Each slice's start and end in the material (frames at `rate`).
    slices: Vec<(u64, u64)>,
    rate: u32,
    /// Transients left out (more than fit).
    dropped: usize,
    /// Keeps the files decoded in the samplers' cache until a sampler has
    /// them.
    held: Vec<Arc<Sample>>,
}

/// What the worker needs to know.
struct Plan {
    by: SliceBy,
    /// Cut points worked out by the session (grid, clips): frames from the
    /// material's start at its rate.
    points: Vec<u64>,
    /// The most slices that fit.
    room: usize,
    /// The transient sensitivity (0–1).
    sensitivity: f32,
    /// A file per slice (pads), one file (a new Sampler) or none.
    files: Files,
    dir: PathBuf,
    name: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Files {
    PerSlice,
    One,
    None,
}

/// The material sliced: rendered or read from a sampler's file.
enum Material {
    Render(SpanRender),
    File(PathBuf),
}

impl Material {
    /// The material's channels and rate (on the worker).
    fn read(self) -> std::result::Result<(Vec<Vec<f32>>, u32), String> {
        match self {
            Material::Render(r) => r.join(),
            Material::File(path) => {
                let s = faderframe_plugin_host::devices::samples::load_cached(&path)?;
                let mut channels = vec![s.data[0].clone()];
                if s.stereo {
                    channels.push(s.data[1].clone());
                }
                Ok((channels, s.rate.round().max(1.0) as u32))
            }
        }
    }
}

/// Whether a sampler's file is an SFZ instrument (not a sample to slice).
fn is_sfz(file: &str) -> bool {
    faderframe_plugin_host::devices::samples::is_sfz(Path::new(file))
}

/// Cut points of `mono` (frames) at its transients: those at least
/// [`MIN_SLICE_SECONDS`] from the start and from the one before, the
/// strongest `room − 1` of them kept. Returns the points and how many
/// were left out.
pub fn transient_points(
    mono: &[f32],
    rate: u32,
    sensitivity: f32,
    room: usize,
) -> (Vec<u64>, usize) {
    let min = onsets::strength_threshold(sensitivity);
    let gap = (MIN_SLICE_SECONDS * f64::from(rate)) as i64;
    let found: Vec<onsets::Onset> = onsets::detect(mono, rate)
        .into_iter()
        .filter(|o| o.strength >= min)
        .collect();
    strongest(found, mono.len() as i64, gap, room)
}

/// Of `found` (inside `0..len`), the cuts at least `gap` from the ends and
/// from each other (the stronger of two too close), the strongest
/// `room − 1` kept, in time order; and how many were left out for want of
/// room.
fn strongest(mut found: Vec<onsets::Onset>, len: i64, gap: i64, room: usize) -> (Vec<u64>, usize) {
    found.retain(|o| o.frame >= gap && o.frame < len - gap);
    found.sort_by_key(|o| o.frame);
    // Too close to the one before: the stronger stays.
    let mut kept: Vec<onsets::Onset> = Vec::new();
    for o in found {
        match kept.last_mut() {
            Some(last) if o.frame - last.frame < gap => {
                if o.strength > last.strength {
                    *last = o;
                }
            }
            _ => kept.push(o),
        }
    }
    let room = room.saturating_sub(1);
    let dropped = kept.len().saturating_sub(room);
    if dropped > 0 {
        kept.sort_by(|a, b| {
            b.strength
                .total_cmp(&a.strength)
                .then(a.frame.cmp(&b.frame))
        });
        kept.truncate(room);
        kept.sort_by_key(|o| o.frame);
    }
    (kept.into_iter().map(|o| o.frame as u64).collect(), dropped)
}

/// `frames` cut into `n` equal parts: the cut points.
pub fn part_points(frames: u64, n: u32) -> Vec<u64> {
    let n = u64::from(n.max(1));
    (1..n).map(|k| frames * k / n).collect()
}

/// The slices between cut points (sorted, inside `frames`).
fn regions(points: &[u64], frames: u64) -> Vec<(u64, u64)> {
    let mut starts = vec![0];
    starts.extend(points.iter().copied().filter(|&p| p > 0 && p < frames));
    starts.dedup();
    starts
        .iter()
        .enumerate()
        .map(|(i, &a)| (a, starts.get(i + 1).copied().unwrap_or(frames)))
        .collect()
}

/// The slicing itself (worker thread).
fn slice(material: Material, plan: Plan) -> std::result::Result<Sliced, String> {
    let (channels, rate) = material.read()?;
    let frames = channels.first().map_or(0, Vec::len) as u64;
    if frames == 0 {
        return Err("there is no audio to slice".into());
    }
    let views: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    let (points, dropped) = match plan.by {
        SliceBy::Transients => {
            transient_points(&onsets::mixdown(&views), rate, plan.sensitivity, plan.room)
        }
        SliceBy::Parts(n) => (part_points(frames, n), 0),
        SliceBy::Grid(_) | SliceBy::Clips => (plan.points.clone(), 0),
    };
    let slices = regions(&points, frames);
    if slices.len() > plan.room {
        return Err(format!(
            "{} slices do not fit: there is room for {}",
            slices.len(),
            plan.room
        ));
    }
    if slices.len() < 2 && plan.by == SliceBy::Transients {
        return Err("no transients to slice at (try the grid or equal parts)".into());
    }
    let mut files = Vec::new();
    let mut held = Vec::new();
    let write = |path: &Path, data: &[Vec<f32>]| {
        write_wav_with(path, data, rate, WavFormat::Float32, Dither::Off)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        faderframe_plugin_host::devices::samples::load_cached(path)
    };
    match plan.files {
        Files::PerSlice => {
            for (k, &(a, b)) in slices.iter().enumerate() {
                let mut part: Vec<Vec<f32>> = channels
                    .iter()
                    .map(|c| c[a as usize..b as usize].to_vec())
                    .collect();
                fade_cuts(&mut part, rate, a > 0, b < frames);
                let path =
                    crate::media::unique_path(&plan.dir, &format!("{} {}", plan.name, k + 1));
                held.push(write(&path, &part)?);
                files.push(path);
            }
        }
        Files::One => {
            let path = crate::media::unique_path(&plan.dir, &plan.name);
            held.push(write(&path, &channels)?);
            files.push(path);
        }
        Files::None => {}
    }
    Ok(Sliced {
        files,
        slices,
        rate,
        dropped,
        held,
    })
}

/// Fade a slice's ends where they cut through sound (`head`/`tail`: the
/// end is a cut, not the material's own end).
fn fade_cuts(channels: &mut [Vec<f32>], rate: u32, head: bool, tail: bool) {
    let len = channels.first().map_or(0, Vec::len);
    let n = ((f64::from(rate) * EDGE_FADE) as usize).max(1);
    if len < 4 * n {
        return;
    }
    let loud = |channels: &[Vec<f32>], i: usize| channels.iter().any(|c| c[i].abs() > 1e-3);
    let (head, tail) = (head && loud(channels, 0), tail && loud(channels, len - 1));
    for c in channels.iter_mut() {
        for i in 0..n {
            let g = i as f32 / n as f32;
            if head {
                c[i] *= g;
            }
            if tail {
                c[len - 1 - i] *= g;
            }
        }
    }
}

impl Session {
    /// Slicings under way.
    pub fn slicing(&self) -> bool {
        !self.slicings.is_empty()
    }

    /// Wait (ticking) until the slicings are placed or `timeout` passed;
    /// whether they were (tests, scripted runs).
    pub fn wait_for_slicing(&mut self, timeout: std::time::Duration) -> bool {
        let end = std::time::Instant::now() + timeout;
        while self.slicing() {
            if std::time::Instant::now() >= end {
                return false;
            }
            self.tick(0.016);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        true
    }

    /// Menu entries for slicing `track` (the clip under the pointer when
    /// there is no selection; see [`Session::sample_choices`]): label,
    /// action, separator before. Empty when there is nothing to slice.
    pub fn slice_choices(
        &self,
        track: TrackId,
        clip: Option<ClipId>,
    ) -> Vec<(String, crate::Action, bool)> {
        let Some((start, end, what)) = self.sample_span(track, clip) else {
            return Vec::new();
        };
        let source = SliceSource::Span { track, start, end };
        let grid = self.grid_step_at(start);
        let cuts = self.clip_cuts(track, start, end).len();
        let slice = |by: SliceBy, target: SliceTarget| crate::Action::Slice {
            source: source.clone(),
            by,
            target,
            midi: true,
        };
        let mut out = vec![
            (
                format!("Slice {what} at Transients to New Drum Sampler"),
                slice(SliceBy::Transients, SliceTarget::NewDrums),
                false,
            ),
            (
                format!("Slice {what} on the Grid to New Drum Sampler"),
                slice(SliceBy::Grid(grid), SliceTarget::NewDrums),
                false,
            ),
        ];
        if cuts > 0 {
            out.push((
                format!("Slice {what} Where the Clips Are Cut to New Drum Sampler"),
                slice(SliceBy::Clips, SliceTarget::NewDrums),
                false,
            ));
        }
        out.push((
            format!("Slice {what} at Transients to New Sampler (a Key Each)"),
            slice(SliceBy::Transients, SliceTarget::NewSampler),
            true,
        ));
        out.push((
            format!("Slice {what} on the Grid to New Sampler (a Key Each)"),
            slice(SliceBy::Grid(grid), SliceTarget::NewSampler),
            false,
        ));
        let mut first = true;
        for t in &self.project.tracks {
            for kit in t
                .slots()
                .into_iter()
                .filter(|s| s.plugin.id == builtin::DRUMS)
            {
                out.push((
                    format!(
                        "Slice {what} at Transients to the Free Pads of ‘{}’",
                        t.name
                    ),
                    slice(SliceBy::Transients, SliceTarget::Pads(kit.id)),
                    std::mem::take(&mut first),
                ));
            }
        }
        out
    }

    /// The menu entries a sampler's editor offers for slicing its sample
    /// in `slot` (label, action, separator before).
    pub fn sample_slice_choices(
        &self,
        plugin: PluginInstanceId,
        slot: usize,
    ) -> Vec<(String, crate::Action, bool)> {
        let Some((_, owner)) = self.plugin_owner(plugin) else {
            return Vec::new();
        };
        let doc = samples::doc_of(owner);
        if doc.file(slot).is_none_or(is_sfz) {
            return Vec::new();
        }
        let source = SliceSource::Sample { plugin, slot };
        let slice = |by: SliceBy, target: SliceTarget, midi: bool| crate::Action::Slice {
            source: source.clone(),
            by,
            target,
            midi,
        };
        let mut out = Vec::new();
        if owner.plugin.id == builtin::SAMPLER {
            out.push((
                "Slice at Transients (a Key Each)".to_string(),
                slice(SliceBy::Transients, SliceTarget::InPlace, false),
                false,
            ));
            for n in [4, 8, 16, 32] {
                out.push((
                    format!("Slice into {n} Equal Parts"),
                    slice(SliceBy::Parts(n), SliceTarget::InPlace, false),
                    false,
                ));
            }
            if !doc.slices.is_empty() {
                out.push((
                    "Slices to a MIDI Clip at the Playhead".to_string(),
                    crate::Action::SlicesToMidi(plugin),
                    true,
                ));
                out.push((
                    "Clear the Slices".to_string(),
                    crate::Action::SetSampleSlices {
                        plugin,
                        slices: Vec::new(),
                    },
                    false,
                ));
            }
            out.push((
                "Slice at Transients to New Drum Sampler".to_string(),
                slice(SliceBy::Transients, SliceTarget::NewDrums, true),
                true,
            ));
        } else if owner.plugin.id == builtin::DRUMS {
            out.push((
                format!("Slice Pad {} at Transients to the Free Pads", slot + 1),
                slice(SliceBy::Transients, SliceTarget::Pads(plugin), true),
                false,
            ));
            for n in [4, 8, 16] {
                out.push((
                    format!(
                        "Slice Pad {} into {n} Equal Parts on the Free Pads",
                        slot + 1
                    ),
                    slice(SliceBy::Parts(n), SliceTarget::Pads(plugin), true),
                    false,
                ));
            }
            out.push((
                format!("Slice Pad {} at Transients to New Drum Sampler", slot + 1),
                slice(SliceBy::Transients, SliceTarget::NewDrums, true),
                true,
            ));
        }
        out
    }

    /// The edit grid's step at `at`.
    fn grid_step_at(&self, at: MusicalTime) -> MusicalTime {
        let sig = self.project.timeline.meter.signature_at(at);
        self.editor.grid.step(sig).max(MusicalTime(1))
    }

    /// Where clips on `track` start inside (`start`, `end`).
    fn clip_cuts(&self, track: TrackId, start: MusicalTime, end: MusicalTime) -> Vec<MusicalTime> {
        let mut cuts: Vec<MusicalTime> = self
            .project
            .clips_of(track)
            .iter()
            .map(|c| c.start)
            .filter(|&s| s > start && s < end)
            .collect();
        cuts.sort();
        cuts.dedup();
        cuts
    }

    /// Start slicing (the result is placed by [`Self::poll_slicings`]).
    pub(crate) fn start_slice(
        &mut self,
        source: SliceSource,
        by: SliceBy,
        target: SliceTarget,
        midi: bool,
    ) -> Result<()> {
        let err = |m: &str| SessionError::Other(m.to_string());
        let dir = self.samples_dir()?;
        let rate = f64::from(self.project.sample_rate);
        // The material, its name, where it starts, and cuts the session
        // knows (grid, clips).
        let (material, base, at, points) = match &source {
            SliceSource::Span { track, start, end } => {
                let (track, start, end) = (*track, *start, *end);
                self.check_sample(track, start, end)?;
                let name = self
                    .project
                    .track(track)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                let tl = &self.project.timeline;
                let s0 = tl.to_samples(start, rate);
                let frame_of = |m: MusicalTime| (tl.to_samples(m, rate) - s0).max(0) as u64;
                let points: Vec<u64> = match &by {
                    SliceBy::Grid(step) => {
                        if *step <= MusicalTime::ZERO {
                            return Err(err("the grid has no step"));
                        }
                        let mut v = Vec::new();
                        let mut m = start + *step;
                        while m < end {
                            v.push(frame_of(m));
                            m += *step;
                        }
                        v
                    }
                    SliceBy::Clips => {
                        let cuts = self.clip_cuts(track, start, end);
                        if cuts.is_empty() {
                            return Err(err(
                                "no clips start inside: cut the clip where the slices go",
                            ));
                        }
                        cuts.into_iter().map(frame_of).collect()
                    }
                    _ => Vec::new(),
                };
                // Rendered once the target is known to take the slices.
                (None, name, start, points)
            }
            SliceSource::Sample { plugin, slot } => {
                if matches!(by, SliceBy::Grid(_) | SliceBy::Clips) {
                    return Err(err(
                        "a sample has no grid or clips: slice it at its transients or into equal parts",
                    ));
                }
                let (_, owner) = self
                    .plugin_owner(*plugin)
                    .ok_or_else(|| err("the sampler is gone"))?;
                if !samples::is_sampler(owner) || owner.plugin.id == builtin::CONVOLUTION_REVERB {
                    return Err(err("only the samplers' samples are sliced"));
                }
                let doc = samples::doc_of(owner);
                let file = doc
                    .file(*slot)
                    .filter(|f| !is_sfz(f))
                    .ok_or_else(|| err("there is no sample to slice there"))?;
                let name = Path::new(file)
                    .file_stem()
                    .map_or_else(|| "Sample".into(), |s| s.to_string_lossy().into_owned());
                let at = self
                    .selection
                    .range
                    .filter(|r| r.end > r.start)
                    .map_or_else(|| self.playhead(), |r| r.start);
                (
                    Some(Material::File(PathBuf::from(file))),
                    name,
                    at,
                    Vec::new(),
                )
            }
        };
        // Where the slices go, and how many fit.
        let (room, pads, files) = match &target {
            SliceTarget::NewDrums => (drums::PADS, (0..drums::PADS).collect(), Files::PerSlice),
            SliceTarget::Pads(kit) => {
                let (_, owner) = self
                    .plugin_owner(*kit)
                    .ok_or_else(|| err("the Drum Sampler is gone"))?;
                if owner.plugin.id != builtin::DRUMS {
                    return Err(err("slices go to a Drum Sampler's pads"));
                }
                let doc = samples::doc_of(owner);
                // A pad sliced keeps the first slice.
                let own = match &source {
                    SliceSource::Sample { plugin, slot } if plugin == kit => Some(*slot),
                    _ => None,
                };
                let pads: Vec<usize> = own
                    .into_iter()
                    .chain((0..drums::PADS).filter(|&p| {
                        Some(p) != own && doc.files.get(p).is_none_or(Option::is_none)
                    }))
                    .collect();
                if pads.len() < 2 && own.is_some() || pads.is_empty() {
                    return Err(err("the Drum Sampler has no free pads"));
                }
                (pads.len(), pads, Files::PerSlice)
            }
            SliceTarget::NewSampler => (MAX_SAMPLER_SLICES, Vec::new(), Files::One),
            SliceTarget::InPlace => {
                let SliceSource::Sample { plugin, .. } = &source else {
                    return Err(err("only a Sampler's own sample is sliced in place"));
                };
                let is_sampler = self
                    .plugin_owner(*plugin)
                    .is_some_and(|(_, s)| s.plugin.id == builtin::SAMPLER);
                if !is_sampler {
                    return Err(err("only a Sampler plays slices a key each"));
                }
                (MAX_SAMPLER_SLICES, Vec::new(), Files::None)
            }
        };
        if let SliceBy::Parts(n) = by
            && (n < 2 || n as usize > room)
        {
            return Err(SessionError::Other(format!(
                "{n} parts do not fit: there is room for 2 to {room}"
            )));
        }
        if matches!(by, SliceBy::Grid(_) | SliceBy::Clips) && points.len() + 1 > room {
            return Err(SessionError::Other(format!(
                "{} slices do not fit: there is room for {room} (a coarser grid, or a shorter \
                 selection)",
                points.len() + 1
            )));
        }
        let name = match target {
            SliceTarget::NewSampler => format!("{base} Sliced"),
            _ => format!("{base} Slices"),
        };
        let plan = Plan {
            by: by.clone(),
            points,
            room,
            sensitivity: self.editor.transient_sensitivity,
            files,
            dir,
            // Pads: "<name> Slice 1", 2, …; a Sampler: the whole as one.
            name: if files == Files::One {
                name.clone()
            } else {
                format!("{base} Slice")
            },
        };
        let material =
            match (material, &source) {
                (Some(m), _) => m,
                (None, SliceSource::Span { track, start, end }) => Material::Render(
                    self.render_span(*track, *start, *end, &format!("{base} Slices"))?,
                ),
                (None, SliceSource::Sample { .. }) => return Err(err("there is nothing to slice")),
            };
        let handle = std::thread::Builder::new()
            .name("faderframe-slice".into())
            .spawn(move || slice(material, plan))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.slicings.push(PendingSlice {
            source,
            target,
            by,
            midi,
            name,
            at,
            pads,
            handle: Some(handle),
        });
        self.notify(NoticeLevel::Info, "slicing…".to_string());
        self.revision += 1;
        Ok(())
    }

    /// Place slicings that are done (from the tick).
    pub(crate) fn poll_slicings(&mut self) {
        let mut i = 0;
        while i < self.slicings.len() {
            if !self.slicings[i]
                .handle
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            {
                i += 1;
                continue;
            }
            let mut p = self.slicings.remove(i);
            let made = p
                .handle
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err("the slicing's worker stopped".into()));
            // Files written for a result that cannot be placed go again.
            let written = made
                .as_ref()
                .map_or_else(|_| Vec::new(), |m| m.files.clone());
            if let Err(e) = made
                .map_err(SessionError::Other)
                .and_then(|m| self.place_slices(&p, m))
            {
                for f in written {
                    let _ = std::fs::remove_file(f);
                }
                self.notify(NoticeLevel::Error, format!("slicing: {e}"));
            }
            self.revision += 1;
        }
    }

    fn place_slices(&mut self, p: &PendingSlice, made: Sliced) -> Result<()> {
        let err = |m: &str| SessionError::Other(m.to_string());
        let n = made.slices.len();
        let files: Vec<String> = made
            .files
            .iter()
            .map(|f| f.to_string_lossy().into_owned())
            .collect();
        let mut commands = Vec::new();
        // The track the MIDI clip goes on, the keys the slices play.
        let (track, first_key, label, opened);
        match &p.target {
            SliceTarget::NewDrums | SliceTarget::NewSampler => {
                let drums = p.target == SliceTarget::NewDrums;
                let (doc, params) = if drums {
                    (
                        SampleDoc {
                            cycles: Vec::new(),
                            files: files.iter().cloned().map(Some).collect(),
                            slices: Vec::new(),
                        },
                        Vec::new(),
                    )
                } else {
                    let mut doc = SampleDoc {
                        cycles: Vec::new(),
                        files: files.first().cloned().map(Some).into_iter().collect(),
                        slices: Vec::new(),
                    };
                    doc.set_slices(made.slices.iter().map(|s| s.0).collect());
                    let mut params = Vec::new();
                    for (id, v) in [
                        (sampler::id::SLICES, 1.0),
                        (sampler::id::SLICE_KEY, f64::from(FIRST_KEY)),
                    ] {
                        params.extend_from_slice(&id.to_le_bytes());
                        params.extend_from_slice(&f64::to_le_bytes(v));
                    }
                    (doc, params)
                };
                let state = faderframe_engine::encode_state(
                    &faderframe_plugin_host::devices::samples::pack(&params, &doc),
                );
                let source_track = match &p.source {
                    SliceSource::Span { track, .. } => Some(*track),
                    SliceSource::Sample { plugin, .. } => self.plugin_owner(*plugin).map(|o| o.0),
                };
                let project = &mut self.project;
                let id: TrackId = project.ids.allocate();
                let plugin: PluginInstanceId = project.ids.allocate();
                let index = source_track
                    .and_then(|t| project.track_index(t))
                    .map_or(project.tracks.len(), |i| i + 1);
                let mut new = Track::new(
                    id,
                    TrackKind::Instrument,
                    p.name.clone(),
                    TrackColor::palette(project.tracks.len()),
                );
                if let Some(c) = source_track.and_then(|t| project.track(t)).map(|t| t.color) {
                    new.color = c;
                }
                let (plugin_id, plugin_name) = if drums {
                    (builtin::DRUMS, "Drum Sampler")
                } else {
                    (builtin::SAMPLER, "Sampler")
                };
                commands.push(Command::AddTrack {
                    track: Box::new(new),
                    index,
                });
                commands.push(Command::InsertPlugin {
                    track: id,
                    index: 0,
                    slot: PluginSlot {
                        id: plugin,
                        plugin: PluginRef::builtin(plugin_id, plugin_name),
                        bypass: false,
                        parameters: Vec::new(),
                        state: Some(state),
                        sidechain: None,
                        mono_to_stereo: false,
                        mix_origin: None,
                    },
                });
                track = id;
                first_key = Some(FIRST_KEY);
                label = if drums {
                    "Slice to Drum Sampler"
                } else {
                    "Slice to Sampler"
                };
                opened = Some((id, plugin));
            }
            SliceTarget::Pads(kit) => {
                let (kit_track, owner) = self
                    .plugin_owner(*kit)
                    .ok_or_else(|| err("the Drum Sampler is gone"))?;
                let mut doc = samples::doc_of(owner);
                // Pads taken meanwhile (another load) are not overwritten.
                let own = match &p.source {
                    SliceSource::Sample { plugin, slot } if plugin == kit => Some(*slot),
                    _ => None,
                };
                let free = |d: &SampleDoc, pad: usize| {
                    Some(pad) == own || d.files.get(pad).is_none_or(Option::is_none)
                };
                if p.pads.iter().take(n).any(|&pad| !free(&doc, pad)) {
                    return Err(err("the pads were taken meanwhile: slice again"));
                }
                for (pad, file) in p.pads.iter().zip(&files) {
                    doc.set(*pad, Some(file.clone()));
                }
                let kit_track_kind = self.project.track(kit_track).map(|t| t.kind);
                commands.push(self.sampler_state_command(*kit, &doc, &[])?);
                track = kit_track;
                first_key = self
                    .device_value(*kit, drums::id::BASE_NOTE)
                    .map(|v| v.round().clamp(0.0, 127.0) as u8);
                label = "Slice to Pads";
                opened = None;
                if !matches!(kit_track_kind, Some(TrackKind::Instrument)) {
                    // No MIDI clips there: the kit is played otherwise.
                    return self.finish_slices(p, made, commands, None, None, label, opened);
                }
            }
            SliceTarget::InPlace => {
                let SliceSource::Sample { plugin, .. } = &p.source else {
                    return Err(err("only a Sampler's own sample is sliced in place"));
                };
                let (owner_track, owner) = self
                    .plugin_owner(*plugin)
                    .ok_or_else(|| err("the Sampler is gone"))?;
                let mut doc = samples::doc_of(owner);
                doc.set_slices(made.slices.iter().map(|s| s.0).collect());
                commands.push(self.sampler_state_command(
                    *plugin,
                    &doc,
                    &[(sampler::id::SLICES, 1.0)],
                )?);
                let kind = self.project.track(owner_track).map(|t| t.kind);
                track = owner_track;
                first_key = self
                    .device_value(*plugin, sampler::id::SLICE_KEY)
                    .map(|v| v.round().clamp(0.0, 127.0) as u8);
                label = "Slice Sample";
                opened = None;
                if !matches!(kind, Some(TrackKind::Instrument)) {
                    return self.finish_slices(p, made, commands, None, None, label, opened);
                }
            }
        }
        let keys: Vec<u8> = match first_key {
            Some(k) => match &p.target {
                SliceTarget::Pads(_) => p
                    .pads
                    .iter()
                    .take(n)
                    .map(|&pad| (usize::from(k) + pad).min(127) as u8)
                    .collect(),
                _ => (0..n)
                    .map(|i| (usize::from(k) + i).min(127) as u8)
                    .collect(),
            },
            None => Vec::new(),
        };
        let clip = (p.midi && keys.len() == n).then(|| self.slice_clip(p, &made, track, &keys));
        self.finish_slices(p, made, commands, clip, Some(track), label, opened)
    }

    /// Apply a slicing's commands (and its MIDI clip, the covered clips
    /// muted) as one step, and say what happened.
    #[allow(clippy::too_many_arguments)]
    fn finish_slices(
        &mut self,
        p: &PendingSlice,
        made: Sliced,
        mut commands: Vec<Command>,
        clip: Option<Clip>,
        track: Option<TrackId>,
        label: &str,
        opened: Option<(TrackId, PluginInstanceId)>,
    ) -> Result<()> {
        let n = made.slices.len();
        let with_clip = clip.is_some();
        if let Some(c) = clip {
            commands.push(Command::AddClip { clip: Box::new(c) });
            // The clip plays the span now: the clips it covers whole fall
            // silent.
            if let SliceSource::Span { track, start, end } = &p.source {
                let p = &self.project;
                for c in p.clips_of(*track) {
                    let c_end = c.end(&p.timeline, p.sample_rate);
                    if !c.muted && c.start >= *start && c_end <= *end {
                        commands.push(Command::SetClipMuted {
                            clip: c.id,
                            muted: true,
                        });
                    }
                }
            }
        }
        self.batch(label, commands)?;
        drop(made.held);
        if let Some((t, plugin)) = opened {
            self.selection.select_tracks(&[t], SelectMode::Replace);
            self.ui_requests.push(UiRequest::PluginEditor {
                track: t,
                plugin,
                generic: false,
            });
        }
        let to = match &p.target {
            SliceTarget::NewDrums => format!("pads 1–{n} of ‘{}’", p.name),
            SliceTarget::Pads(_) => {
                let pads: Vec<String> =
                    p.pads.iter().take(n).map(|p| (p + 1).to_string()).collect();
                format!("pads {}", pads.join(", "))
            }
            SliceTarget::NewSampler => format!("a key each on ‘{}’", p.name),
            SliceTarget::InPlace => "a key each".into(),
        };
        let mut text = format!("{n} slices at {} on {to}", p.by.label());
        if made.dropped > 0 {
            text.push_str(&format!(
                " (the {} weakest transients left out: no room)",
                made.dropped
            ));
        }
        if with_clip {
            text.push_str("; a MIDI clip plays them in the original rhythm");
        } else if p.midi && track.is_none() {
            text.push_str("; no MIDI clip (the kit's track takes none)");
        }
        self.notify(NoticeLevel::Info, text);
        Ok(())
    }

    /// The MIDI clip playing the slices where they were (`keys`: each
    /// slice's).
    fn slice_clip(&mut self, p: &PendingSlice, made: &Sliced, track: TrackId, keys: &[u8]) -> Clip {
        let rate = f64::from(self.project.sample_rate);
        let ratio = rate / f64::from(made.rate.max(1));
        let tl = &self.project.timeline;
        let base = tl.to_samples(p.at, rate);
        let at = |frame: u64| tl.to_musical(base + (frame as f64 * ratio).round() as i64, rate);
        let total = made.slices.last().map_or(0, |s| s.1);
        let length = (at(total) - p.at).max(MusicalTime(1));
        let ids = &mut self.project.ids;
        let notes: Vec<MidiNote> = made
            .slices
            .iter()
            .zip(keys)
            .map(|(&(a, b), &key)| {
                let start = at(a) - p.at;
                MidiNote {
                    id: ids.allocate(),
                    start,
                    length: (at(b) - p.at - start).max(MusicalTime(1)),
                    key,
                    velocity: VELOCITY,
                    channel: 0,
                    muted: false,
                    release: None,
                    time_locked: false,
                    articulation: None,
                    fine: None,
                }
            })
            .collect();
        let id: ClipId = self.project.ids.allocate();
        Clip {
            id,
            track,
            name: p.name.clone(),
            color: None,
            start: p.at,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length,
                notes,
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
                articulations: Vec::new(),
            }),
        }
    }

    /// A built-in's parameter as it is now.
    fn device_value(&self, plugin: PluginInstanceId, id: u32) -> Option<f64> {
        let tap = self.plugin_tap(plugin)?;
        let i = tap.params.infos().iter().position(|p| p.id.0 == id)?;
        Some(f64::from(tap.params.get(i)))
    }

    /// The edit giving a sampler `doc` (its parameters as they are, `set`
    /// changed — explicitly too, so its slot's values do not undo them).
    pub(crate) fn sampler_state_command(
        &self,
        plugin: PluginInstanceId,
        doc: &SampleDoc,
        set: &[(u32, f64)],
    ) -> Result<Command> {
        let (track, owner) = self
            .plugin_owner(plugin)
            .ok_or_else(|| SessionError::Other("the sampler is gone".into()))?;
        let mut parameters = owner.parameters.clone();
        let block = match self.plugin_tap(plugin) {
            Some(tap) => tap.params.save(),
            None => owner
                .state
                .as_deref()
                .and_then(faderframe_engine::decode_state)
                .and_then(|b| {
                    faderframe_plugin_host::devices::samples::unpack(&b).map(|(p, _)| p.to_vec())
                })
                .unwrap_or_default(),
        };
        let values = faderframe_plugin_host::ParamValues::new(match owner.plugin.id.as_str() {
            builtin::SAMPLER => sampler::parameters(),
            _ => drums::parameters(),
        });
        let _ = values.load(&block);
        for &(id, v) in set {
            let _ = values.set_by_id(ParameterId(id), v);
            // An explicit value would be applied over the state's.
            if let Some(p) = parameters.iter_mut().find(|p| p.id.0 == id) {
                p.value = v;
            }
        }
        let state = faderframe_engine::encode_state(
            &faderframe_plugin_host::devices::samples::pack(&values.save(), doc),
        );
        Ok(Command::SetPluginState {
            track,
            plugin,
            state: Some(state),
            parameters,
        })
    }

    /// Set a Sampler's slice points by hand (frames of its sample; one
    /// undo step, "Slice Points").
    pub(crate) fn set_sample_slices(
        &mut self,
        plugin: PluginInstanceId,
        slices: Vec<u64>,
    ) -> Result<()> {
        let (_, owner) = self
            .plugin_owner(plugin)
            .ok_or_else(|| SessionError::Other("the Sampler is gone".into()))?;
        if owner.plugin.id != builtin::SAMPLER {
            return Err(SessionError::Other(
                "only a Sampler has slice points".into(),
            ));
        }
        let mut doc = samples::doc_of(owner);
        let before = doc.slices.clone();
        doc.set_slices(slices);
        if doc.slices == before {
            return Ok(());
        }
        let label = if doc.slices.is_empty() {
            "Clear Slices"
        } else {
            "Slice Points"
        };
        let cmd = self.sampler_state_command(plugin, &doc, &[])?;
        self.batch(label, vec![cmd])
    }

    /// A MIDI clip at the playhead (or the edit selection's start) playing
    /// a sliced Sampler's slices one after another, as recorded.
    pub(crate) fn slices_to_midi(&mut self, plugin: PluginInstanceId) -> Result<()> {
        let err = |m: &str| SessionError::Other(m.to_string());
        let (track, owner) = self
            .plugin_owner(plugin)
            .ok_or_else(|| err("the Sampler is gone"))?;
        if self.project.track(track).map(|t| t.kind) != Some(TrackKind::Instrument) {
            return Err(err("the Sampler's track takes no MIDI clips"));
        }
        let doc = samples::doc_of(owner);
        let file = doc
            .file(0)
            .ok_or_else(|| err("the Sampler has no sample"))?;
        let s = faderframe_plugin_host::devices::samples::load_cached(Path::new(file))
            .map_err(SessionError::Other)?;
        let slices = doc.slice_regions(s.frames);
        if slices.is_empty() {
            return Err(err("the sample has no slices"));
        }
        let first = self
            .device_value(plugin, sampler::id::SLICE_KEY)
            .unwrap_or(f64::from(FIRST_KEY))
            .round()
            .clamp(0.0, 127.0) as usize;
        let keys: Vec<u8> = (0..slices.len())
            .map(|i| (first + i).min(127) as u8)
            .collect();
        let at = self
            .selection
            .range
            .filter(|r| r.end > r.start)
            .map_or_else(|| self.playhead(), |r| r.start);
        let name = Path::new(file).file_stem().map_or_else(
            || "Slices".into(),
            |s| format!("{} Slices", s.to_string_lossy()),
        );
        let p = PendingSlice {
            source: SliceSource::Sample { plugin, slot: 0 },
            target: SliceTarget::InPlace,
            by: SliceBy::Clips,
            midi: true,
            name,
            at,
            pads: Vec::new(),
            handle: None,
        };
        let made = Sliced {
            files: Vec::new(),
            slices,
            rate: s.rate.round().max(1.0) as u32,
            dropped: 0,
            held: Vec::new(),
        };
        let clip = self.slice_clip(&p, &made, track, &keys);
        self.batch(
            "Slices to MIDI Clip",
            vec![Command::AddClip {
                clip: Box::new(clip),
            }],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_cover_the_material_from_its_start() {
        assert_eq!(
            regions(&[100, 300], 1000),
            vec![(0, 100), (100, 300), (300, 1000)]
        );
        assert_eq!(regions(&[0, 1000, 2000], 1000), vec![(0, 1000)]);
        assert_eq!(part_points(1000, 4), vec![250, 500, 750]);
    }

    /// Clicks of decaying noise: the strongest transients stay when there
    /// is no room for all, in time order.
    #[test]
    fn transients_cut_where_the_hits_are_and_the_strongest_stay() {
        let rate = 48_000u32;
        let mut x = vec![0.0f32; rate as usize * 2];
        let hits = [
            (9_600usize, 0.9f32),
            (28_800, 0.3),
            (48_000, 0.9),
            (72_000, 0.6),
        ];
        let mut seed = 1u32;
        for &(at, amp) in &hits {
            for i in 0..4_800 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
                x[at + i] = amp * noise * (-(i as f32) / 600.0).exp();
            }
        }
        let (points, dropped) = transient_points(&x, rate, 1.0, 16);
        assert_eq!(points.len(), hits.len(), "{points:?}");
        assert_eq!(dropped, 0);
        for (p, h) in points.iter().zip(&hits) {
            assert!((*p as i64 - h.0 as i64).abs() < 480, "{p} for {}", h.0);
        }
        // Room for three slices: the two strongest cuts after the start,
        // in time order; two too close: the stronger; none at the ends.
        let o = |frame: i64, strength: f32| onsets::Onset { frame, strength };
        let found = vec![
            o(48_000, 0.9),
            o(9_600, 0.8),
            o(9_900, 0.95),
            o(28_800, 0.3),
            o(72_000, 0.6),
            o(100, 1.0),
            o(95_900, 1.0),
        ];
        let (points, dropped) = strongest(found, 96_000, 960, 3);
        assert_eq!(points, vec![9_900, 48_000]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn only_cuts_through_sound_are_faded() {
        let mut ch = vec![vec![0.5f32; 1_000]];
        fade_cuts(&mut ch, 48_000, true, false);
        assert_eq!(ch[0][0], 0.0);
        assert_eq!(ch[0][999], 0.5, "the material's own end stays");
    }
}

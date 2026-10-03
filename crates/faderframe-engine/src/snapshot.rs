//! Immutable, sample-domain view of the timeline for the realtime thread.
//!
//! The project stores clips in musical time. Whenever clips, notes, the tempo
//! map or the engine sample rate change, the control thread builds a new
//! [`TimelineSnapshot`] (clip regions and MIDI events in absolute samples)
//! and hands it to the audio thread, which swaps it in at a block boundary.
//!
//! Audio regions reference a [`Source`]: in-memory data (generated material,
//! rendered at the engine rate) or a disk-streamed file. For streamed files
//! the controller also derives a [`StreamPlan`] that tells the disk loader
//! which pages to keep resident for a given playhead window.

use faderframe_audio_files::{AudioData, Page, StreamSource, generate};
use faderframe_automation::{AutomationMode, AutomationTarget, SampleLane};
use faderframe_core::{
    AudioSourceId, AutomationLaneId, ParameterId, PluginInstanceId, SendId, TrackId, db_to_gain,
};
use faderframe_midi::MidiEvent;
use faderframe_project::{ClipContent, FadeShape, Project, SourceSpec, TakeFolder};
use faderframe_realtime::{Epoch, Reclaimer};
use faderframe_timeline::Timeline;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

/// Where a region's samples come from.
#[derive(Clone, Debug)]
pub enum Source {
    /// Fully decoded, at the engine sample rate.
    Memory(Arc<AudioData>),
    /// Streamed from disk at the file's own sample rate.
    Stream(Arc<StreamSource>),
}

impl From<Arc<AudioData>> for Source {
    fn from(d: Arc<AudioData>) -> Self {
        Source::Memory(d)
    }
}

impl Source {
    pub fn channels(&self) -> usize {
        match self {
            Source::Memory(d) => d.num_channels(),
            Source::Stream(s) => s.channels(),
        }
    }

    pub fn frames(&self) -> i64 {
        match self {
            Source::Memory(d) => d.frames() as i64,
            Source::Stream(s) => s.frames() as i64,
        }
    }
}

/// Audio per source id.
pub type SourceMap = HashMap<AudioSourceId, Source>;

/// Render every generated source of `project` at `sample_rate`. File
/// sources are opened by the session (see `faderframe_session`).
pub fn render_generated_sources(project: &Project, sample_rate: u32) -> SourceMap {
    project
        .sources
        .values()
        .filter_map(|s| match &s.spec {
            SourceSpec::Generated { generator } => Some((
                s.id,
                Source::Memory(Arc::new(generate(generator, sample_rate))),
            )),
            SourceSpec::File { .. } => None,
        })
        .collect()
}

#[derive(Debug)]
pub struct AudioRegion {
    /// Timeline sample of the first played frame.
    pub start: i64,
    /// Timeline sample after the last played frame.
    pub end: i64,
    pub source: Source,
    /// Source frame (in the source's own rate) played at `start`.
    pub source_start: i64,
    /// Source frames per engine frame (1.0 unless the file's rate differs
    /// from the engine rate).
    pub step: f64,
    pub gain: f32,
    pub fade_in: i64,
    pub fade_out: i64,
    pub fade_in_shape: FadeShape,
    pub fade_out_shape: FadeShape,
    pub reversed: bool,
}

impl AudioRegion {
    /// Clip gain including fades at timeline sample `t`.
    #[inline]
    pub fn gain_at(&self, t: i64) -> f32 {
        let mut g = self.gain;
        let into = t - self.start;
        if self.fade_in > 0 && into < self.fade_in {
            g *= self.fade_in_shape.gain(into as f32 / self.fade_in as f32);
        }
        let left = self.end - t;
        if self.fade_out > 0 && left < self.fade_out {
            g *= self.fade_out_shape.gain(left as f32 / self.fade_out as f32);
        }
        g
    }

    /// Source frame range touched while playing timeline samples `[a, b)`.
    pub fn source_span(&self, a: i64, b: i64) -> (i64, i64) {
        let a = a.max(self.start);
        let b = b.min(self.end);
        if b <= a {
            return (0, 0);
        }
        let len = ((self.end - self.start) as f64 * self.step).ceil() as i64;
        let (ra, rb) = (
            ((a - self.start) as f64 * self.step).floor() as i64,
            ((b - self.start) as f64 * self.step).ceil() as i64 + 1,
        );
        if self.reversed {
            (self.source_start + len - rb, self.source_start + len - ra)
        } else {
            (self.source_start + ra, self.source_start + rb)
        }
    }
}

#[derive(Debug)]
pub struct MidiRegion {
    pub start: i64,
    pub end: i64,
    /// Absolute-sample events, sorted (note-offs first at equal times).
    pub events: Vec<(i64, MidiEvent)>,
}

#[derive(Debug, Default)]
pub struct Lane {
    /// Sorted by start.
    pub audio: Vec<AudioRegion>,
    /// Sorted by start.
    pub midi: Vec<MidiRegion>,
}

/// The automation of one track that drives the engine (lanes in Read,
/// Touch or Latch mode with points, minus lanes being written right now).
/// Values are plain units: dB for volume and sends, -1..1 for pan, 0/1 for
/// mute and bypass, the plugin's own range for plugin parameters.
#[derive(Debug, Default)]
pub struct TrackAutomation {
    pub volume: Option<SampleLane>,
    pub pan: Option<SampleLane>,
    pub mute: Option<SampleLane>,
    pub sends: Vec<(SendId, SampleLane)>,
    pub params: Vec<(PluginInstanceId, ParameterId, SampleLane)>,
    pub bypass: Vec<(PluginInstanceId, SampleLane)>,
}

impl TrackAutomation {
    pub fn is_empty(&self) -> bool {
        self.volume.is_none()
            && self.pan.is_none()
            && self.mute.is_none()
            && self.sends.is_empty()
            && self.params.is_empty()
            && self.bypass.is_empty()
    }

    #[inline]
    pub fn send(&self, id: SendId) -> Option<&SampleLane> {
        self.sends.iter().find(|(s, _)| *s == id).map(|(_, l)| l)
    }

    #[inline]
    pub fn bypass(&self, plugin: PluginInstanceId) -> Option<&SampleLane> {
        self.bypass
            .iter()
            .find(|(p, _)| *p == plugin)
            .map(|(_, l)| l)
    }

    /// Automated parameters of one plugin.
    pub fn plugin_params(
        &self,
        plugin: PluginInstanceId,
    ) -> impl Iterator<Item = (ParameterId, &SampleLane)> {
        self.params
            .iter()
            .filter(move |(p, _, _)| *p == plugin)
            .map(|(_, id, l)| (*id, l))
    }
}

#[derive(Debug)]
pub struct TimelineSnapshot {
    pub sample_rate: f64,
    pub timeline: Timeline,
    lanes: Vec<(TrackId, Lane)>,
    automation: Vec<(TrackId, TrackAutomation)>,
}

impl TimelineSnapshot {
    pub fn empty(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            timeline: Timeline::default(),
            lanes: Vec::new(),
            automation: Vec::new(),
        }
    }

    /// Automation of a track (binary search; realtime-safe).
    #[inline]
    pub fn automation(&self, track: TrackId) -> Option<&TrackAutomation> {
        self.automation
            .binary_search_by_key(&track, |(t, _)| *t)
            .ok()
            .map(|i| &self.automation[i].1)
    }

    /// Lane of a track (binary search; realtime-safe).
    #[inline]
    pub fn lane(&self, track: TrackId) -> Option<&Lane> {
        self.lanes
            .binary_search_by_key(&track, |(t, _)| *t)
            .ok()
            .map(|i| &self.lanes[i].1)
    }

    pub fn region_count(&self) -> usize {
        self.lanes
            .iter()
            .map(|(_, l)| l.audio.len() + l.midi.len())
            .sum()
    }

    /// The disk-loading plan for this snapshot's streamed regions.
    pub fn stream_plan(&self) -> StreamPlan {
        let mut regions = Vec::new();
        for (_, lane) in &self.lanes {
            for r in &lane.audio {
                if let Source::Stream(s) = &r.source {
                    regions.push(StreamRegion {
                        source: Arc::clone(s),
                        start: r.start,
                        end: r.end,
                        source_start: r.source_start,
                        step: r.step,
                        reversed: r.reversed,
                    });
                }
            }
        }
        StreamPlan { regions }
    }

    /// Build from the project (control thread; allocates).
    pub fn build(project: &Project, sources: &SourceMap, sample_rate: u32) -> Self {
        Self::build_with(project, sources, sample_rate, &HashSet::new())
    }

    /// Like [`TimelineSnapshot::build`]; lanes in `suspended` are being
    /// written by the user and do not drive their parameter.
    pub fn build_with(
        project: &Project,
        sources: &SourceMap,
        sample_rate: u32,
        suspended: &HashSet<AutomationLaneId>,
    ) -> Self {
        let sr = sample_rate as f64;
        let project_rate = project.sample_rate.max(1) as f64;
        let tl = &project.timeline;
        let mut lanes: HashMap<TrackId, Lane> = HashMap::new();
        for clip in project.clips.values() {
            if clip.muted {
                continue;
            }
            let start = tl.to_samples(clip.start, sr);
            match &clip.content {
                ClipContent::Audio(a) => {
                    let region = AudioPiece {
                        source: a.source,
                        source_offset: a.source_offset,
                        rel_start: 0,
                        length: a.length,
                        gain_db: a.gain_db,
                        fade_in: (a.fades.fade_in, a.fades.fade_in_shape),
                        fade_out: (a.fades.fade_out, a.fades.fade_out_shape),
                        reversed: a.reversed,
                    }
                    .region(start, sources, sr, project_rate);
                    if let Some(r) = region {
                        lanes.entry(clip.track).or_default().audio.push(r);
                    }
                }
                ClipContent::Takes(f) => {
                    for piece in comp_pieces(f) {
                        if let Some(r) = piece.region(start, sources, sr, project_rate) {
                            lanes.entry(clip.track).or_default().audio.push(r);
                        }
                    }
                }
                ClipContent::Midi(m) => {
                    let clip_end = clip.start + m.length;
                    let end = tl.to_samples(clip_end, sr);
                    let mut events = Vec::with_capacity(m.notes.len() * 2);
                    for n in &m.notes {
                        let on_pos = clip.start + n.start;
                        if on_pos >= clip_end {
                            continue;
                        }
                        let off_pos = (on_pos + n.length).min(clip_end);
                        let on = tl.to_samples(on_pos, sr);
                        let off = tl.to_samples(off_pos, sr).max(on + 1);
                        events.push((
                            on,
                            MidiEvent::NoteOn {
                                channel: n.channel,
                                key: n.key,
                                velocity: n.velocity.max(1),
                            },
                        ));
                        events.push((
                            off,
                            MidiEvent::NoteOff {
                                channel: n.channel,
                                key: n.key,
                                velocity: 0,
                            },
                        ));
                    }
                    events.sort_by_key(|(t, e)| (*t, !matches!(e, MidiEvent::NoteOff { .. })));
                    lanes.entry(clip.track).or_default().midi.push(MidiRegion {
                        start,
                        end,
                        events,
                    });
                }
            }
        }
        let mut lanes: Vec<(TrackId, Lane)> = lanes.into_iter().collect();
        for (_, lane) in &mut lanes {
            lane.audio.sort_by_key(|r| r.start);
            lane.midi.sort_by_key(|r| r.start);
        }
        lanes.sort_by_key(|(t, _)| *t);
        let mut automation: Vec<(TrackId, TrackAutomation)> = Vec::new();
        for t in &project.tracks {
            let mut a = TrackAutomation::default();
            for lane in &t.automation.lanes {
                if lane.mode == AutomationMode::Off
                    || lane.curve.is_empty()
                    || suspended.contains(&lane.id)
                {
                    continue;
                }
                let rt = SampleLane::from_curve(&lane.curve, |m| tl.to_samples(m, sr));
                match lane.target {
                    AutomationTarget::TrackVolume => a.volume = Some(rt),
                    AutomationTarget::TrackPan => a.pan = Some(rt),
                    AutomationTarget::TrackMute => a.mute = Some(rt),
                    AutomationTarget::SendLevel(s) => a.sends.push((s, rt)),
                    AutomationTarget::PluginParameter { plugin, parameter } => {
                        a.params.push((plugin, parameter, rt));
                    }
                    AutomationTarget::PluginBypass(p) => a.bypass.push((p, rt)),
                }
            }
            if !a.is_empty() {
                automation.push((t.id, a));
            }
        }
        automation.sort_by_key(|(t, _)| *t);
        Self {
            sample_rate: sr,
            timeline: project.timeline.clone(),
            lanes,
            automation,
        }
    }
}

/// Part of an audio source placed relative to a clip start (project frames).
struct AudioPiece {
    source: AudioSourceId,
    /// Source frame (project rate) at `rel_start`.
    source_offset: i64,
    rel_start: i64,
    length: i64,
    gain_db: f32,
    fade_in: (i64, FadeShape),
    fade_out: (i64, FadeShape),
    reversed: bool,
}

impl AudioPiece {
    fn region(
        &self,
        clip_start: i64,
        sources: &SourceMap,
        sr: f64,
        project_rate: f64,
    ) -> Option<AudioRegion> {
        let source = sources.get(&self.source)?;
        // Clip offsets/lengths are project-rate frames.
        let to_engine = |frames: i64| (frames as f64 * sr / project_rate).round() as i64;
        let length = to_engine(self.length);
        if length <= 0 {
            return None;
        }
        let start = clip_start + to_engine(self.rel_start);
        let (source_start, step) = match source {
            Source::Memory(_) => (to_engine(self.source_offset), 1.0),
            Source::Stream(s) => {
                let file_rate = s.sample_rate().max(1) as f64;
                (
                    (self.source_offset as f64 * file_rate / project_rate).round() as i64,
                    file_rate / sr,
                )
            }
        };
        Some(AudioRegion {
            start,
            end: start + length,
            source: source.clone(),
            source_start,
            step,
            gain: db_to_gain(self.gain_db),
            fade_in: to_engine(self.fade_in.0).min(length),
            fade_out: to_engine(self.fade_out.0).min(length),
            fade_in_shape: self.fade_in.1,
            fade_out_shape: self.fade_out.1,
            reversed: self.reversed,
        })
    }
}

/// Frames of fade at comp edges that do not crossfade (declick).
const DECLICK: i64 = 64;

/// The pieces a take folder's comp plays, with centred equal-power
/// crossfades where two pieces touch.
fn comp_pieces(f: &TakeFolder) -> Vec<AudioPiece> {
    let pieces = f.pieces();
    let half = f.crossfade.max(0) / 2;
    let mut out = Vec::with_capacity(pieces.len());
    for (i, p) in pieces.iter().enumerate() {
        let take = &f.takes[p.take];
        let touches_prev = i > 0 && pieces[i - 1].end == p.start;
        let touches_next = pieces.get(i + 1).is_some_and(|n| n.start == p.end);
        let a = if touches_prev {
            (p.start - half).max(take.start)
        } else {
            p.start
        };
        let b = if touches_next {
            (p.end + half).min(take.end)
        } else {
            p.end
        };
        let fade_in = if touches_prev {
            (p.start + half - a, FadeShape::EqualPower)
        } else if p.start == 0 {
            (f.fades.fade_in, f.fades.fade_in_shape)
        } else {
            (DECLICK, FadeShape::Linear)
        };
        let fade_out = if touches_next {
            (b - (p.end - half), FadeShape::EqualPower)
        } else if p.end == f.length {
            (f.fades.fade_out, f.fades.fade_out_shape)
        } else {
            (DECLICK, FadeShape::Linear)
        };
        out.push(AudioPiece {
            source: take.source,
            source_offset: take.source_offset + a,
            rel_start: a,
            length: b - a,
            gain_db: f.gain_db + take.gain_db,
            fade_in,
            fade_out,
            reversed: false,
        });
    }
    out
}

/// A streamed region as seen by the disk loader.
#[derive(Clone, Debug)]
pub struct StreamRegion {
    pub source: Arc<StreamSource>,
    pub start: i64,
    pub end: i64,
    pub source_start: i64,
    pub step: f64,
    pub reversed: bool,
}

impl StreamRegion {
    fn pages(&self, a: i64, b: i64) -> Option<Range<usize>> {
        let a = a.max(self.start);
        let b = b.min(self.end);
        if b <= a {
            return None;
        }
        let len = ((self.end - self.start) as f64 * self.step).ceil() as i64;
        let ra = ((a - self.start) as f64 * self.step).floor() as i64;
        let rb = ((b - self.start) as f64 * self.step).ceil() as i64 + 2;
        let (sa, sb) = if self.reversed {
            (self.source_start + len - rb, self.source_start + len - ra)
        } else {
            (self.source_start + ra, self.source_start + rb)
        };
        let r = self.source.page_range(sa, sb);
        (!r.is_empty()).then_some(r)
    }
}

/// Which pages of which files are needed for which timeline windows.
#[derive(Clone, Debug, Default)]
pub struct StreamPlan {
    regions: Vec<StreamRegion>,
}

impl StreamPlan {
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn regions(&self) -> &[StreamRegion] {
        &self.regions
    }

    /// Distinct streamed sources.
    pub fn sources(&self) -> Vec<Arc<StreamSource>> {
        let mut out: Vec<Arc<StreamSource>> = Vec::new();
        for r in &self.regions {
            if !out.iter().any(|s| Arc::ptr_eq(s, &r.source)) {
                out.push(Arc::clone(&r.source));
            }
        }
        out
    }

    /// Page ranges per source needed for timeline windows `[a, b)`.
    pub fn needed(&self, windows: &[(i64, i64)]) -> Vec<(Arc<StreamSource>, Vec<Range<usize>>)> {
        let mut out: Vec<(Arc<StreamSource>, Vec<Range<usize>>)> = Vec::new();
        for r in &self.regions {
            for &(a, b) in windows {
                if let Some(range) = r.pages(a, b) {
                    match out.iter_mut().find(|(s, _)| Arc::ptr_eq(s, &r.source)) {
                        Some((_, v)) => v.push(range),
                        None => out.push((Arc::clone(&r.source), vec![range])),
                    }
                }
            }
        }
        out
    }

    /// Load the pages for `windows` (loader thread). Returns pages loaded.
    pub fn ensure(
        &self,
        windows: &[(i64, i64)],
        epoch: &Epoch,
        reclaimer: &mut Reclaimer<Page>,
        scratch: &mut Vec<u8>,
    ) -> std::io::Result<usize> {
        let mut loaded = 0;
        for (source, ranges) in self.needed(windows) {
            for r in ranges {
                loaded += source.ensure(r, epoch, reclaimer, scratch)?;
            }
        }
        Ok(loaded)
    }

    /// Evict every page not needed for `keep` windows (loader thread).
    pub fn evict_outside(
        &self,
        keep: &[(i64, i64)],
        epoch: &Epoch,
        reclaimer: &mut Reclaimer<Page>,
    ) -> usize {
        let needed = self.needed(keep);
        let mut evicted = 0;
        for s in self.sources() {
            let ranges = needed
                .iter()
                .find(|(n, _)| Arc::ptr_eq(n, &s))
                .map(|(_, r)| r.clone())
                .unwrap_or_default();
            evicted += s.evict_except(&ranges, epoch, reclaimer);
        }
        evicted
    }

    /// Resident bytes across all streamed sources.
    pub fn resident_bytes(&self) -> usize {
        self.sources().iter().map(|s| s.resident_bytes()).sum()
    }
}

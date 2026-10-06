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
use faderframe_project::{
    ClipContent, ExpressionKind, FadeShape, MidiClip, MpeConfig, Project, SourceSpec, TakeFolder,
    WarpAlgorithm,
};
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
    /// Drawn bends of the fade curves (−1…1).
    pub fade_in_bend: f32,
    pub fade_out_bend: f32,
    pub reversed: bool,
}

impl AudioRegion {
    /// Clip gain including fades at timeline sample `t`.
    #[inline]
    pub fn gain_at(&self, t: i64) -> f32 {
        let mut g = self.gain;
        let into = t - self.start;
        if self.fade_in > 0 && into < self.fade_in {
            g *= self
                .fade_in_shape
                .gain_bent(into as f32 / self.fade_in as f32, self.fade_in_bend);
        }
        let left = self.end - t;
        if self.fade_out > 0 && left < self.fade_out {
            g *= self
                .fade_out_shape
                .gain_bent(left as f32 / self.fade_out as f32, self.fade_out_bend);
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

/// How a warped region is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpMode {
    /// Resampled along the time map: pitch follows the speed.
    Varispeed,
    /// Pitch-preserving time stretching.
    Stretch(faderframe_stretch::Preset),
    /// Pitch-synchronous grains (pitch-edited clips): exact pitch, kept
    /// formants, through the time map.
    Psola,
}

/// An audio clip played through a warp time map.
#[derive(Debug)]
pub struct WarpedRegion {
    /// Identity of the clip (stretcher voices follow it across blocks).
    pub key: u64,
    /// Bounds, source, gain and fades (its `source_start`/`step` are the
    /// unwarped values and unused).
    pub region: AudioRegion,
    /// Anchors: output frame relative to `region.start` → source frame (in
    /// the source's own rate); strictly increasing, from 0 to the length.
    pub points: Vec<(i64, f64)>,
    /// Source frames inside the clip; outside them the stretcher hears
    /// silence (no bleed from the rest of the file).
    pub source_lo: f64,
    pub source_hi: f64,
    pub mode: WarpMode,
    /// Pitch correction when the source rate differs from the engine rate
    /// (stretching plays source frames at the engine rate).
    pub transpose: f32,
    /// A pitch edit's corrections.
    pub pitch: Option<PitchCurve>,
}

/// A pitch edit as played: its corrections at regular source frames.
#[derive(Debug)]
pub struct PitchCurve {
    /// Source frame (the source's own rate) of the first value, and
    /// source frames between values.
    pub start: f64,
    pub step: f64,
    /// (semitones, formant semitones, the sung pitch in Hz or 0).
    pub values: Vec<(f32, f32, f32)>,
    pub keep_formants: bool,
}

impl PitchCurve {
    /// The correction at source frame `source` (interpolated; none
    /// outside the curve). Realtime-safe.
    #[inline]
    pub fn at(&self, source: f64) -> (f32, f32, f32) {
        let x = (source - self.start) / self.step.max(1e-9);
        if x < 0.0 || self.values.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let i = x.floor() as usize;
        let Some(a) = self.values.get(i) else {
            return (0.0, 0.0, 0.0);
        };
        let b = self.values.get(i + 1).unwrap_or(a);
        let f = (x - i as f64) as f32;
        // The sung pitch only between voiced values.
        let hz = if a.2 > 0.0 && b.2 > 0.0 {
            a.2 + (b.2 - a.2) * f
        } else {
            a.2.max(b.2)
        };
        (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, hz)
    }
}

impl WarpedRegion {
    /// Segment index for output frame `rel` (the first or last segment
    /// outside the anchors).
    #[inline]
    fn segment(&self, rel: f64) -> usize {
        let n = self.points.len();
        if n < 2 {
            return 0;
        }
        self.points
            .partition_point(|p| (p.0 as f64) <= rel)
            .saturating_sub(1)
            .min(n - 2)
    }

    /// Source frame at output frame `rel` (extrapolated linearly beyond
    /// the ends). Realtime-safe.
    #[inline]
    pub fn source_at(&self, rel: f64) -> f64 {
        if self.points.len() < 2 {
            return self.points.first().map_or(rel, |p| p.1 + rel - p.0 as f64);
        }
        let i = self.segment(rel);
        let (a, b) = (self.points[i], self.points[i + 1]);
        a.1 + (rel - a.0 as f64) * (b.1 - a.1) / (b.0 - a.0).max(1) as f64
    }

    /// Source frames per output frame at `rel`.
    #[inline]
    pub fn rate_at(&self, rel: f64) -> f64 {
        if self.points.len() < 2 {
            return 1.0;
        }
        let i = self.segment(rel);
        let (a, b) = (self.points[i], self.points[i + 1]);
        (b.1 - a.1) / (b.0 - a.0).max(1) as f64
    }
}

#[derive(Debug)]
pub struct MidiRegion {
    pub start: i64,
    pub end: i64,
    /// Absolute-sample events, sorted (note-offs first at equal times).
    pub events: Vec<(i64, MidiEvent)>,
    /// SysEx messages (`F0 … F7`) for the track's plugins, sorted.
    pub sysex: Vec<(i64, Box<[u8]>)>,
}

/// A launcher slot's clip as played: its lane from sample 0, looping over
/// `length` samples.
#[derive(Debug)]
pub struct LaunchLane {
    pub slot: u64,
    pub track: TrackId,
    pub length: i64,
    pub lane: Lane,
}

#[derive(Debug, Default)]
pub struct Lane {
    /// Sorted by start.
    pub audio: Vec<AudioRegion>,
    /// Warped audio clips, sorted by start.
    pub warped: Vec<WarpedRegion>,
    /// Sorted by start.
    pub midi: Vec<MidiRegion>,
    /// The track's instrument speaks MPE (the player announces the zone).
    pub mpe: Option<MpeConfig>,
}

/// One MIDI value of a note's expression on its channel (`None` for what
/// MPE does not carry).
fn expression_event(
    kind: ExpressionKind,
    channel: u8,
    value: f32,
    cfg: MpeConfig,
) -> Option<MidiEvent> {
    let seven = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round() as u8;
    Some(match kind {
        ExpressionKind::Pitch => MidiEvent::PitchBend {
            channel,
            value: (8192.0 + value / cfg.bend_range.max(1) as f32 * 8192.0)
                .round()
                .clamp(0.0, 16383.0) as u16,
        },
        ExpressionKind::Pressure => MidiEvent::ChannelPressure {
            channel,
            pressure: seven(value),
        },
        ExpressionKind::Timbre => MidiEvent::ControlChange {
            channel,
            controller: 74,
            value: seven(value),
        },
        ExpressionKind::Volume
        | ExpressionKind::Pan
        | ExpressionKind::Vibrato
        | ExpressionKind::Expression => return None,
    })
}

/// Native note expression: the curves of a note's expression as
/// [`MidiEvent::NoteExpression`]s for hosted plugins — the value at the
/// note-on (sorted after it), then sampled every 1/128 quarter and sent
/// where it moves by the kind's resolution or more.
fn native_expression_events(
    e: &faderframe_project::NoteExpression,
    note: (u8, u8),
    on_pos: faderframe_timeline::MusicalTime,
    length: faderframe_timeline::MusicalTime,
    to_samples: impl Fn(faderframe_timeline::MusicalTime) -> i64,
    events: &mut Vec<(i64, MidiEvent)>,
) {
    use faderframe_midi::ExpressionValue;
    use faderframe_timeline::MusicalTime;
    let (channel, key) = note;
    let step = MusicalTime(faderframe_timeline::TICKS_PER_QUARTER / 128);
    let event = |kind: ExpressionKind, v: f32| MidiEvent::NoteExpression {
        channel,
        key,
        kind: kind.native(),
        value: ExpressionValue::new(v as f64),
    };
    for kind in ExpressionKind::ALL {
        if e.curve(kind).is_empty() {
            continue;
        }
        let threshold = kind.resolution();
        let mut last = e.value_at(kind, MusicalTime::ZERO);
        events.push((to_samples(on_pos), event(kind, last)));
        let mut t = step;
        while t < length {
            let v = e.value_at(kind, t);
            if (v - last).abs() >= threshold {
                events.push((to_samples(on_pos + t), event(kind, v)));
                last = v;
            }
            t += step;
        }
    }
}

/// MPE: every note gets a member channel of its own (the least recently
/// used free one, else the one freeing first), its expression's initial
/// values right before the note-on, and the curves as pitch bend, channel
/// pressure and CC 74 — sampled every 1/128 quarter, sent where the MIDI
/// value changes.
fn mpe_note_events(
    m: &MidiClip,
    clip_start: faderframe_timeline::MusicalTime,
    clip_end: faderframe_timeline::MusicalTime,
    cfg: MpeConfig,
    to_samples: impl Fn(faderframe_timeline::MusicalTime) -> i64,
    events: &mut Vec<(i64, MidiEvent)>,
) {
    use faderframe_timeline::MusicalTime;
    let members = cfg.members.clamp(1, 15) as usize;
    let mut free_at = [i64::MIN; 16];
    let mut used = [0u64; 16];
    let mut stamp = 0u64;
    let mut notes: Vec<_> = m
        .notes
        .iter()
        .filter(|n| !n.muted && clip_start + n.start < clip_end)
        .collect();
    notes.sort_by_key(|n| (n.start, n.key));
    let step = MusicalTime(faderframe_timeline::TICKS_PER_QUARTER / 128);
    for n in notes {
        let on_pos = clip_start + n.start;
        let off_pos = (on_pos + n.length).min(clip_end);
        let on = to_samples(on_pos);
        let off = to_samples(off_pos).max(on + 1);
        let ch = (1..=members)
            .filter(|&c| free_at[c] <= on)
            .min_by_key(|&c| used[c])
            .or_else(|| (1..=members).min_by_key(|&c| free_at[c]))
            .unwrap_or(1);
        stamp += 1;
        used[ch] = stamp;
        free_at[ch] = off;
        let channel = ch as u8;
        let expr = m.expression(n.id);
        let value = |k: ExpressionKind, t: MusicalTime| expr.map_or(k.rest(), |e| e.value_at(k, t));
        for k in ExpressionKind::MPE {
            if let Some(ev) = expression_event(k, channel, value(k, MusicalTime::ZERO), cfg) {
                events.push((on, ev));
            }
        }
        events.push((
            on,
            MidiEvent::NoteOn {
                channel,
                key: n.key,
                velocity: n.velocity.max(1),
            },
        ));
        if let Some(e) = expr {
            let length = off_pos - on_pos;
            for k in ExpressionKind::MPE {
                if e.curve(k).is_empty() {
                    continue;
                }
                let mut last = expression_event(k, channel, value(k, MusicalTime::ZERO), cfg);
                let mut t = step;
                while t < length {
                    let ev = expression_event(k, channel, value(k, t), cfg);
                    if ev != last
                        && let Some(e) = ev
                    {
                        events.push((to_samples(on_pos + t), e));
                        last = ev;
                    }
                    t += step;
                }
            }
        }
        events.push((
            off,
            MidiEvent::NoteOff {
                channel,
                key: n.key,
                velocity: 0,
            },
        ));
    }
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
    /// Automated faders (dB) of the VCAs scaling the track.
    pub vca_volume: Vec<SampleLane>,
    /// Automated mutes of the VCAs scaling the track.
    pub vca_mute: Vec<SampleLane>,
    pub sends: Vec<(SendId, SampleLane)>,
    pub params: Vec<(PluginInstanceId, ParameterId, SampleLane)>,
    pub bypass: Vec<(PluginInstanceId, SampleLane)>,
}

impl TrackAutomation {
    pub fn is_empty(&self) -> bool {
        self.volume.is_none()
            && self.pan.is_none()
            && self.mute.is_none()
            && self.vca_volume.is_empty()
            && self.vca_mute.is_empty()
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

/// Does the lane drive its target (rather than the static value)?
pub(crate) fn drives(
    lane: &faderframe_automation::AutomationLane,
    suspended: &HashSet<faderframe_core::AutomationLaneId>,
) -> bool {
    lane.mode != AutomationMode::Off && !lane.curve.is_empty() && !suspended.contains(&lane.id)
}

#[derive(Debug)]
pub struct TimelineSnapshot {
    pub sample_rate: f64,
    pub timeline: Timeline,
    lanes: Vec<(TrackId, Lane)>,
    /// The launcher's clips, each in a lane of its own from sample 0, by
    /// slot (see [`crate::launch`]).
    launch: Vec<LaunchLane>,
    automation: Vec<(TrackId, TrackAutomation)>,
    /// The key and chord track in samples (MIDI effects follow them).
    pub harmony: faderframe_plugin_host::Harmony,
}

impl TimelineSnapshot {
    pub fn empty(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            timeline: Timeline::default(),
            lanes: Vec::new(),
            launch: Vec::new(),
            automation: Vec::new(),
            harmony: faderframe_plugin_host::Harmony::default(),
        }
    }

    /// A launcher slot's clip.
    pub fn launch_lane(&self, slot: u64) -> Option<&LaunchLane> {
        self.launch
            .binary_search_by_key(&slot, |l| l.slot)
            .ok()
            .map(|i| &self.launch[i])
    }

    /// The slots that have clips.
    pub fn launch_slots(&self) -> impl Iterator<Item = u64> + '_ {
        self.launch.iter().map(|l| l.slot)
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
            .map(|(_, l)| l.audio.len() + l.warped.len() + l.midi.len())
            .sum()
    }

    /// The disk-loading plan for this snapshot's streamed regions.
    pub fn stream_plan(&self) -> StreamPlan {
        let mut regions = Vec::new();
        for (_, lane) in &self.lanes {
            Self::plan_lane(lane, self.sample_rate, false, &mut regions);
        }
        // The launcher's clips can start any moment: always loaded.
        for l in &self.launch {
            Self::plan_lane(&l.lane, self.sample_rate, true, &mut regions);
        }
        StreamPlan { regions }
    }

    /// A lane's streamed regions (`pinned`: needed wherever the playhead).
    fn plan_lane(lane: &Lane, sample_rate: f64, pinned: bool, regions: &mut Vec<StreamRegion>) {
        for r in &lane.audio {
            if let Source::Stream(s) = &r.source {
                regions.push(StreamRegion {
                    source: Arc::clone(s),
                    start: r.start,
                    end: r.end,
                    source_start: r.source_start,
                    step: r.step,
                    reversed: r.reversed,
                    pinned,
                });
            }
        }
        // Warped clips: one linear piece per warp segment, widened by the
        // stretcher's look-ahead and pre-roll (~0.25 s).
        for w in &lane.warped {
            let Source::Stream(s) = &w.region.source else {
                continue;
            };
            let margin = (sample_rate as i64 / 4).max(1);
            for (k, seg) in w.points.windows(2).enumerate() {
                let (a, b) = (seg[0], seg[1]);
                let step = (b.1 - a.1) / (b.0 - a.0).max(1) as f64;
                let first = k == 0;
                let last = k + 2 == w.points.len();
                let start = w.region.start + a.0 - if first { margin } else { 0 };
                let end = w.region.start + b.0 + if last { margin } else { 0 };
                let source_start =
                    (a.1 - if first { margin as f64 * step } else { 0.0 }).floor() as i64;
                regions.push(StreamRegion {
                    source: Arc::clone(s),
                    start,
                    end,
                    source_start: source_start.max(0),
                    step: step.max(1e-3),
                    reversed: false,
                    pinned,
                });
            }
        }
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
        let mpe_of: HashMap<TrackId, MpeConfig> = project
            .tracks
            .iter()
            .filter_map(|t| t.mpe.map(|m| (t.id, m)))
            .collect();
        for (&track, &cfg) in &mpe_of {
            lanes.entry(track).or_default().mpe = Some(cfg);
        }
        // Frozen tracks play their rendered audio instead of their clips.
        let frozen: HashSet<TrackId> = project
            .tracks
            .iter()
            .filter(|t| t.freeze.is_some())
            .map(|t| t.id)
            .collect();
        for t in project.tracks.iter() {
            let Some(f) = &t.freeze else { continue };
            let piece = AudioPiece {
                source: f.source,
                source_offset: 0,
                rel_start: 0,
                length: f.length,
                gain_db: 0.0,
                fade_in: (0, FadeShape::Linear, 0),
                fade_out: (0, FadeShape::Linear, 0),
                reversed: false,
            };
            if let Some(r) = piece.region(tl.to_samples(f.start, sr), sources, sr, project_rate) {
                lanes.entry(t.id).or_default().audio.push(r);
            }
        }
        // The arrangement's clips, then the launcher's: from the song's
        // start, each in a lane of its own.
        let in_slots: HashMap<faderframe_core::ClipId, u64> = project
            .launcher
            .slots
            .iter()
            .map(|(k, c)| (*c, k.hash()))
            .collect();
        let slot_clips: Vec<faderframe_project::Clip> = in_slots
            .keys()
            .filter_map(|id| project.clips.get(id))
            .map(|c| faderframe_project::Clip {
                start: faderframe_timeline::MusicalTime::ZERO,
                ..c.clone()
            })
            .collect();
        let mut launch_lanes: HashMap<u64, Lane> = HashMap::new();
        let all = project
            .clips
            .values()
            .filter(|c| !in_slots.contains_key(&c.id))
            .chain(slot_clips.iter());
        for clip in all {
            if clip.muted || frozen.contains(&clip.track) {
                continue;
            }
            let slot = in_slots.get(&clip.id).copied();
            let start = tl.to_samples(clip.start, sr);
            match &clip.content {
                ClipContent::Audio(a) => {
                    let region = AudioPiece {
                        source: a.source,
                        source_offset: a.source_offset,
                        rel_start: 0,
                        length: a.length,
                        gain_db: a.gain_db,
                        fade_in: (a.fades.fade_in, a.fades.fade_in_shape, a.fades.fade_in_bend),
                        fade_out: (
                            a.fades.fade_out,
                            a.fades.fade_out_shape,
                            a.fades.fade_out_bend,
                        ),
                        reversed: a.reversed,
                    }
                    .region(start, sources, sr, project_rate);
                    let Some(r) = region else { continue };
                    let lane = lane_for(&mut lanes, &mut launch_lanes, clip.track, slot);
                    let pitched = a.pitch.as_ref().is_some_and(|e| e.edited());
                    match a.warp.as_ref() {
                        Some(w) if !a.reversed && !w.is_identity(a.source_offset, a.length) => {
                            lane.warped.push(warped_region(
                                clip.id.raw(),
                                r,
                                a,
                                w,
                                sr,
                                project_rate,
                            ));
                        }
                        // Pitch edited: through a stretcher, 1:1 in time.
                        _ if pitched && !a.reversed => {
                            let w = faderframe_project::Warp::uniform(a.length);
                            lane.warped.push(warped_region(
                                clip.id.raw(),
                                r,
                                a,
                                &w,
                                sr,
                                project_rate,
                            ));
                        }
                        _ => lane.audio.push(r),
                    }
                }
                ClipContent::Takes(f) => {
                    for piece in comp_pieces(f) {
                        if let Some(r) = piece.region(start, sources, sr, project_rate) {
                            lane_for(&mut lanes, &mut launch_lanes, clip.track, slot)
                                .audio
                                .push(r);
                        }
                    }
                }
                ClipContent::Midi(m) => {
                    let clip_end = clip.start + m.length;
                    let end = tl.to_samples(clip_end, sr);
                    let points: usize = m.controllers.iter().map(|l| l.points.len()).sum();
                    let mut events = Vec::with_capacity(m.notes.len() * 2 + points);
                    for lane in &m.controllers {
                        for p in &lane.points {
                            let at = clip.start + p.time;
                            if at >= clip_end {
                                break;
                            }
                            events.push((
                                tl.to_samples(at, sr),
                                lane.controller.event(lane.channel, p.value),
                            ));
                        }
                    }
                    if let Some(&cfg) = mpe_of.get(&clip.track) {
                        mpe_note_events(
                            m,
                            clip.start,
                            clip_end,
                            cfg,
                            |t| tl.to_samples(t, sr),
                            &mut events,
                        );
                    }
                    let plain = !mpe_of.contains_key(&clip.track);
                    for n in m.notes.iter().filter(|n| plain && !n.muted) {
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
                        if let Some(e) = m.expression(n.id) {
                            native_expression_events(
                                e,
                                (n.channel, n.key),
                                on_pos,
                                off_pos - on_pos,
                                |t| tl.to_samples(t, sr),
                                &mut events,
                            );
                        }
                    }
                    // At equal times: note-offs, then controllers (a pedal or
                    // bend set at a note's start applies to it), then
                    // note-ons, then the expressions addressed to them.
                    events.sort_by_key(|(t, e)| (*t, e.same_time_priority()));
                    let mut sysex: Vec<(i64, Box<[u8]>)> = m
                        .sysex
                        .iter()
                        .filter(|e| e.time < m.length && !e.data.is_empty())
                        .map(|e| {
                            (
                                tl.to_samples(clip.start + e.time, sr),
                                e.data.clone().into(),
                            )
                        })
                        .collect();
                    sysex.sort_by_key(|(t, _)| *t);
                    lane_for(&mut lanes, &mut launch_lanes, clip.track, slot)
                        .midi
                        .push(MidiRegion {
                            start,
                            end,
                            events,
                            sysex,
                        });
                }
            }
        }
        let mut lanes: Vec<(TrackId, Lane)> = lanes.into_iter().collect();
        for (_, lane) in &mut lanes {
            lane.audio.sort_by_key(|r| r.start);
            lane.warped.sort_by_key(|w| w.region.start);
            lane.midi.sort_by_key(|r| r.start);
        }
        let mut launch: Vec<LaunchLane> = slot_clips
            .iter()
            .filter_map(|c| {
                let slot = *in_slots.get(&c.id)?;
                let mut lane = launch_lanes.remove(&slot)?;
                lane.audio.sort_by_key(|r| r.start);
                lane.warped.sort_by_key(|w| w.region.start);
                lane.midi.sort_by_key(|r| r.start);
                lane.mpe = mpe_of.get(&c.track).copied();
                let length = tl.to_samples(c.end(tl, project.sample_rate), sr).max(1);
                Some(LaunchLane {
                    slot,
                    track: c.track,
                    length,
                    lane,
                })
            })
            .collect();
        launch.sort_by_key(|l| l.slot);
        lanes.sort_by_key(|(t, _)| *t);
        let mut automation: Vec<(TrackId, TrackAutomation)> = Vec::new();
        for t in &project.tracks {
            let mut a = TrackAutomation::default();
            if t.kind.has_audio() {
                for v in project.vca_chain(t) {
                    for lane in v.automation.lanes.iter().filter(|l| drives(l, suspended)) {
                        let rt = SampleLane::from_curve(&lane.curve, |m| tl.to_samples(m, sr));
                        match lane.target {
                            AutomationTarget::TrackVolume => a.vca_volume.push(rt),
                            AutomationTarget::TrackMute => a.vca_mute.push(rt),
                            _ => {}
                        }
                    }
                }
            }
            for lane in &t.automation.lanes {
                if !drives(lane, suspended) {
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
        let harmony = faderframe_plugin_host::Harmony {
            keys: project
                .keys
                .iter()
                .map(|k| (tl.to_samples(k.at, sr), k.key))
                .collect(),
            chords: project
                .chords
                .iter()
                .map(|c| {
                    (
                        tl.to_samples(c.start, sr),
                        tl.to_samples(c.end, sr),
                        c.chord,
                    )
                })
                .collect(),
        };
        Self {
            sample_rate: sr,
            timeline: project.timeline.clone(),
            lanes,
            launch,
            automation,
            harmony,
        }
    }
}

/// The lane a clip's regions go to: its track's, or its launcher slot's.
fn lane_for<'a>(
    lanes: &'a mut HashMap<TrackId, Lane>,
    launch: &'a mut HashMap<u64, Lane>,
    track: TrackId,
    slot: Option<u64>,
) -> &'a mut Lane {
    match slot {
        Some(s) => launch.entry(s).or_default(),
        None => lanes.entry(track).or_default(),
    }
}

/// A warped clip's time map in engine units (output: engine frames from
/// the clip start; source: the source's own frames).
fn warped_region(
    key: u64,
    region: AudioRegion,
    a: &faderframe_project::AudioClip,
    w: &faderframe_project::Warp,
    sr: f64,
    project_rate: f64,
) -> WarpedRegion {
    let source_rate = match &region.source {
        Source::Memory(_) => sr,
        Source::Stream(s) => s.sample_rate().max(1) as f64,
    };
    let out = sr / project_rate;
    let src = source_rate / project_rate;
    let len = region.end - region.start;
    let mut points: Vec<(i64, f64)> = w
        .points(a.source_offset, a.length)
        .into_iter()
        .map(|m| ((m.at as f64 * out).round() as i64, m.source as f64 * src))
        .collect();
    // Exact ends, strictly increasing output frames.
    if let Some(last) = points.last_mut() {
        last.0 = len;
    }
    points.dedup_by(|b, a| b.0 <= a.0);
    let channels = region.source.channels();
    let pitch = a
        .pitch
        .as_ref()
        .filter(|e| e.edited())
        .map(|e| pitch_curve(e, a, src, project_rate));
    let mode = match w.algorithm {
        _ if pitch.is_some() => WarpMode::Psola,
        _ if channels > faderframe_stretch::MAX_CHANNELS => WarpMode::Varispeed,
        WarpAlgorithm::Varispeed => WarpMode::Varispeed,
        WarpAlgorithm::Polyphonic => WarpMode::Stretch(faderframe_stretch::Preset::Polyphonic),
        WarpAlgorithm::Rhythmic => WarpMode::Stretch(faderframe_stretch::Preset::Rhythmic),
    };
    WarpedRegion {
        key,
        source_lo: a.source_offset as f64 * src,
        source_hi: (a.source_offset + w.source_length) as f64 * src,
        points,
        mode,
        transpose: (source_rate / sr) as f32,
        pitch,
        region,
    }
}

/// A clip's pitch corrections over its source range, every curve hop.
fn pitch_curve(
    e: &faderframe_project::pitch::PitchEdit,
    a: &faderframe_project::AudioClip,
    src: f64,
    project_rate: f64,
) -> PitchCurve {
    let hop = i64::from(e.hop.max(1));
    let from = a.source_offset - hop;
    let count = (a.source_span() / hop + 3) as usize;
    let values = (0..count)
        .map(|k| {
            let at = from + k as i64 * hop;
            let (st, fm) = e.correction_at(at, project_rate);
            let hz = e
                .note_at(at)
                .and_then(|n| e.notes[n].sung_at(at, e.hop))
                .map_or(0.0, |m| 440.0 * 2f32.powf((m - 69.0) / 12.0));
            (st, fm, hz)
        })
        .collect();
    PitchCurve {
        start: from as f64 * src,
        step: hop as f64 * src,
        values,
        keep_formants: e.keep_formants,
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
    /// Length, shape and drawn bend (percent).
    fade_in: (i64, FadeShape, i16),
    fade_out: (i64, FadeShape, i16),
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
            fade_in_bend: faderframe_project::bend_factor(self.fade_in.2),
            fade_out_bend: faderframe_project::bend_factor(self.fade_out.2),
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
            (p.start + half - a, FadeShape::EqualPower, 0)
        } else if p.start == 0 {
            (f.fades.fade_in, f.fades.fade_in_shape, f.fades.fade_in_bend)
        } else {
            (DECLICK, FadeShape::Linear, 0)
        };
        let fade_out = if touches_next {
            (b - (p.end - half), FadeShape::EqualPower, 0)
        } else if p.end == f.length {
            (
                f.fades.fade_out,
                f.fades.fade_out_shape,
                f.fades.fade_out_bend,
            )
        } else {
            (DECLICK, FadeShape::Linear, 0)
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
    /// Needed wherever the playhead is (launcher clips).
    pub pinned: bool,
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
            let whole = [(r.start, r.end)];
            let windows = if r.pinned { &whole[..] } else { windows };
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

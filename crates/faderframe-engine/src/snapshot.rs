//! Immutable, sample-domain view of the timeline for the realtime thread.
//!
//! The project stores clips in musical time. Whenever clips, notes, the tempo
//! map or the engine sample rate change, the control thread builds a new
//! [`TimelineSnapshot`] (clip regions and MIDI events in absolute samples)
//! and hands it to the audio thread, which swaps it in at a block boundary.

use faderframe_audio_files::{AudioData, generate};
use faderframe_core::{AudioSourceId, TrackId, db_to_gain};
use faderframe_midi::MidiEvent;
use faderframe_project::{ClipContent, FadeShape, Project, SourceSpec};
use faderframe_timeline::Timeline;
use std::collections::HashMap;
use std::sync::Arc;

/// Decoded audio per source, at the engine sample rate.
pub type SourceMap = HashMap<AudioSourceId, Arc<AudioData>>;

/// Render every generated source of `project` at `sample_rate`.
///
/// File sources are not decoded here (the disk-streaming subsystem is still
/// to come); they are skipped and their clips play silence.
pub fn render_generated_sources(project: &Project, sample_rate: u32) -> SourceMap {
    project
        .sources
        .values()
        .filter_map(|s| match &s.spec {
            SourceSpec::Generated { generator } => {
                Some((s.id, Arc::new(generate(generator, sample_rate))))
            }
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
    pub data: Arc<AudioData>,
    /// Source frame played at `start`.
    pub source_offset: i64,
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

#[derive(Debug)]
pub struct TimelineSnapshot {
    pub sample_rate: f64,
    pub timeline: Timeline,
    lanes: Vec<(TrackId, Lane)>,
}

impl TimelineSnapshot {
    pub fn empty(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            timeline: Timeline::default(),
            lanes: Vec::new(),
        }
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

    /// Build from the project (control thread; allocates).
    pub fn build(project: &Project, sources: &SourceMap, sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let ratio = sr / project.sample_rate.max(1) as f64;
        let tl = &project.timeline;
        let mut lanes: HashMap<TrackId, Lane> = HashMap::new();
        for clip in project.clips.values() {
            if clip.muted {
                continue;
            }
            let start = tl.to_samples(clip.start, sr);
            match &clip.content {
                ClipContent::Audio(a) => {
                    let Some(data) = sources.get(&a.source) else {
                        continue;
                    };
                    let scale = |frames: i64| (frames as f64 * ratio).round() as i64;
                    let length = scale(a.length);
                    if length <= 0 {
                        continue;
                    }
                    lanes
                        .entry(clip.track)
                        .or_default()
                        .audio
                        .push(AudioRegion {
                            start,
                            end: start + length,
                            data: Arc::clone(data),
                            source_offset: scale(a.source_offset),
                            gain: db_to_gain(a.gain_db),
                            fade_in: scale(a.fades.fade_in).min(length),
                            fade_out: scale(a.fades.fade_out).min(length),
                            fade_in_shape: a.fades.fade_in_shape,
                            fade_out_shape: a.fades.fade_out_shape,
                            reversed: a.reversed,
                        });
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
        Self {
            sample_rate: sr,
            timeline: project.timeline.clone(),
            lanes,
        }
    }
}

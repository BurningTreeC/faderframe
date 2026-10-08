#![allow(dead_code, clippy::unwrap_used)]

use faderframe_audio_files::{AudioData, GeneratorSpec};
use faderframe_core::{AudioSourceId, ChannelLayout, TrackId};
use faderframe_engine::SourceMap;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, OutputRouting, Project, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::sync::Arc;

/// Builder for small, exactly-checkable test projects.
pub struct TestProject {
    pub project: Project,
    pub sources: SourceMap,
}

impl TestProject {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            project: Project::new("test", sample_rate),
            sources: SourceMap::new(),
        }
    }

    pub fn master(&self) -> TrackId {
        self.project.master_id().unwrap()
    }

    /// Add a track before the master.
    pub fn track(&mut self, kind: TrackKind, name: &str, layout: ChannelLayout) -> TrackId {
        let id: TrackId = self.project.ids.allocate();
        let t = Track::new(id, kind, name, TrackColor::palette(0)).with_layout(layout);
        let at = self.project.tracks.len() - 1;
        self.project.tracks.insert(at, t);
        id
    }

    pub fn route(&mut self, track: TrackId, to: TrackId) {
        self.project.track_mut(track).unwrap().output = OutputRouting::Track { track: to };
    }

    /// Register custom source data (frames at the project rate).
    pub fn source(&mut self, data: AudioData) -> AudioSourceId {
        let id: AudioSourceId = self.project.ids.allocate();
        self.project.sources.insert(
            id,
            AudioSource {
                id,
                name: "test".into(),
                spec: SourceSpec::Generated {
                    generator: GeneratorSpec::Silence {
                        seconds: data.duration_seconds(),
                        channels: data.num_channels() as u16,
                    },
                },
            },
        );
        self.sources.insert(id, Arc::new(data).into());
        id
    }

    /// Disk-streamed source backed by the WAV file at `path`.
    pub fn stream(&mut self, path: &std::path::Path) -> AudioSourceId {
        let s = faderframe_audio_files::StreamSource::open(path).unwrap();
        let id: AudioSourceId = self.project.ids.allocate();
        self.project.sources.insert(
            id,
            AudioSource {
                id,
                name: "file".into(),
                spec: SourceSpec::File {
                    path: path.to_path_buf(),
                    channels: s.channels() as u16,
                    frames: s.frames() as i64,
                    sample_rate: s.sample_rate(),
                },
            },
        );
        self.sources
            .insert(id, faderframe_engine::Source::Stream(s));
        id
    }

    /// Clip with a source offset.
    pub fn clip_at(
        &mut self,
        track: TrackId,
        source: AudioSourceId,
        start: MusicalTime,
        offset: i64,
        length: i64,
    ) {
        self.clip(track, source, start, length);
        let id = *self.project.track(track).unwrap().clips.last().unwrap();
        if let Some(ClipContent::Audio(a)) = self.project.clips.get_mut(&id).map(|c| &mut c.content)
        {
            a.source_offset = offset;
        }
    }

    /// Constant-value source of `frames` frames.
    pub fn dc(&mut self, channels: usize, value: f32, frames: usize) -> AudioSourceId {
        let rate = self.project.sample_rate;
        self.source(AudioData::from_channels(
            rate,
            vec![vec![value; frames]; channels],
        ))
    }

    /// Single 1.0 sample at frame 0 followed by silence.
    pub fn impulse(&mut self, channels: usize, frames: usize) -> AudioSourceId {
        let rate = self.project.sample_rate;
        let mut ch = vec![0.0; frames];
        ch[0] = 1.0;
        self.source(AudioData::from_channels(rate, vec![ch; channels]))
    }

    pub fn clip(&mut self, track: TrackId, source: AudioSourceId, start: MusicalTime, length: i64) {
        let id = self.project.ids.allocate();
        self.project.clips.insert(
            id,
            Clip {
                id,
                track,
                name: "clip".into(),
                color: None,
                start,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source,
                    source_offset: 0,
                    length,
                    gain_db: 0.0,
                    fades: ClipFades::default(),
                    stretch: StretchSettings::Off,
                    reversed: false,
                    warp: None,
                    pitch: None,
                    effects: None,
                    spectral: None,
                }),
            },
        );
        self.project.track_mut(track).unwrap().clips.push(id);
    }
}

pub fn approx(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() <= eps
}

/// Indices of samples whose magnitude exceeds `threshold`.
pub fn hits(signal: &[f32], threshold: f32) -> Vec<usize> {
    signal
        .iter()
        .enumerate()
        .filter(|(_, v)| v.abs() > threshold)
        .map(|(i, _)| i)
        .collect()
}

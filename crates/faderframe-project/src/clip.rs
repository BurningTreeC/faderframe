use crate::TrackColor;
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_timeline::{MusicalTime, Timeline};
use serde::{Deserialize, Serialize};

/// A region on a track lane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub track: TrackId,
    pub name: String,
    /// Overrides the track colour when set.
    #[serde(default)]
    pub color: Option<TrackColor>,
    /// Timeline position of the clip start.
    pub start: MusicalTime,
    #[serde(default)]
    pub muted: bool,
    pub content: ClipContent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClipContent {
    Audio(AudioClip),
    Midi(MidiClip),
}

/// Audio region referencing part of an audio source.
///
/// Offsets and lengths are frames at the **project** sample rate. Sources at
/// other rates are converted on import; if the engine runs at a different
/// rate than the project, the engine scales positions (and sources are
/// rendered/resampled at the engine rate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    pub source: AudioSourceId,
    /// First source frame played.
    pub source_offset: i64,
    /// Played length in frames.
    pub length: i64,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub fades: ClipFades,
    #[serde(default)]
    pub stretch: StretchSettings,
    #[serde(default)]
    pub reversed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeShape {
    #[default]
    Linear,
    /// sin/cos — constant power, the usual crossfade shape.
    EqualPower,
    SCurve,
    Exponential,
}

impl FadeShape {
    /// Fade-in gain at fraction `t` (0..=1); fade-outs use `gain(1 - t)`.
    pub fn gain(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FadeShape::Linear => t,
            FadeShape::EqualPower => (t * std::f32::consts::FRAC_PI_2).sin(),
            FadeShape::SCurve => t * t * (3.0 - 2.0 * t),
            FadeShape::Exponential => t * t * t,
        }
    }
}

/// Fade lengths in frames (project rate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipFades {
    pub fade_in: i64,
    pub fade_out: i64,
    #[serde(default)]
    pub fade_in_shape: FadeShape,
    #[serde(default)]
    pub fade_out_shape: FadeShape,
}

/// Time-stretch / pitch settings (only `Off` is implemented so far; the
/// variant exists so the clip model already accommodates stretching).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum StretchSettings {
    #[default]
    Off,
}

/// MIDI region; notes are relative to the clip start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiClip {
    pub length: MusicalTime,
    #[serde(default)]
    pub notes: Vec<MidiNote>,
}

impl MidiClip {
    pub fn note(&self, id: NoteId) -> Option<&MidiNote> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn note_mut(&mut self, id: NoteId) -> Option<&mut MidiNote> {
        self.notes.iter_mut().find(|n| n.id == id)
    }

    /// Keep notes sorted by start then key (stable editing order).
    pub fn sort_notes(&mut self) {
        self.notes.sort_by_key(|n| (n.start, n.key, n.id));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiNote {
    pub id: NoteId,
    /// Relative to the clip start.
    pub start: MusicalTime,
    pub length: MusicalTime,
    pub key: u8,
    pub velocity: u8,
    #[serde(default)]
    pub channel: u8,
}

impl MidiNote {
    pub fn end(&self) -> MusicalTime {
        self.start + self.length
    }
}

impl Clip {
    /// Musical end position. Audio clips have a fixed length in samples, so
    /// their musical length depends on the tempo map.
    pub fn end(&self, timeline: &Timeline, project_rate: u32) -> MusicalTime {
        match &self.content {
            ClipContent::Audio(a) => {
                timeline.end_of_sample_span(self.start, a.length, project_rate as f64)
            }
            ClipContent::Midi(m) => self.start + m.length,
        }
    }

    pub fn is_audio(&self) -> bool {
        matches!(self.content, ClipContent::Audio(_))
    }

    pub fn as_midi(&self) -> Option<&MidiClip> {
        match &self.content {
            ClipContent::Midi(m) => Some(m),
            ClipContent::Audio(_) => None,
        }
    }

    pub fn as_midi_mut(&mut self) -> Option<&mut MidiClip> {
        match &mut self.content {
            ClipContent::Midi(m) => Some(m),
            ClipContent::Audio(_) => None,
        }
    }

    pub fn as_audio(&self) -> Option<&AudioClip> {
        match &self.content {
            ClipContent::Audio(a) => Some(a),
            ClipContent::Midi(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_shapes_hit_endpoints() {
        for s in [
            FadeShape::Linear,
            FadeShape::EqualPower,
            FadeShape::SCurve,
            FadeShape::Exponential,
        ] {
            assert!(s.gain(0.0).abs() < 1e-6);
            assert!((s.gain(1.0) - 1.0).abs() < 1e-6);
        }
        assert!((FadeShape::EqualPower.gain(0.5) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn audio_clip_end_depends_on_tempo() {
        let mut timeline = Timeline::default(); // 120 BPM
        let clip = Clip {
            id: ClipId(1),
            track: TrackId(1),
            name: "a".into(),
            color: None,
            start: MusicalTime::from_quarters_i(4),
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: AudioSourceId(1),
                source_offset: 0,
                length: 48_000, // one second
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
            }),
        };
        assert_eq!(clip.end(&timeline, 48_000), MusicalTime::from_quarters_i(6));
        timeline.tempo.set_initial_bpm(60.0);
        assert_eq!(clip.end(&timeline, 48_000), MusicalTime::from_quarters_i(5));
    }
}

//! Picture: video files placed on video tracks in absolute time (project
//! samples, so tempo changes never move picture), the picture offset left
//! for what the display adds, and the project's timecode (rate and the
//! label of its start). Edited as a whole with `Command::SetVideo` and
//! `Command::SetTimecode`.

use faderframe_core::timecode::{FrameRate, Timecode};
use faderframe_core::{VideoClipId, VideoSourceId, VideoTrackId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A video file the project shows, and what it holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSource {
    /// Absolute in memory; relative to the project in its file.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Pixel aspect ratio.
    pub par: (u32, u32),
    /// Frames per second as the file says (0/1: variable).
    pub fps: (u32, u32),
    pub codec: String,
    /// Its length (ns).
    pub duration: i64,
    /// The start timecode it carries, with its rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timecode: Option<(Timecode, FrameRate)>,
    /// Its sound streams (channels each), for "take the sound" choices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sound: Vec<u32>,
}

impl VideoSource {
    /// The SMPTE rate of its frames, when they have one.
    pub fn frame_rate(&self) -> Option<FrameRate> {
        FrameRate::from_fraction(self.fps.0, self.fps.1)
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(|| "Video".into(), |n| n.to_string_lossy().into_owned())
    }
}

/// A stretch of a video placed on a video track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoClip {
    pub id: VideoClipId,
    pub source: VideoSourceId,
    /// Where it starts on the timeline (project samples).
    pub start: i64,
    /// Where in the file it starts (ns).
    pub offset: i64,
    /// How long it is (ns).
    pub length: i64,
}

impl VideoClip {
    /// Where it ends on the timeline (project samples).
    pub fn end(&self, rate: u32) -> i64 {
        self.start + ns_to_samples(self.length, rate)
    }

    /// The time in the file showing at timeline position `pos` (samples),
    /// if the clip covers it.
    pub fn file_time(&self, pos: i64, rate: u32) -> Option<i64> {
        let into = samples_to_ns(pos - self.start, rate);
        (0..self.length)
            .contains(&into)
            .then_some(self.offset + into)
    }
}

/// Samples at `rate` of `ns` nanoseconds (rounded).
pub fn ns_to_samples(ns: i64, rate: u32) -> i64 {
    ((ns as i128 * rate as i128 + 500_000_000) / 1_000_000_000) as i64
}

/// Nanoseconds of `samples` at `rate` (floored: a frame's own start lands
/// in it).
pub fn samples_to_ns(samples: i64, rate: u32) -> i64 {
    (samples as i128 * 1_000_000_000).div_euclid(rate.max(1) as i128) as i64
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoTrack {
    pub id: VideoTrackId,
    pub name: String,
    /// By start (normalised).
    pub clips: Vec<VideoClip>,
    /// Not shown (its clips stay).
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    /// Top first: where tracks overlap the top one shows.
    pub tracks: Vec<VideoTrack>,
    pub sources: BTreeMap<VideoSourceId, VideoSource>,
    /// Shown later by this much (ms; negative: earlier), for what the
    /// display chain adds beyond what is measured.
    pub offset_ms: f64,
}

impl Video {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.sources.is_empty() && self.offset_ms == 0.0
    }

    /// Clips by start, sources nobody uses kept (undo may bring them back).
    pub fn normalize(&mut self) {
        for t in &mut self.tracks {
            t.clips.retain(|c| c.length > 0);
            t.clips.sort_by_key(|c| (c.start, c.id));
        }
    }

    /// The clip showing at `pos` (samples): from the top shown track
    /// that has one there; with the time in its file.
    pub fn at(&self, pos: i64, rate: u32) -> Option<(&VideoClip, i64)> {
        self.tracks.iter().filter(|t| !t.hidden).find_map(|t| {
            // The last clip starting at or before `pos` (a later one on
            // top of an earlier one).
            let i = t.clips.partition_point(|c| c.start <= pos);
            t.clips[..i]
                .iter()
                .rev()
                .find_map(|c| c.file_time(pos, rate).map(|ft| (c, ft)))
        })
    }

    /// The clip of `track` showing at `pos` (samples), with the time in
    /// its file.
    pub fn at_track(&self, track: VideoTrackId, pos: i64, rate: u32) -> Option<(&VideoClip, i64)> {
        let t = self.tracks.iter().find(|t| t.id == track)?;
        let i = t.clips.partition_point(|c| c.start <= pos);
        t.clips[..i]
            .iter()
            .rev()
            .find_map(|c| c.file_time(pos, rate).map(|ft| (c, ft)))
    }

    /// The shown tracks, top first.
    pub fn shown_tracks(&self) -> impl Iterator<Item = &VideoTrack> {
        self.tracks.iter().filter(|t| !t.hidden)
    }

    pub fn clip(&self, id: VideoClipId) -> Option<(&VideoTrack, &VideoClip)> {
        self.tracks
            .iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (t, c)))
    }

    pub fn clip_mut(&mut self, id: VideoClipId) -> Option<&mut VideoClip> {
        self.tracks
            .iter_mut()
            .find_map(|t| t.clips.iter_mut().find(|c| c.id == id))
    }

    /// Where the last clip ends (samples; 0 without clips).
    pub fn end(&self, rate: u32) -> i64 {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.end(rate))
            .max()
            .unwrap_or(0)
    }

    /// Whether any clip uses `source`.
    pub fn uses(&self, source: VideoSourceId) -> bool {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .any(|c| c.source == source)
    }
}

/// The project's timecode: its rate and the label at the timeline's
/// start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTimecode {
    pub rate: FrameRate,
    pub start: Timecode,
}

impl Default for ProjectTimecode {
    fn default() -> Self {
        Self {
            rate: FrameRate::Fps25,
            start: Timecode::default(),
        }
    }
}

impl ProjectTimecode {
    /// The label at timeline position `pos` (samples at `rate`).
    pub fn at(&self, pos: i64, rate: u32) -> Timecode {
        let start = self.start.total_frames(self.rate);
        let seconds = pos as f64 / rate.max(1) as f64;
        Timecode::from_frames(start + self.rate.frame_at(seconds), self.rate)
    }

    /// Where the label `tc` is on the timeline (samples; negative before
    /// the start).
    pub fn position_of(&self, tc: Timecode, rate: u32) -> i64 {
        let frames = tc.total_frames(self.rate) - self.start.total_frames(self.rate);
        (self.rate.seconds_of(frames) * rate as f64).round() as i64
    }

    /// The label as shown ("01:00:00:00", ";" for drop-frame).
    pub fn label(&self, pos: i64, rate: u32) -> String {
        self.at(pos, rate).display(self.rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(id: u64, start: i64, offset: i64, length: i64) -> VideoClip {
        VideoClip {
            id: VideoClipId(id),
            source: VideoSourceId(1),
            start,
            offset,
            length,
        }
    }

    #[test]
    fn the_top_track_shows_where_clips_overlap() {
        let rate = 48_000;
        let mut v = Video {
            tracks: vec![
                VideoTrack {
                    id: VideoTrackId(10),
                    name: "V2".into(),
                    clips: vec![clip(2, 96_000, 0, 1_000_000_000)],
                    hidden: false,
                },
                VideoTrack {
                    id: VideoTrackId(11),
                    name: "V1".into(),
                    clips: vec![clip(1, 0, 500_000_000, 10_000_000_000)],
                    hidden: false,
                },
            ],
            ..Video::default()
        };
        // Before V2's clip: V1, half a second into its file.
        let (c, t) = v.at(0, rate).unwrap();
        assert_eq!((c.id.raw(), t), (1, 500_000_000));
        // Over it: V2 from its start.
        let (c, t) = v.at(96_000, rate).unwrap();
        assert_eq!((c.id.raw(), t), (2, 0));
        // After it: V1 again.
        let (c, t) = v.at(96_000 + 48_000, rate).unwrap();
        assert_eq!((c.id.raw(), t), (1, 500_000_000 + 3_000_000_000));
        // Hidden: through to the one below.
        v.tracks[0].hidden = true;
        assert_eq!(v.at(96_000, rate).unwrap().0.id.raw(), 1);
        // Past everything: nothing.
        assert!(v.at(48_000 * 11, rate).is_none());
        assert_eq!(v.end(rate), 48_000 * 10);
    }

    #[test]
    fn timecode_labels_and_positions_agree() {
        let rate = 48_000;
        let tc = ProjectTimecode {
            rate: FrameRate::Fps2997Drop,
            start: Timecode::parse("00:59:58;00", FrameRate::Fps2997Drop).unwrap(),
        };
        assert_eq!(tc.label(0, rate), "00:59:58;00");
        let hour = Timecode::parse("01:00:00;00", FrameRate::Fps2997Drop).unwrap();
        let pos = tc.position_of(hour, rate);
        // Two seconds of labels at 29.97 (60 frames) later.
        assert!((pos - (60.0 * 1001.0 / 30_000.0 * rate as f64) as i64).abs() <= 1);
        assert_eq!(tc.at(pos, rate), hour);
        assert!(tc.position_of(Timecode::parse("00:59:57;29", tc.rate).unwrap(), rate) < 0);
    }

    #[test]
    fn nanoseconds_and_samples_round_trip() {
        for rate in [44_100, 48_000, 96_000] {
            for s in [0i64, 1, 47_999, 1_234_567] {
                assert_eq!(ns_to_samples(samples_to_ns(s, rate), rate), s);
            }
        }
    }
}

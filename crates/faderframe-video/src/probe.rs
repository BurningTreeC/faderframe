//! What a media file holds: its picture (size, rate, codec), its sound
//! streams and its length.

use crate::{Result, VideoError};
use faderframe_core::timecode::FrameRate;
use gst_pbutils::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoStream {
    /// Stored size (pixels).
    pub width: u32,
    pub height: u32,
    /// Pixel aspect ratio (numerator, denominator).
    pub par: (u32, u32),
    /// Frames per second as the file says (0/1: variable or unknown).
    pub fps: (u32, u32),
    /// What it is coded with ("H.264 (High Profile)", "ProRes", …).
    pub codec: String,
}

impl VideoStream {
    /// The nearest SMPTE rate, if the file's is one.
    pub fn frame_rate(&self) -> Option<FrameRate> {
        FrameRate::from_fraction(self.fps.0, self.fps.1)
    }

    /// The size it is shown at (square pixels).
    pub fn display_size(&self) -> (u32, u32) {
        let (n, d) = (self.par.0.max(1), self.par.1.max(1));
        (
            (self.width as u64 * n as u64 / d as u64) as u32,
            self.height,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioStream {
    pub channels: u32,
    pub rate: u32,
    pub codec: String,
    /// Its language or title, when tagged.
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub video: Option<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub duration_ns: i64,
    /// The container ("Quicktime", "Matroska", …).
    pub container: String,
}

fn codec_of(caps: Option<gst::Caps>) -> String {
    caps.map(|c| gst_pbutils::pb_utils_get_codec_description(&c).to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Read what `path` holds.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    crate::init()?;
    let uri = gst::glib::filename_to_uri(path, None)?;
    let disc = gst_pbutils::Discoverer::new(gst::ClockTime::from_seconds(15))?;
    let info = disc
        .discover_uri(&uri)
        .map_err(|e| VideoError::media(path, e.to_string()))?;
    let video = info
        .video_streams()
        .into_iter()
        .find(|v| !v.is_image())
        .map(|v| {
            let fps = v.framerate();
            let par = v.par();
            VideoStream {
                width: v.width(),
                height: v.height(),
                par: (par.numer().max(1) as u32, par.denom().max(1) as u32),
                fps: (fps.numer().max(0) as u32, fps.denom().max(1) as u32),
                codec: codec_of(v.caps()),
            }
        });
    let audio = info
        .audio_streams()
        .into_iter()
        .map(|a| AudioStream {
            channels: a.channels(),
            rate: a.sample_rate(),
            codec: codec_of(a.caps()),
            label: a.language().map(|l| l.to_string()),
        })
        .collect();
    let container = info
        .stream_info()
        .and_then(|s| s.caps())
        .map(|c| codec_of(Some(c)))
        .unwrap_or_default();
    Ok(MediaInfo {
        video,
        audio,
        duration_ns: info.duration().map_or(0, crate::ns),
        container,
    })
}

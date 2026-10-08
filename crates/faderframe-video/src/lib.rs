//! Video for FaderFrame, on GStreamer used as a library only: it demuxes,
//! decodes, encodes and muxes, and never owns time (the audio engine is
//! the clock) nor shows a frame (the shell does).
//!
//! - [`probe`]: what a file holds (picture, audio streams, length).
//! - [`colour`]: what a picture's colours mean; HDR and wide-gamut
//!   pictures mapped for an SDR screen.
//! - [`index`]: every frame's time and the keyframes, read from the
//!   demuxer alone (fast), so variable frame rates, B-frames and edit
//!   lists come out exact; the file's start timecode.
//! - [`decode`]: exact frames as RGBA at a requested size, a keyframe at
//!   once for scrubbing, and frames in order for playback.
//! - [`proxy`]: an all-intra MJPEG copy at display size, where any frame is
//!   a millisecond away.
//! - [`audio`]: a sound stream as a project's media, aligned to the
//!   picture's time zero.
//! - [`mux`]: a movie with the picture copied untouched and new sound.
//! - [`service`]: decoders on threads and a frame cache, answering at once
//!   with the best frame there is.
//! - `zero_copy` (Linux): playback decoded by VA-API into dmabufs the
//!   display imports as they are.
//!
//! Each job decodes only the streams it needs: parsing (`parsebin`) comes
//! first and only the stream asked for is decoded.

#![forbid(unsafe_code)]

pub mod audio;
pub mod colour;
pub mod cuts;
pub mod decode;
pub mod index;
pub mod mux;
pub mod probe;
pub mod proxy;
pub mod qt_timecode;
pub mod service;
mod streams;
pub mod sync_test;
#[cfg(target_os = "linux")]
pub mod zero_copy;

pub use decode::{Decoder, Frame, fit};
pub use index::FrameIndex;
pub use probe::{AudioStream, MediaInfo, VideoStream};
pub use service::{FrameService, Media, Picture, Want};

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("video is not available: {0}")]
    Unavailable(String),
    #[error("{path}: {message}")]
    Media { path: PathBuf, message: String },
    #[error("{0} has no picture")]
    NoPicture(PathBuf),
    #[error("{0} has no sound")]
    NoSound(PathBuf),
    #[error("the GStreamer element ‘{0}’ is missing")]
    Missing(String),
    #[error("{0}")]
    Gst(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Import(#[from] faderframe_audio_files::import::ImportError),
}

pub type Result<T> = std::result::Result<T, VideoError>;

impl From<gst::glib::Error> for VideoError {
    fn from(e: gst::glib::Error) -> Self {
        Self::Gst(e.to_string())
    }
}

impl From<gst::glib::BoolError> for VideoError {
    fn from(e: gst::glib::BoolError) -> Self {
        Self::Gst(e.to_string())
    }
}

impl From<gst::StateChangeError> for VideoError {
    fn from(e: gst::StateChangeError) -> Self {
        Self::Gst(e.to_string())
    }
}

impl VideoError {
    pub(crate) fn media(path: &Path, message: impl Into<String>) -> Self {
        Self::Media {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

/// Start GStreamer once (every entry point calls it).
pub fn init() -> Result<()> {
    static DONE: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    DONE.get_or_init(|| gst::init().map_err(|e| e.to_string()))
        .clone()
        .map_err(VideoError::Unavailable)
}

/// Whether GStreamer has the element `name` (a plugin may be missing).
pub fn has_element(name: &str) -> bool {
    init().is_ok() && gst::ElementFactory::find(name).is_some()
}

/// GStreamer's version, for the about box and bug reports.
pub fn version() -> Option<String> {
    init().ok().map(|()| gst::version_string().to_string())
}

/// Nanoseconds of a GStreamer time.
pub(crate) fn ns(t: gst::ClockTime) -> i64 {
    t.nseconds() as i64
}

/// A GStreamer time of nanoseconds (negative ones are 0).
pub(crate) fn clock(ns: i64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(ns.max(0) as u64)
}

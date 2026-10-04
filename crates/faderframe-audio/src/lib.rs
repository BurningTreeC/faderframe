//! Audio backend abstraction.
//!
//! The engine never talks to JACK, PipeWire, ALSA, WASAPI, ASIO or CoreAudio
//! directly. Backends implement [`AudioBackend`]; the engine implements
//! [`AudioCallback`] and receives device buffers through the
//! [`DeviceBuffers`] trait, which exposes per-channel slices without
//! requiring the backend to build arrays of references (no allocation in the
//! callback).
//!
//! Supported stream formats are any sample rate in [`SAMPLE_RATE_RANGE`]
//! (the standard rates in [`STANDARD_SAMPLE_RATES`] are what the UI offers)
//! and any buffer size up to [`MAX_BUFFER_SIZE`] frames; the engine splits
//! large callbacks into internal blocks, so power-of-two sizes from 16 to
//! 8192 frames and arbitrary sizes (as delivered by some drivers) all work.
//!
//! Backends in this repository:
//! * [`dummy::DummyBackend`] — a timer-driven backend without hardware, used
//!   when no audio server is available and for CI.
//! * `faderframe-audio-jack` — JACK (JACK2 or pipewire-jack).
//!
//! Planned: PipeWire native, ALSA, WASAPI/ASIO (Windows), CoreAudio (macOS).

#![forbid(unsafe_code)]

pub mod dummy;
mod owned;

pub use owned::OwnedBuffers;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Sample rates offered in the UI and covered by the engine test matrix.
pub const STANDARD_SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// Buffer sizes (frames per callback) offered in the UI.
pub const STANDARD_BUFFER_SIZES: [u32; 10] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192];

/// Accepted sample-rate range.
pub const SAMPLE_RATE_RANGE: std::ops::RangeInclusive<u32> = 8_000..=384_000;

/// Largest callback size a backend may deliver.
pub const MAX_BUFFER_SIZE: u32 = 8192;

/// Validate a requested stream format.
pub fn validate_format(sample_rate: u32, buffer_size: u32) -> Result<(), AudioError> {
    if !SAMPLE_RATE_RANGE.contains(&sample_rate) {
        return Err(AudioError::UnsupportedConfig(format!(
            "sample rate {sample_rate} Hz is outside {}..={} Hz",
            SAMPLE_RATE_RANGE.start(),
            SAMPLE_RATE_RANGE.end()
        )));
    }
    if buffer_size == 0 || buffer_size > MAX_BUFFER_SIZE {
        return Err(AudioError::UnsupportedConfig(format!(
            "buffer size {buffer_size} is outside 1..={MAX_BUFFER_SIZE} frames"
        )));
    }
    Ok(())
}

/// Format a sample rate for display ("44.1 kHz", "192 kHz").
pub fn format_sample_rate(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", rate as f64 / 1000.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    #[error("audio backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("audio device not found: {0}")]
    DeviceNotFound(String),
    #[error("unsupported stream configuration: {0}")]
    UnsupportedConfig(String),
    #[error("audio stream error: {0}")]
    Stream(String),
    #[error("operation not supported by this backend: {0}")]
    Unsupported(&'static str),
}

/// A device (or, for server backends such as JACK, the server) as seen by
/// device enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub input_channels: u16,
    pub output_channels: u16,
    /// Rates the device can run at (empty = determined by a server).
    pub sample_rates: Vec<u32>,
    /// Current rate if the device/server has a fixed one.
    pub current_sample_rate: Option<u32>,
    pub current_buffer_size: Option<u32>,
    pub is_default: bool,
}

/// Requested stream parameters. `None` means "backend default".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    pub input_channels: u16,
    pub output_channels: u16,
    /// Name shown to the audio server (JACK/PipeWire client name).
    pub client_name: String,
    /// Connect to the system's physical ports automatically.
    pub auto_connect: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            device: None,
            sample_rate: None,
            buffer_size: None,
            // Enough for multitrack recording; JACK connects them to the
            // physical capture ports.
            input_channels: 8,
            output_channels: 2,
            client_name: "FaderFrame".into(),
            auto_connect: true,
        }
    }
}

/// Actual parameters of an open stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub backend: &'static str,
    pub device: String,
    pub sample_rate: u32,
    /// Nominal frames per callback (callbacks may be shorter).
    pub buffer_size: u32,
    pub input_channels: u16,
    pub output_channels: u16,
    /// Hardware/server latencies in frames, if known.
    pub input_latency: u32,
    pub output_latency: u32,
}

impl fmt::Display for StreamInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} · {} · {} · {} frames ({:.2} ms)",
            self.backend,
            self.device,
            format_sample_rate(self.sample_rate),
            self.buffer_size,
            self.buffer_size as f64 * 1000.0 / self.sample_rate.max(1) as f64
        )
    }
}

/// Device-side buffers for one callback.
///
/// Implemented by each backend over its native buffers; channel indices are
/// relative to the stream's channel counts. All slices have [`Self::frames`]
/// length.
pub trait DeviceBuffers {
    fn frames(&self) -> usize;
    fn input_channels(&self) -> usize;
    fn output_channels(&self) -> usize;
    fn input(&self, channel: usize) -> &[f32];
    fn output(&mut self, channel: usize) -> &mut [f32];
}

/// The realtime processing entry point implemented by the engine.
///
/// `process` is called on the backend's realtime thread and must obey the
/// realtime rules (no allocation, locks, I/O, logging).
pub trait AudioCallback: Send + 'static {
    /// Called once before the first `process` and whenever the stream
    /// format changes (not on the realtime path for format changes driven
    /// by the server, but implementations must stay realtime-safe anyway).
    fn prepare(&mut self, info: &StreamInfo);

    fn process(&mut self, io: &mut dyn DeviceBuffers);
}

/// Live status shared between a backend's threads and the control side.
#[derive(Debug, Default)]
pub struct StreamMonitor {
    xruns: AtomicU64,
    sample_rate: AtomicU32,
    buffer_size: AtomicU32,
    callbacks: AtomicU64,
    running: AtomicBool,
    shut_down: AtomicBool,
}

impl StreamMonitor {
    pub fn new(sample_rate: u32, buffer_size: u32) -> Arc<Self> {
        let m = Self::default();
        m.sample_rate.store(sample_rate, Ordering::Relaxed);
        m.buffer_size.store(buffer_size, Ordering::Relaxed);
        Arc::new(m)
    }

    #[inline]
    pub fn record_xrun(&self) {
        self.xruns.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_callback(&self) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_sample_rate(&self, sr: u32) {
        self.sample_rate.store(sr, Ordering::Relaxed);
    }

    pub fn set_buffer_size(&self, frames: u32) {
        self.buffer_size.store(frames, Ordering::Relaxed);
    }

    pub fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::Relaxed);
    }

    pub fn mark_shut_down(&self) {
        self.shut_down.store(true, Ordering::Relaxed);
        self.running.store(false, Ordering::Relaxed);
    }

    pub fn status(&self) -> StreamStatus {
        StreamStatus {
            xruns: self.xruns.load(Ordering::Relaxed),
            callbacks: self.callbacks.load(Ordering::Relaxed),
            sample_rate: self.sample_rate.load(Ordering::Relaxed),
            buffer_size: self.buffer_size.load(Ordering::Relaxed),
            running: self.running.load(Ordering::Relaxed),
            shut_down: self.shut_down.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStatus {
    pub xruns: u64,
    pub callbacks: u64,
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub running: bool,
    /// The server/device went away; the stream must be reopened.
    pub shut_down: bool,
}

/// An open, running stream. Dropping it stops processing and releases the
/// callback.
pub trait AudioStream: Send {
    fn info(&self) -> StreamInfo;
    fn status(&self) -> StreamStatus;

    /// Ask the backend for a different buffer size (JACK supports this at
    /// runtime; device backends may need a reopen).
    fn request_buffer_size(&mut self, _frames: u32) -> Result<(), AudioError> {
        Err(AudioError::Unsupported("changing the buffer size"))
    }

    /// The device's audio workgroup (macOS: CoreAudio's IO thread is in
    /// it), for threads that do part of the callback's work to join.
    fn io_workgroup(&self) -> Option<faderframe_realtime::Workgroup> {
        None
    }
}

/// A family of audio devices/servers (JACK, ALSA, WASAPI, ...).
pub trait AudioBackend: Send {
    /// Stable identifier ("jack", "dummy").
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;

    /// Cheap probe: can a stream be opened right now?
    fn is_available(&self) -> bool;

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError>;

    fn open_stream(
        &mut self,
        config: StreamConfig,
        callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_formats_validate() {
        for sr in STANDARD_SAMPLE_RATES {
            for bs in STANDARD_BUFFER_SIZES {
                validate_format(sr, bs).unwrap();
            }
        }
        assert!(validate_format(1_000, 64).is_err());
        assert!(validate_format(48_000, 0).is_err());
        assert!(validate_format(48_000, 16_384).is_err());
        // Odd sizes delivered by some drivers are fine.
        validate_format(44_100, 441).unwrap();
    }

    #[test]
    fn sample_rate_labels() {
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(192_000), "192 kHz");
    }
}

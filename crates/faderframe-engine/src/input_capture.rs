//! Capture Audio: the inputs of armed (or monitored) tracks kept while
//! nobody records, so a take played without recording can still be placed
//! afterwards (`faderframe_session::audio_capture`).
//!
//! The audio thread copies the device input channels it is given into a
//! pair of lock-free rings — [`CaptureBlock`] headers and samples — for
//! every chunk it processes, playing or stopped, exactly where the
//! recorder copies a take ([`crate::record::Recorder`]): the same frames
//! (after varispeed's resampler), stamped with the same timeline position.
//! The control side's keeper drains them into the rolling store.
//!
//! Every header carries the engine's frame counter at its first frame (the
//! play history's, continuous whatever the capture's channels are), the
//! timeline position while the transport plays, and whether the recorder
//! took the chunk. A full ring never blocks: the header still goes in,
//! marked `lost`, and the keeper fills the frames with silence so later
//! audio keeps its place. The rings exist only while a capture is set
//! ([`crate::EngineController::capture_inputs`]); replaced or ended, the
//! old ends go back through the garbage queue.

use faderframe_audio::DeviceBuffers;
use rtrb::{Consumer, Producer, RingBuffer};

/// One chunk of captured input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureBlock {
    /// The engine's frame counter at its first frame.
    pub frame: u64,
    pub frames: u32,
    /// Timeline position of its first frame while the transport plays
    /// (`None`: stopped, counting in or scrubbing).
    pub position: Option<i64>,
    /// The recorder took this chunk (a take has it already).
    pub recording: bool,
    /// No room was left: its samples are not in the ring.
    pub lost: bool,
}

/// The control side's ends. The sample ring holds, for each block and
/// each of `channels` in order, `frames` samples.
pub struct CaptureStreams {
    /// Device input channels (from 0), in the order their samples come.
    pub channels: Vec<u16>,
    pub headers: Consumer<CaptureBlock>,
    pub data: Consumer<f32>,
    /// The engine's rate the frames are at.
    pub sample_rate: u32,
}

impl CaptureStreams {
    /// The engine let go of its end (the capture was replaced or ended,
    /// or the engine is gone) and everything was read.
    pub fn is_finished(&self) -> bool {
        self.headers.is_abandoned() && self.headers.is_empty()
    }
}

/// The audio thread's ends.
pub(crate) struct CaptureRt {
    channels: Vec<u16>,
    headers: Producer<CaptureBlock>,
    data: Producer<f32>,
}

/// Rings for `seconds` of `channels` at `rate` (the reader's slack).
pub(crate) fn rings(channels: Vec<u16>, rate: u32, seconds: f64) -> (CaptureRt, CaptureStreams) {
    let frames = (f64::from(rate.max(1)) * seconds.max(0.25)) as usize;
    let (dtx, drx) = RingBuffer::new((frames * channels.len()).max(1024));
    // Chunks are at least ~16 frames except around loop points and
    // count-ins; headers are small.
    let (htx, hrx) = RingBuffer::new((frames / 16).max(1024));
    (
        CaptureRt {
            channels: channels.clone(),
            headers: htx,
            data: dtx,
        },
        CaptureStreams {
            channels,
            headers: hrx,
            data: drx,
            sample_rate: rate,
        },
    )
}

impl CaptureRt {
    /// Copy device input frames `offset..offset + n` (engine frame
    /// `frame`, timeline `position` while playing). Realtime-safe: no
    /// allocation, never waits.
    pub(crate) fn capture(
        &mut self,
        io: &dyn DeviceBuffers,
        offset: usize,
        n: usize,
        frame: u64,
        position: Option<i64>,
        recording: bool,
    ) {
        if n == 0 || self.headers.slots() == 0 {
            // No room even for the header: the keeper sees the frame
            // counter jump and fills the hole.
            return;
        }
        let fits = self.data.slots() >= n * self.channels.len();
        if fits {
            let ins = io.input_channels();
            for &ch in &self.channels {
                let ch = usize::from(ch);
                let pushed = if ch < ins {
                    self.data
                        .push_entire_slice(&io.input(ch)[offset..offset + n])
                } else {
                    self.data.write_chunk_uninit(n).map(|chunk| {
                        chunk.fill_from_iter(std::iter::repeat_n(0.0, n));
                    })
                };
                // Room was checked above.
                debug_assert!(pushed.is_ok());
            }
        }
        // Samples go in before their header: a header read means its
        // samples are there.
        let _ = self.headers.push(CaptureBlock {
            frame,
            frames: n as u32,
            position,
            recording,
            lost: !fits,
        });
    }
}

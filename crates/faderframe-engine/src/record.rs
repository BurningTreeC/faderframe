//! Capturing armed inputs on the audio thread.
//!
//! While the transport records, the processor copies the device input
//! channels of every record target into a pair of lock-free rings owned by
//! a [`Recorder`]: one of [`RecordBlock`] headers, one of samples. A writer
//! thread on the control side ([`RecordStreams`]) drains them to disk. The
//! rings exist only while recording (created by
//! [`crate::EngineController::begin_recording`], dropped on the control
//! thread when recording ends), so an idle engine holds no capture memory.
//!
//! Capture is sample-accurate: only the part of a block inside the record
//! window (`from..to`, the punch range) is captured. Every discontinuity
//! (loop wrap, locate, re-entering the window) starts a new *pass*; a full
//! ring never blocks the audio thread, the block is dropped and counted as
//! an overrun, and the writer fills the hole with silence so later audio
//! stays in place.

use faderframe_audio::DeviceBuffers;
use faderframe_core::TrackId;
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU64, Ordering};

/// One armed input to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordTarget {
    pub track: TrackId,
    /// First device input channel.
    pub first_channel: u16,
    pub channels: u16,
}

/// Header of one captured block. The sample ring holds, for each target in
/// order and for each of its channels, `frames` samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordBlock {
    /// Timeline sample of the first captured frame.
    pub position: i64,
    pub frames: u32,
    /// Contiguous passes (a loop wrap or locate starts a new one).
    pub pass: u32,
}

/// The control-side ends of a recording.
pub struct RecordStreams {
    pub targets: Vec<RecordTarget>,
    pub headers: Consumer<RecordBlock>,
    pub data: Consumer<f32>,
    pub sample_rate: u32,
}

impl RecordStreams {
    pub fn channels(&self) -> usize {
        self.targets.iter().map(|t| t.channels as usize).sum()
    }

    /// The engine dropped its ends (recording ended or the engine is gone).
    pub fn is_finished(&self) -> bool {
        self.headers.is_abandoned() && self.headers.is_empty()
    }
}

/// Audio-thread side.
pub(crate) struct Recorder {
    targets: Vec<RecordTarget>,
    channels: usize,
    headers: Producer<RecordBlock>,
    data: Producer<f32>,
    from: i64,
    to: i64,
    pass: u32,
    /// End of the last captured (or dropped) range.
    expected: Option<i64>,
}

/// Create the rings for `targets`, sized for `seconds` of audio at `rate`.
pub(crate) fn rings(
    targets: Vec<RecordTarget>,
    from: i64,
    to: i64,
    rate: u32,
    seconds: f64,
) -> (Recorder, RecordStreams) {
    let channels: usize = targets.iter().map(|t| t.channels as usize).sum();
    let frames = (rate as f64 * seconds.max(0.5)) as usize;
    let (dtx, drx) = RingBuffer::new((frames * channels).max(1));
    // Blocks are at least ~16 frames except around loop points.
    let (htx, hrx) = RingBuffer::new((frames / 16).max(1024));
    (
        Recorder {
            targets: targets.clone(),
            channels,
            headers: htx,
            data: dtx,
            from,
            to: to.max(from),
            pass: 0,
            expected: None,
        },
        RecordStreams {
            targets,
            headers: hrx,
            data: drx,
            sample_rate: rate,
        },
    )
}

impl Recorder {
    /// Capture device input frames `offset..offset + n`, which play at
    /// timeline position `pos`. Realtime-safe; returns frames captured.
    pub(crate) fn capture(
        &mut self,
        io: &dyn DeviceBuffers,
        offset: usize,
        n: usize,
        pos: i64,
        overruns: &AtomicU64,
    ) -> usize {
        let a = pos.max(self.from);
        let b = (pos + n as i64).min(self.to);
        if b <= a {
            return 0;
        }
        if self.expected.is_some_and(|e| e != a) {
            self.pass += 1;
        }
        self.expected = Some(b);
        let frames = (b - a) as usize;
        let start = offset + (a - pos) as usize;
        if self.data.slots() < frames * self.channels || self.headers.slots() == 0 {
            overruns.fetch_add(frames as u64, Ordering::Relaxed);
            return 0;
        }
        let ins = io.input_channels();
        for t in &self.targets {
            for c in 0..t.channels as usize {
                let ch = t.first_channel as usize + c;
                let pushed = if ch < ins {
                    self.data
                        .push_entire_slice(&io.input(ch)[start..start + frames])
                } else {
                    self.data.write_chunk_uninit(frames).map(|chunk| {
                        chunk.fill_from_iter(std::iter::repeat_n(0.0, frames));
                    })
                };
                // Space was checked above; a failure here is impossible.
                debug_assert!(pushed.is_ok());
            }
        }
        let _ = self.headers.push(RecordBlock {
            position: a,
            frames: frames as u32,
            pass: self.pass,
        });
        frames
    }
}

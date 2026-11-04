//! One device input heard on the control side as it comes in, with when
//! every callback started — for measurements against the clock (A/V
//! calibration hears its beeps through it).
//!
//! The audio thread copies the input channel of every device callback
//! (before varispeed or anything else touches it) into a lock-free ring of
//! samples and a ring of [`TapBlock`] headers stamped with the callback's
//! start on the heard clock's time base
//! ([`crate::history::ns_since_epoch`]), whatever the transport does. A
//! full ring never blocks: the block is counted as lost and its frames are
//! marked so the reader keeps its place. The rings exist only while a tap
//! is set ([`crate::EngineController::tap_input`]); the old ends go back
//! through the garbage queue.
//!
//! The frame `j` of a block of `n` was taken about `callback_ns − (input
//! latency + n − j) / rate`: a callback's input came in over its own frames
//! before it started, and the device's reported input latency before that
//! (devices report their latencies without their buffer; the engine adds
//! the buffer to the output's alike).

use faderframe_audio::DeviceBuffers;
use rtrb::{Consumer, Producer, RingBuffer};

/// One callback's input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapBlock {
    /// When the callback started (ns since the engine's epoch).
    pub callback_ns: u64,
    pub frames: u32,
    /// No room was left: its samples are not in the ring (the reader
    /// fills them with silence).
    pub lost: bool,
}

/// The control side's ends.
pub struct InputTap {
    /// The device input channel (from 0).
    pub channel: u16,
    pub headers: Consumer<TapBlock>,
    pub data: Consumer<f32>,
}

impl InputTap {
    /// What came in since the last call: samples (lost blocks as silence)
    /// and each block's `(first sample's index in what was returned,
    /// callback ns)`.
    pub fn drain(&mut self) -> (Vec<f32>, Vec<(usize, u64)>) {
        let mut samples = Vec::new();
        let mut stamps = Vec::new();
        while let Ok(h) = self.headers.pop() {
            stamps.push((samples.len(), h.callback_ns));
            let n = h.frames as usize;
            if h.lost {
                samples.resize(samples.len() + n, 0.0);
                continue;
            }
            match self.data.read_chunk(n.min(self.data.slots())) {
                Ok(chunk) => {
                    let got = chunk.len();
                    samples.extend(chunk);
                    // A header is written after its samples: they are all
                    // there.
                    samples.resize(samples.len() + (n - got), 0.0);
                }
                Err(_) => samples.resize(samples.len() + n, 0.0),
            }
        }
        (samples, stamps)
    }

    /// The engine let go of its end (the tap was replaced or the engine is
    /// gone).
    pub fn is_finished(&self) -> bool {
        self.headers.is_abandoned() && self.headers.is_empty()
    }
}

/// The audio thread's ends.
pub(crate) struct TapRt {
    channel: u16,
    headers: Producer<TapBlock>,
    data: Producer<f32>,
}

/// Rings for `seconds` of `channel` at `rate`.
pub(crate) fn tap(channel: u16, rate: u32, seconds: f64) -> (TapRt, InputTap) {
    let frames = ((rate.max(1) as f64) * seconds.max(0.25)) as usize;
    let (dtx, drx) = RingBuffer::new(frames.max(1024));
    // Callbacks are at least 16 frames.
    let (htx, hrx) = RingBuffer::new((frames / 16).max(256));
    (
        TapRt {
            channel,
            headers: htx,
            data: dtx,
        },
        InputTap {
            channel,
            headers: hrx,
            data: drx,
        },
    )
}

impl TapRt {
    /// Copy this callback's input (audio thread; allocation-free).
    pub(crate) fn capture(&mut self, io: &dyn DeviceBuffers, callback_ns: u64) {
        let frames = io.frames();
        if frames == 0 || self.headers.slots() == 0 {
            return;
        }
        let ch = self.channel as usize;
        let fits = self.data.slots() >= frames;
        if fits {
            let pushed = if ch < io.input_channels() {
                self.data.push_entire_slice(&io.input(ch)[..frames])
            } else {
                self.data.write_chunk_uninit(frames).map(|chunk| {
                    chunk.fill_from_iter(std::iter::repeat_n(0.0, frames));
                })
            };
            debug_assert!(pushed.is_ok());
        }
        let _ = self.headers.push(TapBlock {
            callback_ns,
            frames: frames as u32,
            lost: !fits,
        });
    }
}

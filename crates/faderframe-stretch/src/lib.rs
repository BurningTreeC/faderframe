//! Pitch-preserving time stretching for warped audio.
//!
//! Wraps [Signalsmith Stretch](https://signalsmith-audio.co.uk/code/stretch/)
//! (MIT, vendored in `vendor/`, see `vendor/VENDORED.md`) through a small C
//! shim. A [`Stretcher`] is configured (and allocates) on the control
//! thread; [`Stretcher::reset`], [`Stretcher::seek`] and
//! [`Stretcher::process`] never allocate, lock or do I/O — the crate's tests
//! count C++ heap allocations to prove it — so they may run on the audio
//! thread.
//!
//! Streaming contract (the vendored algorithm's): after `seek` with input
//! ending at source position `p`, the analysis is centred at `p -
//! input_latency()`, and output appears `output_latency()` output frames
//! after the analysis that produced it.

use std::ffi::c_void;
use std::ptr::NonNull;

/// Channels one stretcher handles (more fall back to varispeed playback).
pub const MAX_CHANNELS: usize = 8;

unsafe extern "C" {
    fn ff_stretch_new(channels: i32, block: i32, interval: i32, seed: u32) -> *mut c_void;
    fn ff_stretch_free(s: *mut c_void);
    fn ff_stretch_reset(s: *mut c_void);
    fn ff_stretch_input_latency(s: *const c_void) -> i32;
    fn ff_stretch_output_latency(s: *const c_void) -> i32;
    fn ff_stretch_block(s: *const c_void) -> i32;
    fn ff_stretch_interval(s: *const c_void) -> i32;
    fn ff_stretch_seek(s: *mut c_void, inputs: *const *const f32, length: i32, rate: f64);
    fn ff_stretch_process(
        s: *mut c_void,
        inputs: *const *const f32,
        input_length: i32,
        outputs: *const *mut f32,
        output_length: i32,
    );
    fn ff_stretch_set_transpose(s: *mut c_void, factor: f32);
}

/// Analysis settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// 120 ms blocks: smooth for any material.
    Polyphonic,
    /// 60 ms blocks: tighter transients for drums and percussive loops.
    Rhythmic,
}

impl Preset {
    fn block_interval(self, sample_rate: f64) -> (usize, usize) {
        let (b, i) = match self {
            Preset::Polyphonic => (0.12, 0.03),
            Preset::Rhythmic => (0.06, 0.015),
        };
        (
            ((sample_rate * b) as usize).max(64),
            ((sample_rate * i) as usize).max(16),
        )
    }
}

/// One time stretcher.
pub struct Stretcher {
    raw: NonNull<c_void>,
    channels: usize,
    preset: Preset,
    input_latency: usize,
    output_latency: usize,
    seek_length: usize,
}

// SAFETY: the C++ object has no thread affinity and is only used through
// `&mut self`; moving it to the audio thread is fine (it is not `Sync`).
unsafe impl Send for Stretcher {}

impl Stretcher {
    /// Configure for `channels` (1..=[`MAX_CHANNELS`]) at `sample_rate`
    /// (allocates; control thread).
    pub fn new(channels: usize, sample_rate: f64, preset: Preset) -> Option<Self> {
        if !(1..=MAX_CHANNELS).contains(&channels) || sample_rate.is_nan() || sample_rate <= 0.0 {
            return None;
        }
        let (block, interval) = preset.block_interval(sample_rate);
        // SAFETY: plain values in; the shim returns null on failure.
        let raw = unsafe {
            ff_stretch_new(
                i32::try_from(channels).ok()?,
                i32::try_from(block).ok()?,
                i32::try_from(interval).ok()?,
                0x5eed,
            )
        };
        let raw = NonNull::new(raw)?;
        // SAFETY: `raw` is a live stretcher.
        let (li, lo, b, i) = unsafe {
            (
                ff_stretch_input_latency(raw.as_ptr()),
                ff_stretch_output_latency(raw.as_ptr()),
                ff_stretch_block(raw.as_ptr()),
                ff_stretch_interval(raw.as_ptr()),
            )
        };
        Some(Self {
            raw,
            channels,
            preset,
            input_latency: li.max(0) as usize,
            output_latency: lo.max(0) as usize,
            seek_length: (b.max(0) + i.max(0)) as usize,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn preset(&self) -> Preset {
        self.preset
    }

    /// Input frames the analysis runs ahead of the stream position.
    pub fn input_latency(&self) -> usize {
        self.input_latency
    }

    /// Output frames between an analysis and its output.
    pub fn output_latency(&self) -> usize {
        self.output_latency
    }

    /// Pre-roll [`Self::seek`] takes (one block and one interval).
    pub fn seek_length(&self) -> usize {
        self.seek_length
    }

    /// Forget all history (realtime-safe).
    pub fn reset(&mut self) {
        // SAFETY: `raw` is a live stretcher owned by `self`.
        unsafe { ff_stretch_reset(self.raw.as_ptr()) }
    }

    /// Pre-roll: `inputs` (one slice per channel, same length; the last
    /// [`Self::seek_length`] frames are used) end where processing will
    /// continue; `rate` is the expected input frames per output frame.
    pub fn seek(&mut self, inputs: &[&[f32]], rate: f64) {
        let len = inputs.iter().map(|c| c.len()).min().unwrap_or(0);
        if inputs.len() < self.channels || len == 0 {
            return;
        }
        let mut ptrs = [std::ptr::null::<f32>(); MAX_CHANNELS];
        for (p, c) in ptrs.iter_mut().zip(inputs) {
            *p = c.as_ptr();
        }
        let len = i32::try_from(len).unwrap_or(i32::MAX);
        // SAFETY: `channels` pointers to at least `len` floats each.
        unsafe { ff_stretch_seek(self.raw.as_ptr(), ptrs.as_ptr(), len, rate) }
    }

    /// Stretch `input_len` frames of `inputs` into `output_len` frames of
    /// `outputs` (the ratio is the playback rate for this call).
    pub fn process(
        &mut self,
        inputs: &[&[f32]],
        input_len: usize,
        outputs: &mut [&mut [f32]],
        output_len: usize,
    ) {
        if inputs.len() < self.channels || outputs.len() < self.channels {
            return;
        }
        let input_len = inputs[..self.channels]
            .iter()
            .map(|c| c.len())
            .fold(input_len, usize::min);
        let output_len = outputs[..self.channels]
            .iter()
            .map(|c| c.len())
            .fold(output_len, usize::min);
        let mut ins = [std::ptr::null::<f32>(); MAX_CHANNELS];
        let mut outs = [std::ptr::null_mut::<f32>(); MAX_CHANNELS];
        for (p, c) in ins.iter_mut().zip(inputs) {
            *p = c.as_ptr();
        }
        for (p, c) in outs.iter_mut().zip(outputs.iter_mut()) {
            *p = c.as_mut_ptr();
        }
        let (i, o) = (
            i32::try_from(input_len).unwrap_or(i32::MAX),
            i32::try_from(output_len).unwrap_or(i32::MAX),
        );
        // SAFETY: `channels` input pointers with ≥ `i` floats and output
        // pointers with ≥ `o` floats each (clamped above); the stretcher
        // only reads/writes within those.
        unsafe { ff_stretch_process(self.raw.as_ptr(), ins.as_ptr(), i, outs.as_ptr(), o) }
    }

    /// Pitch factor (1 = unchanged), e.g. for varispeed-free transposition.
    pub fn set_transpose(&mut self, factor: f32) {
        // SAFETY: `raw` is a live stretcher owned by `self`.
        unsafe { ff_stretch_set_transpose(self.raw.as_ptr(), factor) }
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        // SAFETY: created by `ff_stretch_new`, freed exactly once.
        unsafe { ff_stretch_free(self.raw.as_ptr()) }
    }
}

impl std::fmt::Debug for Stretcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stretcher")
            .field("channels", &self.channels)
            .field("preset", &self.preset)
            .field("input_latency", &self.input_latency)
            .field("output_latency", &self.output_latency)
            .finish()
    }
}

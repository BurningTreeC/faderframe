//! The live input a voice-to-MIDI tracker listens to (the session's voice
//! ports): one device input channel copied into a ring on the audio thread,
//! with the MIDI clock time of each callback, so the tracker can stamp the
//! notes it hears with when they were sung.

use faderframe_realtime::ScopeRing;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// About a second and a half at 48 kHz.
const FRAMES: usize = 1 << 16;
const OFF: u32 = u32::MAX;

#[derive(Debug)]
pub struct VoiceTap {
    pub ring: ScopeRing,
    channel: AtomicU32,
    /// The callback that wrote the newest frames: its MIDI clock time (ns,
    /// when its input was handed over) and the ring's frames after it.
    stamp_ns: AtomicU64,
    stamp_frames: AtomicU64,
    rate: AtomicU32,
}

impl Default for VoiceTap {
    fn default() -> Self {
        Self {
            ring: ScopeRing::new(FRAMES),
            channel: AtomicU32::new(OFF),
            stamp_ns: AtomicU64::new(0),
            stamp_frames: AtomicU64::new(0),
            rate: AtomicU32::new(48_000),
        }
    }
}

impl VoiceTap {
    /// Listen to input `channel` (0-based), or to nothing.
    pub fn set_channel(&self, channel: Option<u16>) {
        self.channel
            .store(channel.map_or(OFF, u32::from), Ordering::Relaxed);
    }

    pub fn channel(&self) -> Option<u16> {
        let c = self.channel.load(Ordering::Relaxed);
        (c != OFF).then_some(c as u16)
    }

    /// The device's rate.
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// The MIDI clock time (ns) ring frame `frame` arrived (from the newest
    /// callback's stamp).
    pub fn time_of(&self, frame: u64) -> u64 {
        let ns = self.stamp_ns.load(Ordering::Acquire);
        let at = self.stamp_frames.load(Ordering::Relaxed);
        let rate = f64::from(self.rate().max(1));
        let behind = at as f64 - frame as f64;
        (ns as f64 - behind * 1e9 / rate).max(0.0) as u64
    }

    /// Copy a callback's input (audio thread; allocation-free).
    #[inline]
    pub(crate) fn capture(&self, input: Option<&[f32]>, now_ns: u64, rate: u32) {
        let Some(x) = input else { return };
        self.ring.push(0, x, x);
        self.rate.store(rate, Ordering::Relaxed);
        self.stamp_frames
            .store(self.ring.written(), Ordering::Relaxed);
        self.stamp_ns.store(now_ns, Ordering::Release);
    }
}

//! Transport: play/stop/record/locate/loop state owned by the audio thread.
//!
//! * The control side sends [`TransportCommand`]s through the engine's
//!   command queue; the audio thread applies them at block boundaries.
//! * The audio thread owns [`TransportState`] and advances it per block,
//!   splitting blocks at loop boundaries so loops are sample-accurate
//!   ([`TransportState::frames_until_wrap`]).
//! * Every block, a plain-data [`TransportInfo`] is derived for processors
//!   and plugins (tempo, meter, beat/bar position, loop state).
//! * [`TransportShared`] mirrors position/state into atomics for the UI.

#![forbid(unsafe_code)]

use faderframe_timeline::{TimeSignature, Timeline};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// A loop range in absolute samples (`start < end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRange {
    pub start: i64,
    pub end: i64,
}

impl LoopRange {
    pub fn new(start: i64, end: i64) -> Option<Self> {
        (end > start).then_some(Self { start, end })
    }

    pub fn len(&self) -> i64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportCommand {
    Play,
    Stop,
    TogglePlay,
    /// Move the playhead to an absolute sample position.
    Locate(i64),
    SetLoopRange(Option<LoopRange>),
    SetLoopEnabled(bool),
    SetRecording(bool),
}

/// Realtime transport state (audio thread).
#[derive(Clone, Debug, Default)]
pub struct TransportState {
    playing: bool,
    recording: bool,
    position: i64,
    loop_range: Option<LoopRange>,
    loop_enabled: bool,
    /// Set whenever playback becomes discontinuous (stop, locate, loop
    /// wrap) so that note-generating processors can release notes.
    discontinuity: bool,
}

impl TransportState {
    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn recording(&self) -> bool {
        self.recording
    }

    pub fn position(&self) -> i64 {
        self.position
    }

    pub fn loop_range(&self) -> Option<LoopRange> {
        self.loop_range
    }

    pub fn looping(&self) -> bool {
        self.loop_enabled && self.loop_range.is_some()
    }

    /// Apply a command (audio thread, block boundary).
    pub fn apply(&mut self, cmd: TransportCommand) {
        match cmd {
            TransportCommand::Play => self.playing = true,
            TransportCommand::Stop => {
                if self.playing {
                    self.discontinuity = true;
                }
                self.playing = false;
                self.recording = false;
            }
            TransportCommand::TogglePlay => {
                if self.playing {
                    self.apply(TransportCommand::Stop);
                } else {
                    self.playing = true;
                }
            }
            TransportCommand::Locate(pos) => {
                if pos != self.position {
                    self.position = pos;
                    self.discontinuity = true;
                }
            }
            TransportCommand::SetLoopRange(range) => {
                self.loop_range = range.filter(|r| !r.is_empty());
            }
            TransportCommand::SetLoopEnabled(on) => self.loop_enabled = on,
            TransportCommand::SetRecording(on) => self.recording = on,
        }
    }

    /// How many of the next `max` frames can be processed before the loop
    /// end is reached (always `>= 1` when `max >= 1`).
    pub fn frames_until_wrap(&self, max: usize) -> usize {
        if self.playing
            && self.loop_enabled
            && let Some(lr) = self.loop_range
            && self.position < lr.end
        {
            return ((lr.end - self.position) as usize).clamp(1, max.max(1));
        }
        max
    }

    /// Advance after processing `frames` frames. Returns `true` if the
    /// playhead wrapped to the loop start.
    pub fn advance(&mut self, frames: usize) -> bool {
        if !self.playing {
            return false;
        }
        let before = self.position;
        self.position += frames as i64;
        if self.loop_enabled
            && let Some(lr) = self.loop_range
            && before < lr.end
            && self.position >= lr.end
        {
            self.position = lr.start + (self.position - lr.end);
            self.discontinuity = true;
            return true;
        }
        false
    }

    /// Returns and clears the discontinuity flag.
    pub fn take_discontinuity(&mut self) -> bool {
        std::mem::take(&mut self.discontinuity)
    }

    /// Derive the per-block info for processors (allocation-free).
    pub fn info(&self, timeline: &Timeline, sample_rate: f64) -> TransportInfo {
        let quarters = timeline
            .tempo
            .samples_to_quarters(self.position, sample_rate);
        let musical = faderframe_timeline::MusicalTime::from_quarters(quarters);
        let bar = timeline.meter.bar_at(musical);
        TransportInfo {
            playing: self.playing,
            recording: self.recording,
            looping: self.looping(),
            sample_position: self.position,
            sample_rate,
            quarter_position: quarters,
            tempo: timeline.tempo.bpm_at(musical),
            time_signature: timeline.meter.signature_of_bar(bar),
            bar_index: bar,
            bar_start_quarters: timeline.meter.bar_start(bar).quarters(),
            loop_range: self.loop_range,
        }
    }
}

/// Plain-data transport snapshot for one block, as plugins expect it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportInfo {
    pub playing: bool,
    pub recording: bool,
    pub looping: bool,
    /// Timeline position of the first frame of the block.
    pub sample_position: i64,
    pub sample_rate: f64,
    /// Position in quarter notes.
    pub quarter_position: f64,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    /// 0-based bar containing the block start.
    pub bar_index: i32,
    pub bar_start_quarters: f64,
    pub loop_range: Option<LoopRange>,
}

impl Default for TransportInfo {
    fn default() -> Self {
        Self {
            playing: false,
            recording: false,
            looping: false,
            sample_position: 0,
            sample_rate: 48_000.0,
            quarter_position: 0.0,
            tempo: 120.0,
            time_signature: TimeSignature::FOUR_FOUR,
            bar_index: 0,
            bar_start_quarters: 0.0,
            loop_range: None,
        }
    }
}

/// Transport state mirrored into atomics for the control/UI side.
#[derive(Debug, Default)]
pub struct TransportShared {
    position: AtomicI64,
    playing: AtomicBool,
    recording: AtomicBool,
    looping: AtomicBool,
}

/// What the UI reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportSnapshot {
    pub position: i64,
    pub playing: bool,
    pub recording: bool,
    pub looping: bool,
}

impl TransportShared {
    /// Publish (audio thread, end of callback).
    #[inline]
    pub fn publish(&self, state: &TransportState) {
        self.position.store(state.position, Ordering::Relaxed);
        self.playing.store(state.playing, Ordering::Relaxed);
        self.recording.store(state.recording, Ordering::Relaxed);
        self.looping.store(state.looping(), Ordering::Relaxed);
    }

    /// Read (control thread).
    pub fn snapshot(&self) -> TransportSnapshot {
        TransportSnapshot {
            position: self.position.load(Ordering::Relaxed),
            playing: self.playing.load(Ordering::Relaxed),
            recording: self.recording.load(Ordering::Relaxed),
            looping: self.looping.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_timeline::MusicalTime;

    #[test]
    fn play_stop_locate() {
        let mut t = TransportState::default();
        assert!(!t.advance(64));
        assert_eq!(t.position(), 0);
        t.apply(TransportCommand::Play);
        t.advance(64);
        assert_eq!(t.position(), 64);
        assert!(!t.take_discontinuity());
        t.apply(TransportCommand::Locate(1000));
        assert!(t.take_discontinuity());
        t.apply(TransportCommand::TogglePlay);
        assert!(!t.playing());
        assert!(t.take_discontinuity());
    }

    #[test]
    fn loop_wrap_is_sample_accurate() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::SetLoopRange(LoopRange::new(100, 250)));
        t.apply(TransportCommand::SetLoopEnabled(true));
        t.apply(TransportCommand::Locate(200));
        t.take_discontinuity();
        t.apply(TransportCommand::Play);
        // A 64-frame block must be split after 50 frames.
        let first = t.frames_until_wrap(64);
        assert_eq!(first, 50);
        assert!(t.advance(first));
        assert_eq!(t.position(), 100);
        assert!(t.take_discontinuity());
        assert_eq!(t.frames_until_wrap(64), 64);
    }

    #[test]
    fn playing_past_loop_end_does_not_wrap() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::SetLoopRange(LoopRange::new(0, 100)));
        t.apply(TransportCommand::SetLoopEnabled(true));
        t.apply(TransportCommand::Locate(500));
        t.apply(TransportCommand::Play);
        assert_eq!(t.frames_until_wrap(64), 64);
        assert!(!t.advance(64));
        assert_eq!(t.position(), 564);
    }

    #[test]
    fn info_reports_musical_position() {
        let timeline = Timeline::default(); // 120 BPM, 4/4
        let mut t = TransportState::default();
        t.apply(TransportCommand::Locate(48_000 * 3)); // 3 s = 6 quarters
        let info = t.info(&timeline, 48_000.0);
        assert!((info.quarter_position - 6.0).abs() < 1e-9);
        assert_eq!(info.bar_index, 1);
        assert!((info.bar_start_quarters - 4.0).abs() < 1e-9);
        assert_eq!(info.tempo, 120.0);
        let _ = MusicalTime::ZERO;

        let shared = TransportShared::default();
        shared.publish(&t);
        assert_eq!(shared.snapshot().position, 144_000);
    }
}

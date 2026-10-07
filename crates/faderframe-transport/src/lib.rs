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
    /// Scrubbing: play `frames` from `position` (faded in and out), then
    /// return there. Repeated commands while a snippet plays continue it
    /// when the position follows on, else fade out and jump. While playing
    /// normally it only locates.
    Scrub {
        position: i64,
        frames: u32,
    },
}

/// A scrub snippet in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scrub {
    /// Where the playhead returns when the snippet ends.
    home: i64,
    /// Frames of the snippet played so far.
    done: u32,
    /// Frames still to play.
    left: u32,
    /// Fade length at both ends.
    fade: u32,
    /// Snippet length (for a pending jump).
    frames: u32,
    /// Jump here after the current snippet has faded out.
    pending: Option<i64>,
}

/// Realtime transport state (audio thread).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransportState {
    playing: bool,
    recording: bool,
    position: i64,
    loop_range: Option<LoopRange>,
    loop_enabled: bool,
    /// Set whenever playback becomes discontinuous (stop, locate, loop
    /// wrap) so that note-generating processors can release notes.
    discontinuity: bool,
    /// Jumps so far: stops, locates and scrub returns, not loop wraps
    /// (wrapping, for the control side to notice one).
    jumps: u32,
    scrub: Option<Scrub>,
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

    /// How often playback has jumped (stop, locate, scrub): loop wraps are
    /// not jumps. Wraps around.
    pub fn jumps(&self) -> u32 {
        self.jumps
    }

    fn jumped(&mut self) {
        self.discontinuity = true;
        self.jumps = self.jumps.wrapping_add(1);
    }

    /// Playing a scrub snippet (not normal playback).
    pub fn scrubbing(&self) -> bool {
        self.scrub.is_some()
    }

    /// Gain of the scrub envelope for frame `i` of the next block (1.0 when
    /// not scrubbing).
    #[inline]
    pub fn scrub_gain(&self, i: usize) -> f32 {
        let Some(s) = self.scrub else { return 1.0 };
        let fade = s.fade.max(1) as f32;
        let i = i as u32;
        let fade_in = ((s.done + i) as f32 / fade).min(1.0);
        let fade_out = (s.left.saturating_sub(i) as f32 / fade).min(1.0);
        fade_in * fade_out
    }

    /// Apply a command (audio thread, block boundary).
    pub fn apply(&mut self, cmd: TransportCommand) {
        match cmd {
            TransportCommand::Play => {
                // Normal playback takes over from a snippet.
                self.end_scrub();
                self.playing = true;
            }
            TransportCommand::Stop => {
                if self.playing {
                    self.jumped();
                }
                self.end_scrub();
                self.playing = false;
                self.recording = false;
            }
            TransportCommand::TogglePlay => {
                if self.playing && self.scrub.is_none() {
                    self.apply(TransportCommand::Stop);
                } else {
                    self.apply(TransportCommand::Play);
                }
            }
            TransportCommand::Scrub { position, frames } => {
                let frames = frames.max(16);
                if self.playing && self.scrub.is_none() {
                    self.apply(TransportCommand::Locate(position));
                    return;
                }
                let current = self.position;
                match &mut self.scrub {
                    Some(s) => {
                        s.home = position;
                        s.frames = frames;
                        let ahead = position - current;
                        if s.pending.is_none() && (-256..=frames as i64 * 2).contains(&ahead) {
                            // Following on: keep playing.
                            s.left = s.left.max(frames);
                        } else {
                            // Fade out, then jump.
                            s.pending = Some(position);
                            s.left = s.left.min(s.fade);
                        }
                    }
                    None => {
                        if self.recording {
                            return;
                        }
                        self.apply(TransportCommand::Locate(position));
                        self.playing = true;
                        self.scrub = Some(Scrub {
                            home: position,
                            done: 0,
                            left: frames,
                            fade: (frames / 16).max(1),
                            frames,
                            pending: None,
                        });
                    }
                }
            }
            TransportCommand::Locate(pos) => {
                if pos != self.position {
                    self.position = pos;
                    self.jumped();
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
        if let Some(s) = self.scrub {
            return (s.left as usize).clamp(1, max.max(1));
        }
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
        if let Some(s) = &mut self.scrub {
            s.done += frames as u32;
            s.left = s.left.saturating_sub(frames as u32);
            if s.left == 0 {
                match s.pending.take() {
                    Some(p) => {
                        s.done = 0;
                        s.left = s.frames;
                        self.position = p;
                    }
                    None => {
                        self.position = s.home;
                        self.scrub = None;
                        self.playing = false;
                    }
                }
                self.jumped();
            }
            return false;
        }
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

    fn end_scrub(&mut self) {
        if let Some(s) = self.scrub.take() {
            self.position = s.home;
            self.playing = false;
            self.jumped();
        }
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
    scrubbing: AtomicBool,
}

/// What the UI reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportSnapshot {
    pub position: i64,
    /// Normal playback (not a scrub snippet).
    pub playing: bool,
    pub recording: bool,
    pub looping: bool,
    /// Playing a scrub snippet: the control side treats this as stopped.
    pub scrubbing: bool,
}

impl TransportShared {
    /// Publish (audio thread, end of callback).
    #[inline]
    pub fn publish(&self, state: &TransportState) {
        self.position.store(state.position, Ordering::Relaxed);
        let scrubbing = state.scrub.is_some();
        self.playing
            .store(state.playing && !scrubbing, Ordering::Relaxed);
        self.recording.store(state.recording, Ordering::Relaxed);
        self.looping.store(state.looping(), Ordering::Relaxed);
        self.scrubbing.store(scrubbing, Ordering::Relaxed);
    }

    /// Read (control thread).
    pub fn snapshot(&self) -> TransportSnapshot {
        TransportSnapshot {
            position: self.position.load(Ordering::Relaxed),
            playing: self.playing.load(Ordering::Relaxed),
            recording: self.recording.load(Ordering::Relaxed),
            looping: self.looping.load(Ordering::Relaxed),
            scrubbing: self.scrubbing.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_timeline::MusicalTime;

    #[test]
    fn locates_stops_and_scrubs_are_jumps_and_loop_wraps_are_not() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::SetLoopRange(LoopRange::new(0, 100)));
        t.apply(TransportCommand::SetLoopEnabled(true));
        t.apply(TransportCommand::Play);
        assert_eq!(t.jumps(), 0, "starting is not a jump");
        for _ in 0..10 {
            t.advance(64);
        }
        assert_eq!(t.jumps(), 0, "nor are loop wraps");
        t.apply(TransportCommand::Locate(30));
        assert_eq!(t.jumps(), 1);
        t.apply(TransportCommand::Locate(30));
        assert_eq!(t.jumps(), 1, "a locate to where it is moves nothing");
        t.apply(TransportCommand::Stop);
        assert_eq!(t.jumps(), 2);
        t.apply(TransportCommand::Scrub {
            position: 500,
            frames: 32,
        });
        assert_eq!(t.jumps(), 3, "a scrub snippet starts where it is asked");
        t.advance(32);
        assert_eq!(t.jumps(), 4, "and ends");
    }

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
    fn scrub_snippets_play_continue_jump_and_return_home() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::Scrub {
            position: 1000,
            frames: 320,
        });
        assert!(t.playing() && t.scrubbing());
        assert_eq!(t.position(), 1000);
        // Faded in and out (fade = 320 / 16 = 20 frames).
        assert_eq!(t.scrub_gain(0), 0.0);
        assert_eq!(t.scrub_gain(10), 0.5);
        assert_eq!(t.scrub_gain(100), 1.0);
        assert_eq!(t.frames_until_wrap(1024), 320);
        t.advance(200);
        // Following on: the snippet is extended, no jump.
        t.apply(TransportCommand::Scrub {
            position: 1250,
            frames: 320,
        });
        assert_eq!(t.position(), 1200);
        assert_eq!(t.frames_until_wrap(1024), 320);
        // Far away: fade out (20 frames), then jump there.
        t.take_discontinuity();
        t.apply(TransportCommand::Scrub {
            position: 50_000,
            frames: 320,
        });
        assert_eq!(t.frames_until_wrap(1024), 20);
        t.advance(20);
        assert!(t.take_discontinuity());
        assert_eq!(t.position(), 50_000);
        assert_eq!(t.scrub_gain(0), 0.0, "fades in again");
        // The snippet ends: stopped back where the pointer is.
        t.advance(320);
        assert!(!t.playing() && !t.scrubbing());
        assert_eq!(t.position(), 50_000);
        // While playing normally, scrubbing only locates.
        t.apply(TransportCommand::Play);
        t.apply(TransportCommand::Scrub {
            position: 7,
            frames: 320,
        });
        assert!(t.playing() && !t.scrubbing());
        assert_eq!(t.position(), 7);
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

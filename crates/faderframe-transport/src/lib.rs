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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

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

/// A planned jump: when playback reaches timeline sample `at` it goes on
/// from `to`, the loop as `then_loop` says from then on (an interactive
/// score's transition: sample-accurate, and predicted by the render-ahead
/// tiers like a loop wrap). `to == at` jumps nowhere and only changes the
/// loop on that frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cue {
    pub at: i64,
    pub to: i64,
    /// The loop range and switch from then on (`None`: as they are).
    pub then_loop: Option<(Option<LoopRange>, bool)>,
}

/// Jumps planned at most at once.
pub const MAX_CUES: usize = 2;

/// Planned jumps in the order they come ([`TransportCommand::SetCues`]):
/// only the first is armed; the next one is armed once it has been taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cues {
    list: [Option<Cue>; MAX_CUES],
}

impl Cues {
    pub const NONE: Cues = Cues {
        list: [None; MAX_CUES],
    };

    /// The first [`MAX_CUES`] of `cues`, in order.
    pub fn new(cues: &[Cue]) -> Self {
        let mut list = [None; MAX_CUES];
        for (slot, c) in list.iter_mut().zip(cues) {
            *slot = Some(*c);
        }
        Self { list }
    }

    /// The armed cue.
    pub fn first(&self) -> Option<Cue> {
        self.list[0]
    }

    pub fn len(&self) -> usize {
        self.list.iter().filter(|c| c.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.list[0].is_none()
    }

    pub fn iter(&self) -> impl Iterator<Item = Cue> + '_ {
        self.list.iter().map_while(|c| *c)
    }

    fn pop(&mut self) {
        self.list.rotate_left(1);
        self.list[MAX_CUES - 1] = None;
    }
}

/// The loop and the planned jumps: what a loop change hands the
/// render-ahead tiers to take on at a frame (see [`TransportState::plan`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopPlan {
    pub range: Option<LoopRange>,
    pub enabled: bool,
    pub cues: Cues,
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
    /// Play after a count-in of this many frames: the transport stands
    /// where it is (not playing: nothing of the arrangement plays, nothing
    /// is recorded) while it counts, then plays from the frame after the
    /// count-in — inside a block ([`TransportState::frames_until_wrap`]
    /// ends a block there). Play while counting changes nothing, Stop (and
    /// TogglePlay) cancels it. 0 frames: Play.
    PlayAfterCountIn(u64),
    /// Plan jumps (replacing those planned): see [`Cue`]. A locate, a stop
    /// or a scrub forgets them.
    SetCues(Cues),
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
    /// Where the newest jump went.
    jumped_to: i64,
    scrub: Option<Scrub>,
    /// Frames of a count-in left before playback starts (0: none; see
    /// [`TransportCommand::PlayAfterCountIn`]).
    count_in: u64,
    /// Jumps planned ([`TransportCommand::SetCues`]).
    cues: Cues,
    /// Cues taken so far (wrapping).
    cues_taken: u32,
}

impl TransportState {
    /// Playback runs (the position advances). Not while counting in.
    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Frames of a count-in left before playback starts (0: not counting
    /// in).
    pub fn counting_in(&self) -> u64 {
        self.count_in
    }

    /// Started: playing, or counting in before it plays.
    pub fn started(&self) -> bool {
        self.playing || self.count_in > 0
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

    /// Whether looping is switched on (whether or not there is a range).
    pub fn loop_enabled(&self) -> bool {
        self.loop_enabled
    }

    /// Set the loop range and switch (as [`TransportCommand::SetLoopRange`]
    /// and [`TransportCommand::SetLoopEnabled`]): never a discontinuity.
    pub fn set_loop(&mut self, range: Option<LoopRange>, enabled: bool) {
        self.loop_range = range.filter(|r| !r.is_empty());
        self.loop_enabled = enabled;
    }

    /// The jumps planned.
    pub fn cues(&self) -> Cues {
        self.cues
    }

    /// How many planned jumps have been taken (wrapping).
    pub fn cues_taken(&self) -> u32 {
        self.cues_taken
    }

    /// The loop and the jumps planned (what a loop change amends).
    pub fn plan(&self) -> LoopPlan {
        LoopPlan {
            range: self.loop_range,
            enabled: self.loop_enabled,
            cues: self.cues,
        }
    }

    /// Take on a loop and planned jumps at once (as [`Self::set_loop`] and
    /// [`TransportCommand::SetCues`]): never a discontinuity.
    pub fn set_plan(&mut self, plan: LoopPlan) {
        self.set_loop(plan.range, plan.enabled);
        self.cues = plan.cues;
    }

    /// How often playback has jumped (stop, locate, scrub): loop wraps are
    /// not jumps. Wraps around.
    pub fn jumps(&self) -> u32 {
        self.jumps
    }

    /// Where the newest jump went (a stop: where it stopped).
    pub fn jumped_to(&self) -> i64 {
        self.jumped_to
    }

    fn jumped(&mut self) {
        self.discontinuity = true;
        self.jumps = self.jumps.wrapping_add(1);
        self.jumped_to = self.position;
        // What was planned from where it was is void.
        self.cues = Cues::NONE;
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
                // Normal playback takes over from a snippet; a count-in
                // goes on (it plays when it is over).
                self.end_scrub();
                if self.count_in == 0 {
                    self.playing = true;
                }
            }
            TransportCommand::PlayAfterCountIn(frames) => {
                if self.playing && self.scrub.is_none() {
                    // Playing already: nothing to count in.
                    return;
                }
                self.end_scrub();
                if frames == 0 {
                    self.count_in = 0;
                    self.playing = true;
                } else {
                    self.count_in = frames;
                }
            }
            TransportCommand::Stop => {
                if self.playing || self.count_in > 0 {
                    self.jumped();
                }
                self.end_scrub();
                self.playing = false;
                self.recording = false;
                self.count_in = 0;
            }
            TransportCommand::TogglePlay => {
                if (self.playing && self.scrub.is_none()) || self.count_in > 0 {
                    self.apply(TransportCommand::Stop);
                } else {
                    self.apply(TransportCommand::Play);
                }
            }
            TransportCommand::Scrub { position, frames } => {
                let frames = frames.max(16);
                if (self.playing || self.count_in > 0) && self.scrub.is_none() {
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
            TransportCommand::SetCues(cues) => self.cues = cues,
        }
    }

    /// The armed cue while playback runs normally (not counting in, not
    /// scrubbing) and it lies ahead.
    fn cue_ahead(&self) -> Option<Cue> {
        if !self.playing || self.count_in > 0 || self.scrub.is_some() {
            return None;
        }
        self.cues.first().filter(|c| c.at > self.position)
    }

    /// How many of the next `max` frames can be processed before the loop
    /// end is reached, or a count-in ends (always `>= 1` when `max >= 1`).
    pub fn frames_until_wrap(&self, max: usize) -> usize {
        if self.count_in > 0 {
            // Playback starts on the frame after the count-in.
            return (self.count_in.min(max as u64) as usize).clamp(1, max.max(1));
        }
        if let Some(s) = self.scrub {
            return (s.left as usize).clamp(1, max.max(1));
        }
        let mut n = max;
        if self.playing
            && self.loop_enabled
            && let Some(lr) = self.loop_range
            && self.position < lr.end
        {
            n = ((lr.end - self.position) as usize).clamp(1, max.max(1));
        }
        if let Some(c) = self.cue_ahead() {
            n = n.min(((c.at - self.position) as usize).clamp(1, max.max(1)));
        }
        n
    }

    /// Advance after processing `frames` frames. Returns `true` if the
    /// playhead wrapped to the loop start or took a planned jump (a cue
    /// wins over a loop end on the same frame; neither counts as a jump).
    /// Counting in, the frames count the count-in down (the position
    /// stands); at its end playback starts (not a jump, as Play from stop
    /// is not).
    pub fn advance(&mut self, frames: usize) -> bool {
        if self.count_in > 0 {
            self.count_in = self.count_in.saturating_sub(frames as u64);
            if self.count_in == 0 {
                self.playing = true;
            }
            return false;
        }
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
        if let Some(c) = self.cues.first()
            && before < c.at
            && self.position >= c.at
        {
            self.position = c.to + (self.position - c.at);
            if let Some((range, enabled)) = c.then_loop {
                self.set_loop(range, enabled);
            }
            self.cues.pop();
            self.cues_taken = self.cues_taken.wrapping_add(1);
            if c.to != c.at {
                self.discontinuity = true;
                return true;
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
    counting_in: AtomicU64,
    jumps: AtomicU32,
    jumped_to: AtomicI64,
    cues_taken: AtomicU32,
    cues_waiting: AtomicU32,
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
    /// Frames of a count-in left before playback starts (0: none). The
    /// engine's snapshot is as processed and not `playing` while counting;
    /// the session's (`Session::transport`) is as heard and `playing`
    /// (the transport is started).
    pub counting_in: u64,
    /// [`TransportState::jumps`] as of the end of the last callback (the
    /// display's: a locate the session showed has happened once it
    /// changes).
    pub jumps: u32,
    /// Where the newest of them went ([`TransportState::jumped_to`]): a
    /// stop's is where it stopped, a locate's its target.
    pub jumped_to: i64,
    /// Planned jumps taken so far ([`TransportState::cues_taken`]).
    pub cues_taken: u32,
    /// Planned jumps still waiting.
    pub cues_waiting: u32,
}

impl TransportSnapshot {
    /// Started: playing, or counting in before playback.
    pub fn started(&self) -> bool {
        self.playing || self.counting_in > 0
    }
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
        self.counting_in.store(state.count_in, Ordering::Relaxed);
        self.jumped_to.store(state.jumped_to, Ordering::Relaxed);
        self.cues_taken.store(state.cues_taken, Ordering::Relaxed);
        self.cues_waiting
            .store(state.cues.len() as u32, Ordering::Relaxed);
        self.jumps.store(state.jumps, Ordering::Release);
    }

    /// Read (control thread).
    pub fn snapshot(&self) -> TransportSnapshot {
        TransportSnapshot {
            position: self.position.load(Ordering::Relaxed),
            playing: self.playing.load(Ordering::Relaxed),
            recording: self.recording.load(Ordering::Relaxed),
            looping: self.looping.load(Ordering::Relaxed),
            scrubbing: self.scrubbing.load(Ordering::Relaxed),
            counting_in: self.counting_in.load(Ordering::Relaxed),
            jumps: self.jumps.load(Ordering::Acquire),
            jumped_to: self.jumped_to.load(Ordering::Relaxed),
            cues_taken: self.cues_taken.load(Ordering::Relaxed),
            cues_waiting: self.cues_waiting.load(Ordering::Relaxed),
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
        assert_eq!((t.jumps(), t.jumped_to()), (1, 30));
        t.apply(TransportCommand::Locate(30));
        assert_eq!(t.jumps(), 1, "a locate to where it is moves nothing");
        t.advance(10);
        t.apply(TransportCommand::Stop);
        assert_eq!(
            (t.jumps(), t.jumped_to()),
            (2, 40),
            "a stop where it stopped"
        );
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

    /// A count-in stands where it is, ends a block on its last frame and
    /// plays from the next; Play keeps counting, Stop cancels it.
    #[test]
    fn a_count_in_stands_then_plays_on_the_frame_after_it() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::Locate(0));
        t.apply(TransportCommand::PlayAfterCountIn(100));
        assert!(!t.playing() && t.started());
        assert_eq!(t.counting_in(), 100);
        assert_eq!(t.frames_until_wrap(64), 64);
        assert!(!t.advance(64));
        assert_eq!((t.position(), t.counting_in()), (0, 36));
        t.apply(TransportCommand::Play);
        assert!(!t.playing(), "Play while counting keeps counting");
        assert_eq!(t.frames_until_wrap(64), 36, "the block ends with it");
        t.advance(36);
        assert!(t.playing() && t.counting_in() == 0);
        assert_eq!(t.position(), 0, "playback starts at the start");
        assert!(!t.take_discontinuity(), "no jump");
        t.advance(64);
        assert_eq!(t.position(), 64);
        // Stop while counting: no playback, a jump (cancelled).
        t.apply(TransportCommand::Stop);
        t.apply(TransportCommand::Locate(0));
        t.take_discontinuity();
        let jumps = t.jumps();
        t.apply(TransportCommand::PlayAfterCountIn(500));
        t.advance(100);
        t.apply(TransportCommand::Stop);
        assert!(!t.started());
        assert_eq!(t.jumps(), jumps + 1);
        assert!(t.take_discontinuity());
        t.advance(1_000);
        assert!(!t.playing() && t.position() == 0);
        // TogglePlay cancels too; no frames is Play.
        t.apply(TransportCommand::PlayAfterCountIn(500));
        t.apply(TransportCommand::TogglePlay);
        assert!(!t.started());
        t.apply(TransportCommand::PlayAfterCountIn(0));
        assert!(t.playing());
        // Playing: a count-in asked for changes nothing.
        t.apply(TransportCommand::PlayAfterCountIn(500));
        assert_eq!(t.counting_in(), 0);
        let shared = TransportShared::default();
        t.apply(TransportCommand::Stop);
        t.apply(TransportCommand::PlayAfterCountIn(48));
        shared.publish(&t);
        let snap = shared.snapshot();
        assert!(!snap.playing);
        assert_eq!(snap.counting_in, 48);
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
    fn planned_jumps_land_on_their_frames_and_change_the_loop() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::SetLoopRange(LoopRange::new(1000, 2000)));
        t.apply(TransportCommand::SetLoopEnabled(true));
        t.apply(TransportCommand::Locate(1900));
        t.apply(TransportCommand::Play);
        t.take_discontinuity();
        // A jump on the loop's end (it wins over the wrap) to 5000, which
        // loops 5000–6000 from then on; then a jump at 5500 back to 1200.
        t.apply(TransportCommand::SetCues(Cues::new(&[
            Cue {
                at: 2000,
                to: 5000,
                then_loop: Some((LoopRange::new(5000, 6000), true)),
            },
            Cue {
                at: 5500,
                to: 1200,
                then_loop: None,
            },
        ])));
        assert_eq!(t.cues().len(), 2);
        assert_eq!(t.frames_until_wrap(512), 100);
        assert!(t.advance(100));
        assert_eq!(t.position(), 5000);
        assert!(t.take_discontinuity());
        assert_eq!(t.loop_range(), LoopRange::new(5000, 6000));
        assert_eq!((t.cues_taken(), t.cues().len()), (1, 1));
        assert_eq!(t.jumps(), 1, "only the locate: cues are not jumps");
        assert_eq!(t.frames_until_wrap(1024), 500);
        assert!(t.advance(500));
        assert_eq!(t.position(), 1200);
        assert_eq!(t.loop_range(), LoopRange::new(5000, 6000), "kept");
        assert!(t.cues().is_empty());
        // A cue behind the position waits for the next time round.
        t.apply(TransportCommand::Locate(5100));
        t.apply(TransportCommand::SetCues(Cues::new(&[Cue {
            at: 5050,
            to: 100,
            then_loop: Some((None, false)),
        }])));
        assert_eq!(t.frames_until_wrap(4096), 900, "the loop's end first");
        t.advance(900);
        assert_eq!(t.position(), 5000);
        assert_eq!(t.frames_until_wrap(4096), 50);
        t.advance(60);
        assert_eq!(t.position(), 110);
        assert!(!t.looping());
        // A cue that jumps nowhere only changes the loop, no discontinuity.
        t.take_discontinuity();
        t.apply(TransportCommand::SetCues(Cues::new(&[Cue {
            at: 200,
            to: 200,
            then_loop: Some((LoopRange::new(0, 400), true)),
        }])));
        assert!(!t.advance(100));
        assert!(!t.take_discontinuity());
        assert_eq!(t.position(), 210);
        assert!(t.looping());
        // A locate or a stop forgets what was planned.
        t.apply(TransportCommand::SetCues(Cues::new(&[Cue {
            at: 300,
            to: 0,
            then_loop: None,
        }])));
        t.apply(TransportCommand::Locate(250));
        assert!(t.cues().is_empty());
        let shared = TransportShared::default();
        shared.publish(&t);
        assert_eq!(shared.snapshot().cues_taken, 4);
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

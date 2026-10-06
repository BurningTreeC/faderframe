//! The clip launcher in the engine.
//!
//! Launch commands arrive in the processor's message queue and take effect
//! at the next quantised position (bar, beat or a number of bars, from
//! the timeline's meter and tempo), sample-accurately: [`LaunchState`]
//! keeps, per track, the clip playing (its slot and the sample it started
//! at) and what comes next (a clip, or stop, and when). Players ask
//! [`pieces`] what to play over a block — the arrangement, a launched
//! clip (looping over its length from where it started) or nothing — at
//! most two pieces, split where a launch lands. A track plays the
//! arrangement until a clip is launched on it, then only launched clips
//! until "back to the arrangement". The state is published to the session
//! through [`LaunchStatus`] (atomics) after every chunk.
//!
//! Everything is preallocated: the state holds up to [`MAX_TRACKS`]
//! tracks.

use faderframe_core::TrackId;
use faderframe_timeline::{MusicalTime, Timeline};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};

/// Tracks the launcher follows at most.
pub const MAX_TRACKS: usize = 256;

/// Where a launch waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantize {
    None,
    Beat,
    Bars(u32),
}

impl From<faderframe_project::launcher::LaunchQuantize> for Quantize {
    fn from(q: faderframe_project::launcher::LaunchQuantize) -> Self {
        use faderframe_project::launcher::LaunchQuantize as Q;
        match q {
            Q::None => Quantize::None,
            Q::Beat => Quantize::Beat,
            Q::Bar => Quantize::Bars(1),
            Q::Bars(n) => Quantize::Bars(u32::from(n.max(1))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LaunchCommand {
    /// Play a slot's clip on its track.
    Launch {
        track: TrackId,
        /// The slot's number, below 2^63 (`SlotKey::hash`).
        slot: u64,
        quantize: Quantize,
        /// Take over the playing clip's position (in time).
        legato: bool,
        /// Start again every this many samples until stopped (0: no).
        repeat: i64,
    },
    /// Stop the track's clip (the track is then silent).
    Stop {
        track: TrackId,
        quantize: Quantize,
    },
    /// A held launch (gate, repeat) let go: the slot's clip stops at the
    /// next launch position, or a quantum after it started if it has not
    /// yet.
    Release {
        track: TrackId,
        slot: u64,
        quantize: Quantize,
    },
    StopAll {
        quantize: Quantize,
    },
    /// Every track plays the arrangement again (at once).
    BackToArrangement,
    /// Play a slot's clip at once as if it had started at `start` (a clip
    /// just recorded goes on looping in time).
    Resume {
        track: TrackId,
        slot: u64,
        start: i64,
    },
}

/// One track's launcher state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackLaunch {
    pub track: TrackId,
    /// The slot playing and the sample its clip started at.
    pub current: Option<(u64, i64)>,
    /// What comes next (a slot, or `None`: stop) and when.
    pub next: Option<(Option<u64>, i64)>,
    /// The arrangement plays (nothing launched since "back to arrangement").
    pub arrangement: bool,
    /// `next` is the playing clip's follow action (not launched).
    pub followed: bool,
    /// `next` takes over the playing clip's position.
    pub legato: bool,
    /// The slot launched to repeat, and every how many samples.
    pub repeat: Option<(u64, i64)>,
    /// Released before its launch took effect: it stops here then.
    pub stop_after: Option<i64>,
}

/// What a player plays over a piece of a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Play {
    Arrangement,
    /// A launched clip that started at timeline sample `start`.
    Clip {
        slot: u64,
        start: i64,
    },
    Silence,
}

impl TrackLaunch {
    fn now(&self) -> Play {
        match self.current {
            Some((slot, start)) => Play::Clip { slot, start },
            None if self.arrangement => Play::Arrangement,
            None => Play::Silence,
        }
    }

    /// Where a clip launched to take effect at `at` starts (a legato one
    /// where the playing clip did).
    fn start_of_next(&self, at: i64) -> i64 {
        match self.current {
            Some((_, start)) if self.legato => start,
            _ => at,
        }
    }
}

/// What to play over the block `[pos, pos + n)`: up to two pieces
/// `(offset, frames, play)` (split where a launch lands).
pub fn pieces(
    track: Option<&TrackLaunch>,
    pos: i64,
    n: usize,
) -> ([(usize, usize, Play); 2], usize) {
    let none = (0, 0, Play::Silence);
    let Some(t) = track else {
        return ([(0, n, Play::Arrangement), none], 1);
    };
    let before = t.now();
    match t.next {
        Some((slot, at)) if at < pos + n as i64 => {
            let split = (at - pos).clamp(0, n as i64) as usize;
            let after = match slot {
                Some(slot) => Play::Clip {
                    slot,
                    start: t.start_of_next(at),
                },
                None => Play::Silence,
            };
            if split == 0 {
                ([(0, n, after), none], 1)
            } else {
                ([(0, split, before), (split, n - split, after)], 2)
            }
        }
        _ => ([(0, n, before), none], 1),
    }
}

/// The launcher state the processor keeps (see the module docs).
pub struct LaunchState {
    tracks: Vec<TrackLaunch>,
    /// The transport played in the last chunk.
    playing: bool,
    /// Where the next chunk starts if playback runs on.
    expected: i64,
    /// Follow actions' random choices.
    rng: u64,
}

impl Default for LaunchState {
    fn default() -> Self {
        Self::new()
    }
}

impl LaunchState {
    /// Room for [`MAX_TRACKS`] (allocates; control thread).
    pub fn new() -> Self {
        Self {
            tracks: Vec::with_capacity(MAX_TRACKS),
            playing: false,
            expected: 0,
            rng: 0x9E37_79B9_7F4A_7C15,
        }
    }

    pub fn track(&self, track: TrackId) -> Option<&TrackLaunch> {
        self.tracks.iter().find(|t| t.track == track)
    }

    pub fn tracks(&self) -> &[TrackLaunch] {
        &self.tracks
    }

    fn entry(&mut self, track: TrackId) -> Option<&mut TrackLaunch> {
        if let Some(i) = self.tracks.iter().position(|t| t.track == track) {
            return Some(&mut self.tracks[i]);
        }
        if self.tracks.len() == self.tracks.capacity() {
            return None;
        }
        self.tracks.push(TrackLaunch {
            track,
            current: None,
            next: None,
            arrangement: true,
            followed: false,
            legato: false,
            repeat: None,
            stop_after: None,
        });
        self.tracks.last_mut()
    }

    /// Apply a command at playback position `pos` (`playing`: the
    /// transport runs; stopped, launches take effect at once).
    pub fn command(
        &mut self,
        cmd: LaunchCommand,
        pos: i64,
        playing: bool,
        timeline: &Timeline,
        rate: f64,
    ) {
        let at = |q: Quantize| {
            if playing {
                next_boundary(q, pos, timeline, rate)
            } else {
                pos
            }
        };
        match cmd {
            LaunchCommand::Launch {
                track,
                slot,
                quantize,
                legato,
                repeat,
            } => {
                let at = at(quantize);
                if let Some(t) = self.entry(track) {
                    t.next = Some((Some(slot), at));
                    t.followed = false;
                    t.legato = legato;
                    t.repeat = (repeat > 0).then_some((slot, repeat));
                    t.stop_after = None;
                }
            }
            LaunchCommand::Stop { track, quantize } => {
                let at = at(quantize);
                if let Some(t) = self.entry(track) {
                    t.repeat = None;
                    t.stop_after = None;
                    if t.current.is_some() || t.next.is_some() {
                        t.next = Some((None, at));
                        t.followed = false;
                        t.legato = false;
                    }
                }
            }
            LaunchCommand::Release {
                track,
                slot,
                quantize,
            } => {
                let at = at(quantize);
                if let Some(t) = self.entry(track) {
                    if t.repeat.is_some_and(|(s, _)| s == slot) {
                        t.repeat = None;
                    }
                    match t.next {
                        // Not started yet: it plays one quantum.
                        Some((Some(s), when)) if s == slot && !t.followed => {
                            t.stop_after = Some(if playing {
                                next_boundary(quantize, when + 1, timeline, rate)
                            } else {
                                when + 1
                            });
                        }
                        _ if t.current.is_some_and(|(s, _)| s == slot) => {
                            t.next = Some((None, at));
                            t.followed = false;
                            t.legato = false;
                            t.stop_after = None;
                        }
                        _ => {}
                    }
                }
            }
            LaunchCommand::StopAll { quantize } => {
                let at = at(quantize);
                for t in &mut self.tracks {
                    t.repeat = None;
                    t.stop_after = None;
                    if t.current.is_some() || t.next.is_some() {
                        t.next = Some((None, at));
                        t.followed = false;
                        t.legato = false;
                    }
                }
            }
            LaunchCommand::Resume { track, slot, start } => {
                if let Some(t) = self.entry(track) {
                    t.current = Some((slot, start.min(pos)));
                    t.next = None;
                    t.followed = false;
                    t.legato = false;
                    t.repeat = None;
                    t.stop_after = None;
                    t.arrangement = false;
                }
            }
            LaunchCommand::BackToArrangement => {
                for t in &mut self.tracks {
                    t.current = None;
                    t.next = None;
                    t.followed = false;
                    t.legato = false;
                    t.repeat = None;
                    t.stop_after = None;
                    t.arrangement = true;
                }
            }
        }
    }

    /// The transport at a chunk's start: playback starting plays what
    /// waits from here; stopping stops launched clips; a jump (locate,
    /// loop) keeps launched clips in their own time.
    pub fn transport(&mut self, playing: bool, pos: i64) {
        if playing && !self.playing {
            for t in &mut self.tracks {
                if let Some((slot, _)) = t.next {
                    t.next = Some((slot, pos));
                }
            }
        } else if !playing && self.playing {
            self.stopped();
        } else if playing && pos != self.expected {
            let delta = pos - self.expected;
            for t in &mut self.tracks {
                if let Some((_, start)) = &mut t.current {
                    *start += delta;
                }
                if let Some((_, at)) = &mut t.next {
                    *at += delta;
                }
            }
        }
        self.playing = playing;
    }

    /// A chunk of `frames` from `pos` has played: launches due in it take
    /// effect.
    pub fn played(&mut self, pos: i64, frames: usize) {
        self.expected = pos + frames as i64;
        if self.playing {
            self.advance(self.expected);
        }
    }

    /// Launches due before `end` take effect (after a chunk).
    pub fn advance(&mut self, end: i64) {
        for t in &mut self.tracks {
            if let Some((slot, at)) = t.next
                && at < end
            {
                let start = t.start_of_next(at);
                t.current = slot.map(|s| (s, start));
                t.next = t.stop_after.take().map(|stop| (None, stop.max(at + 1)));
                t.followed = false;
                t.legato = false;
                t.arrangement = false;
            }
        }
    }

    /// The transport stopped: launched clips stop (the arrangement does
    /// not come back by itself).
    pub fn stopped(&mut self) {
        for t in &mut self.tracks {
            t.next = None;
            t.legato = false;
            t.repeat = None;
            t.stop_after = None;
            if t.current.take().is_some() {
                t.arrangement = false;
            }
        }
    }

    /// Follow actions: a clip playing with nothing queued gets its follow
    /// action queued, or starts again when it repeats (after every chunk
    /// and command).
    pub fn follow(&mut self, timeline: &crate::snapshot::TimelineSnapshot) {
        let mut rng = self.rng;
        let mut draw = || {
            // xorshift64
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for t in &mut self.tracks {
            let (Some((slot, start)), None) = (t.current, t.next) else {
                continue;
            };
            if let Some((repeat, every)) = t.repeat
                && repeat == slot
            {
                t.next = Some((Some(slot), start + every.max(1)));
                t.followed = true;
                continue;
            }
            let Some(f) = timeline.launch_lane(slot).and_then(|l| l.follow.as_ref()) else {
                continue;
            };
            let choice = match &f.second {
                Some(second) if draw() % 100 >= u64::from(f.chance) => second,
                _ => &f.first,
            };
            let target = match choice.targets.len() {
                0 => None,
                n if choice.random => Some(choice.targets[(draw() % n as u64) as usize]),
                _ => Some(choice.targets[0]),
            };
            t.next = Some((target, start + f.after));
            t.followed = true;
        }
        self.rng = rng;
    }

    /// A new snapshot: clips of slots it does not know stop, follow actions
    /// are queued again from it.
    pub fn timeline_changed(&mut self, timeline: &crate::snapshot::TimelineSnapshot) {
        self.retain_slots(|s| timeline.launch_lane(s).is_some());
        for t in &mut self.tracks {
            if t.followed {
                t.next = None;
                t.followed = false;
            }
        }
        self.follow(timeline);
    }

    /// Clips of slots no longer known stop (after a snapshot change).
    pub fn retain_slots(&mut self, known: impl Fn(u64) -> bool) {
        for t in &mut self.tracks {
            if t.current.is_some_and(|(s, _)| !known(s)) {
                t.current = None;
            }
            if t.next.is_some_and(|(s, _)| s.is_some_and(|s| !known(s))) {
                t.next = None;
            }
        }
    }

    pub fn publish(&self, status: &LaunchStatus) {
        for (i, t) in self.tracks.iter().enumerate() {
            let e = &status.entries[i];
            e.track.store(t.track.raw(), Ordering::Relaxed);
            e.playing
                .store(t.current.map_or(0, |(s, _)| s | 1 << 63), Ordering::Relaxed);
            e.start
                .store(t.current.map_or(0, |(_, a)| a), Ordering::Relaxed);
            let (queued, at) = match t.next {
                // A follow action is not shown as waiting.
                _ if t.followed => (0, 0),
                Some((Some(s), at)) => (s | 1 << 63, at),
                Some((None, at)) => (1, at),
                None => (0, 0),
            };
            e.queued.store(queued, Ordering::Relaxed);
            e.queued_at.store(at, Ordering::Relaxed);
            e.arrangement.store(t.arrangement, Ordering::Relaxed);
        }
        status.count.store(self.tracks.len(), Ordering::Release);
    }
}

/// The first quantised position at or after `pos` (timeline samples).
pub fn next_boundary(q: Quantize, pos: i64, timeline: &Timeline, rate: f64) -> i64 {
    let here = timeline.to_musical(pos, rate);
    let target = match q {
        Quantize::None => return pos,
        Quantize::Beat => {
            let bar = timeline.meter.bar_at(here);
            let start = timeline.meter.bar_start(bar);
            let sig = timeline.meter.signature_of_bar(bar);
            let beat = MusicalTime::from_quarters(4.0 / f64::from(sig.denominator.max(1)));
            let mut t = start;
            while t < here {
                t += beat;
            }
            t
        }
        Quantize::Bars(n) => {
            let n = n.max(1) as i32;
            let mut bar = timeline.meter.bar_at(here);
            if timeline.meter.bar_start(bar) < here {
                bar += 1;
            }
            let bar = bar.div_euclid(n) * n + if bar.rem_euclid(n) == 0 { 0 } else { n };
            timeline.meter.bar_start(bar)
        }
    };
    timeline.to_samples(target, rate).max(pos)
}

/// One track's published launcher state.
#[derive(Debug, Default)]
pub struct StatusEntry {
    track: AtomicU64,
    /// Slot | 1 << 63, or 0.
    playing: AtomicU64,
    start: AtomicI64,
    /// Slot | 1 << 63, 1 for a stop, or 0.
    queued: AtomicU64,
    queued_at: AtomicI64,
    arrangement: AtomicBool,
}

/// The launcher state as the session reads it.
#[derive(Debug)]
pub struct LaunchStatus {
    entries: Box<[StatusEntry]>,
    count: AtomicUsize,
}

impl Default for LaunchStatus {
    fn default() -> Self {
        Self {
            entries: (0..MAX_TRACKS).map(|_| StatusEntry::default()).collect(),
            count: AtomicUsize::new(0),
        }
    }
}

/// A track's launcher state, read on the control side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackStatus {
    pub track: TrackId,
    pub playing: Option<(u64, i64)>,
    /// `Some(None)`: stopping.
    pub queued: Option<(Option<u64>, i64)>,
    pub arrangement: bool,
}

impl LaunchStatus {
    pub fn read(&self) -> Vec<TrackStatus> {
        let n = self.count.load(Ordering::Acquire).min(self.entries.len());
        self.entries[..n]
            .iter()
            .map(|e| {
                let playing = e.playing.load(Ordering::Relaxed);
                let queued = e.queued.load(Ordering::Relaxed);
                TrackStatus {
                    track: TrackId(e.track.load(Ordering::Relaxed)),
                    playing: (playing != 0)
                        .then(|| (playing & !(1 << 63), e.start.load(Ordering::Relaxed))),
                    queued: match queued {
                        0 => None,
                        1 => Some((None, e.queued_at.load(Ordering::Relaxed))),
                        q => Some((Some(q & !(1 << 63)), e.queued_at.load(Ordering::Relaxed))),
                    },
                    arrangement: e.arrangement.load(Ordering::Relaxed),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn launches_wait_for_the_bar_and_split_the_block() {
        let tl = Timeline::default(); // 120 BPM, 4/4: a bar is 2 s
        let bar = 96_000;
        let t = TrackId(7);
        let mut s = LaunchState::new();
        assert_eq!(next_boundary(Quantize::Bars(1), 1, &tl, SR), bar);
        assert_eq!(next_boundary(Quantize::Bars(1), bar, &tl, SR), bar);
        assert_eq!(next_boundary(Quantize::Bars(4), bar + 5, &tl, SR), 4 * bar);
        assert_eq!(next_boundary(Quantize::Beat, 10, &tl, SR), 24_000);
        s.command(
            LaunchCommand::Launch {
                track: t,
                slot: 42,
                quantize: Quantize::Bars(1),
                legato: false,
                repeat: 0,
            },
            90_000,
            true,
            &tl,
            SR,
        );
        // The block across the bar: the arrangement, then the clip.
        let (p, n) = pieces(s.track(t), 95_000, 2_000);
        assert_eq!(n, 2);
        assert_eq!(p[0], (0, 1_000, Play::Arrangement));
        assert_eq!(
            p[1],
            (
                1_000,
                1_000,
                Play::Clip {
                    slot: 42,
                    start: bar
                }
            )
        );
        s.advance(97_000);
        assert_eq!(s.track(t).unwrap().current, Some((42, bar)));
        // Stopped at the next bar: silence, not the arrangement.
        s.command(
            LaunchCommand::Stop {
                track: t,
                quantize: Quantize::Bars(1),
            },
            100_000,
            true,
            &tl,
            SR,
        );
        s.advance(3 * bar);
        assert_eq!(pieces(s.track(t), 3 * bar, 64).0[0].2, Play::Silence);
        s.command(LaunchCommand::BackToArrangement, 0, true, &tl, SR);
        assert_eq!(pieces(s.track(t), 0, 64).0[0].2, Play::Arrangement);
        // Published.
        let status = LaunchStatus::default();
        s.publish(&status);
        assert_eq!(status.read()[0].track, t);
    }
}

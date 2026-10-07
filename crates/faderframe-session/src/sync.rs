//! Following an external MIDI clock or MIDI time code (MTC).
//!
//! System messages arrive on the control side with the time they were
//! received on the shared MIDI clock. The pure followers turn them into
//! [`Follow`] events — start here at that moment, still here at that
//! moment, stop, moved — in the master's units (quarter notes for MIDI
//! clock, seconds for MTC). The session converts those into timeline
//! positions and drives the engine:
//!
//! * start: [`EngineController::chase`] locates so that the engine is at the
//!   master's position *plus the output latency* when the master sent it —
//!   our audio is heard that much after it is processed — and plays;
//! * while running, every tick compares the master's position with the
//!   engine's ([`EngineController::position_at`]). Two clocks without a
//!   shared word clock drift apart slowly: with varispeed (the default) a
//!   PI loop on the (smoothed) distance sets the engine's speed within
//!   ±1 % (`faderframe_engine::varispeed`), so it follows without jumps,
//!   and only a distance far beyond the tolerance for a few ticks chases
//!   again; without it, beyond the tolerance for a few ticks in a row the
//!   engine is chased again (each counted as a re-lock);
//! * stop and song-position / full-frame messages stop or move the
//!   transport.
//!
//! MIDI clock positions are mapped through the project's tempo map. The
//! master's tempo is measured (a least-squares fit over up to eight seconds
//! of clock timestamps, accurate to about 0.001 %); with "follow tempo" the
//! project tempo is set to it — one undoable edit — whenever the master
//! starts or stops and it differs. It is never changed while playing:
//! running audio clips would jump. Projects with tempo changes keep theirs.
//!
//! [`EngineController::chase`]: faderframe_engine::EngineController::chase
//! [`EngineController::position_at`]: faderframe_engine::EngineController::position_at

use crate::{Result, Session};
use faderframe_midi::{MidiSystemEvent, SystemMessage};
use faderframe_timeline::MusicalTime;
use std::collections::VecDeque;

/// Where the transport takes its timing from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SyncSource {
    #[default]
    Internal,
    MidiClock,
    Mtc,
}

impl SyncSource {
    pub const ALL: [SyncSource; 3] = [Self::Internal, Self::MidiClock, Self::Mtc];

    pub fn id(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::MidiClock => "midi-clock",
            Self::Mtc => "mtc",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.id() == id)
            .unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Internal => "Internal",
            Self::MidiClock => "MIDI Clock",
            Self::Mtc => "MIDI Time Code",
        }
    }
}

pub use faderframe_midi::timecode::{MtcRate, Timecode};

/// External synchronisation settings (per machine).
#[derive(Clone, Debug, PartialEq)]
pub struct SyncSettings {
    pub source: SyncSource,
    /// Input port key to listen to (`None`: any input).
    pub port: Option<String>,
    /// MTC: the timecode at the start of the project.
    pub offset: Timecode,
    /// Re-lock when the engine is further from the master than this.
    pub tolerance_ms: f64,
    /// MIDI clock: set the project tempo to the master's when it starts
    /// or stops.
    pub follow_tempo: bool,
    /// The rate MTC is sent at (from the project start's timecode,
    /// `offset`).
    pub mtc_out_rate: MtcRate,
    /// The timecode MTC output starts from at the project start.
    pub mtc_out_offset: Timecode,
    /// Follow by varispeed (no jumps) rather than by chasing.
    pub varispeed: bool,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            source: SyncSource::Internal,
            port: None,
            offset: Timecode::default(),
            tolerance_ms: 15.0,
            follow_tempo: true,
            mtc_out_rate: MtcRate::default(),
            mtc_out_offset: Timecode::default(),
            varispeed: true,
        }
    }
}

/// What the UI shows about synchronisation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SyncStatus {
    pub source: SyncSource,
    /// The master is running and we follow it.
    pub running: bool,
    /// Messages from the master arrive (clock ticks or quarter frames).
    pub receiving: bool,
    /// MIDI clock: the master's tempo.
    pub tempo: Option<f64>,
    /// The master's tempo differs from the project's (MIDI clock).
    pub tempo_differs: bool,
    /// MTC: the master's position and rate.
    pub timecode: Option<(Timecode, MtcRate)>,
    /// Times the engine had drifted too far and was chased again.
    pub relocks: u32,
    /// Last measured distance to the master, in milliseconds.
    pub error_ms: f64,
    /// The engine's speed (varispeed; 1 without).
    pub speed: f64,
}

/// A follower's verdict, in the master's units (quarters or seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Follow {
    /// Run from `at` at time `at_ns`.
    Start {
        at: f64,
        at_ns: u64,
    },
    /// Running: at `at` at time `at_ns`.
    Tick {
        at: f64,
        at_ns: u64,
    },
    Stop,
    /// Stopped, moved to `at`.
    Moved {
        at: f64,
    },
}

/// No clock for this long while running: the master is gone.
const CLOCK_TIMEOUT_NS: u64 = 500_000_000;
/// No quarter frame for this long: the master stopped.
const MTC_TIMEOUT_NS: u64 = 150_000_000;

/// Clock timestamps kept for the tempo fit (8 s at 120 BPM).
const TEMPO_WINDOW: usize = 384;

/// MIDI clock: 24 clocks per quarter, Start/Continue/Stop, song position.
#[derive(Debug, Default)]
pub(crate) struct ClockFollower {
    /// Clocks from the song start to the current position.
    ticks: u64,
    running: bool,
    /// Start/Continue received: the next clock is the first one played.
    pending: bool,
    last_ns: u64,
    /// Arrival times of the latest uninterrupted run of clocks.
    times: VecDeque<u64>,
}

impl ClockFollower {
    /// The master's tempo: a least-squares fit of arrival time against
    /// clock number (jitter averages out over the window).
    pub(crate) fn tempo(&self) -> Option<f64> {
        let n = self.times.len();
        if n < 12 {
            return None;
        }
        let t0 = self.times[0];
        let mean_i = (n - 1) as f64 / 2.0;
        let mean_t = self.times.iter().map(|&t| (t - t0) as f64).sum::<f64>() / n as f64;
        let (mut num, mut den) = (0.0, 0.0);
        for (i, &t) in self.times.iter().enumerate() {
            let di = i as f64 - mean_i;
            num += di * ((t - t0) as f64 - mean_t);
            den += di * di;
        }
        let ns_per_clock = num / den;
        (ns_per_clock > 0.0).then(|| 60e9 / (24.0 * ns_per_clock))
    }

    /// Enough clocks for a tempo to set the project to (about a second).
    pub(crate) fn settled_tempo(&self) -> Option<f64> {
        (self.times.len() >= 48).then(|| self.tempo()).flatten()
    }

    pub(crate) fn receiving(&self, now: u64) -> bool {
        self.last_ns > 0 && now.saturating_sub(self.last_ns) < CLOCK_TIMEOUT_NS
    }

    pub(crate) fn on(&mut self, msg: &SystemMessage, t: u64) -> Option<Follow> {
        let quarters = |ticks: u64| ticks as f64 / 24.0;
        match msg {
            SystemMessage::Clock => {
                let d = t.saturating_sub(self.last_ns);
                let n = self.times.len();
                let regular = match (self.times.front(), n) {
                    (Some(&first), 2..) => {
                        let expected = (self.last_ns - first) as f64 / (n - 1) as f64;
                        (0.7 * expected..1.3 * expected).contains(&(d as f64))
                    }
                    _ => true,
                };
                if self.last_ns == 0 || d == 0 || d >= 250_000_000 || !regular {
                    // A gap, a burst or a new tempo: fit afresh.
                    self.times.clear();
                }
                self.times.push_back(t);
                while self.times.len() > TEMPO_WINDOW {
                    self.times.pop_front();
                }
                self.last_ns = t;
                if self.pending {
                    self.pending = false;
                    self.running = true;
                    return Some(Follow::Start {
                        at: quarters(self.ticks),
                        at_ns: t,
                    });
                }
                if self.running {
                    self.ticks += 1;
                    return Some(Follow::Tick {
                        at: quarters(self.ticks),
                        at_ns: t,
                    });
                }
                None
            }
            SystemMessage::Start => {
                self.ticks = 0;
                self.pending = true;
                None
            }
            SystemMessage::Continue => {
                self.pending = true;
                None
            }
            SystemMessage::Stop => {
                let was = self.running || self.pending;
                self.running = false;
                self.pending = false;
                was.then_some(Follow::Stop)
            }
            SystemMessage::SongPosition(sixteenths) => {
                self.ticks = *sixteenths as u64 * 6;
                (!self.running).then_some(Follow::Moved {
                    at: quarters(self.ticks),
                })
            }
            _ => None,
        }
    }

    /// The master went silent while running.
    pub(crate) fn poll(&mut self, now: u64) -> Option<Follow> {
        if self.running && !self.receiving(now) {
            self.running = false;
            return Some(Follow::Stop);
        }
        None
    }
}

/// MTC: quarter frames (8 make a timecode, two frames long) and full-frame
/// SysEx messages.
#[derive(Debug, Default)]
pub(crate) struct MtcFollower {
    pieces: [u8; 8],
    /// Pieces received since piece 0, in order.
    seen: u8,
    last_piece: Option<u8>,
    rate: Option<MtcRate>,
    /// Frames (with quarter fractions) at the last quarter frame.
    frames: Option<f64>,
    last_ns: u64,
    running: bool,
}

impl MtcFollower {
    pub(crate) fn rate(&self) -> Option<MtcRate> {
        self.rate
    }

    /// The master's timecode at the last quarter frame.
    pub(crate) fn timecode(&self) -> Option<Timecode> {
        let rate = self.rate?;
        Some(Timecode::from_seconds(self.frames? / rate.fps(), rate))
    }

    pub(crate) fn receiving(&self, now: u64) -> bool {
        self.last_ns > 0 && now.saturating_sub(self.last_ns) < MTC_TIMEOUT_NS
    }

    pub(crate) fn on_quarter_frame(&mut self, data: u8, t: u64) -> Option<Follow> {
        let piece = (data >> 4) & 7;
        let value = data & 0x0F;
        let forward = self.last_piece.is_none_or(|l| (l + 1) % 8 == piece);
        self.last_piece = Some(piece);
        self.last_ns = t;
        if !forward {
            // Jumped or running backwards: wait for a clean sequence.
            self.seen = 0;
            self.frames = None;
            return None;
        }
        if piece == 0 {
            self.seen = 0;
        }
        self.pieces[piece as usize] = value;
        self.seen |= 1 << piece;
        if let Some(f) = &mut self.frames {
            *f += 0.25;
        }
        if piece == 7 && self.seen == 0xFF {
            let p = &self.pieces;
            let rate = MtcRate::from_bits(p[7] >> 1);
            let tc = Timecode {
                frames: p[0] | (p[1] & 1) << 4,
                seconds: p[2] | (p[3] & 3) << 4,
                minutes: p[4] | (p[5] & 3) << 4,
                hours: p[6] | (p[7] & 1) << 4,
            };
            self.rate = Some(rate);
            // The timecode is the time of piece 0; piece 7 comes 7/4 of a
            // frame later.
            self.frames = Some(tc.total_frames(rate) as f64 + 1.75);
        }
        let seconds = self.frames? / self.rate?.fps();
        let started = !self.running;
        self.running = true;
        Some(if started {
            Follow::Start {
                at: seconds,
                at_ns: t,
            }
        } else {
            Follow::Tick {
                at: seconds,
                at_ns: t,
            }
        })
    }

    /// `F0 7F <device> 01 01 hr mn sc fr F7`: a position while stopped.
    pub(crate) fn on_sysex(&mut self, bytes: &[u8]) -> Option<Follow> {
        let [0xF0, 0x7F, _, 0x01, 0x01, hr, mn, sc, fr, 0xF7] = *bytes else {
            return None;
        };
        let rate = MtcRate::from_bits(hr >> 5);
        let tc = Timecode {
            hours: hr & 0x1F,
            minutes: mn & 0x3F,
            seconds: sc & 0x3F,
            frames: fr & 0x1F,
        };
        self.rate = Some(rate);
        self.frames = Some(tc.total_frames(rate) as f64);
        self.seen = 0;
        self.last_piece = None;
        (!self.running).then(|| Follow::Moved {
            at: tc.to_seconds(rate),
        })
    }

    pub(crate) fn poll(&mut self, now: u64) -> Option<Follow> {
        if self.running && !self.receiving(now) {
            self.running = false;
            self.seen = 0;
            self.last_piece = None;
            return Some(Follow::Stop);
        }
        None
    }
}

/// Synchronisation state of a session.
#[derive(Debug, Default)]
pub(crate) struct SyncState {
    pub(crate) settings: SyncSettings,
    clock: ClockFollower,
    mtc: MtcFollower,
    running: bool,
    relocks: u32,
    error_ms: f64,
    /// Consecutive ticks beyond the tolerance.
    off: u32,
    now_ns: u64,
    /// Varispeed's loop: the smoothed distance (s), its integral, the last
    /// tick's time.
    filtered: Option<f64>,
    integral: f64,
    last_tick_ns: u64,
    /// A manual varispeed (percent), used while FaderFrame is the master.
    manual: Option<f64>,
}

/// How far a manual varispeed goes (percent).
pub const MANUAL_SPEED_RANGE: f64 = 10.0;

/// Varispeed's loop: proportional and integral gains (per second), the
/// distance's smoothing per tick, the speed's range and the distance (a
/// multiple of the tolerance) beyond which the engine is chased instead.
const KP: f64 = 1.0;
const KI: f64 = 0.5;
const SMOOTHING: f64 = 0.15;
const MAX_SPEED_DEVIATION: f64 = 0.01;
const CHASE_BEYOND: f64 = 4.0;

impl Session {
    pub fn sync_settings(&self) -> &SyncSettings {
        &self.sync.settings
    }

    /// Change the timing source; followers start from scratch.
    pub fn set_sync_settings(&mut self, settings: SyncSettings) {
        // Following a master by varispeed, or a manual speed while
        // FaderFrame is the master.
        let follows = settings.varispeed && settings.source != SyncSource::Internal;
        let manual = settings.source == SyncSource::Internal && self.sync.manual.is_some();
        if !follows && !manual && self.engine.varispeed() {
            let _ = self.engine.set_varispeed(false);
        }
        if settings.source != SyncSource::Internal {
            // The master sets the speed from now on.
            self.engine.set_speed(1.0);
        }
        let rate = settings.mtc_out_rate;
        self.engine
            .midi_shared()
            .set_mtc(rate, settings.mtc_out_offset.total_frames(rate));
        if settings != self.sync.settings {
            let manual = self.sync.manual;
            self.sync = SyncState {
                settings,
                manual,
                ..SyncState::default()
            };
            self.apply_manual_speed();
            self.revision += 1;
        }
    }

    pub fn sync_status(&self) -> SyncStatus {
        let s = &self.sync;
        let now = s.now_ns;
        SyncStatus {
            source: s.settings.source,
            running: s.running,
            receiving: match s.settings.source {
                SyncSource::Internal => false,
                SyncSource::MidiClock => s.clock.receiving(now),
                SyncSource::Mtc => s.mtc.receiving(now),
            },
            tempo: (s.settings.source == SyncSource::MidiClock)
                .then(|| s.clock.tempo())
                .flatten(),
            tempo_differs: s.settings.source == SyncSource::MidiClock
                && s.clock.settled_tempo().is_some_and(|t| {
                    let cur = self.project.timeline.tempo.points()[0].bpm;
                    ((t - cur) / cur).abs() > 5e-4
                }),
            timecode: (s.settings.source == SyncSource::Mtc)
                .then(|| s.mtc.timecode().zip(s.mtc.rate()))
                .flatten(),
            relocks: s.relocks,
            speed: self.engine.speed(),
            error_ms: s.error_ms,
        }
    }

    /// Feed system messages (control thread, from the MIDI tick).
    pub(crate) fn tick_sync(&mut self, events: &[MidiSystemEvent], now_ns: u64) {
        self.sync.now_ns = now_ns;
        let source = self.sync.settings.source;
        if source == SyncSource::Internal {
            return;
        }
        let mut follows = Vec::new();
        for ev in events {
            if let Some(key) = &self.sync.settings.port
                && self.midi.hub.port_key(ev.port) != Some(key.as_str())
            {
                continue;
            }
            let f = match (source, &ev.message) {
                (SyncSource::MidiClock, m) => self.sync.clock.on(m, ev.time_ns),
                (SyncSource::Mtc, SystemMessage::QuarterFrame(d)) => {
                    self.sync.mtc.on_quarter_frame(*d, ev.time_ns)
                }
                (SyncSource::Mtc, SystemMessage::SysEx(b)) => self.sync.mtc.on_sysex(b),
                _ => None,
            };
            follows.extend(f);
        }
        let timeout = match source {
            SyncSource::MidiClock => self.sync.clock.poll(now_ns),
            SyncSource::Mtc => self.sync.mtc.poll(now_ns),
            SyncSource::Internal => None,
        };
        follows.extend(timeout);
        // Only the newest tick matters for drift; starts and stops all do.
        let last_tick = follows
            .iter()
            .rposition(|f| matches!(f, Follow::Tick { .. }));
        for (i, f) in follows.into_iter().enumerate() {
            if matches!(f, Follow::Tick { .. }) && Some(i) != last_tick {
                continue;
            }
            if let Err(e) = self.apply_follow(f) {
                self.notify(crate::NoticeLevel::Warning, format!("sync: {e}"));
            }
        }
    }

    /// The master's position (quarters or seconds) as a timeline sample.
    fn sync_samples(&self, at: f64) -> i64 {
        let rate = self.engine.sample_rate() as f64;
        match self.sync.settings.source {
            SyncSource::MidiClock => self
                .engine
                .musical_to_samples(&self.project, MusicalTime::from_quarters(at)),
            SyncSource::Mtc => {
                let rate_mtc = self.sync.mtc.rate().unwrap_or_default();
                let offset = self.sync.settings.offset.to_seconds(rate_mtc);
                ((at - offset) * rate).round() as i64
            }
            SyncSource::Internal => 0,
        }
    }

    fn apply_follow(&mut self, f: Follow) -> Result<()> {
        let latency = self.engine.output_latency() as i64;
        match f {
            Follow::Start { at, at_ns } => {
                self.follow_external_tempo();
                self.start_varispeed()?;
                let target = self.sync_samples(at) + latency;
                self.sync_start(target, at_ns)?;
                self.sync.running = true;
                self.sync.off = 0;
                let what = self.sync.settings.source.label();
                self.notify(crate::NoticeLevel::Info, format!("following {what}"));
            }
            Follow::Tick { at, at_ns } => {
                // A new stream (or engine) came without varispeed.
                if self.sync.settings.varispeed
                    && !self.engine.varispeed()
                    && self.engine.stream_sample_rate() > 0
                {
                    self.start_varispeed()?;
                }
                let target = self.sync_samples(at) + latency;
                let rate = self.engine.sample_rate().max(1) as f64;
                match self.engine.position_at(at_ns) {
                    Some(pos) if self.transport.playing && self.engine.varispeed() => {
                        let err = (target - pos) as f64 / rate;
                        self.sync.error_ms = err * 1000.0;
                        let far = self.sync.settings.tolerance_ms * CHASE_BEYOND / 1000.0;
                        if err.abs() > far {
                            self.sync.off += 1;
                        } else {
                            self.sync.off = 0;
                        }
                        if self.sync.off >= 3 {
                            self.sync.off = 0;
                            self.sync.relocks += 1;
                            self.start_varispeed()?;
                            self.engine.chase(target, at_ns, true)?;
                        } else {
                            self.steer(err, at_ns);
                        }
                    }
                    Some(pos) if self.transport.playing => {
                        let err = (target - pos) as f64 / rate * 1000.0;
                        self.sync.error_ms = err;
                        if err.abs() > self.sync.settings.tolerance_ms {
                            self.sync.off += 1;
                        } else {
                            self.sync.off = 0;
                        }
                        // A few ticks in a row: not just jitter.
                        if self.sync.off >= 3 {
                            self.sync.off = 0;
                            self.sync.relocks += 1;
                            self.engine.chase(target, at_ns, true)?;
                        }
                    }
                    _ => {
                        // Running master, stopped engine: join in.
                        self.start_varispeed()?;
                        self.sync_start(target, at_ns)?;
                    }
                }
                self.sync.running = true;
            }
            Follow::Stop => {
                self.sync.running = false;
                self.engine.set_speed(1.0);
                self.sync.filtered = None;
                self.sync.integral = 0.0;
                self.follow_external_tempo();
                let what = self.sync.settings.source.label();
                self.notify(crate::NoticeLevel::Info, format!("{what} stopped"));
                if self.transport.playing {
                    self.engine
                        .transport(faderframe_transport::TransportCommand::Stop)?;
                    self.transport.playing = false;
                    self.stop_recording()?;
                    self.automation_play_stopped();
                }
            }
            Follow::Moved { at } => {
                let target = self.sync_samples(at).max(0);
                if !self.transport.playing {
                    self.engine
                        .transport(faderframe_transport::TransportCommand::Locate(target))?;
                    self.show_position(target);
                }
            }
        }
        self.revision += 1;
        Ok(())
    }

    /// Set the project tempo to the master's (MIDI clock, single-tempo
    /// projects, only while stopped; one undoable edit).
    fn follow_external_tempo(&mut self) {
        let s = &self.sync;
        if s.settings.source != SyncSource::MidiClock || !s.settings.follow_tempo {
            return;
        }
        let Some(tempo) = s.clock.settled_tempo() else {
            return;
        };
        let points = self.project.timeline.tempo.points();
        if points.len() != 1 {
            return;
        }
        let bpm = (tempo * 1000.0).round() / 1000.0;
        if ((bpm - points[0].bpm) / points[0].bpm).abs() < 2e-5 {
            return;
        }
        let edit = crate::Action::Edit(faderframe_project::Command::SetTempo { bpm });
        if let Err(e) = self.dispatch(edit) {
            self.notify(crate::NoticeLevel::Warning, format!("sync: {e}"));
        } else {
            self.notify(
                crate::NoticeLevel::Info,
                format!("tempo set to the master's {bpm:.2} BPM"),
            );
        }
    }

    /// Play from `target` as of `at_ns` (like the play button: recording
    /// and automation writing start as usual).
    /// A manual varispeed in percent (±[`MANUAL_SPEED_RANGE`]; `None`:
    /// off): the song plays that much faster or slower, higher or lower.
    /// While a master is followed it sets the speed; the manual one comes
    /// back when FaderFrame is the master again.
    pub fn set_manual_speed(&mut self, percent: Option<f64>) {
        self.sync.manual = percent
            .filter(|p| p.is_finite())
            .map(|p| p.clamp(-MANUAL_SPEED_RANGE, MANUAL_SPEED_RANGE));
        self.apply_manual_speed();
        self.revision += 1;
    }

    pub fn manual_speed(&self) -> Option<f64> {
        self.sync.manual
    }

    /// The manual varispeed onto the engine (when FaderFrame is the master
    /// and the stream runs: the resampler is made for its channels).
    pub(crate) fn apply_manual_speed(&mut self) {
        if self.sync.settings.source != SyncSource::Internal {
            return;
        }
        match self.sync.manual {
            Some(p) => {
                if !self.engine.varispeed() {
                    if self.engine.stream_sample_rate() == 0 {
                        // Not prepared yet: the next tick tries again.
                        return;
                    }
                    if let Err(e) = self.engine.set_varispeed(true) {
                        self.notify(crate::NoticeLevel::Warning, format!("varispeed: {e}"));
                        return;
                    }
                }
                self.engine.set_speed(1.0 + p / 100.0);
            }
            None => {
                if self.engine.varispeed() {
                    let _ = self.engine.set_varispeed(false);
                }
            }
        }
    }

    /// Varispeed on (when set) at speed 1, its loop from scratch.
    fn start_varispeed(&mut self) -> Result<()> {
        self.sync.filtered = None;
        self.sync.integral = 0.0;
        self.sync.last_tick_ns = 0;
        if self.sync.settings.varispeed {
            if !self.engine.varispeed() {
                self.engine.set_varispeed(true)?;
            }
            self.engine.set_speed(1.0);
        }
        Ok(())
    }

    /// One step of varispeed's loop on the distance `err` (s, positive:
    /// the master is ahead).
    fn steer(&mut self, err: f64, at_ns: u64) {
        let s = &mut self.sync;
        let dt = if s.last_tick_ns == 0 {
            0.0
        } else {
            (at_ns.saturating_sub(s.last_tick_ns) as f64 / 1e9).min(0.5)
        };
        s.last_tick_ns = at_ns;
        let e = match s.filtered {
            Some(f) => f + SMOOTHING * (err - f),
            None => err,
        };
        s.filtered = Some(e);
        let limit = MAX_SPEED_DEVIATION / KI;
        s.integral = (s.integral + e * dt).clamp(-limit, limit);
        let speed =
            1.0 + (KP * e + KI * s.integral).clamp(-MAX_SPEED_DEVIATION, MAX_SPEED_DEVIATION);
        self.engine.set_speed(speed);
    }

    fn sync_start(&mut self, target: i64, at_ns: u64) -> Result<()> {
        if !self.transport.playing
            && self.recording.as_ref().is_some_and(|r| r.from != target)
            && self
                .project
                .punch_range
                .filter(|_| self.project.punch_enabled)
                .is_none()
        {
            self.stop_recording()?;
            self.start_recording(target)?;
        }
        self.engine.chase(target, at_ns, true)?;
        self.show_position(target);
        self.loader.wake();
        if !self.transport.playing {
            self.transport.playing = true;
            self.automation_play_started();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn timecode_round_trips_at_every_rate() {
        for rate in [
            MtcRate::Fps24,
            MtcRate::Fps25,
            MtcRate::Fps2997Drop,
            MtcRate::Fps30,
        ] {
            for s in [0.0, 1.0, 59.97, 60.5, 600.0, 3723.4] {
                let tc = Timecode::from_seconds(s, rate);
                let back = tc.to_seconds(rate);
                assert!(
                    (back - s).abs() < 1.0 / rate.fps() + 1e-6,
                    "{rate:?} {s} → {tc} → {back}"
                );
            }
        }
        // Drop-frame: labels ;00 and ;01 are skipped at minute 1, so frame
        // 1800 is 00:01:00;02 (and minute 10 keeps all labels).
        let df = MtcRate::Fps2997Drop;
        let tc = Timecode::from_seconds(1800.0 / df.fps(), df);
        assert_eq!(tc.to_string(), "00:01:00:02");
        assert_eq!(tc.total_frames(df), 1800);
        let ten = Timecode::from_seconds(17_982.0 / df.fps(), df);
        assert_eq!(ten.to_string(), "00:10:00:00");
        assert_eq!(
            Timecode::parse("01:02:03:04").unwrap().to_string(),
            "01:02:03:04"
        );
        assert_eq!(Timecode::parse("1:2:3"), None);
    }

    #[test]
    fn clock_follows_start_ticks_tempo_and_stop() {
        let mut c = ClockFollower::default();
        let tick = 500 * MS / 24; // 120 BPM
        let mut t = 1_000 * MS;
        // Clocks before Start: tempo only.
        for _ in 0..30 {
            assert_eq!(c.on(&SystemMessage::Clock, t), None);
            t += tick;
        }
        assert!((c.tempo().unwrap() - 120.0).abs() < 0.01);
        assert_eq!(c.on(&SystemMessage::Start, t), None);
        t += tick;
        assert_eq!(
            c.on(&SystemMessage::Clock, t),
            Some(Follow::Start { at: 0.0, at_ns: t })
        );
        for i in 1..=48 {
            t += tick;
            assert_eq!(
                c.on(&SystemMessage::Clock, t),
                Some(Follow::Tick {
                    at: i as f64 / 24.0,
                    at_ns: t
                })
            );
        }
        assert_eq!(c.on(&SystemMessage::Stop, t), Some(Follow::Stop));
        // Song position while stopped moves; Continue resumes there.
        assert_eq!(
            c.on(&SystemMessage::SongPosition(16), t),
            Some(Follow::Moved { at: 4.0 })
        );
        c.on(&SystemMessage::Continue, t);
        t += tick;
        assert_eq!(
            c.on(&SystemMessage::Clock, t),
            Some(Follow::Start { at: 4.0, at_ns: t })
        );
        // The master disappears.
        assert_eq!(c.poll(t + 100 * MS), None);
        assert_eq!(c.poll(t + 600 * MS), Some(Follow::Stop));
    }

    /// Quarter frames for a timecode starting at `tc`, `count` of them.
    fn quarter_frames(tc: Timecode, rate: MtcRate, count: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut frames = tc.total_frames(rate);
        while out.len() < count {
            let t = Timecode::from_seconds(frames as f64 / rate.fps() + 1e-4, rate);
            let rate_bits = match rate {
                MtcRate::Fps24 => 0,
                MtcRate::Fps25 => 1,
                MtcRate::Fps2997Drop => 2,
                MtcRate::Fps30 => 3,
            };
            let pieces = [
                t.frames & 0x0F,
                t.frames >> 4,
                t.seconds & 0x0F,
                t.seconds >> 4,
                t.minutes & 0x0F,
                t.minutes >> 4,
                t.hours & 0x0F,
                (t.hours >> 4) | rate_bits << 1,
            ];
            for (i, p) in pieces.into_iter().enumerate() {
                out.push((i as u8) << 4 | p);
            }
            frames += 2;
        }
        out.truncate(count);
        out
    }

    #[test]
    fn mtc_locks_after_a_full_sequence_and_tracks_quarter_frames() {
        let rate = MtcRate::Fps25;
        let start = Timecode {
            hours: 1,
            minutes: 0,
            seconds: 10,
            frames: 0,
        };
        let qf = 10 * MS; // 25 fps: 40 ms per frame, 10 ms per quarter
        let mut m = MtcFollower::default();
        let mut t = 0;
        let mut first = None;
        let mut last = None;
        for (i, d) in quarter_frames(start, rate, 40).into_iter().enumerate() {
            t += qf;
            match m.on_quarter_frame(d, t) {
                Some(f @ Follow::Start { .. }) => {
                    assert_eq!(i, 7, "locks on the 8th quarter frame");
                    first = Some(f);
                }
                Some(f @ Follow::Tick { .. }) => last = Some((f, i)),
                _ => assert!(first.is_none() || i < 7),
            }
        }
        let Some(Follow::Start { at, .. }) = first else {
            panic!("no start")
        };
        let base = start.to_seconds(rate);
        assert!((at - (base + 1.75 / 25.0)).abs() < 1e-9);
        // Every quarter frame advances by a quarter frame.
        let Some((Follow::Tick { at, .. }, i)) = last else {
            panic!("no ticks")
        };
        assert!((at - (base + (i as f64 + 1.0) / 4.0 / 25.0 - 0.25 / 25.0)).abs() < 1e-6);
        assert_eq!(m.rate(), Some(rate));
        assert_eq!(m.poll(t + 200 * MS), Some(Follow::Stop));
        // Full frame while stopped: moved there.
        let f = m.on_sysex(&[0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x21, 0x02, 0x03, 0x04, 0xF7]);
        let Some(Follow::Moved { at }) = f else {
            panic!("{f:?}")
        };
        let tc = Timecode {
            hours: 1,
            minutes: 2,
            seconds: 3,
            frames: 4,
        };
        assert!((at - tc.to_seconds(MtcRate::Fps25)).abs() < 1e-9);
    }

    #[test]
    fn mtc_ignores_out_of_order_quarter_frames() {
        let mut m = MtcFollower::default();
        let qfs = quarter_frames(Timecode::default(), MtcRate::Fps30, 16);
        // Starting mid-sequence: no lock until a clean 0…7 run.
        let mut t = 0;
        let mut locked_at = None;
        for (i, d) in qfs.iter().enumerate().skip(3) {
            t += 8 * MS;
            if m.on_quarter_frame(*d, t).is_some() && locked_at.is_none() {
                locked_at = Some(i);
            }
        }
        assert_eq!(locked_at, Some(15));
    }
}

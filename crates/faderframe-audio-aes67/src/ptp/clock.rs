//! The PTP time this process keeps: a software clock on top of the
//! process's monotonic clock (`faderframe_audio::follow::now_ns`), never
//! the system clock — nothing here needs privileges, and the audio
//! threads read it without a system call.
//!
//! [`VirtualClock`] maps local nanoseconds to PTP nanoseconds (TAI since
//! 1970) by an anchor and a rate; the servo moves it by re-anchoring, so
//! the mapping is continuous except where it steps on purpose. It is
//! published to other threads through [`ClockMap`], a sequence lock over
//! atomics (the audio thread reads it; a write never blocks a reader).
//! Media clocks are the PTP time counted in samples from the epoch
//! (AES67's `a=mediaclk:direct=0`): [`media_frame`] and [`frame_ns`].

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering, fence};
use std::time::{SystemTime, UNIX_EPOCH};

/// TAI − UTC since 2017 (the PTP timescale runs this far ahead of the
/// system's UTC clock).
pub const TAI_OFFSET_S: u64 = 37;

/// Local nanoseconds → PTP nanoseconds: `ptp0 + (local − local0) · rate`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualClock {
    pub local0: u64,
    pub ptp0: u64,
    /// PTP nanoseconds per local nanosecond.
    pub rate: f64,
}

impl VirtualClock {
    /// A clock reading `ptp` at local `local`, running at the local rate.
    pub fn at(local: u64, ptp: u64) -> Self {
        Self {
            local0: local,
            ptp0: ptp,
            rate: 1.0,
        }
    }

    /// The system's UTC clock as TAI, read now against local `local`: what
    /// the clock starts from before it follows anyone.
    pub fn from_system(local: u64) -> Self {
        Self::at(local, system_tai_ns())
    }

    pub fn ptp(&self, local: u64) -> u64 {
        let d = local as i128 - self.local0 as i128;
        let scaled = d as f64 * self.rate;
        (self.ptp0 as i128 + scaled.round() as i128).clamp(0, u64::MAX as i128) as u64
    }

    pub fn local(&self, ptp: u64) -> u64 {
        let d = ptp as i128 - self.ptp0 as i128;
        let scaled = d as f64 / self.rate.max(1e-6);
        (self.local0 as i128 + scaled.round() as i128).clamp(0, u64::MAX as i128) as u64
    }

    /// The same clock re-anchored at `local` with a new rate (continuous:
    /// it reads the same at `local`), moved by `step` nanoseconds.
    pub fn retuned(&self, local: u64, rate: f64, step: i64) -> Self {
        let now = self.ptp(local) as i128 + i128::from(step);
        Self {
            local0: local,
            ptp0: now.clamp(0, u64::MAX as i128) as u64,
            rate,
        }
    }
}

/// The system clock's UTC as TAI nanoseconds.
pub fn system_tai_ns() -> u64 {
    let utc = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    utc + TAI_OFFSET_S * 1_000_000_000
}

/// Where the published clock comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockState {
    /// The system clock, not followed by anything (no grandmaster yet).
    FreeRunning,
    /// Following a grandmaster, not settled yet.
    Locking,
    /// Following a grandmaster, settled.
    Locked,
    /// This clock is the grandmaster.
    Master,
    /// The system clock, kept to PTP by another program (ptp4l and
    /// phc2sys).
    System,
}

impl ClockState {
    fn code(self) -> u8 {
        match self {
            ClockState::FreeRunning => 0,
            ClockState::Locking => 1,
            ClockState::Locked => 2,
            ClockState::Master => 3,
            ClockState::System => 4,
        }
    }

    fn from_code(c: u8) -> Self {
        match c {
            1 => ClockState::Locking,
            2 => ClockState::Locked,
            3 => ClockState::Master,
            4 => ClockState::System,
            _ => ClockState::FreeRunning,
        }
    }

    /// Others can follow the media clock (a grandmaster leads it).
    pub fn is_shared(self) -> bool {
        matches!(
            self,
            ClockState::Locked | ClockState::Master | ClockState::System
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            ClockState::FreeRunning => "running free (no grandmaster)",
            ClockState::Locking => "locking to the grandmaster",
            ClockState::Locked => "locked to the grandmaster",
            ClockState::Master => "grandmaster (others follow this computer)",
            ClockState::System => "the system clock (kept by ptp4l/phc2sys)",
        }
    }
}

/// One copy of the clock behind a sequence lock.
#[derive(Debug, Default)]
struct Slot {
    seq: AtomicU64,
    local0: AtomicU64,
    ptp0: AtomicU64,
    rate: AtomicU64,
}

impl Slot {
    fn write(&self, clock: VirtualClock) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.local0.store(clock.local0, Ordering::Relaxed);
        self.ptp0.store(clock.ptp0, Ordering::Relaxed);
        self.rate.store(clock.rate.to_bits(), Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    fn read(&self) -> Option<VirtualClock> {
        let s1 = self.seq.load(Ordering::Acquire);
        if s1 & 1 == 1 {
            return None;
        }
        let clock = VirtualClock {
            local0: self.local0.load(Ordering::Relaxed),
            ptp0: self.ptp0.load(Ordering::Relaxed),
            rate: f64::from_bits(self.rate.load(Ordering::Relaxed)),
        };
        fence(Ordering::Acquire);
        (self.seq.load(Ordering::Relaxed) == s1).then_some(clock)
    }
}

/// A [`VirtualClock`] shared between threads: written by the PTP service,
/// read by the audio and network threads (realtime safe: atomics only).
/// Two copies: the writer fills the one readers are not directed to and
/// then turns them to it, so a writer interrupted half way never holds a
/// reader up (the audio thread outranks it). `steps` counts jumps (a new
/// grandmaster, the first lock): readers that count samples from it start
/// again when it changes.
#[derive(Debug, Default)]
pub struct ClockMap {
    slots: [Slot; 2],
    /// The copy to read (its low bit), counting publications.
    active: AtomicU64,
    steps: AtomicU64,
    state: AtomicU8,
}

impl ClockMap {
    pub fn new(clock: VirtualClock) -> Self {
        let m = Self::default();
        m.slots[0].write(clock);
        m.slots[1].write(clock);
        m
    }

    /// Publish `clock`; `stepped`: it jumped (readers resynchronise). One
    /// writer at a time (the PTP service's lock).
    pub fn publish(&self, clock: VirtualClock, stepped: bool) {
        let next = self.active.load(Ordering::Relaxed).wrapping_add(1);
        self.slots[(next & 1) as usize].write(clock);
        self.active.store(next, Ordering::Release);
        if stepped {
            self.steps.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn set_state(&self, state: ClockState) {
        self.state.store(state.code(), Ordering::Relaxed);
    }

    pub fn state(&self) -> ClockState {
        ClockState::from_code(self.state.load(Ordering::Relaxed))
    }

    /// Times the clock jumped.
    pub fn steps(&self) -> u64 {
        self.steps.load(Ordering::Relaxed)
    }

    /// The clock as last published. A read only retries when the writer
    /// published twice while it read (it publishes a few times a second).
    pub fn read(&self) -> VirtualClock {
        let mut last = None;
        for _ in 0..1_000 {
            let k = self.active.load(Ordering::Acquire);
            let slot = &self.slots[(k & 1) as usize];
            if let Some(c) = slot.read() {
                return c;
            }
            last = Some(k);
            std::hint::spin_loop();
        }
        // Unreachable in practice; the other copy is complete.
        let k = last.unwrap_or(0).wrapping_add(1);
        self.slots[(k & 1) as usize]
            .read()
            .unwrap_or(VirtualClock::at(0, 0))
    }

    /// PTP time at local `local`.
    pub fn ptp(&self, local: u64) -> u64 {
        self.read().ptp(local)
    }

    /// PTP time now.
    pub fn now(&self) -> u64 {
        self.ptp(faderframe_audio::follow::now_ns())
    }

    /// The local time at which the clock reads `ptp`.
    pub fn local(&self, ptp: u64) -> u64 {
        self.read().local(ptp)
    }
}

/// The media clock at PTP time `ns`: samples at `rate` since the epoch.
pub fn media_frame(ns: u64, rate: u32) -> u64 {
    (u128::from(ns) * u128::from(rate) / 1_000_000_000) as u64
}

/// The PTP time sample `frame` of a media clock at `rate` begins (the
/// first nanosecond at which [`media_frame`] reads it).
pub fn frame_ns(frame: u64, rate: u32) -> u64 {
    let r = u128::from(rate.max(1));
    ((u128::from(frame) * 1_000_000_000).div_ceil(r)) as u64
}

/// The full media frame an RTP timestamp `ts` (its low 32 bits) stands for,
/// the one nearest `near`.
pub fn unwrap_timestamp(ts: u32, near: u64) -> u64 {
    let back = (near as u32).wrapping_sub(ts) as i32;
    (near as i64).wrapping_sub(i64::from(back)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retuned_clock_is_continuous() {
        let c = VirtualClock::at(1_000, 5_000_000_000_000_000_000);
        assert_eq!(c.ptp(2_000), 5_000_000_000_000_001_000);
        let r = c.retuned(2_000, 1.0 + 50e-6, 0);
        assert_eq!(r.ptp(2_000), c.ptp(2_000));
        // 1 s later the faster clock is 50 µs ahead.
        assert_eq!(
            r.ptp(1_000_002_000) - c.ptp(1_000_002_000),
            50_000,
            "rate applied"
        );
        let stepped = c.retuned(2_000, 1.0, -300);
        assert_eq!(stepped.ptp(2_000), c.ptp(2_000) - 300);
        // local() inverts ptp().
        for l in [0u64, 7_777, 3_000_000_000] {
            let back = r.local(r.ptp(l));
            assert!(back.abs_diff(l) <= 1, "{l} → {back}");
        }
    }

    #[test]
    fn the_shared_clock_reads_what_was_published() {
        let m = ClockMap::new(VirtualClock::at(10, 20));
        assert_eq!(m.read(), VirtualClock::at(10, 20));
        m.publish(VirtualClock::at(30, 40).retuned(30, 1.0001, 0), true);
        assert_eq!(m.read().rate, 1.0001);
        assert_eq!(m.steps(), 1);
        m.set_state(ClockState::Locked);
        assert_eq!(m.state(), ClockState::Locked);
        assert!(ClockState::Locked.is_shared() && !ClockState::Locking.is_shared());
    }

    #[test]
    fn media_frames_count_from_the_epoch() {
        // 1.7e9 s at 48 kHz.
        let ns = 1_700_000_000_000_000_000u64;
        assert_eq!(media_frame(ns, 48_000), 81_600_000_000_000);
        assert_eq!(frame_ns(81_600_000_000_000, 48_000), ns);
        // A frame begins at the first nanosecond that reads it.
        for f in [1u64, 2, 3, 12_345_678_901] {
            let t = frame_ns(f, 44_100);
            assert_eq!(media_frame(t, 44_100), f);
            assert_eq!(media_frame(t - 1, 44_100), f - 1);
        }
        // RTP timestamps are the low 32 bits.
        let near = 81_600_000_000_000u64;
        for d in [-5_000i64, 0, 7_000] {
            let f = (near as i64 + d) as u64;
            assert_eq!(unwrap_timestamp(f as u32, near), f);
        }
    }
}

//! Priorities of the threads on the audio path.
//!
//! The audio callback's thread is on top: the audio server's (PipeWire's
//! data loop, JACK's process thread, CoreAudio's IO thread) or, where a
//! backend runs its own, made realtime here ([`make_audio_thread_realtime`]
//! — ALSA and WASAPI through cpal). The DSP workers take its scheduling
//! as it is (`WorkerPool`), as do sandboxed plugins' audio threads. The
//! threads that work for it follow it a step below ([`follow_audio`]), so
//! that they never preempt it nor the workers it waits for, while nothing
//! else on the system preempts them:
//!
//! | thread | Linux (`SCHED_FIFO`) | Windows (MMCSS "Pro Audio") |
//! |---|---|---|
//! | audio callback, DSP workers, sandbox audio | P (the server's) | critical / high |
//! | circuit reservoirs, the shallow render-ahead tier | P − 1 | high |
//! | the deep render-ahead tier and its workers, MIDI in/out | P − 5 | normal |
//! | disk streaming, recording to disk | P − 10 | low |
//!
//! The UI and every other thread (imports, renders, analyses, video) stay
//! at normal priority: a busy one would otherwise compete with the audio.
//! The audio thread publishes its scheduling now and then
//! ([`note_audio_thread`], one system call every few thousand callbacks);
//! helpers re-apply theirs when it changes (an atomic load per loop).
//! macOS: CoreAudio's thread is realtime already; helpers keep the system's
//! choice. Linux without `RLIMIT_RTPRIO`: a thread asks RealtimeKit through
//! the process's broker (`rtkit`) and carries on — never waits for it —;
//! disk streaming and the recording writer do not ask (rtkit's realtime
//! threads are few).

use crate::pool::sched;
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

/// What a helper thread does for the audio thread (how far below it runs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    /// Computes audio the callback needs within a buffer or two (circuit
    /// reservoirs, the shallow render-ahead tier).
    Near,
    /// Renders well ahead (the deep render-ahead tier), or carries MIDI
    /// between devices and the engine.
    Ahead,
    /// Moves audio between disk and memory with seconds of slack (disk
    /// streaming, the recording writer).
    Feeder,
}

impl Helper {
    /// Steps below the audio thread's `SCHED_FIFO` priority (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn below(self) -> i32 {
        match self {
            Helper::Near => 1,
            Helper::Ahead => 5,
            Helper::Feeder => 10,
        }
    }
}

/// The audio thread's scheduling as `sched::current` packs it (0: not
/// known yet), and how often it changed.
static AUDIO: AtomicU64 = AtomicU64::new(0);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Threads that could not take the priority they should have.
static FAILURES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The generation this thread last followed.
    static FOLLOWED: Cell<u64> = const { Cell::new(u64::MAX) };
}

/// The `SCHED_FIFO` priority FaderFrame gives an audio thread it runs
/// itself (where the audio server does not: ALSA), below the kernel's
/// interrupt threads; capped by `RLIMIT_RTPRIO`.
pub const OWN_AUDIO_PRIORITY: i32 = 80;

/// On the audio callback's thread (once, at stream start: a system call):
/// make it realtime if it is not already. Returns whether it is now —
/// where RealtimeKit has to grant it, it is asked for (never waited for:
/// [`crate::rtkit`]) and published for the helpers once granted.
pub fn make_audio_thread_realtime() -> bool {
    let applied = imp::make_audio_thread_realtime();
    if applied == Applied::Failed {
        FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    note_audio_thread();
    applied == Applied::Now
}

/// Start what grants realtime where the process may not take it itself
/// (Linux: RealtimeKit's broker thread, `rtkit::start`); a control
/// thread, at start-up — later calls do nothing. Elsewhere nothing.
pub fn start_requests() {
    #[cfg(target_os = "linux")]
    crate::rtkit::start();
}

/// How a thread's scheduling was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// It has it now.
    Now,
    /// Asked for (RealtimeKit grants it through the broker, or not).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Asked,
    /// It could not be had.
    Failed,
}

/// The audio thread was made realtime from outside (RealtimeKit, asked
/// by the broker): its scheduling published as [`note_audio_thread`]
/// would, so the helpers follow without waiting for its next note.
#[cfg(target_os = "linux")]
pub(crate) fn audio_thread_granted(packed: u64) {
    if crate::pool::sched::is_realtime(packed) && AUDIO.swap(packed, Ordering::Relaxed) != packed {
        GENERATION.fetch_add(1, Ordering::Release);
    }
}

/// On the audio thread: publish its scheduling (a system call — call it
/// once and then every few thousand callbacks).
pub fn note_audio_thread() {
    let (packed, _) = sched::current();
    if AUDIO.swap(packed, Ordering::Relaxed) != packed {
        GENERATION.fetch_add(1, Ordering::Release);
    }
}

/// How often the audio thread's scheduling changed (helpers follow it when
/// it does).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Threads made realtime from outside (RealtimeKit's grants, asked by the
/// broker): a pool's caller may be one.
static GRANTS: AtomicU64 = AtomicU64::new(0);

/// A grant came (the broker; one atomic).
#[cfg(target_os = "linux")]
pub(crate) fn note_grant() {
    GRANTS.fetch_add(1, Ordering::Release);
}

/// Changes whenever the audio thread's scheduling changes or a thread was
/// made realtime from outside: pools re-read their caller's scheduling
/// then (a render-ahead tier granted after its pool's first run).
pub(crate) fn scheduling_epoch() -> u64 {
    GENERATION
        .load(Ordering::Acquire)
        .wrapping_add(GRANTS.load(Ordering::Acquire))
}

/// Whether the audio thread is known to run realtime.
pub fn audio_thread_realtime() -> bool {
    sched::is_realtime(AUDIO.load(Ordering::Relaxed))
}

/// On a helper thread, in its loop: take `helper`'s place below the audio
/// thread when the audio thread's scheduling changed since this thread
/// last looked (an atomic load otherwise). Without a realtime audio thread
/// (the dummy device, no permission) helpers run normally.
pub fn follow_audio(helper: Helper) {
    breathe();
    let generation = GENERATION.load(Ordering::Acquire);
    if FOLLOWED.with(|f| f.replace(generation)) == generation {
        return;
    }
    // The realtime time limit may have come with the audio thread (set by
    // RealtimeKit's users: PipeWire's client, ours).
    note_rttime_limit();
    if imp::follow(AUDIO.load(Ordering::Relaxed), helper) == Applied::Failed {
        FAILURES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Microseconds between two breaths of a realtime helper (0: no
/// realtime time limit, no breaths): a quarter of the `RLIMIT_RTTIME` soft
/// limit.
static BREATHE_US: AtomicU64 = AtomicU64::new(0);
/// How long a breath blocks: long enough that the thread really sleeps (a
/// shorter timer can expire before the thread is put to sleep).
const BREATH: std::time::Duration = std::time::Duration::from_micros(50);

thread_local! {
    /// When this thread last blocked for a breath.
    static BREATHED: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// Read the process's realtime time limit (`RLIMIT_RTTIME`: a realtime
/// thread running that long without blocking gets `SIGXCPU`, at the hard
/// limit the process `SIGKILL`), keep a margin under it (Linux: the soft
/// limit below the hard one and `SIGXCPU` caught — the thread that ran too
/// long returns to normal scheduling instead of the process being killed;
/// see `rttime`) and pace breaths by it ([`breathe`]). Helpers do it when
/// the audio thread's scheduling changes; call it at start-up, after
/// opening an audio stream (PipeWire's client sets the limit again with
/// every context: soft = hard) and now and then from a control thread
/// (a system call or two).
pub fn note_rttime_limit() {
    let us = imp::guard_rttime().map_or(0, |soft| (soft / 4).max(1_000));
    BREATHE_US.store(us, Ordering::Relaxed);
}

/// Threads of this process returned to normal scheduling because they ran
/// realtime past the time limit without sleeping (Linux; counted with
/// [`priority_failures`] too), and the system's id of the last one.
pub fn rttime_demotions() -> (u64, i32) {
    imp::rttime_demotions()
}

/// Whether this thread is due a breath (under a realtime time limit, a
/// quarter of it since it last took one).
pub(crate) fn must_breathe() -> bool {
    let every = BREATHE_US.load(Ordering::Relaxed);
    every != 0
        && BREATHED.with(|b| {
            let now = std::time::Instant::now();
            match b.get() {
                Some(t) => now.duration_since(t).as_micros() as u64 >= every,
                None => {
                    b.set(Some(now));
                    false
                }
            }
        })
}

fn breathed() {
    BREATHED.with(|b| b.set(Some(std::time::Instant::now())));
}

/// Under a realtime time limit (RealtimeKit's), a helper that computes for
/// long stretches — the deep render-ahead tier catching up after a locate,
/// the disk loader filling pages from the cache — blocks briefly now and
/// then: the kernel counts realtime time only since a thread last slept,
/// and would otherwise demote it (or kill the process). A load of an
/// atomic when there is no limit.
pub fn breathe() {
    if must_breathe() {
        std::thread::sleep(BREATH);
        breathed();
    }
}

/// Times a thread of the audio path could not take its priority (no
/// realtime permission): shown with the device's state.
pub fn priority_failures() -> u64 {
    FAILURES.load(Ordering::Relaxed)
}

/// Count a thread that lost or could not take its priority (signal-safe:
/// one atomic; RealtimeKit's `SIGXCPU` handler).
#[cfg(target_os = "linux")]
pub(crate) fn note_failure() {
    FAILURES.fetch_add(1, Ordering::Relaxed);
}

/// Keeps the CPUs out of deep sleep states while it lives (Linux:
/// `/dev/cpu_dma_latency` held at 0 µs, as the kernel's PM QoS
/// interface wants: the request lasts while the file is open). Waking a
/// core from a deep C-state takes up to hundreds of microseconds — every
/// callback, for workers that sleep between callbacks. Held while the
/// audio stream runs; `None` where the system does not allow it (the file
/// is writable for the `realtime` group on most audio setups) or with
/// `FADERFRAME_CPU_LATENCY=0`.
pub struct CpuWakeLatency {
    #[cfg(target_os = "linux")]
    _file: std::fs::File,
}

impl CpuWakeLatency {
    pub fn hold() -> Option<Self> {
        if std::env::var("FADERFRAME_CPU_LATENCY").as_deref() == Ok("0") {
            return None;
        }
        #[cfg(target_os = "linux")]
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/cpu_dma_latency")
                .ok()?;
            file.write_all(&0i32.to_ne_bytes()).ok()?;
            Some(Self { _file: file })
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Applied, Helper};
    use crate::pool::sched;

    /// The `RLIMIT_RTTIME` soft limit kept (µs), if there is a limit.
    pub fn guard_rttime() -> Option<u64> {
        crate::rttime::guard()
    }

    pub fn rttime_demotions() -> (u64, i32) {
        crate::rttime::demotions()
    }

    fn rtprio_limit() -> i32 {
        // SAFETY: getrlimit fills the struct it is given.
        let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to an rlimit.
        if unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut lim) } != 0 {
            return 0;
        }
        if lim.rlim_cur == libc::RLIM_INFINITY {
            99
        } else {
            lim.rlim_cur.min(99) as i32
        }
    }

    /// The calling thread's scheduling; a realtime one RealtimeKit grants
    /// where the process may not take it itself, asked for through the
    /// broker when `ask` (never waited for).
    fn set(policy: i32, priority: i32, ask: bool) -> Applied {
        let param = libc::sched_param {
            sched_priority: priority,
        };
        // SAFETY: sets the calling thread's scheduling with a valid param.
        if unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) == 0 } {
            Applied::Now
        } else if ask
            && sched::is_realtime(sched::pack(policy, priority))
            && crate::rtkit::request_for_this_thread(priority, false)
        {
            Applied::Asked
        } else {
            Applied::Failed
        }
    }

    pub fn make_audio_thread_realtime() -> Applied {
        if sched::is_realtime(sched::current().0) {
            return Applied::Now;
        }
        let policy = libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK;
        let priority = super::OWN_AUDIO_PRIORITY.min(rtprio_limit());
        if priority > 0 {
            set(policy, priority, true)
        } else if crate::rtkit::request_for_this_thread(super::OWN_AUDIO_PRIORITY, true) {
            // No realtime of its own: RealtimeKit's, asked by the broker
            // (and published for the helpers once granted).
            Applied::Asked
        } else {
            Applied::Failed
        }
    }

    pub fn follow(audio: u64, helper: Helper) -> Applied {
        if !sched::is_realtime(audio) {
            // Back to normal (the audio thread is not realtime, or not yet),
            // the reset-on-fork flag kept: an unprivileged thread may not
            // clear it, and rtkit's threads have it.
            return if sched::is_realtime(sched::current().0) {
                set(libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, 0, false)
            } else {
                Applied::Now
            };
        }
        let audio_priority = (audio - 1) as u32 as i32;
        let priority = (audio_priority - helper.below()).max(1);
        // Threads (and processes) these start are not realtime. Disk
        // streaming and the recording writer, with seconds of slack, do
        // not use up RealtimeKit's few realtime threads (it grants a user
        // 25, PipeWire's among them): realtime where the process may take
        // it itself, else normal.
        match set(
            libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK,
            priority,
            helper != Helper::Feeder,
        ) {
            Applied::Failed if helper == Helper::Feeder => Applied::Now,
            applied => applied,
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{Applied, Helper};
    use crate::pool::sched::join;
    use windows_sys::Win32::System::Threading::{
        AVRT_PRIORITY_CRITICAL, AVRT_PRIORITY_HIGH, AVRT_PRIORITY_LOW, AVRT_PRIORITY_NORMAL,
    };

    /// No realtime time limit here.
    pub fn guard_rttime() -> Option<u64> {
        None
    }

    pub fn rttime_demotions() -> (u64, i32) {
        (0, 0)
    }

    pub fn make_audio_thread_realtime() -> Applied {
        join(AVRT_PRIORITY_CRITICAL)
    }

    pub fn follow(_audio: u64, helper: Helper) -> Applied {
        join(match helper {
            Helper::Near => AVRT_PRIORITY_HIGH,
            Helper::Ahead => AVRT_PRIORITY_NORMAL,
            Helper::Feeder => AVRT_PRIORITY_LOW,
        })
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::{Applied, Helper};
    use crate::pool::sched;

    /// CoreAudio's IO thread has its time-constraint policy already.
    /// No realtime time limit here.
    pub fn guard_rttime() -> Option<u64> {
        None
    }

    pub fn rttime_demotions() -> (u64, i32) {
        (0, 0)
    }

    pub fn make_audio_thread_realtime() -> Applied {
        if sched::is_realtime(sched::current().0) {
            Applied::Now
        } else {
            Applied::Failed
        }
    }

    pub fn follow(_audio: u64, _helper: Helper) -> Applied {
        Applied::Now
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn current() -> (i32, i32) {
        let v = sched::current().0 - 1;
        (
            (v >> 32) as i32 & !libc::SCHED_RESET_ON_FORK,
            v as u32 as i32,
        )
    }

    /// With realtime permission (`RLIMIT_RTPRIO`): an audio thread of our
    /// own becomes `SCHED_FIFO`, and helpers follow it below, by role.
    #[test]
    fn helpers_follow_the_audio_thread_below_it() {
        // SAFETY: getrlimit fills the struct it is given.
        let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
        unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut lim) };
        if lim.rlim_cur < 20 {
            eprintln!("skipped: no realtime permission");
            return;
        }
        let audio = std::thread::spawn(|| {
            assert!(make_audio_thread_realtime());
            current()
        })
        .join()
        .unwrap();
        assert_eq!(audio, (libc::SCHED_FIFO, OWN_AUDIO_PRIORITY));
        for (helper, below) in [(Helper::Near, 1), (Helper::Ahead, 5), (Helper::Feeder, 10)] {
            let got = std::thread::spawn(move || {
                follow_audio(helper);
                current()
            })
            .join()
            .unwrap();
            assert_eq!(
                got,
                (libc::SCHED_FIFO, OWN_AUDIO_PRIORITY - below),
                "{helper:?}"
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod flag_tests {
    /// PipeWire's data loop runs `SCHED_FIFO | SCHED_RESET_ON_FORK`: that
    /// is realtime, and helpers follow it.
    #[test]
    fn reset_on_fork_is_still_realtime() {
        let packed = |policy: i32, priority: i32| {
            (((policy as u32 as u64) << 32) | priority as u32 as u64) + 1
        };
        let fifo = packed(libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, 83);
        assert!(crate::pool::sched::is_realtime(fifo));
        assert!(crate::pool::sched::is_realtime(packed(libc::SCHED_RR, 50)));
        assert!(!crate::pool::sched::is_realtime(packed(
            libc::SCHED_OTHER,
            0
        )));
        assert!(!crate::pool::sched::is_realtime(packed(
            libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK,
            0
        )));
    }
}

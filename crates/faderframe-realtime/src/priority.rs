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
//! choice.

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
/// make it realtime if it is not already. Returns whether it is now.
pub fn make_audio_thread_realtime() -> bool {
    let ok = imp::make_audio_thread_realtime();
    if !ok {
        FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    note_audio_thread();
    ok
}

/// On the audio thread: publish its scheduling (a system call — call it
/// once and then every few thousand callbacks).
pub fn note_audio_thread() {
    let (packed, _) = sched::current();
    if AUDIO.swap(packed, Ordering::Relaxed) != packed {
        GENERATION.fetch_add(1, Ordering::Release);
    }
}

/// How often the audio thread's scheduling changed (pools re-read their
/// caller's when it does).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
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
    if !imp::follow(AUDIO.load(Ordering::Relaxed), helper) {
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
/// limit `SIGKILL`) and pace breaths by it ([`breathe`]). Helpers do it
/// when the audio thread's scheduling changes; call it after setting the
/// limit yourself.
pub fn note_rttime_limit() {
    let us = imp::rttime_soft_limit_us().map_or(0, |soft| (soft / 4).max(1_000));
    BREATHE_US.store(us, Ordering::Relaxed);
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
    use super::Helper;
    use crate::pool::sched;

    /// The `RLIMIT_RTTIME` soft limit (µs), if there is one.
    pub fn rttime_soft_limit_us() -> Option<u64> {
        // SAFETY: getrlimit fills the struct it is given.
        let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to an rlimit.
        if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut lim) } != 0
            || lim.rlim_cur == libc::RLIM_INFINITY
        {
            return None;
        }
        Some(lim.rlim_cur as u64)
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
    /// where the process may not take it itself.
    fn set(policy: i32, priority: i32) -> bool {
        let param = libc::sched_param {
            sched_priority: priority,
        };
        // SAFETY: sets the calling thread's scheduling with a valid param.
        let done =
            unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) == 0 };
        done || (sched::is_realtime(sched::pack(policy, priority))
            && crate::rtkit::make_thread_realtime(priority))
    }

    pub fn make_audio_thread_realtime() -> bool {
        if sched::is_realtime(sched::current().0) {
            return true;
        }
        let policy = libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK;
        let priority = super::OWN_AUDIO_PRIORITY.min(rtprio_limit());
        if priority > 0 {
            set(policy, priority)
        } else {
            // No realtime of its own: RealtimeKit's.
            crate::rtkit::make_thread_realtime(super::OWN_AUDIO_PRIORITY)
        }
    }

    pub fn follow(audio: u64, helper: Helper) -> bool {
        if !sched::is_realtime(audio) {
            // Back to normal (the audio thread is not realtime, or not yet).
            return !sched::is_realtime(sched::current().0) || set(libc::SCHED_OTHER, 0);
        }
        let audio_priority = (audio - 1) as u32 as i32;
        let priority = (audio_priority - helper.below()).max(1);
        // Threads (and processes) these start are not realtime.
        set(libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, priority)
    }
}

#[cfg(windows)]
mod imp {
    use super::Helper;
    use windows_sys::Win32::System::Threading::{
        AVRT_PRIORITY, AVRT_PRIORITY_CRITICAL, AVRT_PRIORITY_HIGH, AVRT_PRIORITY_LOW,
        AVRT_PRIORITY_NORMAL, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
    };

    /// "Pro Audio", NUL-terminated UTF-16.
    const TASK: [u16; 10] = [
        b'P' as u16,
        b'r' as u16,
        b'o' as u16,
        b' ' as u16,
        b'A' as u16,
        b'u' as u16,
        b'd' as u16,
        b'i' as u16,
        b'o' as u16,
        0,
    ];

    fn join(priority: AVRT_PRIORITY) -> bool {
        let mut index = 0u32;
        // SAFETY: a NUL-terminated task name and a valid out pointer; the
        // registration lasts for the thread's life.
        unsafe {
            let task = AvSetMmThreadCharacteristicsW(TASK.as_ptr(), &mut index);
            !task.is_null() && AvSetMmThreadPriority(task, priority) != 0
        }
    }

    /// No realtime time limit here.
    pub fn rttime_soft_limit_us() -> Option<u64> {
        None
    }

    pub fn make_audio_thread_realtime() -> bool {
        join(AVRT_PRIORITY_CRITICAL)
    }

    pub fn follow(_audio: u64, helper: Helper) -> bool {
        join(match helper {
            Helper::Near => AVRT_PRIORITY_HIGH,
            Helper::Ahead => AVRT_PRIORITY_NORMAL,
            Helper::Feeder => AVRT_PRIORITY_LOW,
        })
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::Helper;
    use crate::pool::sched;

    /// CoreAudio's IO thread has its time-constraint policy already.
    /// No realtime time limit here.
    pub fn rttime_soft_limit_us() -> Option<u64> {
        None
    }

    pub fn make_audio_thread_realtime() -> bool {
        sched::is_realtime(sched::current().0)
    }

    pub fn follow(_audio: u64, _helper: Helper) -> bool {
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn current() -> (i32, i32) {
        let mut policy = 0;
        // SAFETY: plain query with valid out pointers.
        let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
        unsafe { libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) };
        (policy & !libc::SCHED_RESET_ON_FORK, param.sched_priority)
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

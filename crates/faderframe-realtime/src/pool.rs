//! Fixed worker threads for parallel audio processing.
//!
//! [`WorkerPool::run`] executes a [`PoolJob`] on the calling (audio)
//! thread and on up to `helpers` worker threads at once, and returns only
//! after every worker has left the job — a scoped broadcast, so the job may
//! borrow from the caller's stack. Nothing allocates or locks:
//!
//! * the job is published through an atomic pointer; workers register in
//!   an `active` counter *before* reading it, and the caller clears it and
//!   waits for `active == 0` before returning (sequentially consistent, so
//!   no worker can still hold the pointer afterwards);
//! * idle workers spin for a short while after a run (consecutive chunks of
//!   a callback catch them awake), then sleep on a futex — one non-blocking
//!   `FUTEX_WAKE` from the audio thread wakes as many as needed;
//! * workers run at the audio thread's urgency, so they are not preempted
//!   by ordinary threads while the audio thread waits for them: on Linux
//!   they adopt its scheduling policy and priority (`SCHED_FIFO` under
//!   JACK/PipeWire), on macOS its Mach time-constraint policy (CoreAudio's
//!   IO thread has one) and join the device's audio workgroup
//!   ([`WorkerPool::set_workgroup`]), on Windows they join MMCSS's "Pro
//!   Audio" task like the WASAPI thread; and they flush denormals like it.
//!   Without realtime permission (Linux) a worker asks RealtimeKit through
//!   its broker (`rtkit`); a realtime caller's cycles are for the workers
//!   that run realtime, the others sit out until they do (`helpers_for`).
//!
//! The pool may be shared (e.g. by an engine and its replacement), but only
//! one `run` executes at a time; a concurrent caller runs its job alone.

use crate::denormals::flush_denormals_on_this_thread;
use crate::priority::Applied;
use crate::workgroup::{Membership, Workgroup};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Work shared by the threads of one [`WorkerPool::run`].
pub trait PoolJob: Sync {
    /// Called on every participating thread; returns when this thread has
    /// nothing more to do for the job.
    fn work(&self);
}

#[derive(Clone, Copy, Debug)]
pub struct PoolConfig {
    /// Worker threads (the caller of `run` participates as well).
    pub threads: usize,
    /// How long idle workers spin before sleeping.
    pub spin: Duration,
    /// Called on each worker thread when it starts (tests: arm counters).
    pub on_start: Option<fn()>,
}

impl PoolConfig {
    pub fn new(threads: usize) -> Self {
        Self {
            threads,
            spin: Duration::from_micros(100),
            on_start: None,
        }
    }
}

/// Worker threads for "all cores minus the audio thread", at most 31.
pub fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .saturating_sub(1)
        .min(31)
}

/// Physical CPU cores (SMT siblings counted once).
pub fn physical_cores() -> usize {
    let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
    #[cfg(target_os = "linux")]
    {
        let mut cores = std::collections::HashSet::new();
        if let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") {
            for e in dir.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("cpu") || !name[3..].chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let topo = e.path().join("topology");
                let siblings = std::fs::read_to_string(topo.join("core_cpus_list"))
                    .or_else(|_| std::fs::read_to_string(topo.join("thread_siblings_list")));
                if let Ok(s) = siblings {
                    cores.insert(s.trim().to_string());
                }
            }
        }
        if !cores.is_empty() {
            return cores.len().min(logical);
        }
    }
    logical
}

/// The processing threads the user allows, the calling thread included (0:
/// one per core). See [`set_processing_threads`].
static PROCESSING_THREADS: AtomicUsize = AtomicUsize::new(0);

/// The user's processing threads (the audio thread included; `None`: one
/// per core), set by whoever owns the engine's [`WorkerPool`]. Devices that
/// solve their channels side by side on helpers of their own (circuit
/// preamps, console buses, the Guitar Station) keep each solve within it as
/// well: 1 solves every channel on the device's own thread.
pub fn set_processing_threads(threads: Option<usize>) {
    PROCESSING_THREADS.store(threads.unwrap_or(0), Ordering::Relaxed);
}

/// See [`set_processing_threads`]. Realtime-safe.
pub fn processing_threads() -> Option<usize> {
    match PROCESSING_THREADS.load(Ordering::Relaxed) {
        0 => None,
        n => Some(n),
    }
}

/// Helper threads for a device that solves `channels` independent channels
/// side by side, the calling thread taking one: one more thread per channel,
/// within the machine's physical cores (the caller included) and `most`.
/// Channels beyond that share threads. Reads the CPU topology: call it
/// while building, off the audio thread.
pub fn channel_helpers(channels: usize, most: usize) -> usize {
    let cores = physical_cores().saturating_sub(1).max(1);
    channels.saturating_sub(1).min(cores).min(most)
}

/// How many threads (the caller included) one solve of `channels` channels
/// takes with a pool of `helpers` threads: one per channel, within the pool
/// and the user's processing threads. Realtime-safe.
pub fn channel_threads(channels: usize, helpers: usize) -> usize {
    let allowed = processing_threads().unwrap_or(usize::MAX).max(1);
    channels.min(helpers + 1).min(allowed).max(1)
}

/// Workers worth using for one run, of `workers` of which `realtime` run
/// realtime, on a machine of `physical` cores. A realtime caller (audio
/// thread `SCHED_FIFO`/`SCHED_RR`, time-constrained or in MMCSS) uses the
/// workers that run realtime too — all of them, SMT siblings included —
/// and only those: the others sit out (`worker`). A normal caller uses
/// workers within the physical cores (a worker may be preempted while it
/// holds a job, and the cycle waits; the SMT siblings are left to the rest
/// of the system).
///
/// Why a realtime caller never mixes: everyone in a cycle spins while it
/// waits for a job someone holds, and a normal worker preempted while it
/// holds one may wait behind those spinning realtime threads for as long
/// as the scheduler leaves it there. End to end (FaderFrame under
/// PipeWire, realtime only through a stand-in rtkit, a machine busy with
/// builds, 7 realtime + 8 normal workers all used): the data loop and the
/// realtime workers spun until the realtime time limit's `SIGXCPU`
/// demoted them — 8 demotions in the first second, 16 in 25 s, nothing
/// realtime left. With the realtime ones only: none.
/// (`cycles_with_workers_refused_realtime`, a synthetic cycle, did not
/// catch it — normal workers looked as good as realtime ones there.)
/// 0.22.0 ran a realtime caller serially as soon as one worker had been
/// refused; now the refused ones sit out and the rest keep helping.
pub(crate) fn helpers_for(
    caller_realtime: bool,
    workers: usize,
    realtime: usize,
    physical: usize,
) -> usize {
    if caller_realtime {
        realtime.min(workers)
    } else {
        workers.min(physical.saturating_sub(1).max(1))
    }
}

/// How long a worker sitting out (without the realtime scheduling of its
/// realtime caller) waits before it looks again.
const SIT_OUT: Duration = Duration::from_millis(20);

struct JobRef<'a>(&'a dyn PoolJob);

thread_local! {
    /// 1 + this thread's index among its pool's workers (0: not a worker).
    static SEAT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Which pool worker this thread is: 1 + its index, 0 for any other
/// thread (the caller of [`WorkerPool::run`]). Stable for a thread's life,
/// so a job can be given to the thread that ran it before (its caches).
#[inline]
pub fn worker_seat() -> usize {
    SEAT.with(std::cell::Cell::get)
}

struct Shared {
    /// Physical cores of the machine.
    physical: usize,
    /// Bumped by every run; the futex workers sleep on.
    generation: AtomicU32,
    job: AtomicPtr<()>,
    active: AtomicUsize,
    sleeping: AtomicU32,
    stop: AtomicBool,
    busy: AtomicBool,
    spin_ns: u64,
    /// Caller's scheduling as `sched::current` packs it (0 = unknown) and
    /// the second word some platforms need.
    sched: AtomicU64,
    sched_extra: AtomicU64,
    sched_failures: AtomicU64,
    /// Workers running realtime now (while the caller is).
    realtime: AtomicUsize,
    runs: AtomicU64,
    /// The audio thread's priority generation last seen
    /// (`crate::priority`): a change re-reads the caller's scheduling.
    priority_seen: AtomicU64,
    /// The audio workgroup to join (macOS), and its changes.
    workgroup: Mutex<Option<Workgroup>>,
    workgroup_changes: AtomicU32,
    /// Workers in the workgroup now.
    members: AtomicU32,
    #[cfg(not(target_os = "linux"))]
    threads: std::sync::OnceLock<Vec<std::thread::Thread>>,
}

pub struct WorkerPool {
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
mod wait {
    use std::sync::atomic::AtomicU32;

    pub fn sleep(word: &AtomicU32, expected: u32) {
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 50_000_000,
        };
        // SAFETY: FUTEX_WAIT on a live, aligned u32; returns at once if the
        // value is no longer `expected`. The timeout bounds a missed wake.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                expected,
                &timeout as *const libc::timespec,
            );
        }
    }

    pub fn wake(word: &AtomicU32, n: usize) {
        // SAFETY: FUTEX_WAKE never blocks.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word.as_ptr(),
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                n.min(i32::MAX as usize) as i32,
            );
        }
    }
}

/// The calling thread's scheduling, packed (`0` = unknown) with a
/// platform extra (macOS: the time-constraint computation); a sandboxed
/// plugin's audio thread copies it from the host's audio thread.
pub fn thread_scheduling() -> (u64, u64) {
    sched::current()
}

/// Apply scheduling packed by [`thread_scheduling`] to the calling thread
/// (Linux without realtime permission: asked of RealtimeKit through its
/// broker, not waited for). False when it could not be had or asked for.
/// Realtime-safe.
pub fn apply_thread_scheduling(packed: u64, extra: u64) -> bool {
    packed != 0 && sched::apply(packed, extra, true) != Applied::Failed
}

#[cfg(target_os = "linux")]
pub(crate) mod sched {
    use crate::priority::Applied;

    /// This thread's policy and priority, packed (0 = unknown) — the
    /// kernel's: glibc's `pthread_getschedparam` answers from what it saw
    /// first or set itself, and rtkit (from outside) and the realtime time
    /// limit's `SIGXCPU` handler change a thread behind its back.
    pub fn current() -> (u64, u64) {
        // SAFETY: plain queries on the calling thread (0) with a valid out
        // pointer; raw system calls (some C libraries stub the wrappers).
        let policy = unsafe { libc::syscall(libc::SYS_sched_getscheduler, 0) };
        let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
        let r = unsafe {
            libc::syscall(
                libc::SYS_sched_getparam,
                0,
                &mut param as *mut libc::sched_param,
            )
        };
        if policy < 0 || r != 0 {
            return (0, 0);
        }
        (pack(policy as i32, param.sched_priority), 0)
    }

    /// A policy and priority, packed as [`current`] packs them.
    pub fn pack(policy: i32, priority: i32) -> u64 {
        (((policy as u32 as u64) << 32) | priority as u32 as u64) + 1
    }

    /// Apply a packed policy/priority to this thread (a realtime one
    /// asked of RealtimeKit, through its broker, where the process may not
    /// take it itself and `ask` — never waited for). Realtime-safe.
    pub fn apply(packed: u64, _: u64, ask: bool) -> Applied {
        let v = packed - 1;
        // Threads (and processes) workers start are not realtime; and a
        // worker made realtime with the flag (by rtkit) may only go back to
        // normal with it: an unprivileged thread cannot clear it.
        let policy = (v >> 32) as i32 | libc::SCHED_RESET_ON_FORK;
        let param = libc::sched_param {
            sched_priority: v as u32 as i32,
        };
        // SAFETY: sets the calling thread's scheduling with a valid param.
        if unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) == 0 } {
            Applied::Now
        } else if ask
            && is_realtime(packed)
            && crate::rtkit::request_for_this_thread(param.sched_priority, false)
        {
            Applied::Asked
        } else {
            Applied::Failed
        }
    }

    /// Whether this thread runs realtime now (the kernel's word: a grant
    /// comes from outside); `applied` is what applying gave.
    pub fn holds(_applied: Applied) -> bool {
        is_realtime(current().0)
    }

    /// Whether packed scheduling is a realtime policy (whatever its flags:
    /// PipeWire's data loop is `SCHED_FIFO | SCHED_RESET_ON_FORK`).
    pub fn is_realtime(packed: u64) -> bool {
        let policy = packed
            .checked_sub(1)
            .map(|v| (v >> 32) as i32 & !libc::SCHED_RESET_ON_FORK);
        matches!(policy, Some(libc::SCHED_FIFO | libc::SCHED_RR))
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod sched {
    //! Mach time-constraint scheduling: CoreAudio's IO thread has a policy
    //! (period, computation, constraint in absolute-time units); workers
    //! copy it.
    use crate::priority::Applied;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct TimeConstraint {
        period: u32,
        computation: u32,
        constraint: u32,
        preemptible: u32,
    }

    const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
    const COUNT: u32 = 4;

    unsafe extern "C" {
        fn pthread_mach_thread_np(thread: libc::pthread_t) -> u32;
        fn thread_policy_get(
            thread: u32,
            flavor: u32,
            info: *mut TimeConstraint,
            count: *mut u32,
            get_default: *mut u32,
        ) -> i32;
        fn thread_policy_set(
            thread: u32,
            flavor: u32,
            info: *const TimeConstraint,
            count: u32,
        ) -> i32;
    }

    /// This thread's time constraint, packed (0: it has none).
    pub fn current() -> (u64, u64) {
        let mut tc = TimeConstraint::default();
        let mut count = COUNT;
        let mut get_default = 0u32;
        // SAFETY: queries the calling thread's policy into `tc` (room for
        // `count` integers).
        let r = unsafe {
            thread_policy_get(
                pthread_mach_thread_np(libc::pthread_self()),
                THREAD_TIME_CONSTRAINT_POLICY,
                &mut tc,
                &mut count,
                &mut get_default,
            )
        };
        if r != 0 || get_default != 0 || tc.period == 0 {
            return (0, 0);
        }
        (
            ((u64::from(tc.period) << 32) | u64::from(tc.computation)) + 1,
            (u64::from(tc.preemptible) << 32) | u64::from(tc.constraint),
        )
    }

    pub fn apply(packed: u64, extra: u64, _ask: bool) -> Applied {
        let v = packed - 1;
        let tc = TimeConstraint {
            period: (v >> 32) as u32,
            computation: v as u32,
            constraint: extra as u32,
            preemptible: (extra >> 32) as u32,
        };
        // SAFETY: sets the calling thread's policy from a valid struct.
        let set = unsafe {
            thread_policy_set(
                pthread_mach_thread_np(libc::pthread_self()),
                THREAD_TIME_CONSTRAINT_POLICY,
                &tc,
                COUNT,
            ) == 0
        };
        if set { Applied::Now } else { Applied::Failed }
    }

    /// Whether this thread has the caller's urgency: what applying gave.
    pub fn holds(applied: Applied) -> bool {
        applied == Applied::Now
    }

    pub fn is_realtime(packed: u64) -> bool {
        packed != 0
    }
}

#[cfg(windows)]
pub(crate) mod sched {
    //! MMCSS: the WASAPI thread runs in the "Pro Audio" task (cpal puts it
    //! there); workers join the same task at high priority.
    use crate::priority::Applied;
    use windows_sys::Win32::System::Threading::{
        AVRT_PRIORITY_HIGH, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
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

    pub fn current() -> (u64, u64) {
        (1, 0)
    }

    pub fn apply(_: u64, _: u64, _: bool) -> Applied {
        let mut index = 0u32;
        // SAFETY: a NUL-terminated task name and a valid out pointer; the
        // registration lasts for the thread's life.
        let joined = unsafe {
            let task = AvSetMmThreadCharacteristicsW(TASK.as_ptr(), &mut index);
            !task.is_null() && AvSetMmThreadPriority(task, AVRT_PRIORITY_HIGH) != 0
        };
        if joined {
            Applied::Now
        } else {
            Applied::Failed
        }
    }

    /// Whether this thread is in the task: what joining gave.
    pub fn holds(applied: Applied) -> bool {
        applied == Applied::Now
    }

    pub fn is_realtime(packed: u64) -> bool {
        packed != 0
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) mod sched {
    use crate::priority::Applied;
    pub fn current() -> (u64, u64) {
        (0, 0)
    }
    pub fn apply(_: u64, _: u64, _: bool) -> Applied {
        Applied::Now
    }
    pub fn holds(_: Applied) -> bool {
        true
    }
    pub fn is_realtime(_: u64) -> bool {
        false
    }
}

fn worker(shared: Arc<Shared>, on_start: Option<fn()>, index: usize) {
    SEAT.with(|s| s.set(index + 1));
    flush_denormals_on_this_thread();
    // The stack deep processing reaches, mapped now (not page by page
    // while the audio thread waits for this worker).
    crate::memory::prefault_stack(256 * 1024);
    if let Some(f) = on_start {
        f();
    }
    let mut seen = shared.generation.load(Ordering::Acquire);
    let mut applied = 0u64;
    // What applying the caller's scheduling gave, whether this worker runs
    // realtime (counted in `shared.realtime`), and when it last looked.
    let mut outcome = Applied::Now;
    let mut realtime = false;
    let mut looked = Instant::now();
    // The workgroup this thread is in, and the change it reflects.
    let mut joined: Option<(Workgroup, Membership)> = None;
    let mut workgroup_seen = 0u32;
    let leave = |joined: &mut Option<(Workgroup, Membership)>| {
        if let Some((wg, m)) = joined.take() {
            wg.leave(m);
            shared.members.fetch_sub(1, Ordering::Relaxed);
        }
    };
    loop {
        // Wait for the next run: spin briefly, then sleep.
        let idle_since = Instant::now();
        let mut spins = 0u32;
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                leave(&mut joined);
                if realtime {
                    shared.realtime.fetch_sub(1, Ordering::Relaxed);
                }
                return;
            }
            let g = shared.generation.load(Ordering::Acquire);
            if g != seen {
                seen = g;
                break;
            }
            spins = spins.wrapping_add(1);
            // Under a realtime time limit, block briefly now and then (cycles
            // so close together that it never stops spinning would have the
            // kernel demote it); the caller takes the jobs meanwhile.
            if spins.is_multiple_of(64) && crate::priority::must_breathe() {
                crate::priority::breathe();
                continue;
            }
            if spins.is_multiple_of(64) && idle_since.elapsed().as_nanos() as u64 > shared.spin_ns {
                // Sitting out (below): not asleep where the caller's
                // wake-ups for its cycle's workers would find it.
                if !realtime && sched::is_realtime(shared.sched.load(Ordering::Relaxed)) {
                    break;
                }
                shared.sleeping.fetch_add(1, Ordering::SeqCst);
                if shared.generation.load(Ordering::SeqCst) == seen {
                    #[cfg(target_os = "linux")]
                    wait::sleep(&shared.generation, seen);
                    #[cfg(not(target_os = "linux"))]
                    std::thread::park_timeout(Duration::from_millis(50));
                }
                shared.sleeping.fetch_sub(1, Ordering::SeqCst);
            } else {
                std::hint::spin_loop();
            }
        }
        // A new workgroup: leave the old one, join the new (rare: when the
        // audio device changes; never blocks — tried again next time).
        let changes = shared.workgroup_changes.load(Ordering::Acquire);
        if changes != workgroup_seen
            && let Ok(current) = shared.workgroup.try_lock()
        {
            let next = current.clone();
            drop(current);
            leave(&mut joined);
            if let Some(wg) = next
                && let Some(m) = wg.join()
            {
                shared.members.fetch_add(1, Ordering::Relaxed);
                joined = Some((wg, m));
            }
            workgroup_seen = changes;
        }
        let s = shared.sched.load(Ordering::Relaxed);
        let mut look = false;
        if s != applied && s != 0 {
            // Tests: a refusal (the worker stays as it is).
            #[cfg(test)]
            let refused = index >= tests::REFUSED_FROM.load(Ordering::Relaxed);
            #[cfg(not(test))]
            let refused = false;
            // RealtimeKit's realtime threads are few (25 a user, PipeWire's
            // among them): workers past the physical cores (SMT siblings)
            // do not ask for theirs — the threads within the cores and the
            // helpers on the callback's path come first. They work at
            // normal priority where the process may not set realtime
            // itself.
            let ask = index + 1 < shared.physical;
            outcome = if refused {
                Applied::Failed
            } else {
                sched::apply(s, shared.sched_extra.load(Ordering::Relaxed), ask)
            };
            if outcome == Applied::Failed && ask {
                shared.sched_failures.fetch_add(1, Ordering::Relaxed);
            }
            applied = s;
            look = true;
        }
        // Whether this worker runs realtime: after applying, while it waits
        // for a grant (RealtimeKit's, from outside) now and then, and once
        // a second while it has it (the time limit's `SIGXCPU` demotes from
        // outside too).
        let every = if realtime {
            Duration::from_secs(1)
        } else {
            SIT_OUT
        };
        if look || (sched::is_realtime(s) && looked.elapsed() >= every) {
            looked = Instant::now();
            let now = sched::holds(outcome);
            if now != realtime {
                realtime = now;
                if now {
                    shared.realtime.fetch_add(1, Ordering::Relaxed);
                } else {
                    shared.realtime.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
        // A realtime caller's cycles are for realtime workers only (see
        // `helpers_for`): this one sits out — takes no job, and does not
        // sleep where the caller's wake-ups go — and looks again in a
        // while.
        if sched::is_realtime(s) && !realtime {
            std::thread::sleep(SIT_OUT);
            continue;
        }
        // Register before looking at the job (see the module docs).
        shared.active.fetch_add(1, Ordering::SeqCst);
        let p = shared.job.load(Ordering::SeqCst);
        if !p.is_null() {
            // SAFETY: the caller of `run` keeps the job alive until `active`
            // drops to zero, and it only reads `active` after clearing the
            // pointer.
            let job = unsafe { &*(p as *const JobRef<'_>) };
            // A plugin may have cleared the FP control flags: set again
            // (one register write).
            flush_denormals_on_this_thread();
            // A panic must not leave `active` raised: the caller would
            // wait for this worker for ever.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.0.work()));
        }
        shared.active.fetch_sub(1, Ordering::Release);
    }
}

impl WorkerPool {
    pub fn new(config: PoolConfig) -> Self {
        // Workers that may not take the caller's realtime scheduling
        // themselves ask RealtimeKit's broker (a control thread: here).
        crate::priority::start_requests();
        let shared = Arc::new(Shared {
            physical: physical_cores(),
            generation: AtomicU32::new(0),
            job: AtomicPtr::new(std::ptr::null_mut()),
            active: AtomicUsize::new(0),
            sleeping: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            spin_ns: config.spin.as_nanos() as u64,
            sched: AtomicU64::new(0),
            sched_extra: AtomicU64::new(0),
            sched_failures: AtomicU64::new(0),
            realtime: AtomicUsize::new(0),
            runs: AtomicU64::new(0),
            priority_seen: AtomicU64::new(u64::MAX),
            workgroup: Mutex::new(None),
            workgroup_changes: AtomicU32::new(0),
            members: AtomicU32::new(0),
            #[cfg(not(target_os = "linux"))]
            threads: std::sync::OnceLock::new(),
        });
        let handles: Vec<JoinHandle<()>> = (0..config.threads)
            .filter_map(|i| {
                let shared = Arc::clone(&shared);
                let on_start = config.on_start;
                std::thread::Builder::new()
                    .name(format!("ff-dsp-{}", i + 1))
                    .spawn(move || worker(shared, on_start, i))
                    .ok()
            })
            .collect();
        #[cfg(not(target_os = "linux"))]
        let _ = shared
            .threads
            .set(handles.iter().map(|h| h.thread().clone()).collect());
        Self { shared, handles }
    }

    /// Number of worker threads (not counting the caller of `run`).
    pub fn threads(&self) -> usize {
        self.handles.len()
    }

    /// The audio device's workgroup (macOS: CoreAudio's IO thread is in
    /// it) for the workers to join, or none. Control thread; the workers
    /// change membership before their next job.
    pub fn set_workgroup(&self, workgroup: Option<Workgroup>) {
        if let Ok(mut g) = self.shared.workgroup.lock() {
            *g = workgroup;
        }
        self.shared
            .workgroup_changes
            .fetch_add(1, Ordering::Release);
    }

    /// Workers in the workgroup now.
    pub fn workgroup_members(&self) -> usize {
        self.shared.members.load(Ordering::Relaxed) as usize
    }

    /// Times a worker could neither take the audio thread's scheduling nor
    /// ask for it (no realtime permission, no RealtimeKit broker room).
    pub fn priority_failures(&self) -> u64 {
        self.shared.sched_failures.load(Ordering::Relaxed)
    }

    /// Workers without the caller's realtime scheduling while it has it
    /// (refused, or still waiting for RealtimeKit's grant); 0 while the
    /// caller is not realtime.
    pub fn workers_without_realtime(&self) -> usize {
        if sched::is_realtime(self.shared.sched.load(Ordering::Relaxed)) {
            self.handles
                .len()
                .saturating_sub(self.shared.realtime.load(Ordering::Relaxed))
        } else {
            0
        }
    }

    /// Workers worth using for one run (see `helpers_for`: for a realtime
    /// caller its realtime workers, else those within the physical cores).
    /// Realtime-safe.
    pub fn useful_helpers(&self) -> usize {
        self.observe_caller(false);
        helpers_for(
            sched::is_realtime(self.shared.sched.load(Ordering::Relaxed)),
            self.handles.len(),
            self.shared.realtime.load(Ordering::Relaxed),
            self.shared.physical,
        )
    }

    /// The caller's scheduling (on the caller's thread), re-read at once
    /// when the audio thread's scheduling changed or a thread was made
    /// realtime from outside (helpers that run pools follow the audio
    /// thread; a render-ahead tier may be granted realtime after its pool
    /// first looked — and a pool without realtime workers is not run), and
    /// every 4096 runs (`periodic`: the audio server may restart its
    /// thread). An atomic load otherwise.
    fn observe_caller(&self, periodic: bool) {
        let epoch = crate::priority::scheduling_epoch();
        let changed = self.shared.priority_seen.swap(epoch, Ordering::Relaxed) != epoch;
        if changed
            || (periodic
                && self
                    .shared
                    .runs
                    .fetch_add(1, Ordering::Relaxed)
                    .is_multiple_of(4096))
        {
            let (packed, extra) = sched::current();
            self.shared.sched_extra.store(extra, Ordering::Relaxed);
            self.shared.sched.store(packed, Ordering::Relaxed);
        }
    }

    fn wake(&self, n: usize) {
        if self.shared.sleeping.load(Ordering::SeqCst) == 0 {
            return;
        }
        #[cfg(target_os = "linux")]
        wait::wake(&self.shared.generation, n);
        #[cfg(not(target_os = "linux"))]
        if let Some(t) = self.shared.threads.get() {
            for t in t.iter().take(n) {
                t.unpark();
            }
        }
    }

    /// Run `job` on this thread and up to `helpers` workers. Returns when
    /// this thread's `work` returned and no worker is inside the job any
    /// more. Realtime-safe.
    pub fn run(&self, job: &dyn PoolJob, helpers: usize) {
        let helpers = helpers.min(self.handles.len());
        if helpers == 0 || self.shared.busy.swap(true, Ordering::Acquire) {
            job.work();
            return;
        }
        self.observe_caller(true);
        let r = JobRef(job);
        self.shared
            .job
            .store(&r as *const JobRef<'_> as *mut (), Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.wake(helpers);
        job.work();
        self.shared
            .job
            .store(std::ptr::null_mut(), Ordering::SeqCst);
        while self.shared.active.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
        self.shared.busy.store(false, Ordering::Release);
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        #[cfg(target_os = "linux")]
        wait::wake(&self.shared.generation, usize::MAX);
        #[cfg(not(target_os = "linux"))]
        if let Some(t) = self.shared.threads.get() {
            for t in t {
                t.unpark();
            }
        }
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Workers at or past this index never take the caller's scheduling,
    /// as if their realtime request had been refused (the measurement
    /// below).
    pub(super) static REFUSED_FROM: AtomicUsize = AtomicUsize::new(usize::MAX);

    /// Items of a set amount of work each, taken by whoever comes.
    #[cfg(target_os = "linux")]
    struct Spread {
        next: AtomicUsize,
        total: usize,
        spins: u64,
    }

    #[cfg(target_os = "linux")]
    impl PoolJob for Spread {
        fn work(&self) {
            while self.next.fetch_add(1, Ordering::Relaxed) < self.total {
                let mut x = 1u64;
                for _ in 0..self.spins {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            }
        }
    }

    /// Measurement for the policy of a realtime caller whose workers did
    /// not all get realtime (RealtimeKit refusing some): cycles of 48
    /// items of ~25 µs every 2.67 ms (128 frames at 48 kHz) by a
    /// `SCHED_FIFO` caller — alone, with 7 realtime workers, with 7 normal
    /// ones (0.21's fallback), 3 realtime + 4 normal all used, and the 3
    /// realtime ones alone — interleaved in rounds so that every variant
    /// meets the machine's load alike. `FADERFRAME_TEST_HOGS=<n>`: that
    /// many busy threads at normal priority in this process meanwhile
    /// (the program's own competition: its UI, analyses, a decoder).
    /// Needs realtime permission; run with `--release --ignored
    /// --nocapture`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn cycles_with_workers_refused_realtime() {
        let normal = || {
            let param = libc::sched_param { sched_priority: 0 };
            // SAFETY: the calling thread's scheduling with a valid param.
            unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_OTHER, &param) };
            assert!(!sched::is_realtime(sched::current().0), "a normal thread");
        };
        // Everything made while this thread is normal: threads inherit
        // their maker's scheduling (realtime busy threads would starve
        // the machine).
        normal();
        let calibrate = |spins: u64| {
            let job = Spread {
                next: AtomicUsize::new(0),
                total: 200,
                spins,
            };
            let t = Instant::now();
            job.work();
            t.elapsed().as_nanos() as u64 / 200
        };
        let per = calibrate(100_000).max(1);
        // `FADERFRAME_TEST_ITEMS=<n>x<µs>`: the cycle's work (default 48 of 25 µs).
        let (items, item_us) = std::env::var("FADERFRAME_TEST_ITEMS")
            .ok()
            .and_then(|v| {
                let (n, us) = v.split_once('x')?;
                Some((n.parse::<usize>().ok()?, us.parse::<u64>().ok()?))
            })
            .unwrap_or((48, 25));
        let spins = 100_000 * item_us * 1000 / per;
        eprintln!("item: {} ns", calibrate(spins));
        let period = Duration::from_nanos(2_666_667);
        // (name, workers, refused from, helpers)
        let variants: [(&str, usize, usize, usize); 5] = [
            ("serial", 0, usize::MAX, 0),
            ("7 realtime", 7, usize::MAX, 7),
            ("7 normal", 7, 0, 7),
            ("3 realtime + 4 normal", 7, 3, 7),
            ("3 realtime alone", 3, usize::MAX, 3),
        ];
        let pools: Vec<WorkerPool> = variants
            .iter()
            .map(|&(_, workers, _, _)| WorkerPool::new(PoolConfig::new(workers)))
            .collect();
        let hogs: usize = std::env::var("FADERFRAME_TEST_HOGS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let stop = Arc::new(AtomicBool::new(false));
        let hogs: Vec<_> = (0..hogs)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    normal();
                    while !stop.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
            })
            .collect();
        let param = libc::sched_param { sched_priority: 70 };
        // SAFETY: the calling thread's scheduling with a valid param.
        if unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) }
            != 0
        {
            stop.store(true, Ordering::Relaxed);
            eprintln!("skipped: no realtime permission");
            return;
        }
        // The first runs: the workers take the caller's scheduling (or,
        // refused, stay normal).
        for (v, &(_, _, refused, helpers)) in variants.iter().enumerate() {
            REFUSED_FROM.store(refused, Ordering::Relaxed);
            for _ in 0..20 {
                let job = Spread {
                    next: AtomicUsize::new(0),
                    total: items,
                    spins,
                };
                pools[v].run(&job, helpers);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        REFUSED_FROM.store(usize::MAX, Ordering::Relaxed);
        let (rounds, cycles) = if hogs.is_empty() { (10, 200) } else { (5, 100) };
        let mut times: Vec<Vec<u64>> = vec![Vec::new(); variants.len()];
        for _round in 0..rounds {
            for (v, &(_, _, _, helpers)) in variants.iter().enumerate() {
                let mut next = Instant::now();
                // A variant that stalls gets two seconds a round.
                let until = next + Duration::from_secs(2);
                for _ in 0..cycles {
                    if Instant::now() > until {
                        break;
                    }
                    let job = Spread {
                        next: AtomicUsize::new(0),
                        total: items,
                        spins,
                    };
                    let t = Instant::now();
                    pools[v].run(&job, helpers);
                    times[v].push(t.elapsed().as_nanos() as u64);
                    next += period;
                    if let Some(d) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(d);
                    } else {
                        next = Instant::now();
                    }
                }
            }
        }
        normal();
        stop.store(true, Ordering::Relaxed);
        for h in hogs {
            let _ = h.join();
        }
        // SAFETY: getloadavg fills up to 1 value.
        let mut load = [0f64; 1];
        unsafe { libc::getloadavg(load.as_mut_ptr(), 1) };
        eprintln!("load average {:.1}; cycle times (µs):", load[0]);
        for (v, t) in times.iter_mut().enumerate() {
            t.sort_unstable();
            let at = |q: f64| t[((t.len() - 1) as f64 * q) as usize] as f64 / 1000.0;
            let over = |ns: u64| t.iter().filter(|&&x| x > ns).count();
            eprintln!(
                "{:<24} p50 {:>6.0}  p99 {:>6.0}  p99.9 {:>7.0}  max {:>7.0}  >1.33 ms {:>4}  >2.67 ms {:>4} of {}",
                variants[v].0,
                at(0.5),
                at(0.99),
                at(0.999),
                at(1.0),
                over(1_333_333),
                over(2_666_667),
                t.len()
            );
        }
    }

    /// How many workers a run uses: a realtime caller its realtime workers
    /// (all of them, SMT siblings too) and no others — never none because
    /// one was refused (0.22.0 ran serially then) —; a normal caller those
    /// within the physical cores.
    #[test]
    fn a_realtime_caller_uses_its_realtime_workers() {
        // A realtime caller, 15 workers, 8 cores.
        assert_eq!(helpers_for(true, 15, 15, 8), 15, "all realtime: all");
        assert_eq!(helpers_for(true, 15, 14, 8), 14, "one refused: the others");
        assert_eq!(helpers_for(true, 15, 7, 8), 7, "the realtime ones");
        assert_eq!(helpers_for(true, 15, 0, 8), 0, "none realtime: alone");
        // A normal caller: within the cores.
        assert_eq!(helpers_for(false, 15, 0, 8), 7);
        assert_eq!(helpers_for(false, 15, 15, 8), 7);
        assert_eq!(helpers_for(false, 4, 0, 1), 1);
        assert_eq!(helpers_for(true, 0, 0, 8), 0, "no workers");
    }

    /// On the kernel (needs realtime permission): a realtime caller whose
    /// workers were partly refused realtime counts them, runs its jobs on
    /// the realtime ones — not alone, as 0.22.0 did — and the refused ones
    /// sit out.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pool_with_workers_refused_realtime_still_runs_in_parallel() {
        let pool = WorkerPool::new(PoolConfig::new(4));
        let caller = std::thread::spawn(move || {
            let param = libc::sched_param { sched_priority: 30 };
            // SAFETY: the calling thread's scheduling with a valid param.
            if unsafe {
                libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param)
            } != 0
            {
                return None;
            }
            REFUSED_FROM.store(2, Ordering::Relaxed);
            let job = || Counter {
                next: AtomicUsize::new(0),
                total: 4_000,
                done: AtomicUsize::new(0),
                threads_seen: AtomicUsize::new(0),
            };
            // The workers take (or are refused) the caller's scheduling.
            let until = Instant::now() + Duration::from_secs(5);
            while pool.workers_without_realtime() != 2 && Instant::now() < until {
                pool.run(&job(), 4);
                std::thread::sleep(Duration::from_millis(2));
            }
            REFUSED_FROM.store(usize::MAX, Ordering::Relaxed);
            let without = pool.workers_without_realtime();
            let helpers = pool.useful_helpers();
            let mut most = 0;
            for _ in 0..30 {
                let j = job();
                pool.run(&j, helpers);
                most = most.max(j.threads_seen.load(Ordering::Relaxed));
            }
            Some((without, helpers, most))
        })
        .join()
        .unwrap();
        let Some((without, helpers, most)) = caller else {
            eprintln!("skipped: no realtime permission");
            return;
        };
        assert_eq!(without, 2, "two refused");
        assert_eq!(helpers, 2, "the realtime ones");
        assert!(most > 1, "the jobs ran on workers too");
        assert!(most <= 3, "{most} threads: the refused ones took part");
    }

    /// A caller made realtime from outside after its pool first looked (a
    /// render-ahead tier RealtimeKit granted a moment later): the grant is
    /// noted and the pool looks again at once — its workers follow and
    /// take part — not 4096 runs later. Needs realtime permission.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_caller_made_realtime_later_is_followed_at_once() {
        let pool = WorkerPool::new(PoolConfig::new(2));
        let seen = std::thread::spawn(move || {
            let job = || Counter {
                next: AtomicUsize::new(0),
                total: 1000,
                done: AtomicUsize::new(0),
                threads_seen: AtomicUsize::new(0),
            };
            for _ in 0..8 {
                pool.run(&job(), pool.useful_helpers());
            }
            assert_eq!(pool.workers_without_realtime(), 0, "a normal caller");
            let param = libc::sched_param { sched_priority: 20 };
            // SAFETY: the calling thread's scheduling with a valid param.
            if unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_RR, &param) }
                != 0
            {
                return None;
            }
            crate::priority::note_grant();
            // The workers follow within a few runs.
            let until = Instant::now() + Duration::from_secs(5);
            let mut helpers = 0;
            while helpers < 2 && Instant::now() < until {
                helpers = pool.useful_helpers();
                pool.run(&job(), helpers);
                std::thread::sleep(Duration::from_millis(1));
            }
            Some(helpers)
        })
        .join()
        .unwrap();
        match seen {
            None => eprintln!("skipped: no realtime permission"),
            Some(helpers) => assert_eq!(helpers, 2, "both workers realtime and used"),
        }
    }

    /// Counts work items taken by every thread.
    struct Counter {
        next: AtomicUsize,
        total: usize,
        done: AtomicUsize,
        threads_seen: AtomicUsize,
    }

    impl PoolJob for Counter {
        fn work(&self) {
            self.threads_seen.fetch_add(1, Ordering::Relaxed);
            loop {
                let i = self.next.fetch_add(1, Ordering::Relaxed);
                if i >= self.total {
                    break;
                }
                // A little work per item.
                let mut x = i as u64;
                for _ in 0..200 {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
                self.done.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Workers join an audio workgroup before their next job and leave it
    /// when it is withdrawn (a work interval of our own: CI machines may
    /// have no audio device). The default output device's workgroup, when
    /// there is one, takes a thread too.
    #[cfg(target_os = "macos")]
    #[test]
    fn workers_join_and_leave_an_audio_workgroup() {
        let job = || Counter {
            next: AtomicUsize::new(0),
            total: 1000,
            done: AtomicUsize::new(0),
            threads_seen: AtomicUsize::new(0),
        };
        let wait_for = |pool: &WorkerPool, members: usize| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while pool.workgroup_members() != members && Instant::now() < deadline {
                pool.run(&job(), 2);
                std::thread::sleep(Duration::from_millis(1));
            }
            pool.workgroup_members()
        };
        let wg = Workgroup::new_interval(c"faderframe-test").expect("a work interval");
        let pool = WorkerPool::new(PoolConfig::new(2));
        pool.set_workgroup(Some(wg));
        assert_eq!(wait_for(&pool, 2), 2);
        pool.set_workgroup(None);
        assert_eq!(wait_for(&pool, 0), 0);
        if let Some(device) = Workgroup::of_default_output() {
            let m = device.join().expect("joins the device's workgroup");
            device.leave(m);
        }
    }

    #[test]
    fn runs_jobs_on_workers_and_waits_for_them() {
        let pool = WorkerPool::new(PoolConfig::new(3));
        assert_eq!(pool.threads(), 3);
        for round in 0..200 {
            let job = Counter {
                next: AtomicUsize::new(0),
                total: 10_000,
                done: AtomicUsize::new(0),
                threads_seen: AtomicUsize::new(0),
            };
            pool.run(&job, 3);
            // Everything finished before `run` returned.
            assert_eq!(job.done.load(Ordering::Relaxed), 10_000, "round {round}");
            assert!(job.threads_seen.load(Ordering::Relaxed) >= 1);
        }
        // Workers fall asleep and wake again.
        std::thread::sleep(Duration::from_millis(5));
        let job = Counter {
            next: AtomicUsize::new(0),
            total: 100_000,
            done: AtomicUsize::new(0),
            threads_seen: AtomicUsize::new(0),
        };
        pool.run(&job, 3);
        assert_eq!(job.done.load(Ordering::Relaxed), 100_000);
    }

    #[test]
    fn without_workers_the_caller_does_everything() {
        let pool = WorkerPool::new(PoolConfig::new(0));
        let job = Counter {
            next: AtomicUsize::new(0),
            total: 100,
            done: AtomicUsize::new(0),
            threads_seen: AtomicUsize::new(0),
        };
        pool.run(&job, 4);
        assert_eq!(job.done.load(Ordering::Relaxed), 100);
        assert_eq!(job.threads_seen.load(Ordering::Relaxed), 1);
    }

    /// A channel a thread: a 5.1 device's helpers are five (within the
    /// cores), and a solve takes six threads unless the pool or the user's
    /// processing threads allow fewer.
    #[test]
    fn surround_channels_get_a_thread_each_within_the_limits() {
        let others = physical_cores().saturating_sub(1).max(1);
        assert_eq!(channel_helpers(6, 63), 5.min(others));
        assert_eq!(channel_helpers(12, 63), 11.min(others));
        assert_eq!(channel_helpers(1, 63), 0);
        assert_eq!(channel_helpers(64, 7), 7.min(others));
        assert_eq!(channel_threads(6, 5), 6);
        assert_eq!(channel_threads(6, 2), 3);
        assert_eq!(channel_threads(1, 5), 1);
        set_processing_threads(Some(4));
        assert_eq!(channel_threads(6, 5), 4);
        set_processing_threads(Some(1));
        assert_eq!(channel_threads(6, 5), 1);
        set_processing_threads(None);
        assert_eq!(channel_threads(6, 5), 6);
    }
}

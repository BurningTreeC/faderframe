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
//!   IO thread has one), on Windows they join MMCSS's "Pro Audio" task like
//!   the WASAPI thread; and they flush denormals like it.
//!
//! The pool may be shared (e.g. by an engine and its replacement), but only
//! one `run` executes at a time; a concurrent caller runs its job alone.

use crate::denormals::flush_denormals_on_this_thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
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

struct JobRef<'a>(&'a dyn PoolJob);

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
    runs: AtomicU64,
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

/// Apply scheduling packed by [`thread_scheduling`] to the calling thread.
pub fn apply_thread_scheduling(packed: u64, extra: u64) -> bool {
    packed != 0 && sched::apply(packed, extra)
}

#[cfg(target_os = "linux")]
mod sched {
    /// This thread's policy and priority, packed (0 = unknown).
    pub fn current() -> (u64, u64) {
        let mut policy = 0;
        // SAFETY: plain query on the calling thread with valid out pointers.
        let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
        let r =
            unsafe { libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) };
        if r != 0 {
            return (0, 0);
        }
        (
            (((policy as u32 as u64) << 32) | param.sched_priority as u32 as u64) + 1,
            0,
        )
    }

    /// Apply a packed policy/priority to this thread.
    pub fn apply(packed: u64, _: u64) -> bool {
        let v = packed - 1;
        let policy = (v >> 32) as i32;
        let param = libc::sched_param {
            sched_priority: v as u32 as i32,
        };
        // SAFETY: sets the calling thread's scheduling with a valid param.
        unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) == 0 }
    }

    /// Whether packed scheduling is a realtime policy.
    pub fn is_realtime(packed: u64) -> bool {
        let policy = packed.checked_sub(1).map(|v| (v >> 32) as i32);
        matches!(policy, Some(libc::SCHED_FIFO | libc::SCHED_RR))
    }
}

#[cfg(target_os = "macos")]
mod sched {
    //! Mach time-constraint scheduling: CoreAudio's IO thread has a policy
    //! (period, computation, constraint in absolute-time units); workers
    //! copy it.

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

    pub fn apply(packed: u64, extra: u64) -> bool {
        let v = packed - 1;
        let tc = TimeConstraint {
            period: (v >> 32) as u32,
            computation: v as u32,
            constraint: extra as u32,
            preemptible: (extra >> 32) as u32,
        };
        // SAFETY: sets the calling thread's policy from a valid struct.
        unsafe {
            thread_policy_set(
                pthread_mach_thread_np(libc::pthread_self()),
                THREAD_TIME_CONSTRAINT_POLICY,
                &tc,
                COUNT,
            ) == 0
        }
    }

    pub fn is_realtime(packed: u64) -> bool {
        packed != 0
    }
}

#[cfg(windows)]
mod sched {
    //! MMCSS: the WASAPI thread runs in the "Pro Audio" task (cpal puts it
    //! there); workers join the same task at high priority.
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

    pub fn apply(_: u64, _: u64) -> bool {
        let mut index = 0u32;
        // SAFETY: a NUL-terminated task name and a valid out pointer; the
        // registration lasts for the thread's life.
        unsafe {
            let task = AvSetMmThreadCharacteristicsW(TASK.as_ptr(), &mut index);
            !task.is_null() && AvSetMmThreadPriority(task, AVRT_PRIORITY_HIGH) != 0
        }
    }

    pub fn is_realtime(packed: u64) -> bool {
        packed != 0
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod sched {
    pub fn current() -> (u64, u64) {
        (0, 0)
    }
    pub fn apply(_: u64, _: u64) -> bool {
        true
    }
    pub fn is_realtime(_: u64) -> bool {
        false
    }
}

fn worker(shared: Arc<Shared>, on_start: Option<fn()>) {
    flush_denormals_on_this_thread();
    if let Some(f) = on_start {
        f();
    }
    let mut seen = shared.generation.load(Ordering::Acquire);
    let mut applied = 0u64;
    loop {
        // Wait for the next run: spin briefly, then sleep.
        let idle_since = Instant::now();
        let mut spins = 0u32;
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            let g = shared.generation.load(Ordering::Acquire);
            if g != seen {
                seen = g;
                break;
            }
            spins = spins.wrapping_add(1);
            if spins.is_multiple_of(64) && idle_since.elapsed().as_nanos() as u64 > shared.spin_ns {
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
        let s = shared.sched.load(Ordering::Relaxed);
        if s != applied && s != 0 {
            if !sched::apply(s, shared.sched_extra.load(Ordering::Relaxed)) {
                shared.sched_failures.fetch_add(1, Ordering::Relaxed);
            }
            applied = s;
        }
        // Register before looking at the job (see the module docs).
        shared.active.fetch_add(1, Ordering::SeqCst);
        let p = shared.job.load(Ordering::SeqCst);
        if !p.is_null() {
            // SAFETY: the caller of `run` keeps the job alive until `active`
            // drops to zero, and it only reads `active` after clearing the
            // pointer.
            let job = unsafe { &*(p as *const JobRef<'_>) };
            job.0.work();
        }
        shared.active.fetch_sub(1, Ordering::Release);
    }
}

impl WorkerPool {
    pub fn new(config: PoolConfig) -> Self {
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
            runs: AtomicU64::new(0),
            #[cfg(not(target_os = "linux"))]
            threads: std::sync::OnceLock::new(),
        });
        let handles: Vec<JoinHandle<()>> = (0..config.threads)
            .filter_map(|i| {
                let shared = Arc::clone(&shared);
                let on_start = config.on_start;
                std::thread::Builder::new()
                    .name(format!("ff-dsp-{}", i + 1))
                    .spawn(move || worker(shared, on_start))
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

    /// Times a worker could not take the audio thread's scheduling (e.g. no
    /// realtime permission).
    pub fn priority_failures(&self) -> u64 {
        self.shared.sched_failures.load(Ordering::Relaxed)
    }

    /// Workers worth using for one run. With realtime scheduling (audio
    /// thread and workers `SCHED_FIFO`/`SCHED_RR`, time-constrained or in
    /// MMCSS) every worker helps, SMT siblings included. Without it a worker
    /// may be preempted while it holds a job and the whole cycle waits, so
    /// the pool stays within the physical cores, leaving the SMT siblings
    /// to the rest of the system.
    pub fn useful_helpers(&self) -> usize {
        let realtime = sched::is_realtime(self.shared.sched.load(Ordering::Relaxed));
        if realtime && self.priority_failures() == 0 {
            self.handles.len()
        } else {
            self.handles
                .len()
                .min(self.shared.physical.saturating_sub(1).max(1))
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
        // The caller's scheduling, re-read now and then (it may change when
        // the audio server restarts its thread).
        if self
            .shared
            .runs
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(4096)
        {
            let (packed, extra) = sched::current();
            self.shared.sched_extra.store(extra, Ordering::Relaxed);
            self.shared.sched.store(packed, Ordering::Relaxed);
        }
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
}

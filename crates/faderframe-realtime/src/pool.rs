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
//! * workers adopt the scheduling policy and priority of the thread that
//!   runs jobs (`SCHED_FIFO` under JACK/PipeWire), so they are not
//!   preempted by ordinary threads while the audio thread waits for them,
//!   and flush denormals like it.
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
    /// Caller's scheduling (`(policy << 32 | priority) + 1`; 0 = unknown).
    sched: AtomicU64,
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

#[cfg(target_os = "linux")]
mod sched {
    /// This thread's policy and priority, packed (0 = unknown).
    pub fn current() -> u64 {
        let mut policy = 0;
        // SAFETY: plain query on the calling thread with valid out pointers.
        let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
        let r =
            unsafe { libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) };
        if r != 0 {
            return 0;
        }
        (((policy as u32 as u64) << 32) | param.sched_priority as u32 as u64) + 1
    }

    /// Apply a packed policy/priority to this thread.
    pub fn apply(packed: u64) -> bool {
        let v = packed - 1;
        let policy = (v >> 32) as i32;
        let param = libc::sched_param {
            sched_priority: v as u32 as i32,
        };
        // SAFETY: sets the calling thread's scheduling with a valid param.
        unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) == 0 }
    }
}

#[cfg(not(target_os = "linux"))]
mod sched {
    pub fn current() -> u64 {
        0
    }
    pub fn apply(_: u64) -> bool {
        true
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
            if !sched::apply(s) {
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
    /// thread and workers `SCHED_FIFO`/`SCHED_RR`) every worker helps, SMT
    /// siblings included. Without it a worker may be preempted while it
    /// holds a job and the whole cycle waits, so the pool stays within the
    /// physical cores, leaving the SMT siblings to the rest of the system.
    pub fn useful_helpers(&self) -> usize {
        let packed = self.shared.sched.load(Ordering::Relaxed);
        let policy = packed.checked_sub(1).map(|v| (v >> 32) as i32);
        #[cfg(target_os = "linux")]
        let realtime = matches!(policy, Some(libc::SCHED_FIFO | libc::SCHED_RR));
        #[cfg(not(target_os = "linux"))]
        let realtime = policy.is_some();
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
            self.shared.sched.store(sched::current(), Ordering::Relaxed);
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

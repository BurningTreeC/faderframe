//! The realtime time limit must never kill the process. A realtime thread
//! that computes without sleeping past `RLIMIT_RTTIME`'s hard limit gets
//! the whole process `SIGKILL`ed (no log, no core), so FaderFrame keeps the
//! soft limit below the hard one, catches `SIGXCPU` and puts the thread
//! back to normal scheduling. Against the kernel, with the threads made
//! as rtkit, PipeWire and FaderFrame make them (`SCHED_RESET_ON_FORK` set,
//! which an unprivileged thread may not clear), and the limit set as
//! PipeWire's client sets it under rtkit (soft = hard = 200 ms).
//!
//! Each case that could kill runs in a child process (this test binary,
//! started again), so a kill shows as its exit status. Needs realtime
//! permission (`RLIMIT_RTPRIO`); skipped without it.

#![allow(clippy::unwrap_used)]
#![cfg(target_os = "linux")]

use faderframe_realtime::{PoolConfig, PoolJob, WorkerPool, priority};
use std::process::Command;
use std::time::{Duration, Instant};

const CHILD: &str = "FADERFRAME_RTTIME_CHILD";
/// A child's exit code when it cannot run realtime here.
const SKIPPED: i32 = 77;

fn set_limit(soft: u64, hard: u64) {
    let lim = libc::rlimit {
        rlim_cur: soft as libc::rlim_t,
        rlim_max: hard as libc::rlim_t,
    };
    // SAFETY: a valid rlimit; lowering limits needs no privilege.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &lim) }, 0);
}

fn soft_limit() -> u64 {
    // SAFETY: getrlimit fills the struct it is given.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: a valid pointer to an rlimit.
    unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut lim) };
    lim.rlim_cur as u64
}

/// The calling thread `SCHED_FIFO | SCHED_RESET_ON_FORK` (as FaderFrame
/// and PipeWire make them; rtkit's are `SCHED_RR` with the flag).
fn realtime_with_reset_on_fork() -> bool {
    let param = libc::sched_param { sched_priority: 5 };
    // SAFETY: this thread, a valid param.
    unsafe {
        libc::pthread_setschedparam(
            libc::pthread_self(),
            libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK,
            &param,
        ) == 0
    }
}

/// The kernel's view (glibc's `pthread_getschedparam` answers from a cache
/// that a change from a signal handler or from rtkit does not reach).
fn is_realtime() -> bool {
    // SAFETY: plain query on the calling thread.
    let policy = unsafe { libc::syscall(libc::SYS_sched_getscheduler, 0) } as i32;
    matches!(
        policy & !libc::SCHED_RESET_ON_FORK,
        libc::SCHED_FIFO | libc::SCHED_RR
    )
}

fn spin_for(d: Duration) {
    let end = Instant::now() + d;
    let mut x = 0u64;
    while Instant::now() < end {
        for i in 0..1_000u64 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
    }
}

/// In the child: the limit as PipeWire's client leaves it (`soft`, `hard`),
/// FaderFrame's guard, then a realtime thread computing for 500 ms without
/// sleeping. Exits 0 when the process lived through it and the thread
/// ended at normal scheduling.
fn child(soft: u64, hard: u64) -> ! {
    set_limit(soft, hard);
    priority::note_rttime_limit();
    let kept = soft_limit();
    let outcome = std::thread::spawn(|| {
        if !realtime_with_reset_on_fork() {
            return None;
        }
        spin_for(Duration::from_millis(500));
        Some(is_realtime())
    })
    .join()
    .unwrap();
    match outcome {
        None => std::process::exit(SKIPPED),
        Some(still_realtime) => {
            println!(
                "soft limit kept: {kept} µs; thread still realtime: {still_realtime}; \
                 demotions: {:?}",
                priority::rttime_demotions()
            );
            std::process::exit(if still_realtime { 2 } else { 0 })
        }
    }
}

/// Run `case` in a child; `None` when it could not run realtime here.
fn in_child(test: &str, case: &str) -> Option<std::process::Output> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, case)
        .output()
        .unwrap();
    if out.status.code() == Some(SKIPPED) {
        eprintln!("no realtime permission here: skipped");
        return None;
    }
    Some(out)
}

fn assert_lived(out: &std::process::Output, what: &str) {
    use std::os::unix::process::ExitStatusExt;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.signal(),
        Some(libc::SIGKILL),
        "{what}: the kernel killed the process at the hard limit\n{stdout}"
    );
    assert!(out.status.success(), "{what}: {:?}\n{stdout}", out.status);
}

/// PipeWire's client under rtkit sets soft = hard = 200 ms: without a
/// margin the first sign is the `SIGKILL`.
#[test]
fn a_thread_past_the_limit_pipewire_sets_is_demoted_not_killed() {
    if std::env::var(CHILD).as_deref() == Ok("pipewire") {
        child(200_000, 200_000);
    }
    if let Some(out) = in_child(
        "a_thread_past_the_limit_pipewire_sets_is_demoted_not_killed",
        "pipewire",
    ) {
        assert_lived(&out, "soft = hard");
    }
}

/// FaderFrame's own setting (soft half the hard limit): the `SIGXCPU`
/// handler's demotion must work on a thread with `SCHED_RESET_ON_FORK`.
#[test]
fn a_reset_on_fork_thread_past_the_soft_limit_is_demoted() {
    if std::env::var(CHILD).as_deref() == Ok("own") {
        child(100_000, 200_000);
    }
    if let Some(out) = in_child(
        "a_reset_on_fork_thread_past_the_soft_limit_is_demoted",
        "own",
    ) {
        assert_lived(&out, "soft < hard");
    }
}

/// A helper made realtime by rtkit (reset-on-fork) goes back to normal
/// scheduling when the audio thread is not realtime (another device).
#[test]
fn a_reset_on_fork_helper_follows_the_audio_thread_back_to_normal() {
    let normal = std::thread::spawn(|| {
        if !realtime_with_reset_on_fork() {
            return None;
        }
        // No realtime audio thread known in this process: normal.
        priority::follow_audio(priority::Helper::Feeder);
        Some(!is_realtime())
    })
    .join()
    .unwrap();
    match normal {
        None => eprintln!("no realtime permission here: skipped"),
        Some(normal) => assert!(normal, "the helper stayed realtime"),
    }
}

/// Nothing to do.
struct Idle;

impl PoolJob for Idle {
    fn work(&self) {}
}

/// DSP workers take their caller's scheduling, back to normal too: the
/// session keeps its pool across audio devices, and after a realtime
/// audio thread (PipeWire's data loop, rtkit's) the next caller may be an
/// ordinary thread (the dummy device's).
#[test]
fn pool_workers_follow_a_normal_caller_back_to_normal() {
    let pool = std::sync::Arc::new(WorkerPool::new(PoolConfig::new(2)));
    let p = std::sync::Arc::clone(&pool);
    let realtime = std::thread::spawn(move || {
        if !realtime_with_reset_on_fork() {
            return None;
        }
        // The workers become realtime with the caller.
        for _ in 0..64 {
            p.run(&Idle, 2);
        }
        std::thread::sleep(Duration::from_millis(20));
        p.run(&Idle, 2);
        Some(p.priority_failures())
    })
    .join()
    .unwrap();
    let Some(realtime) = realtime else {
        eprintln!("no realtime permission here: skipped");
        return;
    };
    assert_eq!(realtime, 0, "the workers became realtime");
    let p = std::sync::Arc::clone(&pool);
    let normal = std::thread::spawn(move || {
        // An ordinary thread (no flags); the pool reads it within 4096
        // runs.
        for _ in 0..8192 {
            p.run(&Idle, 2);
        }
        std::thread::sleep(Duration::from_millis(20));
        p.run(&Idle, 2);
        p.priority_failures()
    })
    .join()
    .unwrap();
    assert_eq!(normal, 0, "the workers could not go back to normal");
}

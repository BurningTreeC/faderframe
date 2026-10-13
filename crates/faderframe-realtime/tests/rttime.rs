//! Breaths under a realtime time limit, against the kernel: a realtime
//! thread that computes without blocking for longer than `RLIMIT_RTTIME`'s
//! soft limit gets `SIGXCPU`; one that calls `priority::breathe` in its
//! loop does not. Its own test binary: the limit stays in this process.
//! Needs realtime permission (`RLIMIT_RTPRIO`); skipped without it.

#![allow(clippy::unwrap_used)]
#![cfg(target_os = "linux")]

use faderframe_realtime::priority;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigxcpu(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// Soft limit 40 ms, hard 2 s (never reached here).
fn limit() {
    let lim = libc::rlimit {
        rlim_cur: 40_000,
        rlim_max: 2_000_000,
    };
    // SAFETY: a valid rlimit; lowering limits needs no privilege.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &lim) }, 0);
}

/// On a fresh `SCHED_FIFO` thread: compute for `ms` (calling `breathe`
/// when `breathing`); whether `SIGXCPU` came, or `None` without realtime
/// permission.
fn spin(ms: u64, breathing: bool) -> Option<bool> {
    std::thread::spawn(move || {
        let param = libc::sched_param { sched_priority: 5 };
        // SAFETY: this thread, a valid param.
        if unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) }
            != 0
        {
            return None;
        }
        SIGNALLED.store(false, Ordering::SeqCst);
        let end = Instant::now() + Duration::from_millis(ms);
        let mut x = 0u64;
        while Instant::now() < end {
            for i in 0..10_000u64 {
                x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
            }
            if breathing {
                priority::breathe();
            }
        }
        Some(SIGNALLED.load(Ordering::SeqCst))
    })
    .join()
    .unwrap()
}

#[test]
fn realtime_helpers_breathe_within_the_time_limit() {
    // SAFETY: a plain handler for SIGXCPU (an atomic store).
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigxcpu as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::sigaction(libc::SIGXCPU, &action, std::ptr::null_mut());
    }
    limit();
    priority::note_rttime_limit();
    // Without breaths the kernel steps in (the limit works here).
    let Some(signalled) = spin(150, false) else {
        eprintln!("no realtime permission here: skipped");
        return;
    };
    assert!(signalled, "the kernel's realtime time limit is in force");
    // The kernel raised the soft limit by a second: back to 40 ms.
    limit();
    // With them, several times the limit passes without a signal.
    assert_eq!(
        spin(300, true),
        Some(false),
        "a breathing thread is left alone"
    );
}

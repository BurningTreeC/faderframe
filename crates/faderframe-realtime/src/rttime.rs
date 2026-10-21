//! The realtime time limit (`RLIMIT_RTTIME`, Linux) and what happens at it.
//!
//! A realtime thread that runs that long without sleeping gets `SIGXCPU`
//! at the soft limit, and at the hard limit the kernel kills the whole
//! process with `SIGKILL` — no log, no core dump, a "random crash".
//! RealtimeKit wants the limit set (FaderFrame sets it when it asks rtkit,
//! `rtkit::limit_rttime`), and PipeWire's client library sets it too, on
//! every context it makes (its `module-rt` under rtkit: soft = hard =
//! rtkit's `RTTimeUSecMax`, 200 ms — soft equal to hard means no `SIGXCPU`
//! before the `SIGKILL`). [`guard`] keeps a margin whoever set the limit:
//! the soft limit half the hard one (or the soft one already set below
//! it), and `SIGXCPU` caught. The handler puts the thread that ran too
//! long back to normal scheduling — keeping `SCHED_RESET_ON_FORK`: rtkit,
//! PipeWire and FaderFrame make realtime threads with that flag, and an
//! unprivileged thread may not clear it (`EPERM`: the demotion would fail
//! and the hard limit kill the process a moment later) — and puts the soft
//! limit back (the kernel raises it by a second at each `SIGXCPU`, past
//! the hard limit).

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

/// The soft limit kept (µs; 0: none known).
static SOFT: AtomicU64 = AtomicU64::new(0);
/// Threads `SIGXCPU` returned to normal scheduling, and the last one's id.
static DEMOTED: AtomicU64 = AtomicU64::new(0);
static LAST_DEMOTED: AtomicI32 = AtomicI32::new(0);

/// The process's `RLIMIT_RTTIME` (soft, hard), `RLIM_INFINITY` for none.
pub fn limits() -> Option<(libc::rlim_t, libc::rlim_t)> {
    // SAFETY: getrlimit fills the struct it is given.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: a valid pointer to an rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut lim) } != 0 {
        return None;
    }
    Some((lim.rlim_cur, lim.rlim_max))
}

/// The soft limit to keep under `(soft, hard)`: the one set when it is
/// below the hard limit (or there is no hard limit), else half the hard
/// one. `None`: no limit at all.
pub fn margin(soft: libc::rlim_t, hard: libc::rlim_t) -> Option<libc::rlim_t> {
    if soft == libc::RLIM_INFINITY && hard == libc::RLIM_INFINITY {
        return None;
    }
    if hard == libc::RLIM_INFINITY || soft < hard {
        return Some(soft);
    }
    Some((hard / 2).max(1))
}

/// Under a realtime time limit: keep the soft limit below the hard one and
/// `SIGXCPU` caught (see the module docs). The soft limit kept, in µs
/// (`None`: no limit). Cheap (a system call or two); control threads.
// `rlim_t` is not 64 bits on every Linux target.
#[allow(clippy::unnecessary_cast)]
pub fn guard() -> Option<u64> {
    let (soft, hard) = limits()?;
    let Some(keep) = margin(soft, hard) else {
        SOFT.store(0, Ordering::Relaxed);
        return None;
    };
    // The handler first: a signal coming meanwhile never meets the
    // default action (which ends the process).
    catch_sigxcpu();
    SOFT.store(keep as u64, Ordering::Relaxed);
    if keep != soft {
        let new = libc::rlimit {
            rlim_cur: keep,
            rlim_max: hard,
        };
        // SAFETY: a valid rlimit; lowering the soft limit needs no
        // privilege.
        unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &new) };
    }
    Some(keep as u64)
}

/// Threads put back to normal scheduling for running too long, and the
/// id (`gettid`) of the last one.
pub fn demotions() -> (u64, i32) {
    (
        DEMOTED.load(Ordering::Relaxed),
        LAST_DEMOTED.load(Ordering::Relaxed),
    )
}

#[allow(clippy::unnecessary_cast)]
extern "C" fn on_sigxcpu(_: libc::c_int) {
    // Only system calls and atomics: signal-safe. errno as it was for the
    // code interrupted.
    // SAFETY: this thread's errno.
    let errno = unsafe { *libc::__errno_location() };
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: the calling thread (0) back to normal scheduling, the
    // reset-on-fork flag kept (see the module docs); a raw system call
    // (some C libraries do not implement the wrapper).
    unsafe {
        libc::syscall(
            libc::SYS_sched_setscheduler,
            0,
            libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK,
            &param as *const libc::sched_param,
        )
    };
    let soft = SOFT.load(Ordering::Relaxed) as libc::rlim_t;
    // SAFETY: getrlimit/setrlimit with valid pointers (thin system calls).
    unsafe {
        let mut lim: libc::rlimit = std::mem::zeroed();
        if soft != 0
            && libc::getrlimit(libc::RLIMIT_RTTIME, &mut lim) == 0
            && lim.rlim_cur != soft
            && (lim.rlim_max == libc::RLIM_INFINITY || soft < lim.rlim_max)
        {
            lim.rlim_cur = soft;
            libc::setrlimit(libc::RLIMIT_RTTIME, &lim);
        }
    }
    // SAFETY: plain system call.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
    LAST_DEMOTED.store(tid, Ordering::Relaxed);
    DEMOTED.fetch_add(1, Ordering::Relaxed);
    crate::priority::note_failure();
    // SAFETY: as above.
    unsafe { *libc::__errno_location() = errno };
}

/// Catch `SIGXCPU` unless someone else does (once is enough: the action is
/// the process's).
fn catch_sigxcpu() {
    let ours = on_sigxcpu as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: reads the current disposition into a valid struct.
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: as above (no new action).
    if unsafe { libc::sigaction(libc::SIGXCPU, std::ptr::null(), &mut old) } != 0
        || old.sa_sigaction == ours
        || !matches!(old.sa_sigaction, libc::SIG_DFL | libc::SIG_IGN)
    {
        // Ours already, or someone else's handler: theirs.
        return;
    }
    // SAFETY: a zeroed sigaction with the handler and flags set.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = ours;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: a valid action for SIGXCPU.
    unsafe { libc::sigaction(libc::SIGXCPU, &action, std::ptr::null_mut()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    const INF: libc::rlim_t = libc::RLIM_INFINITY;

    /// PipeWire's client under rtkit sets soft = hard: half of it is kept;
    /// a soft limit below the hard one, or without one, stays.
    #[test]
    fn the_soft_limit_stays_below_the_hard_one() {
        assert_eq!(margin(INF, INF), None);
        assert_eq!(margin(200_000, 200_000), Some(100_000));
        assert_eq!(margin(INF, 200_000), Some(100_000));
        assert_eq!(
            margin(1_200_000, 200_000),
            Some(100_000),
            "raised by SIGXCPU"
        );
        assert_eq!(margin(50_000, 200_000), Some(50_000));
        assert_eq!(margin(40_000, INF), Some(40_000));
    }
}

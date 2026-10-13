//! What the calling thread has cost so far: its CPU time (where the system
//! keeps a clock per thread) and, on Linux, its page faults and how often
//! it waited (voluntary context switches: a lock, I/O, a sleep). Used to
//! check what code does on an audio thread.

/// Page faults and waits of one thread so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadUsage {
    pub minor_faults: u64,
    pub major_faults: u64,
    /// The thread gave up the CPU to wait for something.
    pub voluntary_switches: u64,
}

impl ThreadUsage {
    /// What happened between `before` and `self`.
    pub fn since(self, before: ThreadUsage) -> ThreadUsage {
        ThreadUsage {
            minor_faults: self.minor_faults.saturating_sub(before.minor_faults),
            major_faults: self.major_faults.saturating_sub(before.major_faults),
            voluntary_switches: self
                .voluntary_switches
                .saturating_sub(before.voluntary_switches),
        }
    }
}

/// The calling thread's usage (`None` where the system does not count it
/// per thread).
pub fn thread_usage() -> Option<ThreadUsage> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getrusage fills the struct it is given.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to a rusage.
        if unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut u) } != 0 {
            return None;
        }
        Some(ThreadUsage {
            minor_faults: u.ru_minflt as u64,
            major_faults: u.ru_majflt as u64,
            voluntary_switches: u.ru_nvcsw as u64,
        })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// The calling thread's CPU time in nanoseconds (`None` without a clock
/// per thread).
pub fn thread_cpu_ns() -> Option<u64> {
    #[cfg(unix)]
    {
        // SAFETY: clock_gettime fills the timespec it is given.
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        // SAFETY: a valid clock id and pointer.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } != 0 {
            return None;
        }
        Some(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
    }
    #[cfg(not(unix))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_counts_its_cpu_and_its_waits() {
        let Some(c0) = thread_cpu_ns() else { return };
        let mut x = 0u64;
        for i in 0..3_000_000u64 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
        assert!(thread_cpu_ns().unwrap() > c0, "{x}");
        #[cfg(target_os = "linux")]
        {
            let before = thread_usage().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
            let waited = thread_usage().unwrap().since(before);
            assert!(waited.voluntary_switches >= 1, "{waited:?}");
        }
    }
}

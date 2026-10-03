use std::sync::atomic::{AtomicU64, Ordering};

const SUB_BUCKETS_LOG2: u32 = 3; // 8 sub-buckets per octave (~9 % resolution)
const SUB_BUCKETS: u64 = 1 << SUB_BUCKETS_LOG2;
const MIN_OCTAVE: u32 = 10; // 2^10 ns ≈ 1 µs
const MAX_OCTAVE: u32 = 31; // 2^31 ns ≈ 2.1 s
const BUCKETS: usize = 1 + ((MAX_OCTAVE - MIN_OCTAVE + 1) as usize) * SUB_BUCKETS as usize;

#[inline]
fn bucket_index(ns: u64) -> usize {
    if ns < (1 << MIN_OCTAVE) {
        return 0;
    }
    let octave = 63 - ns.leading_zeros();
    if octave > MAX_OCTAVE {
        return BUCKETS - 1;
    }
    let sub = (ns >> (octave - SUB_BUCKETS_LOG2)) & (SUB_BUCKETS - 1);
    1 + ((octave - MIN_OCTAVE) as usize) * SUB_BUCKETS as usize + sub as usize
}

/// Upper bound (exclusive) in ns of a bucket — percentiles are reported
/// conservatively.
fn bucket_upper_ns(index: usize) -> u64 {
    if index == 0 {
        return 1 << MIN_OCTAVE;
    }
    let i = (index - 1) as u64;
    let octave = MIN_OCTAVE as u64 + i / SUB_BUCKETS;
    let sub = i % SUB_BUCKETS;
    (SUB_BUCKETS + sub + 1) << (octave - SUB_BUCKETS_LOG2 as u64)
}

/// Audio callback timing statistics.
///
/// The audio thread calls [`CallbackMetrics::record`] once per callback with
/// the measured processing time and the deadline (block duration). Recording
/// is a handful of relaxed atomic increments. Percentiles are derived from a
/// log-scaled histogram on the control side, so worst-case behaviour (p99,
/// max, deadline misses) is visible, not just averages.
#[derive(Debug)]
pub struct CallbackMetrics {
    callbacks: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    last_ns: AtomicU64,
    last_budget_ns: AtomicU64,
    deadline_misses: AtomicU64,
    xruns: AtomicU64,
    histogram: Box<[AtomicU64]>,
}

impl Default for CallbackMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Point-in-time view of [`CallbackMetrics`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetricsSnapshot {
    pub callbacks: u64,
    pub mean_ns: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
    pub last_ns: u64,
    pub last_budget_ns: u64,
    pub deadline_misses: u64,
    pub xruns: u64,
}

impl MetricsSnapshot {
    /// DSP load of the most recent callback (processing time / deadline).
    pub fn last_load(&self) -> f64 {
        if self.last_budget_ns == 0 {
            0.0
        } else {
            self.last_ns as f64 / self.last_budget_ns as f64
        }
    }

    /// Load at the p99 processing time relative to the last deadline.
    pub fn p99_load(&self) -> f64 {
        if self.last_budget_ns == 0 {
            0.0
        } else {
            self.p99_ns as f64 / self.last_budget_ns as f64
        }
    }
}

impl CallbackMetrics {
    pub fn new() -> Self {
        Self {
            callbacks: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            last_budget_ns: AtomicU64::new(0),
            deadline_misses: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            histogram: (0..BUCKETS).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Record one callback (audio thread).
    #[inline]
    pub fn record(&self, duration_ns: u64, budget_ns: u64) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(duration_ns, Ordering::Relaxed);
        self.max_ns.fetch_max(duration_ns, Ordering::Relaxed);
        self.last_ns.store(duration_ns, Ordering::Relaxed);
        self.last_budget_ns.store(budget_ns, Ordering::Relaxed);
        self.histogram[bucket_index(duration_ns)].fetch_add(1, Ordering::Relaxed);
        if duration_ns > budget_ns {
            self.deadline_misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a backend-reported xrun (any thread).
    #[inline]
    pub fn record_xrun(&self) {
        self.xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Reset all counters (control thread; racing records are tolerated).
    pub fn reset(&self) {
        for a in [
            &self.callbacks,
            &self.total_ns,
            &self.max_ns,
            &self.last_ns,
            &self.deadline_misses,
            &self.xruns,
        ] {
            a.store(0, Ordering::Relaxed);
        }
        for b in self.histogram.iter() {
            b.store(0, Ordering::Relaxed);
        }
    }

    /// Compute a snapshot including percentiles (control thread).
    pub fn snapshot(&self) -> MetricsSnapshot {
        let counts: Vec<u64> = self
            .histogram
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect();
        let total: u64 = counts.iter().sum();
        let percentile = |p: f64| -> u64 {
            if total == 0 {
                return 0;
            }
            let target = ((total as f64) * p).ceil().max(1.0) as u64;
            let mut acc = 0;
            for (i, &c) in counts.iter().enumerate() {
                acc += c;
                if acc >= target {
                    return bucket_upper_ns(i);
                }
            }
            bucket_upper_ns(BUCKETS - 1)
        };
        let callbacks = self.callbacks.load(Ordering::Relaxed);
        let max_ns = self.max_ns.load(Ordering::Relaxed);
        MetricsSnapshot {
            callbacks,
            mean_ns: self
                .total_ns
                .load(Ordering::Relaxed)
                .checked_div(callbacks)
                .unwrap_or(0),
            // A bucket bound can exceed the true maximum; never report more.
            p50_ns: percentile(0.50).min(max_ns),
            p95_ns: percentile(0.95).min(max_ns),
            p99_ns: percentile(0.99).min(max_ns),
            max_ns,
            last_ns: self.last_ns.load(Ordering::Relaxed),
            last_budget_ns: self.last_budget_ns.load(Ordering::Relaxed),
            deadline_misses: self.deadline_misses.load(Ordering::Relaxed),
            xruns: self.xruns.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_monotonic_and_bounded() {
        let mut last = 0;
        for ns in (0..40).map(|i| 1u64 << i) {
            let b = bucket_index(ns);
            assert!(b >= last);
            assert!(b < BUCKETS);
            last = b;
        }
        for ns in [1_500u64, 50_000, 1_333_333, 20_000_000] {
            let b = bucket_index(ns);
            assert!(bucket_upper_ns(b) > ns, "ns={ns}");
            assert!(
                bucket_upper_ns(b) as f64 <= ns as f64 * 1.13 + 1.0,
                "ns={ns}"
            );
        }
    }

    #[test]
    fn percentiles_and_misses() {
        let m = CallbackMetrics::new();
        let budget = 1_333_333;
        for _ in 0..98 {
            m.record(100_000, budget);
        }
        m.record(900_000, budget);
        m.record(2_000_000, budget);
        m.record_xrun();
        let s = m.snapshot();
        assert_eq!(s.callbacks, 100);
        assert_eq!(s.deadline_misses, 1);
        assert_eq!(s.xruns, 1);
        assert_eq!(s.max_ns, 2_000_000);
        assert!(s.p50_ns >= 100_000 && s.p50_ns < 115_000);
        assert!(s.p99_ns >= 900_000 && s.p99_ns < 1_020_000, "{}", s.p99_ns);
        assert!((s.last_load() - 1.5).abs() < 1e-6);
        m.reset();
        assert_eq!(m.snapshot().callbacks, 0);
    }
}

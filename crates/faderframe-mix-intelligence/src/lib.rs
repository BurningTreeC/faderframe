//! Mix Intelligence: a deterministic diagnosis of a multitrack mix.
//!
//! Rendered tracks go in; findings with their evidence come out. Nothing
//! here learns, guesses or calls out: every number is a documented
//! measurement, every finding comes from an explicit rule with
//! thresholds named in [`config`], and the same audio, settings and
//! [`ALGORITHM_VERSION`] give the same findings.
//!
//! * [`profile`] — per track: Hann-windowed FFT frames summed into bands
//!   ([`bands`]: third octaves by default), per frame and band the left and
//!   right levels and their correlation; the whole range's peak, true
//!   peak, RMS, overs and BS.1770 loudness (`faderframe_analysis`).
//! * [`masking`] — pairs of tracks: where one sounds close to the other's
//!   level in a band that is a prominent part of it, at the same time, at
//!   a similar stereo placement; ranges, events and persistence.
//! * [`stereo`] — mid/side, correlation, mono fold-down per band.
//! * [`health`] — the health check: DC offsets, overs and clipping between
//!   devices, devices producing invalid samples, silencing their input or
//!   changing nothing (with what they cost), bypassed devices.
//! * [`reference`] — the mix against a reference track, loudness-matched:
//!   loudness, spectral balance, balance per section, stereo and mono.
//! * [`rules`] — the rules turning those into [`finding::Finding`]s with
//!   severity, confidence (kept apart, both with the terms they came from)
//!   and conservative [`finding::Experiment`]s; ranked by relevance.
//! * [`phase`] — pairs of tracks heard together (chosen by role,
//!   selection, masking findings or levels moving together; bounded):
//!   delay and polarity (Auto-Align's estimator), per-band coherence,
//!   phase and their sum against their energies, onset timing; rules in
//!   [`rules_phase`] ([`rules_phase::diagnose_extra`]).
//! * [`width`] — frequency-dependent width and mono fold-down per band;
//!   rules in [`rules_width`].
//! * [`dynamics`] — a 5 ms envelope and BS.1770 loudness over time per
//!   signal; crest factor, PLR, LRA, short-term distribution,
//!   transient-to-sustain; rules in [`rules_dynamics`] (possible transient
//!   flattening against the source).
//! * [`details`] — what those measured, kept for the view (summaries,
//!   pairs compared, charts).
//! * [`same`] — where two tracks carry the same audio (per-band coherence
//!   of their band sums, window by window): left out of the masking rule,
//!   reported by [`rules_same`].
//!
//! Which signal a profile measures (processed, as mixed; or the source
//! before processing) is the caller's: the session renders it
//! (`faderframe_session::mix`: one render of the whole mix with a tap per
//! track, bus and return, and probes after the devices). This crate knows
//! no project, engine or toolkit.

#![forbid(unsafe_code)]

pub mod bands;
pub mod config;
pub mod details;
pub mod dynamics;
pub mod finding;
pub mod health;
pub mod masking;
pub mod phase;
pub mod profile;
pub mod reference;
mod rulekit;
pub mod rules;
pub mod rules_dynamics;
pub mod rules_phase;
pub mod rules_same;
pub mod rules_width;
pub mod same;
pub mod stereo;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_more;
pub mod width;

pub use config::{AnalysisConfig, Rules};
pub use details::{Chart, ChartSeries, Details};
pub use finding::{
    Category, Evidence, Experiment, Finding, FrequencyRange, Grade, Pattern, Recommendation,
    RuleId, SignalPoint, Subject, TimeSpan, Unit,
};
pub use phase::{PairCandidate, PairPhase, PairReason, PairSettings};
pub use profile::{ProfileRequest, TrackProfile, analyse};
pub use rules::{DiagnoseInput, Report, Summary, TrackInput, diagnose};
pub use rules_phase::{ExtraInput, PairInput, SourceInput, diagnose_extra};

/// The version of the calculations: part of every cache key and shown with
/// every finding. Bump it when a measurement or rule changes its results.
/// 2: dynamics traces in profiles; phase, timing, width and dynamics
/// rules. 3: band sums (phase) in profiles; the same audio left out of the
/// masking rule.
/// 4: processed signals from one render of the whole mix (each track,
/// bus and return where it reaches its destination), DC offsets in
/// profiles, the health check and the reference comparisons.
pub const ALGORITHM_VERSION: u32 = 5;

/// The most threads [`map_parallel`] uses.
pub const MAX_THREADS: usize = 8;

/// `f` over `items` on a bounded number of threads (half the machine's
/// cores, at most [`MAX_THREADS`]; one for a few items), the results in the
/// items' order: the same output as a plain map, whatever the threads did.
pub fn map_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = (cores / 2).clamp(1, MAX_THREADS).min(items.len());
    if threads <= 1 {
        return items.iter().map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    let done: Vec<Vec<(usize, R)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(i) else {
                            break;
                        };
                        mine.push((i, f(item)));
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    for (i, r) in done.into_iter().flatten() {
        out[i] = Some(r);
    }
    // Every index was taken by exactly one thread.
    out.into_iter().flatten().collect()
}

/// FNV-1a, 64 bits: stable fingerprints and ids (unlike `std`'s hasher,
/// the same in every build and process).
#[derive(Clone, Copy, Debug)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv {
    pub const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub fn write_u64(&mut self, v: u64) {
        self.write(&v.to_le_bytes());
    }

    pub fn finish(&self) -> u64 {
        self.0
    }

    /// The fingerprint of a string.
    pub fn of_str(s: &str) -> u64 {
        let mut h = Self::new();
        h.write(s.as_bytes());
        h.finish()
    }
}

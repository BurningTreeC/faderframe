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
//! * [`rules`] — the rules turning those into [`finding::Finding`]s with
//!   severity, confidence (kept apart, both with the terms they came from)
//!   and conservative [`finding::Experiment`]s; ranked by relevance.
//!
//! Which signal a profile measures (processed, as mixed; or the source
//! before processing) is the caller's: the session renders it
//! (`faderframe_session::mix`). This crate knows no project, engine or
//! toolkit.

#![forbid(unsafe_code)]

pub mod bands;
pub mod config;
pub mod finding;
pub mod masking;
pub mod profile;
pub mod rules;
pub mod stereo;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use config::{AnalysisConfig, Rules};
pub use finding::{
    Category, Evidence, Experiment, Finding, FrequencyRange, Grade, Pattern, Recommendation,
    RuleId, SignalPoint, Subject, TimeSpan, Unit,
};
pub use profile::{ProfileRequest, TrackProfile, analyse};
pub use rules::{DiagnoseInput, Report, Summary, TrackInput, diagnose};

/// The version of the calculations: part of every cache key and shown with
/// every finding. Bump it when a measurement or rule changes its results.
pub const ALGORITHM_VERSION: u32 = 1;

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

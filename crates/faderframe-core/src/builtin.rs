//! Identifiers of the plugins built into FaderFrame.
//!
//! Projects reference plugins by `(format, id)`; these are the ids of the
//! `Builtin` format, shared by the project model and the plugin host.

/// Polyphonic subtractive synthesizer (instrument).
pub const SYNTH: &str = "faderframe.synth";
/// Stereo feedback delay (effect).
pub const ECHO: &str = "faderframe.echo";
/// Compressor with a sidechain (key) input (effect).
pub const COMPRESSOR: &str = "faderframe.compressor";
/// Utility gain (effect).
pub const GAIN: &str = "faderframe.gain";
/// Pure delay that reports its delay as latency (testing PDC).
pub const LATENCY_PROBE: &str = "faderframe.latency-probe";

//! Identifiers of the plugins built into FaderFrame.
//!
//! Projects reference plugins by `(format, id)`; these are the ids of the
//! `Builtin` format, shared by the project model and the plugin host.

/// Polyphonic subtractive synthesizer (instrument).
pub const SYNTH: &str = "faderframe.synth";
/// Stereo feedback delay (effect).
pub const ECHO: &str = "faderframe.echo";
/// Compressor in five styles with a sidechain (key) input (effect).
pub const COMPRESSOR: &str = "faderframe.compressor";
/// Lookahead true peak limiter (effect).
pub const LIMITER: &str = "faderframe.limiter";
/// Gate, expander and ducker with a sidechain (effect).
pub const GATE: &str = "faderframe.gate";
/// De-esser: split or wide, relative or absolute (effect).
pub const DEESSER: &str = "faderframe.deesser";
/// Saturator: six curves, oversampled (effect).
pub const SATURATOR: &str = "faderframe.saturator";
/// Algorithmic reverb: five types, a 16-line network (effect).
pub const REVERB: &str = "faderframe.reverb";
/// Chorus, ensemble, flanger, phaser and vibrato (effect).
pub const MODULATION: &str = "faderframe.modulation";
/// Tuner (utility).
pub const TUNER: &str = "faderframe.tuner";
/// Utility: gain, balance, width, mono bass, polarity, channels (effect).
pub const GAIN: &str = "faderframe.gain";
/// Pure delay that reports its delay as latency (testing PDC).
pub const LATENCY_PROBE: &str = "faderframe.latency-probe";
/// 24 band parametric and dynamic equaliser (effect).
pub const EQ: &str = "faderframe.eq";
/// Circuit modelled passive program equaliser with a tube make-up stage,
/// from PultEQFx (effect).
pub const PROGRAM_EQ: &str = "faderframe.program-eq";

/// Built-ins with an editor of their own (others get the generic one).
pub fn has_editor(id: &str) -> bool {
    matches!(
        id,
        EQ | PROGRAM_EQ
            | TUNER
            | COMPRESSOR
            | LIMITER
            | SATURATOR
            | DEESSER
            | GATE
            | GAIN
            | ECHO
            | MODULATION
            | REVERB
    )
}

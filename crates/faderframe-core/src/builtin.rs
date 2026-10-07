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
/// Channel strip: filters, gate, compressor, four-band EQ and drive in
/// one insert (effect).
pub const CHANNEL_STRIP: &str = "faderframe.channel-strip";
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
/// Sampler: one sample across the keys, or an SFZ instrument (instrument).
pub const SAMPLER: &str = "faderframe.sampler";
/// Drum sampler: sixteen pads (instrument).
pub const DRUMS: &str = "faderframe.drums";
/// Utility: gain, balance, width, mono bass, polarity, channels (effect).
pub const GAIN: &str = "faderframe.gain";
/// Arpeggiator: the held notes one after another (MIDI effect).
pub const ARPEGGIATOR: &str = "faderframe.arpeggiator";
/// Chord: each note a chord — intervals, scale chords or the chord track
/// (MIDI effect).
pub const CHORD: &str = "faderframe.chord";
/// Scale: notes kept in a key, transposed by degrees (MIDI effect).
pub const SCALE: &str = "faderframe.scale";
/// Note Echo: repeats of each note, fading (MIDI effect).
pub const NOTE_ECHO: &str = "faderframe.note-echo";
/// Pure delay that reports its delay as latency (testing PDC).
pub const LATENCY_PROBE: &str = "faderframe.latency-probe";
/// 24 band parametric and dynamic equaliser (effect).
pub const EQ: &str = "faderframe.eq";
/// Circuit modelled passive program equaliser with a tube make-up stage,
/// from PultEQFx (effect).
pub const PROGRAM_EQ: &str = "faderframe.program-eq";

/// Guitar Station: a line of pedals, a modelled amplifier with its power
/// stage, a loudspeaker in a cabinet and two microphones, and a DI, from
/// GainStageFx (effect).
pub const GUITAR_STATION: &str = "faderframe.guitar-station";

/// Parallel chains of devices, mixed (see `faderframe_project::container`).
pub const CONTAINER: &str = "faderframe.container";

/// Built-ins with an editor of their own (others get the generic one).
pub fn has_editor(id: &str) -> bool {
    matches!(
        id,
        EQ | PROGRAM_EQ
            | CONTAINER
            | DRUMS
            | SAMPLER
            | SYNTH
            | TUNER
            | COMPRESSOR
            | LIMITER
            | SATURATOR
            | DEESSER
            | GATE
            | CHANNEL_STRIP
            | GUITAR_STATION
            | GAIN
            | ECHO
            | MODULATION
            | REVERB
            | ARPEGGIATOR
            | CHORD
            | SCALE
            | NOTE_ECHO
    )
}

/// Dedicated microphone preamplifiers; never offered as ordinary inserts.
/// IDs and catalogue order are persistent project data.
pub const PREAMPS: [(&str, &str, [u8; 3]); 6] = [
    ("faderframe.preamp.british73", "British 73", [65, 87, 108]),
    (
        "faderframe.preamp.american312",
        "American 312",
        [47, 55, 64],
    ),
    ("faderframe.preamp.console-e", "British 4K E", [84, 88, 89]),
    ("faderframe.preamp.tube610", "Tube 610", [61, 57, 51]),
    ("faderframe.preamp.british47", "British 47", [155, 162, 155]),
    ("faderframe.preamp.german76", "German 76", [132, 137, 130]),
];

pub fn preamp_index(id: &str) -> Option<usize> {
    PREAMPS.iter().position(|p| p.0 == id)
}

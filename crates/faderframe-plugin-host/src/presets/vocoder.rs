//! Vocoder presets: the synth carrier in chords, following the voice, and
//! noise; band counts from a gritty eight to a clear forty, formants moved.
//! None plays from MIDI or a sidechain (they need routing to sound).

use super::{FactoryPreset, preset};

pub(super) fn vocoder() -> Vec<FactoryPreset> {
    use crate::devices::vocoder::id::*;
    const NOISE_CARRIER: f64 = 1.0;
    const FOLLOW: f64 = 2.0;
    // Chords (`devices::vocoder::CHORDS`).
    const UNISON: f64 = 0.0;
    const OCTAVES: f64 = 1.0;
    const FIFTH: f64 = 2.0;
    const MAJOR: f64 = 3.0;
    const SUS4: f64 = 5.0;
    const MAJOR7: f64 = 6.0;
    const MINOR7: f64 = 7.0;
    // Band counts (`BAND_COUNTS` indices).
    const B8: f64 = 0.0;
    const B12: f64 = 1.0;
    const B24: f64 = 4.0;
    const B32: f64 = 5.0;
    const B40: f64 = 6.0;
    vec![
        preset("Classic Vocoder", &[(CHORD, OCTAVES), (WAVE, 0.4)]),
        preset(
            "Choir Pad",
            &[
                (CHORD, MAJOR),
                (BANDS, B32),
                (DETUNE, 25.0),
                (WIDTH, 0.8),
                (RELEASE, 120.0),
            ],
        ),
        // The voice's own melody, an octave up, in tune.
        preset(
            "Robot Singer",
            &[
                (PITCH, FOLLOW),
                (TRANSPOSE, 12.0),
                (BANDS, B24),
                (WAVE, 0.6),
            ],
        ),
        preset(
            "Talking Synth",
            &[(CHORD, FIFTH), (BANDS, B40), (BANDWIDTH, 0.8), (WAVE, 0.2)],
        ),
        // No pitch at all: a whisper.
        preset(
            "Whisper",
            &[(CARRIER, NOISE_CARRIER), (BANDS, B32), (UNVOICED, 0.6)],
        ),
        preset(
            "Low Formant",
            &[(CHORD, MINOR7), (FORMANT, -5.0), (ROOT, 43.0)],
        ),
        preset(
            "Chipmunk Formant",
            &[(CHORD, MAJOR), (FORMANT, 7.0), (ROOT, 60.0)],
        ),
        preset(
            "Square Octaves",
            &[(CHORD, OCTAVES), (WAVE, 1.0), (DETUNE, 6.0), (ROOT, 45.0)],
        ),
        preset(
            "Sus Drone",
            &[(CHORD, SUS4), (RELEASE, 250.0), (NOISE, 0.15)],
        ),
        preset(
            "Lush Seventh",
            &[
                (CHORD, MAJOR7),
                (BANDS, B24),
                (DETUNE, 30.0),
                (WIDTH, 1.0),
                (ATTACK, 10.0),
            ],
        ),
        preset(
            "Gritty Eight",
            &[
                (BANDS, B8),
                (CHORD, FIFTH),
                (BANDWIDTH, 1.6),
                (WAVE, 0.6),
                (DETUNE, 20.0),
            ],
        ),
        preset(
            "Telephone Robot",
            &[
                (BANDS, B12),
                (LOW, 300.0),
                (HIGH, 3_400.0),
                (CHORD, UNISON),
                (ROOT, 50.0),
                (UNVOICED, 0.0),
            ],
        ),
        // Half the voice under it.
        preset(
            "Doubled Voice",
            &[(PITCH, FOLLOW), (MIX, 0.5), (BANDS, B32), (WIDTH, 0.7)],
        ),
    ]
}

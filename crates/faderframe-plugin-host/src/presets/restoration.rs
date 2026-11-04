//! The restoration devices' presets: the noises a recording comes with —
//! hiss, a room's air, a ground loop, a scratched record, a clipped take, a
//! boomy room — each with the settings that usually take it out. The
//! De-noise presets leave a learned profile (and the choice to use it) as
//! it is: it is the recording's noise, not a setting.

use super::{FactoryPreset, preset};

pub(super) fn denoise() -> Vec<FactoryPreset> {
    use crate::devices::denoise::id::*;
    const LOW_LATENCY: f64 = 0.0;
    const QUALITY: f64 = 2.0;
    vec![
        preset("Gentle Hiss", &[(REDUCTION, 8.0), (SMOOTHING, 0.5)]),
        preset(
            "Tape Hiss",
            &[
                (REDUCTION, 14.0),
                (TILT, 6.0),
                (LOW, 1_500.0),
                (SPECTRAL, 0.4),
            ],
        ),
        preset(
            "Room Tone",
            &[
                (REDUCTION, 12.0),
                (SMOOTHING, 0.6),
                (SPECTRAL, 0.4),
                (MODE, QUALITY),
            ],
        ),
        preset(
            "Air Conditioner",
            &[
                (REDUCTION, 15.0),
                (TILT, -6.0),
                (HIGH, 4_000.0),
                (THRESHOLD, 4.0),
            ],
        ),
        preset(
            "Dialogue Cleanup",
            &[
                (REDUCTION, 12.0),
                (THRESHOLD, 4.0),
                (TRANSIENTS, 0.5),
                (SMOOTHING, 0.6),
                (LOW, 60.0),
            ],
        ),
        preset(
            "Heavy Noise",
            &[
                (REDUCTION, 18.0),
                (THRESHOLD, 4.0),
                (SMOOTHING, 0.8),
                (SPECTRAL, 0.6),
                (MODE, QUALITY),
            ],
        ),
        preset(
            "Live Monitoring",
            &[(MODE, LOW_LATENCY), (REDUCTION, 10.0), (SMOOTHING, 0.4)],
        ),
        preset(
            "Guitar Amp Hiss",
            &[
                (REDUCTION, 12.0),
                (LOW, 2_500.0),
                (TRANSIENTS, 0.6),
                (TILT, 3.0),
            ],
        ),
        preset(
            "Preamp Floor",
            &[(REDUCTION, 6.0), (THRESHOLD, 0.0), (SMOOTHING, 0.3)],
        ),
        preset(
            "Vinyl Surface",
            &[
                (REDUCTION, 10.0),
                (TILT, 4.0),
                (SPECTRAL, 0.5),
                (TRANSIENTS, 0.4),
            ],
        ),
        preset(
            "Field Recording",
            &[
                (REDUCTION, 18.0),
                (SMOOTHING, 0.7),
                (THRESHOLD, 5.0),
                (MODE, QUALITY),
                (TRANSIENTS, 0.5),
            ],
        ),
    ]
}

pub(super) fn declick() -> Vec<FactoryPreset> {
    use crate::devices::declick::id::*;
    vec![
        preset("Vinyl Clicks", &[(SENSITIVITY, 0.5), (WIDTH, 1.0)]),
        preset(
            "Vinyl Crackle",
            &[(SENSITIVITY, 0.55), (WIDTH, 0.5), (CRACKLE, 0.6)],
        ),
        preset(
            "Old 78",
            &[(SENSITIVITY, 0.7), (WIDTH, 2.5), (CRACKLE, 0.8)],
        ),
        preset("Gentle", &[(SENSITIVITY, 0.3), (WIDTH, 0.6)]),
        preset("Digital Clicks", &[(SENSITIVITY, 0.6), (WIDTH, 0.2)]),
        preset(
            "Mouth Clicks",
            &[(SENSITIVITY, 0.65), (WIDTH, 1.5), (LINK, 0.0)],
        ),
        preset("Edit Clicks", &[(SENSITIVITY, 0.45), (WIDTH, 3.0)]),
        preset(
            "Heavy Damage",
            &[(SENSITIVITY, 0.85), (WIDTH, 3.5), (CRACKLE, 0.5)],
        ),
        preset(
            "Static Pops",
            &[(SENSITIVITY, 0.5), (WIDTH, 2.0), (LINK, 0.0)],
        ),
        preset(
            "Light Crackle",
            &[(SENSITIVITY, 0.4), (WIDTH, 0.3), (CRACKLE, 0.3)],
        ),
    ]
}

pub(super) fn dehum() -> Vec<FactoryPreset> {
    use crate::devices::dehum::id::*;
    const HZ_50: f64 = 1.0;
    const HZ_60: f64 = 2.0;
    const CUSTOM: f64 = 3.0;
    vec![
        preset(
            "Mains 50 Hz",
            &[(FUNDAMENTAL, HZ_50), (HARMONICS, 6.0), (WIDTH, 2.0)],
        ),
        preset(
            "Mains 60 Hz",
            &[(FUNDAMENTAL, HZ_60), (HARMONICS, 6.0), (WIDTH, 2.0)],
        ),
        preset("Ground Loop Buzz", &[(HARMONICS, 24.0), (WIDTH, 3.0)]),
        preset("Guitar Amp Hum", &[(HARMONICS, 12.0), (WIDTH, 4.0)]),
        preset(
            "Gentle Notches",
            &[(HARMONICS, 4.0), (WIDTH, 1.0), (DEPTH, 0.8)],
        ),
        preset("Dimmer Buzz", &[(HARMONICS, 32.0), (WIDTH, 5.0)]),
        preset(
            "Generator",
            &[
                (FUNDAMENTAL, CUSTOM),
                (FREQUENCY, 55.0),
                (HARMONICS, 10.0),
                (WIDTH, 6.0),
            ],
        ),
        preset("Fundamental Only", &[(HARMONICS, 1.0), (WIDTH, 3.0)]),
        preset("Wide and Deep", &[(HARMONICS, 16.0), (WIDTH, 10.0)]),
        preset(
            "Fixed Notches",
            &[(TRACK, 0.0), (HARMONICS, 8.0), (WIDTH, 1.5)],
        ),
    ]
}

pub(super) fn declip() -> Vec<FactoryPreset> {
    use crate::devices::declip::id::*;
    const FAST: f64 = 0.0;
    vec![
        preset("Light Clipping", &[(TOLERANCE, 0.05), (MAX_LENGTH, 1.0)]),
        preset(
            "Hard Clipping",
            &[(TOLERANCE, 0.1), (MAX_LENGTH, 3.0), (OUTPUT, -3.0)],
        ),
        preset(
            "Overloaded Recording",
            &[(TOLERANCE, 0.3), (MAX_LENGTH, 5.0), (OUTPUT, -6.0)],
        ),
        preset(
            "Soft Clipped",
            &[(TOLERANCE, 1.5), (MAX_LENGTH, 2.0), (OUTPUT, -3.0)],
        ),
        preset("Fast", &[(QUALITY, FAST), (OUTPUT, -2.0)]),
        preset(
            "Converter Ceiling",
            &[(AUTO, 0.0), (LEVEL, -0.1), (TOLERANCE, 0.05)],
        ),
        preset(
            "Clipped at −6 dB",
            &[(AUTO, 0.0), (LEVEL, -6.0), (TOLERANCE, 0.1)],
        ),
        preset(
            "Loud Master",
            &[(TOLERANCE, 0.2), (MAX_LENGTH, 1.5), (OUTPUT, -4.0)],
        ),
        preset(
            "Drums",
            &[(MAX_LENGTH, 0.8), (TOLERANCE, 0.1), (OUTPUT, -3.0)],
        ),
        preset(
            "Brickwall Limited",
            &[(TOLERANCE, 0.5), (MAX_LENGTH, 0.5), (OUTPUT, -1.0)],
        ),
    ]
}

pub(super) fn dereverb() -> Vec<FactoryPreset> {
    use crate::devices::dereverb::id::*;
    const SET: f64 = 0.0;
    vec![
        preset(
            "Small Room",
            &[(AUTO_TAIL, SET), (TAIL, 400.0), (EARLY, 30.0)],
        ),
        preset(
            "Living Room",
            &[(AUTO_TAIL, SET), (TAIL, 600.0), (EARLY, 40.0)],
        ),
        preset(
            "Large Hall",
            &[
                (AUTO_TAIL, SET),
                (TAIL, 2_500.0),
                (EARLY, 80.0),
                (AMOUNT, 0.6),
            ],
        ),
        preset(
            "Church",
            &[
                (AUTO_TAIL, SET),
                (TAIL, 4_000.0),
                (EARLY, 100.0),
                (AMOUNT, 0.6),
            ],
        ),
        preset(
            "Dialogue in a Room",
            &[
                (AMOUNT, 0.7),
                (EARLY, 40.0),
                (DAMPING, 0.4),
                (SMOOTHING, 0.5),
            ],
        ),
        preset("Gentle", &[(AMOUNT, 0.25)]),
        preset("Strong", &[(AMOUNT, 0.9), (SMOOTHING, 0.5)]),
        preset("Bright Room", &[(DAMPING, 0.0), (AMOUNT, 0.5)]),
        preset("Dark Room", &[(DAMPING, 0.7), (AMOUNT, 0.5)]),
        preset("Smooth", &[(SMOOTHING, 0.8), (AMOUNT, 0.6)]),
    ]
}

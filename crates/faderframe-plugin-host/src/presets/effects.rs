//! Saturator, delay, reverb, modulation and utility presets.

use super::{FactoryPreset, preset};

pub(super) fn saturator() -> Vec<FactoryPreset> {
    use crate::devices::saturator::id::*;
    const SOFT: f64 = 0.0;
    const TAPE: f64 = 1.0;
    const TUBE: f64 = 2.0;
    const TRANSISTOR: f64 = 3.0;
    const FOLD: f64 = 4.0;
    const CLIP: f64 = 5.0;
    vec![
        preset("Gentle Warmth", &[(TYPE, TAPE), (DRIVE, 4.0), (TONE, -0.1)]),
        preset(
            "Tape Glue",
            &[
                (TYPE, TAPE),
                (DRIVE, 7.0),
                (TONE, -0.15),
                (HIGH_CUT, 18_000.0),
            ],
        ),
        // Even harmonics from the bias, a little lift, mixed in.
        preset(
            "Tube Vocal Sheen",
            &[
                (TYPE, TUBE),
                (DRIVE, 8.0),
                (BIAS, 0.15),
                (TONE, 0.15),
                (MIX, 0.7),
            ],
        ),
        preset(
            "Warm Bass",
            &[
                (TYPE, TUBE),
                (DRIVE, 10.0),
                (BIAS, 0.2),
                (TONE, -0.3),
                (HIGH_CUT, 8_000.0),
                (MIX, 0.8),
            ],
        ),
        // Harmonics a sub needs to be heard on small speakers.
        preset("808 Grit", &[(TYPE, SOFT), (DRIVE, 16.0), (TONE, 0.1)]),
        preset(
            "Drum Bus Crunch",
            &[(TYPE, TRANSISTOR), (DRIVE, 12.0), (TONE, 0.1), (MIX, 0.6)],
        ),
        // Hard clipping just over the hits, 8× oversampled: loudness
        // without a limiter's movement.
        preset(
            "Drum Peak Clipper",
            &[(TYPE, CLIP), (DRIVE, 3.0), (OVERSAMPLING, 3.0)],
        ),
        preset(
            "Tube Overdrive",
            &[
                (TYPE, TUBE),
                (DRIVE, 20.0),
                (BIAS, 0.1),
                (TONE, 0.2),
                (LOW_CUT, 80.0),
                (HIGH_CUT, 9_000.0),
            ],
        ),
        preset(
            "Fuzz Box",
            &[
                (TYPE, CLIP),
                (DRIVE, 30.0),
                (BIAS, 0.3),
                (TONE, 0.2),
                (LOW_CUT, 100.0),
                (HIGH_CUT, 6_500.0),
                (OVERSAMPLING, 3.0),
            ],
        ),
        preset(
            "AM Radio",
            &[
                (TYPE, TRANSISTOR),
                (DRIVE, 14.0),
                (TONE, 0.2),
                (LOW_CUT, 400.0),
                (HIGH_CUT, 3_500.0),
            ],
        ),
        preset(
            "Worn Cassette",
            &[
                (TYPE, TAPE),
                (DRIVE, 12.0),
                (TONE, -0.5),
                (LOW_CUT, 40.0),
                (HIGH_CUT, 10_000.0),
            ],
        ),
        // A wave folder's metallic overtones for synths.
        preset(
            "West Coast Fold",
            &[
                (TYPE, FOLD),
                (DRIVE, 18.0),
                (TONE, 0.1),
                (OVERSAMPLING, 3.0),
            ],
        ),
        // Only the highs are driven, and mixed in under the dry signal.
        preset(
            "Harmonic Exciter",
            &[
                (TYPE, SOFT),
                (DRIVE, 9.0),
                (LOW_CUT, 1_000.0),
                (TONE, 0.4),
                (MIX, 0.25),
            ],
        ),
    ]
}

pub(super) fn delay() -> Vec<FactoryPreset> {
    use crate::devices::delay::id::*;
    const STEREO: f64 = 0.0;
    const PING_PONG: f64 = 1.0;
    const MONO: f64 = 2.0;
    const TAPE: f64 = 1.0;
    const ANALOG: f64 = 2.0;
    // Divisions (crate::dsp::lfo::DIVISIONS).
    const D16: f64 = 5.0;
    const D8T: f64 = 6.0;
    const D8: f64 = 8.0;
    const D8D: f64 = 10.0;
    const D4: f64 = 11.0;
    const D4D: f64 = 13.0;
    const D2: f64 = 14.0;
    vec![
        preset(
            "Slapback",
            &[
                (MODE, MONO),
                (TIME, 110.0),
                (FEEDBACK, 0.1),
                (DAMPING, 0.4),
                (STYLE, TAPE),
                (SATURATION, 0.2),
                (LOW_CUT, 120.0),
                (MIX, 0.3),
            ],
        ),
        // Artificial double tracking: short, a touch of wow, wide.
        preset(
            "ADT Doubler",
            &[
                (MODE, STEREO),
                (TIME, 18.0),
                (OFFSET, 0.5),
                (FEEDBACK, 0.0),
                (DAMPING, 0.2),
                (STYLE, TAPE),
                (WOW, 0.1),
                (WOW_RATE, 0.8),
                (WIDTH, 1.5),
                (MIX, 0.35),
            ],
        ),
        preset(
            "Quarter Note Echo",
            &[
                (MODE, STEREO),
                (SYNC, 1.0),
                (DIVISION, D4),
                (FEEDBACK, 0.35),
                (DAMPING, 0.35),
                (LOW_CUT, 150.0),
                (MIX, 0.25),
            ],
        ),
        // The dotted eighth that turns a picked part into a rhythm.
        preset(
            "Dotted Eighth",
            &[
                (MODE, STEREO),
                (SYNC, 1.0),
                (DIVISION, D8D),
                (FEEDBACK, 0.4),
                (DAMPING, 0.2),
                (LOW_CUT, 200.0),
                (MIX, 0.35),
            ],
        ),
        preset(
            "Ping-Pong Eighths",
            &[
                (MODE, PING_PONG),
                (SYNC, 1.0),
                (DIVISION, D8),
                (FEEDBACK, 0.45),
                (DAMPING, 0.45),
                (LOW_CUT, 200.0),
                (MIX, 0.25),
            ],
        ),
        preset(
            "Triplet Bounce",
            &[
                (MODE, PING_PONG),
                (SYNC, 1.0),
                (DIVISION, D8T),
                (FEEDBACK, 0.4),
                (DAMPING, 0.3),
                (LOW_CUT, 180.0),
                (MIX, 0.25),
            ],
        ),
        preset(
            "Sixteenth Ripple",
            &[
                (MODE, STEREO),
                (SYNC, 1.0),
                (DIVISION, D16),
                (OFFSET, 0.12),
                (FEEDBACK, 0.3),
                (DAMPING, 0.2),
                (LOW_CUT, 250.0),
                (WIDTH, 1.2),
                (MIX, 0.2),
            ],
        ),
        // The repeats wait for the singer to stop.
        preset(
            "Vocal Throw (Ducked)",
            &[
                (MODE, STEREO),
                (SYNC, 1.0),
                (DIVISION, D4),
                (FEEDBACK, 0.3),
                (DAMPING, 0.4),
                (LOW_CUT, 250.0),
                (DUCKING, 0.7),
                (MIX, 0.3),
            ],
        ),
        preset(
            "Tape Echo",
            &[
                (MODE, STEREO),
                (TIME, 350.0),
                (OFFSET, 0.04),
                (FEEDBACK, 0.55),
                (DAMPING, 0.5),
                (STYLE, TAPE),
                (SATURATION, 0.35),
                (WOW, 0.25),
                (WOW_RATE, 0.5),
                (LOW_CUT, 100.0),
                (MIX, 0.3),
            ],
        ),
        preset(
            "Analog Bucket Brigade",
            &[
                (MODE, STEREO),
                (TIME, 280.0),
                (OFFSET, -0.06),
                (FEEDBACK, 0.5),
                (DAMPING, 0.3),
                (STYLE, ANALOG),
                (SATURATION, 0.3),
                (WOW, 0.15),
                (WOW_RATE, 0.4),
                (MIX, 0.3),
            ],
        ),
        // Long feedback through saturating tape: it swells, never explodes.
        preset(
            "Dub Echo",
            &[
                (MODE, PING_PONG),
                (SYNC, 1.0),
                (DIVISION, D4D),
                (FEEDBACK, 0.88),
                (DAMPING, 0.55),
                (STYLE, TAPE),
                (SATURATION, 0.45),
                (WOW, 0.2),
                (WOW_RATE, 0.35),
                (LOW_CUT, 300.0),
                (MIX, 0.35),
            ],
        ),
        preset(
            "Lo-Fi Cassette Echo",
            &[
                (MODE, STEREO),
                (SYNC, 1.0),
                (DIVISION, D8),
                (FEEDBACK, 0.4),
                (DAMPING, 0.7),
                (STYLE, TAPE),
                (SATURATION, 0.5),
                (WOW, 0.6),
                (WOW_RATE, 1.2),
                (LOW_CUT, 250.0),
                (MIX, 0.3),
            ],
        ),
        preset(
            "Lead Guitar Echo",
            &[
                (MODE, STEREO),
                (TIME, 420.0),
                (OFFSET, 0.03),
                (FEEDBACK, 0.4),
                (DAMPING, 0.45),
                (STYLE, TAPE),
                (SATURATION, 0.2),
                (WOW, 0.1),
                (DUCKING, 0.35),
                (LOW_CUT, 150.0),
                (MIX, 0.3),
            ],
        ),
        preset(
            "Ambient Wash",
            &[
                (MODE, PING_PONG),
                (SYNC, 1.0),
                (DIVISION, D2),
                (FEEDBACK, 0.7),
                (DAMPING, 0.6),
                (STYLE, ANALOG),
                (WOW, 0.2),
                (WOW_RATE, 0.25),
                (LOW_CUT, 200.0),
                (WIDTH, 1.3),
                (MIX, 0.3),
            ],
        ),
    ]
}

pub(super) fn reverb() -> Vec<FactoryPreset> {
    use crate::devices::reverb::id::*;
    const ROOM: f64 = 0.0;
    const HALL: f64 = 1.0;
    const PLATE: f64 = 2.0;
    const CHAMBER: f64 = 3.0;
    const AMBIENCE: f64 = 4.0;
    vec![
        preset(
            "Small Drum Room",
            &[
                (TYPE, ROOM),
                (SIZE, 0.35),
                (DECAY, 0.5),
                (PRE_DELAY, 2.0),
                (DAMPING, 7_000.0),
                (BASS, 1.0),
                (DIFFUSION, 0.7),
                (MODULATION, 0.1),
                (BALANCE, 0.35),
                (LOW_CUT, 80.0),
                (HIGH_CUT, 14_000.0),
                (MIX, 0.2),
            ],
        ),
        preset(
            "Live Room",
            &[
                (TYPE, ROOM),
                (SIZE, 0.7),
                (DECAY, 0.9),
                (PRE_DELAY, 5.0),
                (DAMPING, 8_000.0),
                (BASS, 1.1),
                (BALANCE, 0.45),
                (LOW_CUT, 60.0),
                (MIX, 0.22),
            ],
        ),
        // Pre-delay keeps the words clear of the plate's bloom.
        preset(
            "Vocal Plate",
            &[
                (TYPE, PLATE),
                (SIZE, 0.6),
                (DECAY, 1.8),
                (PRE_DELAY, 30.0),
                (DAMPING, 9_000.0),
                (BASS, 0.8),
                (DIFFUSION, 0.85),
                (MODULATION, 0.3),
                (BALANCE, 0.8),
                (LOW_CUT, 200.0),
                (HIGH_CUT, 16_000.0),
                (MIX, 0.2),
            ],
        ),
        preset(
            "Bright Snare Plate",
            &[
                (TYPE, PLATE),
                (SIZE, 0.5),
                (DECAY, 1.3),
                (PRE_DELAY, 10.0),
                (DAMPING, 12_000.0),
                (BASS, 0.7),
                (DIFFUSION, 0.85),
                (BALANCE, 0.75),
                (LOW_CUT, 300.0),
                (MIX, 0.25),
            ],
        ),
        preset(
            "Vocal Chamber",
            &[
                (TYPE, CHAMBER),
                (SIZE, 0.55),
                (DECAY, 1.4),
                (PRE_DELAY, 20.0),
                (DAMPING, 7_500.0),
                (BASS, 0.9),
                (LOW_CUT, 150.0),
                (MIX, 0.2),
            ],
        ),
        preset(
            "Large Chamber",
            &[
                (TYPE, CHAMBER),
                (SIZE, 0.9),
                (DECAY, 2.8),
                (PRE_DELAY, 20.0),
                (DAMPING, 6_000.0),
                (BASS, 1.2),
                (LOW_CUT, 60.0),
                (MIX, 0.25),
            ],
        ),
        preset(
            "Concert Hall",
            &[
                (TYPE, HALL),
                (SIZE, 0.8),
                (DECAY, 2.6),
                (PRE_DELAY, 25.0),
                (DAMPING, 5_500.0),
                (BASS, 1.3),
                (DIFFUSION, 0.8),
                (MODULATION, 0.25),
                (BALANCE, 0.55),
                (LOW_CUT, 40.0),
                (MIX, 0.25),
            ],
        ),
        // More early reflections than tail: a room for an orchestra.
        preset(
            "Scoring Stage",
            &[
                (TYPE, HALL),
                (SIZE, 0.65),
                (DECAY, 1.8),
                (PRE_DELAY, 15.0),
                (DAMPING, 7_000.0),
                (BASS, 1.1),
                (BALANCE, 0.4),
                (LOW_CUT, 40.0),
                (MIX, 0.22),
            ],
        ),
        preset(
            "Piano Hall",
            &[
                (TYPE, HALL),
                (SIZE, 0.6),
                (DECAY, 1.9),
                (PRE_DELAY, 12.0),
                (DAMPING, 8_000.0),
                (BASS, 1.0),
                (WIDTH, 1.2),
                (LOW_CUT, 50.0),
                (MIX, 0.2),
            ],
        ),
        // The tail ducks under the vocal and blooms between phrases.
        preset(
            "Ducked Vocal Hall",
            &[
                (TYPE, HALL),
                (SIZE, 0.7),
                (DECAY, 2.4),
                (PRE_DELAY, 40.0),
                (DAMPING, 7_000.0),
                (BASS, 0.9),
                (MODULATION, 0.3),
                (BALANCE, 0.7),
                (LOW_CUT, 180.0),
                (DUCKING, 0.6),
                (MIX, 0.25),
            ],
        ),
        preset(
            "Cathedral",
            &[
                (TYPE, HALL),
                (SIZE, 1.0),
                (DECAY, 7.0),
                (PRE_DELAY, 45.0),
                (DAMPING, 4_000.0),
                (BASS, 1.4),
                (DIFFUSION, 0.85),
                (MODULATION, 0.3),
                (MOD_RATE, 0.3),
                (BALANCE, 0.6),
                (LOW_CUT, 30.0),
                (MIX, 0.3),
            ],
        ),
        preset(
            "Ambient Wash",
            &[
                (TYPE, HALL),
                (SIZE, 1.0),
                (DECAY, 12.0),
                (PRE_DELAY, 80.0),
                (DAMPING, 3_500.0),
                (BASS, 1.2),
                (DIFFUSION, 0.9),
                (MODULATION, 0.7),
                (MOD_RATE, 0.25),
                (BALANCE, 0.9),
                (WIDTH, 1.4),
                (LOW_CUT, 120.0),
                (MIX, 0.4),
            ],
        ),
        // Mostly early reflections: space without a tail.
        preset(
            "Drum Ambience",
            &[
                (TYPE, AMBIENCE),
                (SIZE, 0.4),
                (DECAY, 0.35),
                (PRE_DELAY, 0.0),
                (DAMPING, 10_000.0),
                (BALANCE, 0.25),
                (LOW_CUT, 100.0),
                (MIX, 0.25),
            ],
        ),
        preset(
            "Dialogue Room",
            &[
                (TYPE, AMBIENCE),
                (SIZE, 0.3),
                (DECAY, 0.4),
                (PRE_DELAY, 0.0),
                (DAMPING, 6_000.0),
                (BASS, 0.8),
                (BALANCE, 0.35),
                (LOW_CUT, 150.0),
                (MIX, 0.12),
            ],
        ),
    ]
}

pub(super) fn modulation() -> Vec<FactoryPreset> {
    use crate::devices::modulation::id::*;
    const CHORUS: f64 = 0.0;
    const ENSEMBLE: f64 = 1.0;
    const FLANGER: f64 = 2.0;
    const PHASER: f64 = 3.0;
    const VIBRATO: f64 = 4.0;
    const TRIANGLE: f64 = 1.0;
    const DRIFT: f64 = 5.0;
    // Divisions (crate::dsp::lfo::DIVISIONS).
    const D2: f64 = 14.0;
    const D1: f64 = 17.0;
    vec![
        preset(
            "Classic Chorus",
            &[
                (MODE, CHORUS),
                (RATE, 0.8),
                (DEPTH, 0.35),
                (DELAY, 7.0),
                (VOICES, 1.0),
                (SPREAD, 90.0),
                (MIX, 0.5),
            ],
        ),
        preset(
            "Lush Chorus",
            &[
                (MODE, CHORUS),
                (RATE, 0.4),
                (DEPTH, 0.5),
                (DELAY, 12.0),
                (VOICES, 3.0),
                (SPREAD, 120.0),
                (WIDTH, 1.3),
                (MIX, 0.5),
            ],
        ),
        // Slow, shallow and opposite on each side: width, not wobble.
        preset(
            "Subtle Widener",
            &[
                (MODE, CHORUS),
                (RATE, 0.3),
                (DEPTH, 0.15),
                (DELAY, 9.0),
                (VOICES, 1.0),
                (SPREAD, 180.0),
                (WIDTH, 1.4),
                (MIX, 0.4),
            ],
        ),
        preset(
            "String Ensemble",
            &[
                (MODE, ENSEMBLE),
                (RATE, 0.6),
                (DEPTH, 0.5),
                (DELAY, 10.0),
                (SPREAD, 120.0),
                (MIX, 0.6),
            ],
        ),
        preset(
            "Jet Flanger",
            &[
                (MODE, FLANGER),
                (RATE, 0.12),
                (DEPTH, 0.9),
                (FEEDBACK, 0.7),
                (DELAY, 5.0),
                (SPREAD, 30.0),
                (MIX, 0.5),
            ],
        ),
        preset(
            "Stereo Flanger",
            &[
                (MODE, FLANGER),
                (RATE, 0.25),
                (DEPTH, 0.6),
                (FEEDBACK, 0.3),
                (DELAY, 3.0),
                (SPREAD, 90.0),
                (MIX, 0.4),
            ],
        ),
        // Negative feedback: hollow notches instead of a ring.
        preset(
            "Hollow Flanger",
            &[
                (MODE, FLANGER),
                (RATE, 0.2),
                (DEPTH, 0.8),
                (FEEDBACK, -0.6),
                (DELAY, 4.0),
                (SPREAD, 60.0),
                (MIX, 0.5),
            ],
        ),
        preset(
            "Bar-Synced Flanger",
            &[
                (MODE, FLANGER),
                (SYNC, 1.0),
                (DIVISION, D1),
                (SHAPE, TRIANGLE),
                (DEPTH, 0.8),
                (FEEDBACK, 0.55),
                (DELAY, 6.0),
                (SPREAD, 45.0),
                (MIX, 0.5),
            ],
        ),
        // Four stages: the little orange pedal's swirl.
        preset(
            "Vintage 4-Stage Phaser",
            &[
                (MODE, PHASER),
                (STAGES, 1.0),
                (RATE, 0.5),
                (DEPTH, 0.7),
                (FEEDBACK, 0.3),
                (CENTER, 700.0),
                (SPREAD, 0.0),
                (MIX, 0.5),
            ],
        ),
        preset(
            "Deep 12-Stage Phaser",
            &[
                (MODE, PHASER),
                (STAGES, 5.0),
                (RATE, 0.2),
                (DEPTH, 0.8),
                (FEEDBACK, 0.5),
                (CENTER, 1_000.0),
                (SPREAD, 90.0),
                (MIX, 0.5),
            ],
        ),
        preset(
            "Half-Note Phaser",
            &[
                (MODE, PHASER),
                (SYNC, 1.0),
                (DIVISION, D2),
                (STAGES, 3.0),
                (DEPTH, 0.7),
                (FEEDBACK, 0.4),
                (CENTER, 900.0),
                (SPREAD, 90.0),
                (MIX, 0.5),
            ],
        ),
        // A spinning horn's Doppler: fast and wide.
        preset(
            "Rotary Speaker (Fast)",
            &[
                (MODE, CHORUS),
                (RATE, 6.5),
                (DEPTH, 0.2),
                (DELAY, 3.0),
                (VOICES, 0.0),
                (SPREAD, 180.0),
                (MIX, 0.5),
            ],
        ),
        preset(
            "Gentle Vibrato",
            &[
                (MODE, VIBRATO),
                (RATE, 5.5),
                (DEPTH, 0.3),
                (DELAY, 3.0),
                (SPREAD, 0.0),
                (MIX, 1.0),
            ],
        ),
        // Slow random drift: a tape machine's warble.
        preset(
            "Tape Warble",
            &[
                (MODE, VIBRATO),
                (SHAPE, DRIFT),
                (RATE, 0.8),
                (DEPTH, 0.25),
                (DELAY, 5.0),
                (SPREAD, 0.0),
                (MIX, 1.0),
            ],
        ),
    ]
}

pub(super) fn utility() -> Vec<FactoryPreset> {
    use crate::devices::utility::id::*;
    const LEFT: f64 = 1.0;
    const RIGHT: f64 = 2.0;
    const SWAP: f64 = 3.0;
    const MID: f64 = 4.0;
    const SIDE: f64 = 5.0;
    vec![
        preset("Mono", &[(WIDTH, 0.0)]),
        // Below 120 Hz in the middle, the stereo image above untouched.
        preset(
            "Mono Bass (120 Hz)",
            &[(MONO_BASS, 1.0), (BASS_FREQ, 120.0)],
        ),
        // A cutting lathe wants the lows mono.
        preset(
            "Vinyl Mono Bass (150 Hz)",
            &[(MONO_BASS, 1.0), (BASS_FREQ, 150.0)],
        ),
        preset(
            "Wider (130 %)",
            &[(WIDTH, 1.3), (MONO_BASS, 1.0), (BASS_FREQ, 100.0)],
        ),
        preset(
            "Extra Wide (160 %)",
            &[(WIDTH, 1.6), (MONO_BASS, 1.0), (BASS_FREQ, 150.0)],
        ),
        preset("Narrower (70 %)", &[(WIDTH, 0.7)]),
        preset("Swap Left and Right", &[(CHANNELS, SWAP)]),
        preset("Left Channel on Both", &[(CHANNELS, LEFT)]),
        preset("Right Channel on Both", &[(CHANNELS, RIGHT)]),
        preset("Mid Only", &[(CHANNELS, MID)]),
        preset("Side Only", &[(CHANNELS, SIDE)]),
        preset("Polarity Inverted", &[(INVERT_L, 1.0), (INVERT_R, 1.0)]),
        preset("Pad −6 dB", &[(GAIN, -6.0)]),
        preset("DC Filter", &[(DC, 1.0)]),
    ]
}

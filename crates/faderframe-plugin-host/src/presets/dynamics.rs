//! Compressor, limiter, gate and de-esser presets.

use super::{FactoryPreset, preset};

pub(super) fn compressor() -> Vec<FactoryPreset> {
    use crate::devices::compressor::id::*;
    const CLEAN: f64 = 0.0;
    const PUNCH: f64 = 1.0;
    const OPTO: f64 = 2.0;
    const VINTAGE: f64 = 3.0;
    const BUS: f64 = 4.0;
    vec![
        // An optical leveller: slow, programme dependent, smooth.
        preset(
            "Vocal Leveler",
            &[
                (STYLE, OPTO),
                (THRESHOLD, -24.0),
                (RATIO, 3.0),
                (ATTACK, 10.0),
                (RELEASE, 250.0),
                (KNEE, 6.0),
                (MAKEUP, 4.0),
                (SC_LOW, 80.0),
                (COLOR, 0.1),
            ],
        ),
        // The FET at 4:1: fast, a little bite, the voice in front.
        preset(
            "Vocal Upfront",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -18.0),
                (RATIO, 4.0),
                (ATTACK, 2.0),
                (RELEASE, 70.0),
                (KNEE, 0.0),
                (MAKEUP, 5.0),
                (SC_LOW, 100.0),
                (COLOR, 0.25),
            ],
        ),
        // A bus compressor at 2:1, 30 ms, auto release; the sidechain's low
        // cut keeps the kick from pumping the mix.
        preset(
            "Mix Bus Glue",
            &[
                (STYLE, BUS),
                (THRESHOLD, -14.0),
                (RATIO, 2.0),
                (ATTACK, 30.0),
                (RELEASE, 200.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 2.0),
                (MAKEUP, 1.5),
                (SC_LOW, 60.0),
            ],
        ),
        preset(
            "Drum Bus Punch",
            &[
                (STYLE, PUNCH),
                (THRESHOLD, -20.0),
                (RATIO, 4.0),
                (ATTACK, 10.0),
                (RELEASE, 80.0),
                (KNEE, 3.0),
                (MAKEUP, 3.5),
                (SC_LOW, 50.0),
            ],
        ),
        // "New York" parallel compression: crushed, mixed under the dry.
        preset(
            "Parallel Smash",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -32.0),
                (RATIO, 20.0),
                (ATTACK, 1.0),
                (RELEASE, 60.0),
                (KNEE, 0.0),
                (MAKEUP, 12.0),
                (MIX, 0.35),
                (COLOR, 0.5),
            ],
        ),
        // A slow attack lets the beater's click through before it clamps.
        preset(
            "Kick Snap",
            &[
                (STYLE, PUNCH),
                (THRESHOLD, -16.0),
                (RATIO, 4.0),
                (ATTACK, 25.0),
                (RELEASE, 50.0),
                (KNEE, 2.0),
                (MAKEUP, 3.0),
            ],
        ),
        preset(
            "Snare Crack",
            &[
                (STYLE, PUNCH),
                (THRESHOLD, -20.0),
                (RATIO, 5.0),
                (ATTACK, 6.0),
                (RELEASE, 90.0),
                (KNEE, 2.0),
                (MAKEUP, 4.0),
                (COLOR, 0.1),
            ],
        ),
        preset(
            "Bass Even",
            &[
                (STYLE, OPTO),
                (THRESHOLD, -26.0),
                (RATIO, 4.0),
                (ATTACK, 15.0),
                (RELEASE, 200.0),
                (KNEE, 4.0),
                (MAKEUP, 5.0),
            ],
        ),
        preset(
            "Acoustic Guitar Smooth",
            &[
                (STYLE, CLEAN),
                (DETECTOR, 1.0),
                (THRESHOLD, -24.0),
                (RATIO, 2.5),
                (ATTACK, 15.0),
                (RELEASE, 160.0),
                (KNEE, 10.0),
                (MAKEUP, 3.0),
            ],
        ),
        preset(
            "Piano Transparent",
            &[
                (STYLE, CLEAN),
                (DETECTOR, 1.0),
                (THRESHOLD, -20.0),
                (RATIO, 1.8),
                (ATTACK, 25.0),
                (RELEASE, 250.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 12.0),
                (MAKEUP, 2.0),
            ],
        ),
        preset(
            "Room Mic Crush",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -36.0),
                (RATIO, 20.0),
                (ATTACK, 0.5),
                (RELEASE, 350.0),
                (MAKEUP, 14.0),
                (COLOR, 0.45),
            ],
        ),
        // Keyed from a kick on the sidechain: the classic pump, held to
        // 15 dB at most.
        preset(
            "Sidechain Pump",
            &[
                (STYLE, CLEAN),
                (EXTERNAL, 1.0),
                (THRESHOLD, -30.0),
                (RATIO, 10.0),
                (ATTACK, 1.0),
                (RELEASE, 160.0),
                (KNEE, 3.0),
                (RANGE, 15.0),
            ],
        ),
        // Lookahead and a capped range: only the spikes come down.
        preset(
            "Peak Tamer",
            &[
                (STYLE, CLEAN),
                (THRESHOLD, -12.0),
                (RATIO, 8.0),
                (ATTACK, 0.1),
                (RELEASE, 40.0),
                (KNEE, 3.0),
                (LOOKAHEAD, 2.0),
                (RANGE, 8.0),
            ],
        ),
        preset(
            "Clean Guitar Sustain",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -30.0),
                (RATIO, 8.0),
                (ATTACK, 1.0),
                (RELEASE, 300.0),
                (KNEE, 4.0),
                (MAKEUP, 9.0),
                (COLOR, 0.15),
            ],
        ),
    ]
}

pub(super) fn limiter() -> Vec<FactoryPreset> {
    use crate::devices::limiter::id::*;
    const PUNCHY: f64 = 1.0;
    const AGGRESSIVE: f64 = 2.0;
    vec![
        // Catches the odd over and nothing else.
        preset(
            "Safety Net",
            &[
                (GAIN, 0.0),
                (CEILING, -0.5),
                (RELEASE, 50.0),
                (LOOKAHEAD, 2.0),
            ],
        ),
        // True peaks under −1 dBTP for lossy encoding.
        preset(
            "Streaming Master",
            &[
                (GAIN, 4.0),
                (CEILING, -1.0),
                (RELEASE, 100.0),
                (LOOKAHEAD, 5.0),
            ],
        ),
        preset(
            "Broadcast Master (−2 dBTP)",
            &[
                (GAIN, 2.0),
                (CEILING, -2.0),
                (RELEASE, 150.0),
                (LOOKAHEAD, 6.0),
            ],
        ),
        preset(
            "CD Master (−0.3 dBFS)",
            &[
                (GAIN, 5.0),
                (CEILING, -0.3),
                (TRUE_PEAK, 0.0),
                (STYLE, PUNCHY),
                (RELEASE, 90.0),
                (LOOKAHEAD, 4.0),
            ],
        ),
        // A long release and lookahead: level without audible pumping.
        preset(
            "Acoustic & Classical",
            &[
                (GAIN, 2.0),
                (CEILING, -1.0),
                (RELEASE, 400.0),
                (LOOKAHEAD, 10.0),
            ],
        ),
        preset(
            "Punchy Pop Master",
            &[
                (GAIN, 6.0),
                (CEILING, -1.0),
                (STYLE, PUNCHY),
                (RELEASE, 80.0),
                (LOOKAHEAD, 4.0),
            ],
        ),
        preset(
            "Loud Modern Master",
            &[
                (GAIN, 9.0),
                (CEILING, -1.0),
                (STYLE, PUNCHY),
                (RELEASE, 50.0),
                (LOOKAHEAD, 3.0),
            ],
        ),
        preset(
            "Aggressive EDM",
            &[
                (GAIN, 11.0),
                (CEILING, -1.0),
                (STYLE, AGGRESSIVE),
                (RELEASE, 30.0),
                (LOOKAHEAD, 1.5),
            ],
        ),
        // Short lookahead and release: the hits stay hits.
        preset(
            "Drum Bus Peaks",
            &[
                (GAIN, 4.0),
                (CEILING, -3.0),
                (STYLE, AGGRESSIVE),
                (RELEASE, 15.0),
                (AUTO_RELEASE, 0.0),
                (LOOKAHEAD, 1.0),
                (TRUE_PEAK, 0.0),
            ],
        ),
        // A long release so the gain does not follow the bass's waveform.
        preset(
            "Bass Level",
            &[
                (GAIN, 5.0),
                (CEILING, -3.0),
                (RELEASE, 300.0),
                (LOOKAHEAD, 10.0),
                (TRUE_PEAK, 0.0),
            ],
        ),
        preset(
            "Vocal Peak Catcher",
            &[
                (GAIN, 2.0),
                (CEILING, -6.0),
                (RELEASE, 60.0),
                (LOOKAHEAD, 3.0),
                (TRUE_PEAK, 0.0),
            ],
        ),
        // Partly unlinked: each side limited more on its own, denser.
        preset(
            "Dense Master (Partly Unlinked)",
            &[
                (GAIN, 6.0),
                (CEILING, -1.0),
                (LINK, 0.4),
                (RELEASE, 120.0),
                (LOOKAHEAD, 4.0),
            ],
        ),
        // Hear what the limiting does to the sound, not the extra level.
        preset(
            "Compare at Unity Gain",
            &[
                (GAIN, 6.0),
                (CEILING, -1.0),
                (UNITY, 1.0),
                (RELEASE, 100.0),
                (LOOKAHEAD, 5.0),
            ],
        ),
    ]
}

pub(super) fn gate() -> Vec<FactoryPreset> {
    use crate::devices::gate::id::*;
    const EXPANDER: f64 = 1.0;
    const DUCKER: f64 = 2.0;
    vec![
        // Keyed on the kick's low end, so snare and hats leave it shut.
        preset(
            "Kick Gate",
            &[
                (THRESHOLD, -30.0),
                (RANGE, -40.0),
                (ATTACK, 0.05),
                (HOLD, 40.0),
                (RELEASE, 120.0),
                (HYSTERESIS, 6.0),
                (LOOKAHEAD, 1.0),
                (SC_LOW, 30.0),
                (SC_HIGH, 700.0),
            ],
        ),
        preset(
            "Snare Gate",
            &[
                (THRESHOLD, -28.0),
                (RANGE, -24.0),
                (ATTACK, 0.1),
                (HOLD, 60.0),
                (RELEASE, 180.0),
                (HYSTERESIS, 6.0),
                (LOOKAHEAD, 1.0),
                (SC_LOW, 200.0),
                (SC_HIGH, 8_000.0),
            ],
        ),
        // Long hold and release: the toms ring out.
        preset(
            "Tom Gate",
            &[
                (THRESHOLD, -32.0),
                (RANGE, -30.0),
                (ATTACK, 0.2),
                (HOLD, 120.0),
                (RELEASE, 450.0),
                (HYSTERESIS, 6.0),
                (LOOKAHEAD, 1.0),
                (SC_LOW, 60.0),
                (SC_HIGH, 5_000.0),
            ],
        ),
        preset(
            "Hi-Hat Tidy",
            &[
                (THRESHOLD, -36.0),
                (RANGE, -18.0),
                (ATTACK, 0.05),
                (HOLD, 20.0),
                (RELEASE, 80.0),
                (HYSTERESIS, 4.0),
                (SC_LOW, 2_000.0),
            ],
        ),
        preset(
            "High-Gain Guitar Gate",
            &[
                (THRESHOLD, -48.0),
                (RANGE, -80.0),
                (ATTACK, 0.5),
                (HOLD, 30.0),
                (RELEASE, 120.0),
                (HYSTERESIS, 8.0),
            ],
        ),
        preset(
            "Vocal Breath Reducer",
            &[
                (MODE, EXPANDER),
                (THRESHOLD, -45.0),
                (RATIO, 2.0),
                (RANGE, -12.0),
                (ATTACK, 2.0),
                (HOLD, 50.0),
                (RELEASE, 250.0),
                (SC_LOW, 100.0),
            ],
        ),
        preset(
            "Amp Hiss Expander",
            &[
                (MODE, EXPANDER),
                (THRESHOLD, -55.0),
                (RATIO, 3.0),
                (RANGE, -24.0),
                (ATTACK, 1.0),
                (HOLD, 30.0),
                (RELEASE, 200.0),
            ],
        ),
        preset(
            "Dialogue Cleanup",
            &[
                (MODE, EXPANDER),
                (THRESHOLD, -50.0),
                (RATIO, 1.8),
                (RANGE, -10.0),
                (ATTACK, 5.0),
                (HOLD, 100.0),
                (RELEASE, 400.0),
                (SC_LOW, 120.0),
                (SC_HIGH, 8_000.0),
            ],
        ),
        preset(
            "Mic Bleed Softener",
            &[
                (MODE, EXPANDER),
                (THRESHOLD, -40.0),
                (RATIO, 1.5),
                (RANGE, -8.0),
                (ATTACK, 1.0),
                (HOLD, 50.0),
                (RELEASE, 200.0),
                (LOOKAHEAD, 2.0),
            ],
        ),
        // On a snare's reverb, keyed by the snare: a big room cut short.
        preset(
            "80s Gated Reverb",
            &[
                (THRESHOLD, -30.0),
                (RANGE, -90.0),
                (ATTACK, 0.5),
                (HOLD, 250.0),
                (RELEASE, 40.0),
                (HYSTERESIS, 3.0),
            ],
        ),
        // Keyed by a rhythm on the sidechain: a pad chopped to it.
        preset(
            "Keyed Stutter Gate",
            &[
                (EXTERNAL, 1.0),
                (THRESHOLD, -24.0),
                (RANGE, -90.0),
                (ATTACK, 0.01),
                (HOLD, 0.0),
                (RELEASE, 8.0),
                (HYSTERESIS, 2.0),
            ],
        ),
        // The music goes down while the voice on the sidechain speaks.
        preset(
            "Voice-Over Ducker",
            &[
                (MODE, DUCKER),
                (EXTERNAL, 1.0),
                (THRESHOLD, -36.0),
                (RANGE, -12.0),
                (ATTACK, 20.0),
                (HOLD, 300.0),
                (RELEASE, 600.0),
                (SC_LOW, 100.0),
                (SC_HIGH, 8_000.0),
            ],
        ),
        preset(
            "Kick Ducks Bass",
            &[
                (MODE, DUCKER),
                (EXTERNAL, 1.0),
                (THRESHOLD, -30.0),
                (RANGE, -9.0),
                (ATTACK, 1.0),
                (HOLD, 20.0),
                (RELEASE, 120.0),
                (SC_HIGH, 500.0),
            ],
        ),
    ]
}

pub(super) fn deesser() -> Vec<FactoryPreset> {
    use crate::devices::deesser::id::*;
    const BAND: f64 = 1.0;
    const WIDE: f64 = 1.0;
    const ABSOLUTE: f64 = 1.0;
    vec![
        preset(
            "Female Vocal",
            &[
                (FREQUENCY, 7_500.0),
                (THRESHOLD, -30.0),
                (RANGE, 8.0),
                (ATTACK, 0.5),
                (RELEASE, 60.0),
                (LOOKAHEAD, 1.0),
            ],
        ),
        preset(
            "Male Vocal",
            &[
                (FREQUENCY, 5_500.0),
                (THRESHOLD, -30.0),
                (RANGE, 8.0),
                (ATTACK, 0.5),
                (RELEASE, 70.0),
                (LOOKAHEAD, 1.0),
            ],
        ),
        preset(
            "Gentle Touch",
            &[
                (FREQUENCY, 6_500.0),
                (THRESHOLD, -26.0),
                (RANGE, 4.0),
                (RELEASE, 90.0),
                (LOOKAHEAD, 1.5),
            ],
        ),
        preset(
            "Strong Sibilance",
            &[
                (FREQUENCY, 6_500.0),
                (THRESHOLD, -36.0),
                (RANGE, 14.0),
                (ATTACK, 0.3),
                (RELEASE, 50.0),
                (LOOKAHEAD, 2.0),
            ],
        ),
        // Only the band round 6 kHz moves: the air above stays.
        preset(
            "Narrow S Band (6 kHz)",
            &[
                (SHAPE, BAND),
                (FREQUENCY, 6_000.0),
                (Q, 3.0),
                (THRESHOLD, -32.0),
                (RANGE, 10.0),
            ],
        ),
        preset(
            "Keep the Air (8 kHz Band)",
            &[
                (SHAPE, BAND),
                (FREQUENCY, 8_000.0),
                (Q, 2.0),
                (THRESHOLD, -30.0),
                (RANGE, 8.0),
            ],
        ),
        preset(
            "Voice-Over (Wide)",
            &[
                (MODE, WIDE),
                (FREQUENCY, 5_000.0),
                (THRESHOLD, -30.0),
                (RANGE, 6.0),
                (RELEASE, 80.0),
            ],
        ),
        preset(
            "Podcast Speech",
            &[
                (FREQUENCY, 5_200.0),
                (THRESHOLD, -32.0),
                (RANGE, 8.0),
                (RELEASE, 80.0),
                (LOOKAHEAD, 1.0),
            ],
        ),
        preset(
            "Rap Vocal",
            &[
                (FREQUENCY, 6_000.0),
                (THRESHOLD, -32.0),
                (RANGE, 10.0),
                (ATTACK, 0.2),
                (RELEASE, 45.0),
                (LOOKAHEAD, 1.5),
            ],
        ),
        // Stacked voices pile their esses up.
        preset(
            "Backing Vocal Stack",
            &[
                (FREQUENCY, 6_800.0),
                (THRESHOLD, -34.0),
                (RANGE, 12.0),
                (RELEASE, 60.0),
            ],
        ),
        preset(
            "Harsh Cymbals",
            &[
                (FREQUENCY, 9_000.0),
                (DETECTION, ABSOLUTE),
                (THRESHOLD, -28.0),
                (RANGE, 6.0),
                (ATTACK, 1.0),
                (RELEASE, 120.0),
            ],
        ),
        preset(
            "Mix Bus Polish",
            &[
                (FREQUENCY, 8_000.0),
                (THRESHOLD, -24.0),
                (RANGE, 3.0),
                (RELEASE, 100.0),
                (LOOKAHEAD, 1.0),
            ],
        ),
    ]
}

/// The 76 Compressor's: what the FET limiting amplifier is reached for —
/// the voice in front, bass and drums with bite, the all-buttons smash,
/// peaks caught, and its line amplifier alone. Attack and release run 1
/// (slowest) to 7 (fastest), as on the hardware; Output keeps each level
/// with the dry signal.
pub(super) fn compressor_76() -> Vec<FactoryPreset> {
    use crate::devices::fet76::id::*;
    // The ratio buttons (bits: 4, 8, 12, 20 to one).
    const R4: f64 = 1.0;
    const R8: f64 = 2.0;
    const R12: f64 = 4.0;
    const R20: f64 = 8.0;
    const ALL: f64 = 15.0;
    vec![
        // 4:1, a medium attack and a quick release: the voice sits in
        // front without losing its consonants.
        preset(
            "Vocal Upfront",
            &[
                (INPUT, 14.0),
                (OUTPUT, 4.1),
                (ATTACK, 3.0),
                (RELEASE, 6.0),
                (RATIO, R4),
            ],
        ),
        // A slower attack and release, a little less: levelling, not
        // grabbing.
        preset(
            "Vocal Gentle",
            &[
                (INPUT, 9.0),
                (OUTPUT, 6.2),
                (ATTACK, 2.0),
                (RELEASE, 3.0),
                (RATIO, R4),
            ],
        ),
        // 8:1, fast both ways and pushed: the hard, edgy rap and rock voice.
        preset(
            "Vocal Grit",
            &[
                (INPUT, 22.0),
                (OUTPUT, 3.9),
                (ATTACK, 5.0),
                (RELEASE, 7.0),
                (RATIO, R8),
            ],
        ),
        // 4:1, slow attack, slow release: the notes even, no distortion
        // from a release riding the waveform.
        preset(
            "Bass Even",
            &[
                (INPUT, 12.0),
                (OUTPUT, 6.0),
                (ATTACK, 2.0),
                (RELEASE, 2.0),
                (RATIO, R4),
            ],
        ),
        // 8:1, fast: the release rides the low notes into grit.
        preset(
            "Bass Growl",
            &[
                (INPUT, 20.0),
                (OUTPUT, 4.2),
                (ATTACK, 6.0),
                (RELEASE, 7.0),
                (RATIO, R8),
            ],
        ),
        // The slowest attack lets the beater through, the fast release
        // brings the body up.
        preset(
            "Kick Snap",
            &[
                (INPUT, 14.0),
                (OUTPUT, 3.8),
                (ATTACK, 1.0),
                (RELEASE, 7.0),
                (RATIO, R4),
            ],
        ),
        // 8:1 with a quick release: the crack first, then the ring up.
        preset(
            "Snare Crack",
            &[
                (INPUT, 16.0),
                (OUTPUT, 4.9),
                (ATTACK, 2.0),
                (RELEASE, 6.0),
                (RATIO, R8),
            ],
        ),
        // Every button in: the ratio bends, the attack overshoots and the
        // room explodes — the classic smash.
        preset(
            "All Buttons Room",
            &[
                (INPUT, 24.0),
                (OUTPUT, 13.7),
                (ATTACK, 7.0),
                (RELEASE, 7.0),
                (RATIO, ALL),
            ],
        ),
        // The same smash under the dry drums (parallel, inside the unit).
        preset(
            "All Buttons Parallel",
            &[
                (INPUT, 24.0),
                (OUTPUT, 5.0),
                (ATTACK, 7.0),
                (RELEASE, 7.0),
                (RATIO, ALL),
                (MIX, 0.4),
            ],
        ),
        // 4:1 on the drum bus, fast release; the sidechain's high-pass at
        // 120 Hz keeps the kick from pumping the cymbals.
        preset(
            "Drum Bus Crunch",
            &[
                (INPUT, 12.0),
                (OUTPUT, 3.7),
                (ATTACK, 3.0),
                (RELEASE, 7.0),
                (RATIO, R4),
                (SC_HPF, 2.0),
            ],
        ),
        // 4:1, medium: strums even, the picking still there.
        preset(
            "Acoustic Strum",
            &[
                (INPUT, 11.0),
                (OUTPUT, 5.5),
                (ATTACK, 3.0),
                (RELEASE, 4.0),
                (RATIO, R4),
                (SC_HPF, 1.0),
            ],
        ),
        // 12:1 with a slow release: sustain for leads and clean guitar.
        preset(
            "Guitar Sustain",
            &[
                (INPUT, 18.0),
                (OUTPUT, 5.2),
                (ATTACK, 4.0),
                (RELEASE, 3.0),
                (RATIO, R12),
            ],
        ),
        // 20:1, the fastest attack: only the peaks are caught.
        preset(
            "Peak Catcher",
            &[
                (INPUT, 8.0),
                (OUTPUT, 4.8),
                (ATTACK, 7.0),
                (RELEASE, 5.0),
                (RATIO, R20),
            ],
        ),
        // Attack off: no compression, the signal through the input and
        // output transformers and the line amplifier, driven.
        preset(
            "Line Amp Colour",
            &[(INPUT, 10.0), (OUTPUT, -7.0), (ATTACK, 0.0), (RATIO, R4)],
        ),
    ]
}

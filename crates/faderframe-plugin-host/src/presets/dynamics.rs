//! Compressor, limiter, gate, de-esser and channel strip presets.

use super::{FactoryPreset, preset};

pub(super) fn compressor() -> Vec<FactoryPreset> {
    use crate::devices::compressor::id::*;
    const CLEAN: f64 = 0.0;
    const PUNCH: f64 = 1.0;
    const OPTO: f64 = 2.0;
    const VINTAGE: f64 = 3.0;
    const BUS: f64 = 4.0;
    const MASTERING: f64 = 5.0;
    const PUMPING: f64 = 6.0;
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
        // The groups below use what the second revision added: hold, M/S,
        // upward compression, oversampled colour, delta, the Mastering and
        // Pumping styles. Levelled for programme peaking round −6 dB.
        // Vocals.
        preset(
            "Vocal Rider (Upward)",
            &[
                (STYLE, CLEAN),
                (DETECTOR, 1.0),
                (THRESHOLD, -28.0),
                (RATIO, 2.5),
                (ATTACK, 20.0),
                (RELEASE, 200.0),
                (KNEE, 10.0),
                (UPWARD, 4.0),
                (MAKEUP, -1.0),
                (SC_LOW, 80.0),
            ],
        )
        .in_group("Vocals"),
        // Two stages in series: the opto levels, the FET catches the rest.
        preset(
            "Vocal Serial 1 — Opto Level",
            &[
                (STYLE, OPTO),
                (THRESHOLD, -22.0),
                (RATIO, 2.5),
                (ATTACK, 15.0),
                (RELEASE, 400.0),
                (KNEE, 8.0),
                (MAKEUP, 3.0),
                (SC_LOW, 100.0),
            ],
        )
        .in_group("Vocals"),
        preset(
            "Vocal Serial 2 — FET Catch",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -16.0),
                (RATIO, 6.0),
                (ATTACK, 1.0),
                (RELEASE, 60.0),
                (KNEE, 2.0),
                (MAKEUP, 3.0),
                (COLOR, 0.2),
                (OVERSAMPLE, 1.0),
            ],
        )
        .in_group("Vocals"),
        preset(
            "Rap Vocal In Your Face",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -20.0),
                (RATIO, 8.0),
                (ATTACK, 0.5),
                (RELEASE, 80.0),
                (KNEE, 0.0),
                (MAKEUP, 7.0),
                (COLOR, 0.35),
                (OVERSAMPLE, 1.0),
                (SC_LOW, 120.0),
            ],
        )
        .in_group("Vocals"),
        preset(
            "Backing Vocals Tuck",
            &[
                (STYLE, BUS),
                (THRESHOLD, -24.0),
                (RATIO, 3.0),
                (ATTACK, 10.0),
                (RELEASE, 150.0),
                (AUTO_RELEASE, 1.0),
                (HOLD, 20.0),
                (KNEE, 6.0),
                (MAKEUP, 3.0),
            ],
        )
        .in_group("Vocals"),
        // Drums.
        preset(
            "Kick Hold & Punch",
            &[
                (STYLE, PUNCH),
                (THRESHOLD, -18.0),
                (RATIO, 6.0),
                (ATTACK, 20.0),
                (HOLD, 60.0),
                (RELEASE, 40.0),
                (KNEE, 2.0),
                (MAKEUP, 0.5),
            ],
        )
        .in_group("Drums"),
        preset(
            "Snare Pump",
            &[
                (STYLE, PUMPING),
                (THRESHOLD, -22.0),
                (RATIO, 6.0),
                (ATTACK, 8.0),
                (HOLD, 80.0),
                (RELEASE, 120.0),
                (KNEE, 2.0),
                (MAKEUP, 4.0),
            ],
        )
        .in_group("Drums"),
        // Mid and side on their own: the kick in the middle does not pump
        // the cymbals at the sides.
        preset(
            "Drum Bus Mid/Side Glue",
            &[
                (STYLE, BUS),
                (CHANNELS, 1.0),
                (LINK, 0.3),
                (THRESHOLD, -18.0),
                (RATIO, 3.0),
                (ATTACK, 20.0),
                (RELEASE, 120.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 4.0),
                (MAKEUP, 2.5),
            ],
        )
        .in_group("Drums"),
        preset(
            "Overheads Smooth",
            &[
                (STYLE, CLEAN),
                (DETECTOR, 1.0),
                (THRESHOLD, -26.0),
                (RATIO, 2.5),
                (ATTACK, 20.0),
                (RELEASE, 250.0),
                (KNEE, 8.0),
                (MAKEUP, 2.0),
                (SC_LOW, 150.0),
            ],
        )
        .in_group("Drums"),
        preset(
            "Room Mics Pump",
            &[
                (STYLE, PUMPING),
                (THRESHOLD, -30.0),
                (RATIO, 10.0),
                (ATTACK, 1.0),
                (HOLD, 100.0),
                (RELEASE, 200.0),
                (KNEE, 3.0),
                (MAKEUP, 10.0),
                (COLOR, 0.3),
                (MIX, 0.7),
            ],
        )
        .in_group("Drums"),
        // A slow attack passes the hit, the rest clamps: more transient.
        preset(
            "Drum Transient Shaper",
            &[
                (STYLE, PUNCH),
                (THRESHOLD, -14.0),
                (RATIO, 10.0),
                (ATTACK, 30.0),
                (RELEASE, 60.0),
                (KNEE, 0.0),
                (MAKEUP, 0.0),
            ],
        )
        .in_group("Drums"),
        // Bass.
        preset(
            "Bass Upfront FET",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -22.0),
                (RATIO, 4.0),
                (ATTACK, 3.0),
                (RELEASE, 150.0),
                (KNEE, 3.0),
                (MAKEUP, 5.0),
                (COLOR, 0.2),
                (OVERSAMPLE, 1.0),
            ],
        )
        .in_group("Bass"),
        preset(
            "Bass Constant (Upward)",
            &[
                (STYLE, CLEAN),
                (DETECTOR, 1.0),
                (THRESHOLD, -24.0),
                (RATIO, 4.0),
                (ATTACK, 10.0),
                (RELEASE, 150.0),
                (KNEE, 6.0),
                (UPWARD, 5.0),
                (MAKEUP, -1.0),
            ],
        )
        .in_group("Bass"),
        // Keyed by the lows only: the sub is held even, the top plays.
        preset(
            "Sub Bass Tamer",
            &[
                (STYLE, MASTERING),
                (THRESHOLD, -20.0),
                (RATIO, 3.0),
                (ATTACK, 30.0),
                (RELEASE, 400.0),
                (KNEE, 6.0),
                (SC_HIGH, 500.0),
                (MAKEUP, 2.0),
            ],
        )
        .in_group("Bass"),
        preset(
            "Slap Bass Peaks",
            &[
                (STYLE, PUNCH),
                (THRESHOLD, -12.0),
                (RATIO, 8.0),
                (ATTACK, 0.5),
                (RELEASE, 60.0),
                (KNEE, 2.0),
                (LOOKAHEAD, 1.0),
                (RANGE, 10.0),
                (MAKEUP, 1.0),
            ],
        )
        .in_group("Bass"),
        // Bus.
        preset(
            "Mix Bus Mid/Side",
            &[
                (STYLE, BUS),
                (CHANNELS, 1.0),
                (LINK, 0.5),
                (THRESHOLD, -16.0),
                (RATIO, 2.0),
                (ATTACK, 30.0),
                (RELEASE, 300.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 4.0),
                (MAKEUP, 1.5),
            ],
        )
        .in_group("Bus"),
        preset(
            "Mix Bus 2:1 Slow",
            &[
                (STYLE, MASTERING),
                (THRESHOLD, -18.0),
                (RATIO, 2.0),
                (ATTACK, 50.0),
                (RELEASE, 500.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 6.0),
                (MAKEUP, 1.0),
            ],
        )
        .in_group("Bus"),
        preset(
            "Guitar Bus Even",
            &[
                (STYLE, OPTO),
                (THRESHOLD, -22.0),
                (RATIO, 3.0),
                (ATTACK, 20.0),
                (RELEASE, 300.0),
                (KNEE, 6.0),
                (MAKEUP, 3.0),
            ],
        )
        .in_group("Bus"),
        preset(
            "Synth Bus Pump (Keyed)",
            &[
                (STYLE, PUMPING),
                (EXTERNAL, 1.0),
                (THRESHOLD, -30.0),
                (RATIO, 10.0),
                (ATTACK, 1.0),
                (HOLD, 40.0),
                (RELEASE, 200.0),
                (KNEE, 3.0),
                (RANGE, 12.0),
            ],
        )
        .in_group("Bus"),
        preset(
            "Pads Leveler (Upward)",
            &[
                (STYLE, MASTERING),
                (THRESHOLD, -26.0),
                (RATIO, 2.0),
                (ATTACK, 40.0),
                (RELEASE, 600.0),
                (KNEE, 10.0),
                (UPWARD, 6.0),
                (MAKEUP, 2.0),
            ],
        )
        .in_group("Bus"),
        // Master.
        preset(
            "Mastering Gentle 1.5:1",
            &[
                (STYLE, MASTERING),
                (THRESHOLD, -20.0),
                (RATIO, 1.5),
                (ATTACK, 30.0),
                (RELEASE, 400.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 12.0),
                (MAKEUP, 1.0),
            ],
        )
        .in_group("Master"),
        preset(
            "Mastering Mid/Side",
            &[
                (STYLE, MASTERING),
                (CHANNELS, 1.0),
                (LINK, 0.2),
                (THRESHOLD, -18.0),
                (RATIO, 1.8),
                (ATTACK, 30.0),
                (RELEASE, 500.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 10.0),
                (MAKEUP, 1.0),
            ],
        )
        .in_group("Master"),
        preset(
            "Mastering Upward Lift",
            &[
                (STYLE, MASTERING),
                (THRESHOLD, -24.0),
                (RATIO, 1.5),
                (ATTACK, 50.0),
                (RELEASE, 800.0),
                (KNEE, 12.0),
                (UPWARD, 4.0),
                (MAKEUP, 0.5),
            ],
        )
        .in_group("Master"),
        preset(
            "Mastering Colour (4× OS)",
            &[
                (STYLE, BUS),
                (THRESHOLD, -16.0),
                (RATIO, 2.0),
                (ATTACK, 30.0),
                (RELEASE, 300.0),
                (AUTO_RELEASE, 1.0),
                (KNEE, 6.0),
                (COLOR, 0.3),
                (OVERSAMPLE, 2.0),
                (MAKEUP, 1.5),
            ],
        )
        .in_group("Master"),
        // Creative.
        preset(
            "Delta — Hear What Goes",
            &[
                (STYLE, CLEAN),
                (DELTA, 1.0),
                (THRESHOLD, -24.0),
                (RATIO, 4.0),
                (ATTACK, 10.0),
                (RELEASE, 120.0),
            ],
        )
        .in_group("Creative"),
        preset(
            "Sidechain Pump (Held)",
            &[
                (STYLE, PUMPING),
                (EXTERNAL, 1.0),
                (THRESHOLD, -35.0),
                (RATIO, 20.0),
                (ATTACK, 0.5),
                (HOLD, 60.0),
                (RELEASE, 150.0),
                (KNEE, 0.0),
                (RANGE, 24.0),
            ],
        )
        .in_group("Creative"),
        preset(
            "Lo-Fi Crush (4× OS)",
            &[
                (STYLE, VINTAGE),
                (THRESHOLD, -40.0),
                (RATIO, 20.0),
                (ATTACK, 0.1),
                (RELEASE, 100.0),
                (KNEE, 0.0),
                (COLOR, 1.0),
                (OVERSAMPLE, 2.0),
                (MAKEUP, 16.0),
                (MIX, 0.8),
            ],
        )
        .in_group("Creative"),
        preset(
            "Breathing Pad",
            &[
                (STYLE, MASTERING),
                (THRESHOLD, -30.0),
                (RATIO, 100.0),
                (ATTACK, 100.0),
                (RELEASE, 1000.0),
                (KNEE, 12.0),
                (UPWARD, 12.0),
                (MAKEUP, 2.0),
            ],
        )
        .in_group("Creative"),
        preset(
            "Stutter Hold",
            &[
                (STYLE, PUMPING),
                (THRESHOLD, -24.0),
                (RATIO, 10.0),
                (ATTACK, 0.1),
                (HOLD, 300.0),
                (RELEASE, 10.0),
                (KNEE, 0.0),
                (MAKEUP, 4.0),
            ],
        )
        .in_group("Creative"),
    ]
}

pub(super) fn limiter() -> Vec<FactoryPreset> {
    use crate::devices::limiter::id::*;
    const PUNCHY: f64 = 1.0;
    const AGGRESSIVE: f64 = 2.0;
    const ALLROUND: f64 = 3.0;
    const BUS: f64 = 4.0;
    const SAFE: f64 = 5.0;
    const MODERN: f64 = 6.0;
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
        // The groups below use the later styles (Allround, Bus, Safe,
        // Modern), the output trim and the release link.
        // Vocals.
        preset(
            "Vocal Safety (−3 dB)",
            &[
                (GAIN, 0.0),
                (CEILING, -3.0),
                (STYLE, SAFE),
                (RELEASE, 80.0),
                (LOOKAHEAD, 4.0),
            ],
        )
        .in_group("Vocals"),
        preset(
            "Vocal Dense",
            &[
                (GAIN, 4.0),
                (CEILING, -2.0),
                (STYLE, ALLROUND),
                (RELEASE, 60.0),
                (LOOKAHEAD, 3.0),
            ],
        )
        .in_group("Vocals"),
        preset(
            "Vocal Modern Leveler",
            &[
                (GAIN, 6.0),
                (CEILING, -4.0),
                (STYLE, MODERN),
                (RELEASE, 40.0),
                (LOOKAHEAD, 2.0),
            ],
        )
        .in_group("Vocals"),
        // Drums.
        preset(
            "Drum Bus Allround",
            &[
                (GAIN, 5.0),
                (CEILING, -2.0),
                (STYLE, ALLROUND),
                (RELEASE, 30.0),
                (LOOKAHEAD, 2.0),
                (TRUE_PEAK, 0.0),
            ],
        )
        .in_group("Drums"),
        preset(
            "Drum Bus Modern Slam",
            &[
                (GAIN, 10.0),
                (CEILING, -1.0),
                (STYLE, MODERN),
                (RELEASE, 20.0),
                (LOOKAHEAD, 1.5),
            ],
        )
        .in_group("Drums"),
        // The shortest window: the kick's top is shaved, not shaped.
        preset(
            "Kick Peak Shave",
            &[
                (GAIN, 3.0),
                (CEILING, -3.0),
                (STYLE, AGGRESSIVE),
                (RELEASE, 10.0),
                (AUTO_RELEASE, 0.0),
                (LOOKAHEAD, 0.5),
                (TRUE_PEAK, 0.0),
            ],
        )
        .in_group("Drums"),
        preset(
            "Overheads Glass",
            &[
                (GAIN, 2.0),
                (CEILING, -3.0),
                (STYLE, SAFE),
                (RELEASE, 200.0),
                (LOOKAHEAD, 8.0),
            ],
        )
        .in_group("Drums"),
        // Bass.
        preset(
            "Bass Modern Tight",
            &[
                (GAIN, 6.0),
                (CEILING, -3.0),
                (STYLE, MODERN),
                (RELEASE, 150.0),
                (LOOKAHEAD, 6.0),
                (TRUE_PEAK, 0.0),
                (RELEASE_LINK, 1.0),
            ],
        )
        .in_group("Bass"),
        preset(
            "Sub Safe",
            &[
                (GAIN, 3.0),
                (CEILING, -2.0),
                (STYLE, SAFE),
                (RELEASE, 400.0),
                (LOOKAHEAD, 10.0),
            ],
        )
        .in_group("Bass"),
        // Bus.
        preset(
            "Bus Glue Limiter",
            &[
                (GAIN, 3.0),
                (CEILING, -2.0),
                (STYLE, BUS),
                (RELEASE, 150.0),
                (LOOKAHEAD, 5.0),
            ],
        )
        .in_group("Bus"),
        preset(
            "Stem Export Safe (−1 dBTP)",
            &[
                (GAIN, 0.0),
                (CEILING, -1.0),
                (STYLE, SAFE),
                (RELEASE, 100.0),
                (LOOKAHEAD, 5.0),
            ],
        )
        .in_group("Bus"),
        preset(
            "Instrument Bus Allround",
            &[
                (GAIN, 4.0),
                (CEILING, -2.0),
                (STYLE, ALLROUND),
                (RELEASE, 80.0),
                (LOOKAHEAD, 4.0),
            ],
        )
        .in_group("Bus"),
        // Master.
        preset(
            "Streaming Safe (−14 LUFS)",
            &[
                (GAIN, 5.0),
                (CEILING, -1.0),
                (STYLE, SAFE),
                (RELEASE, 120.0),
                (LOOKAHEAD, 6.0),
            ],
        )
        .in_group("Master"),
        preset(
            "Apple Digital Masters (−1 dBTP)",
            &[
                (GAIN, 3.0),
                (CEILING, -1.0),
                (STYLE, SAFE),
                (RELEASE, 100.0),
                (LOOKAHEAD, 5.0),
            ],
        )
        .in_group("Master"),
        preset(
            "Loud Master (−9 LUFS)",
            &[
                (GAIN, 10.0),
                (CEILING, -1.0),
                (STYLE, MODERN),
                (RELEASE, 60.0),
                (LOOKAHEAD, 3.0),
            ],
        )
        .in_group("Master"),
        preset(
            "Club Master",
            &[
                (GAIN, 8.0),
                (CEILING, -0.5),
                (STYLE, BUS),
                (RELEASE, 50.0),
                (LOOKAHEAD, 3.0),
                (LINK, 0.5),
                (RELEASE_LINK, 0.7),
            ],
        )
        .in_group("Master"),
        preset(
            "Vinyl Pre-Master (−3 dB)",
            &[
                (GAIN, 2.0),
                (CEILING, -3.0),
                (STYLE, SAFE),
                (RELEASE, 300.0),
                (LOOKAHEAD, 10.0),
            ],
        )
        .in_group("Master"),
        // Limited loud, heard 6 dB down: compare against the unlimited
        // mix at one level.
        preset(
            "Reference Level (Output −6 dB)",
            &[
                (GAIN, 6.0),
                (CEILING, -1.0),
                (STYLE, ALLROUND),
                (RELEASE, 100.0),
                (LOOKAHEAD, 4.0),
                (OUTPUT, -6.0),
            ],
        )
        .in_group("Master"),
        preset(
            "Dense Linked Release",
            &[
                (GAIN, 6.0),
                (CEILING, -1.0),
                (STYLE, ALLROUND),
                (RELEASE, 120.0),
                (LOOKAHEAD, 4.0),
                (LINK, 0.3),
                (RELEASE_LINK, 1.0),
            ],
        )
        .in_group("Master"),
        // Creative.
        preset(
            "Pump Limiter",
            &[
                (GAIN, 14.0),
                (CEILING, -1.0),
                (STYLE, AGGRESSIVE),
                (RELEASE, 400.0),
                (AUTO_RELEASE, 0.0),
                (LOOKAHEAD, 1.0),
            ],
        )
        .in_group("Creative"),
        preset(
            "Brick Wall Distortion",
            &[
                (GAIN, 24.0),
                (CEILING, -1.0),
                (STYLE, AGGRESSIVE),
                (RELEASE, 5.0),
                (AUTO_RELEASE, 0.0),
                (LOOKAHEAD, 0.5),
                (TRUE_PEAK, 0.0),
            ],
        )
        .in_group("Creative"),
        preset(
            "Loudness Check at Unity",
            &[
                (GAIN, 8.0),
                (CEILING, -1.0),
                (STYLE, SAFE),
                (RELEASE, 100.0),
                (LOOKAHEAD, 5.0),
                (UNITY, 1.0),
            ],
        )
        .in_group("Creative"),
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

/// The Channel Strip's: a console channel set up for what is plugged
/// into it — filters, gate, compressor, the four bands, a touch of drive.
pub(super) fn channel_strip() -> Vec<FactoryPreset> {
    use crate::devices::channel_strip::id::*;
    const EXPANDER: f64 = 1.0;
    const BLACK: f64 = 1.0;
    const DYNAMICS_FIRST: f64 = 1.0;
    const SLOPE_12: f64 = 0.0;
    const SLOPE_24: f64 = 2.0;
    vec![
        preset(
            "Lead Vocal",
            &[
                (HPF, 90.0),
                (COMP_THRESHOLD, -18.0),
                (COMP_RATIO, 3.0),
                (COMP_ATTACK, 10.0),
                (COMP_RELEASE, 150.0),
                (COMP_KNEE, 4.0),
                (COMP_MAKEUP, 3.0),
                (LF_GAIN, -1.5),
                (LF_FREQ, 200.0),
                (LMF_GAIN, -2.0),
                (LMF_FREQ, 400.0),
                (LMF_Q, 1.0),
                (HMF_GAIN, 2.0),
                (HMF_FREQ, 3_000.0),
                (HF_GAIN, 2.5),
                (HF_FREQ, 10_000.0),
                (DRIVE, 0.15),
            ],
        ),
        // The expander takes the room between phrases, the compressor
        // holds the voice in front, presence and air on top.
        preset(
            "Broadcast Vocal",
            &[
                (HPF, 100.0),
                (HPF_SLOPE, SLOPE_24),
                (GATE, 1.0),
                (GATE_MODE, EXPANDER),
                (GATE_THRESHOLD, -45.0),
                (GATE_RANGE, -15.0),
                (GATE_HOLD, 80.0),
                (GATE_RELEASE, 250.0),
                (COMP_THRESHOLD, -22.0),
                (COMP_RATIO, 4.0),
                (COMP_ATTACK, 5.0),
                (COMP_RELEASE, 120.0),
                (COMP_KNEE, 3.0),
                (COMP_MAKEUP, 5.0),
                (HMF_GAIN, 3.0),
                (HMF_FREQ, 2_500.0),
                (HF_GAIN, 2.0),
                (HF_FREQ, 8_000.0),
                (DRIVE, 0.2),
            ],
        ),
        // Gated on the beater, the dynamics before the equaliser so the
        // low boost does not pump the compressor.
        preset(
            "Kick Drum",
            &[
                (HPF, 30.0),
                (GATE, 1.0),
                (GATE_THRESHOLD, -35.0),
                (GATE_RANGE, -40.0),
                (GATE_ATTACK, 0.2),
                (GATE_HOLD, 60.0),
                (GATE_RELEASE, 150.0),
                (GATE_HYSTERESIS, 6.0),
                (COMP_THRESHOLD, -12.0),
                (COMP_RATIO, 4.0),
                (COMP_ATTACK, 20.0),
                (COMP_RELEASE, 80.0),
                (COMP_PEAK, 1.0),
                (COMP_MAKEUP, 2.0),
                (LF_GAIN, 4.0),
                (LF_FREQ, 60.0),
                (LF_BELL, 1.0),
                (LMF_GAIN, -4.0),
                (LMF_FREQ, 350.0),
                (LMF_Q, 1.2),
                (HMF_GAIN, 3.0),
                (HMF_FREQ, 4_000.0),
                (HMF_Q, 0.9),
                (ORDER, DYNAMICS_FIRST),
            ],
        ),
        preset(
            "Snare",
            &[
                (HPF, 80.0),
                (GATE, 1.0),
                (GATE_THRESHOLD, -30.0),
                (GATE_RANGE, -24.0),
                (GATE_ATTACK, 0.1),
                (GATE_HOLD, 80.0),
                (GATE_RELEASE, 180.0),
                (GATE_HYSTERESIS, 6.0),
                (COMP_THRESHOLD, -14.0),
                (COMP_RATIO, 4.0),
                (COMP_ATTACK, 8.0),
                (COMP_RELEASE, 90.0),
                (COMP_PEAK, 1.0),
                (COMP_MAKEUP, 2.0),
                (LMF_GAIN, -2.0),
                (LMF_FREQ, 500.0),
                (LMF_Q, 1.0),
                (HMF_GAIN, 2.5),
                (HMF_FREQ, 3_000.0),
                (HF_GAIN, 2.0),
                (HF_FREQ, 10_000.0),
                (DRIVE, 0.25),
            ],
        ),
        preset(
            "Toms",
            &[
                (HPF, 50.0),
                (GATE, 1.0),
                (GATE_THRESHOLD, -32.0),
                (GATE_RANGE, -30.0),
                (GATE_ATTACK, 0.3),
                (GATE_HOLD, 150.0),
                (GATE_RELEASE, 400.0),
                (GATE_HYSTERESIS, 6.0),
                (COMP_THRESHOLD, -12.0),
                (COMP_RATIO, 3.0),
                (COMP_ATTACK, 15.0),
                (COMP_RELEASE, 120.0),
                (COMP_PEAK, 1.0),
                (LF_GAIN, 3.0),
                (LF_FREQ, 90.0),
                (LMF_GAIN, -4.0),
                (LMF_FREQ, 450.0),
                (LMF_Q, 1.5),
                (HMF_GAIN, 2.0),
                (HMF_FREQ, 4_500.0),
            ],
        ),
        // Gentle, RMS-detected glue and air; the Black curves narrow the
        // cuts as they deepen.
        preset(
            "Drum Overheads",
            &[
                (HPF, 200.0),
                (HPF_SLOPE, SLOPE_12),
                (COMP_THRESHOLD, -16.0),
                (COMP_RATIO, 2.0),
                (COMP_ATTACK, 25.0),
                (COMP_RELEASE, 200.0),
                (COMP_KNEE, 6.0),
                (COMP_MAKEUP, 1.5),
                (LMF_GAIN, -1.5),
                (LMF_FREQ, 600.0),
                (HF_GAIN, 2.5),
                (HF_FREQ, 12_000.0),
                (EQ_TYPE, BLACK),
            ],
        ),
        preset(
            "Bass DI",
            &[
                (LPF, 8_000.0),
                (COMP_THRESHOLD, -20.0),
                (COMP_RATIO, 4.0),
                (COMP_ATTACK, 15.0),
                (COMP_RELEASE, 200.0),
                (COMP_KNEE, 6.0),
                (COMP_MAKEUP, 4.0),
                (LF_GAIN, 2.5),
                (LF_FREQ, 80.0),
                (LMF_GAIN, 1.5),
                (LMF_FREQ, 700.0),
                (LMF_Q, 0.7),
                (HMF_GAIN, -2.0),
                (HMF_FREQ, 2_500.0),
                (DRIVE, 0.35),
            ],
        ),
        preset(
            "Acoustic Guitar",
            &[
                (HPF, 80.0),
                (COMP_THRESHOLD, -22.0),
                (COMP_RATIO, 2.5),
                (COMP_ATTACK, 15.0),
                (COMP_RELEASE, 160.0),
                (COMP_KNEE, 8.0),
                (COMP_MAKEUP, 3.0),
                (LF_GAIN, -2.0),
                (LF_FREQ, 150.0),
                (LMF_GAIN, -2.5),
                (LMF_FREQ, 300.0),
                (LMF_Q, 1.0),
                (HMF_GAIN, 1.5),
                (HMF_FREQ, 2_000.0),
                (HF_GAIN, 2.0),
                (HF_FREQ, 9_000.0),
                (EQ_TYPE, BLACK),
            ],
        ),
        preset(
            "Electric Guitar",
            &[
                (HPF, 100.0),
                (LPF, 9_000.0),
                (COMP_THRESHOLD, -16.0),
                (COMP_RATIO, 3.0),
                (COMP_ATTACK, 10.0),
                (COMP_RELEASE, 100.0),
                (COMP_MAKEUP, 2.0),
                (LMF_GAIN, 2.0),
                (LMF_FREQ, 800.0),
                (LMF_Q, 0.9),
                (HMF_GAIN, -2.0),
                (HMF_FREQ, 3_500.0),
                (HMF_Q, 1.2),
                (DRIVE, 0.3),
            ],
        ),
        preset(
            "Piano",
            &[
                (HPF, 40.0),
                (COMP_THRESHOLD, -20.0),
                (COMP_RATIO, 1.8),
                (COMP_ATTACK, 25.0),
                (COMP_RELEASE, 250.0),
                (COMP_KNEE, 10.0),
                (COMP_MAKEUP, 2.0),
                (LF_GAIN, 1.0),
                (LF_FREQ, 100.0),
                (HF_GAIN, 1.5),
                (HF_FREQ, 10_000.0),
                (ORDER, DYNAMICS_FIRST),
            ],
        ),
        // The compressor keyed from the kick on the sidechain: the bass
        // ducks under each hit.
        preset(
            "Synth Bass (Kick Ducked)",
            &[
                (HPF, 30.0),
                (EXTERNAL, 1.0),
                (COMP_THRESHOLD, -30.0),
                (COMP_RATIO, 10.0),
                (COMP_ATTACK, 1.0),
                (COMP_RELEASE, 150.0),
                (COMP_KNEE, 3.0),
                (COMP_PEAK, 1.0),
                (LF_GAIN, 2.0),
                (LF_FREQ, 70.0),
            ],
        ),
        preset(
            "Mix Bus Glue",
            &[
                (COMP_THRESHOLD, -14.0),
                (COMP_RATIO, 2.0),
                (COMP_ATTACK, 30.0),
                (COMP_RELEASE, 200.0),
                (COMP_KNEE, 4.0),
                (COMP_MAKEUP, 1.0),
                (LF_GAIN, 1.0),
                (LF_FREQ, 80.0),
                (HF_GAIN, 1.0),
                (HF_FREQ, 12_000.0),
                (DRIVE, 0.1),
            ],
        ),
        preset(
            "Dialogue",
            &[
                (HPF, 80.0),
                (HPF_SLOPE, SLOPE_24),
                (LPF, 12_000.0),
                (GATE, 1.0),
                (GATE_MODE, EXPANDER),
                (GATE_THRESHOLD, -50.0),
                (GATE_RANGE, -12.0),
                (GATE_ATTACK, 5.0),
                (GATE_HOLD, 100.0),
                (GATE_RELEASE, 300.0),
                (COMP_THRESHOLD, -24.0),
                (COMP_RATIO, 3.0),
                (COMP_ATTACK, 5.0),
                (COMP_RELEASE, 100.0),
                (COMP_KNEE, 6.0),
                (COMP_MAKEUP, 4.0),
                (LMF_GAIN, -2.0),
                (LMF_FREQ, 250.0),
                (HMF_GAIN, 2.5),
                (HMF_FREQ, 3_000.0),
                (HF_GAIN, -1.0),
                (HF_FREQ, 9_000.0),
            ],
        ),
        // Nothing but the console's drive: the dynamics off.
        preset(
            "Console Colour",
            &[(INPUT, 2.0), (COMP, 0.0), (DRIVE, 0.6), (OUTPUT, -2.0)],
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

/// The 2A Leveler's: what the optical leveller is reached for — the voice
/// held steady and in front, bass notes evened, a bus glued, the
/// broadcast limiter with its HF emphasis. Peak Reduction sets how much
/// (Compress about 3:1, Limit flattening as it deepens), Gain brings the
/// level back to the dry signal's; the cell's own timing does the rest.
pub(super) fn leveler_2a() -> Vec<FactoryPreset> {
    use crate::devices::leveler::id::*;
    const COMPRESS: f64 = 0.0;
    const LIMIT: f64 = 1.0;
    vec![
        // The classic: 4–6 dB of levelling on a vocal, Compress.
        preset(
            "Vocal Leveler",
            &[(MODE, COMPRESS), (PEAK_REDUCTION, 62.0), (GAIN, 57.6)],
        ),
        // A little less, for a voice that is nearly there.
        preset(
            "Vocal Gentle",
            &[(MODE, COMPRESS), (PEAK_REDUCTION, 52.0), (GAIN, 52.4)],
        ),
        // Limit and more: the voice pinned in front of the track.
        preset(
            "Vocal Upfront",
            &[(MODE, LIMIT), (PEAK_REDUCTION, 72.0), (GAIN, 64.9)],
        ),
        // The spoken word for broadcast: Limit, R37 turned for the highs
        // (sibilance levels harder, as the 3-66 manual meant it for FM).
        preset(
            "Broadcast Voice",
            &[
                (MODE, LIMIT),
                (PEAK_REDUCTION, 66.0),
                (EMPHASIS, 0.6),
                (GAIN, 54.0),
            ],
        ),
        // Bass notes evened: the slow release keeps them from pumping.
        preset(
            "Bass Even",
            &[(MODE, COMPRESS), (PEAK_REDUCTION, 66.0), (GAIN, 59.3)],
        ),
        // Limit, pushed: a thick, steady bass line.
        preset(
            "Bass Thick",
            &[(MODE, LIMIT), (PEAK_REDUCTION, 76.0), (GAIN, 66.9)],
        ),
        // A couple of decibels across a bus, both sides together.
        preset(
            "Bus Glue",
            &[
                (MODE, COMPRESS),
                (PEAK_REDUCTION, 48.0),
                (GAIN, 50.9),
                (LINK, 1.0),
            ],
        ),
        // Squeezed hard and blended under the dry bus.
        preset(
            "Bus Parallel",
            &[
                (MODE, LIMIT),
                (PEAK_REDUCTION, 82.0),
                (GAIN, 72.0),
                (MIX, 0.4),
                (LINK, 1.0),
            ],
        ),
        // Strummed or picked: the dynamics smoothed, the attack let through.
        preset(
            "Acoustic Guitar",
            &[(MODE, COMPRESS), (PEAK_REDUCTION, 56.0), (GAIN, 54.7)],
        ),
        // A room or overheads squashed into a bloom.
        preset(
            "Room Bloom",
            &[
                (MODE, LIMIT),
                (PEAK_REDUCTION, 88.0),
                (GAIN, 74.0),
                (MIX, 0.6),
            ],
        ),
        // Strings and pads lifted where they thin out.
        preset(
            "Strings Smooth",
            &[(MODE, COMPRESS), (PEAK_REDUCTION, 58.0), (GAIN, 55.8)],
        ),
    ]
}

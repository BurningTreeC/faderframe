//! Arpeggiator, chord, scale and note echo presets.

use super::{FactoryPreset, preset};

// Divisions (crate::dsp::lfo::DIVISIONS).
const D32: f64 = 2.0;
const D16: f64 = 5.0;
const D8T: f64 = 6.0;
const D8: f64 = 8.0;
const D8D: f64 = 10.0;
const D4: f64 = 11.0;
const D4D: f64 = 13.0;

pub(super) fn arpeggiator() -> Vec<FactoryPreset> {
    use crate::devices::arpeggiator::id::*;
    const DOWN: f64 = 1.0;
    const UP_DOWN: f64 = 2.0;
    const CONVERGE: f64 = 4.0;
    const DIVERGE: f64 = 5.0;
    const AS_PLAYED: f64 = 6.0;
    const RANDOM: f64 = 7.0;
    const CHORD: f64 = 8.0;
    vec![
        preset("Classic Up", &[(RATE, D16), (GATE, 0.8)]),
        preset(
            "Up Two Octaves",
            &[(RATE, D16), (OCTAVES, 2.0), (GATE, 0.6)],
        ),
        // The chord as rhythmic stabs.
        preset("Chord Stabs", &[(MODE, CHORD), (RATE, D16), (GATE, 0.45)]),
        preset(
            "Up-Down Eighths",
            &[(MODE, UP_DOWN), (RATE, D8), (GATE, 0.9)],
        ),
        preset(
            "Falling Triplets",
            &[(MODE, DOWN), (RATE, D8T), (GATE, 0.7)],
        ),
        preset(
            "Converging Sixteenths",
            &[(MODE, CONVERGE), (RATE, D16), (OCTAVES, 2.0), (GATE, 0.5)],
        ),
        preset(
            "Dotted Diverge",
            &[(MODE, DIVERGE), (RATE, D8D), (OCTAVES, 2.0), (GATE, 0.8)],
        ),
        preset(
            "Random Sparkle",
            &[(MODE, RANDOM), (RATE, D16), (OCTAVES, 3.0), (GATE, 0.3)],
        ),
        preset(
            "Swung Sixteenths",
            &[(RATE, D16), (SWING, 0.55), (GATE, 0.7)],
        ),
        preset("Double Hits", &[(RATE, D16), (REPEATS, 2.0), (GATE, 0.4)]),
        // Played order, latched: play a phrase once, it keeps going.
        preset(
            "Latched Phrase",
            &[(MODE, AS_PLAYED), (RATE, D8), (HOLD, 1.0), (GATE, 0.9)],
        ),
        // Notes overlapping the next: a smooth, pad-like arpeggio.
        preset(
            "Legato Pad Arp",
            &[(RATE, D8), (OCTAVES, 2.0), (GATE, 1.25)],
        ),
        preset(
            "Rising Thirty-Seconds",
            &[(RATE, D32), (OCTAVES, 3.0), (GATE, 0.5), (VELOCITY, 100.0)],
        ),
    ]
}

pub(super) fn chord() -> Vec<FactoryPreset> {
    use crate::devices::chord::id::*;
    let shifts = |name: &'static str, s: &[f64], extra: &[(u32, f64)]| {
        let mut set: Vec<(u32, f64)> = (0..5)
            .map(|i| (SHIFT + i as u32, s.get(i).copied().unwrap_or(0.0)))
            .collect();
        set.extend_from_slice(extra);
        preset(name, &set)
    };
    vec![
        shifts("Major Triad", &[4.0, 7.0], &[]),
        shifts("Minor Triad", &[3.0, 7.0], &[]),
        shifts("Power Chord", &[7.0, 12.0], &[]),
        shifts("Octaves", &[12.0], &[]),
        shifts("Major Seventh", &[4.0, 7.0, 11.0], &[]),
        shifts("Minor Seventh", &[3.0, 7.0, 10.0], &[]),
        shifts("Suspended Fourth", &[5.0, 7.0], &[]),
        // Fifths stacked: open and modern.
        shifts("Stacked Fifths", &[7.0, 14.0], &[(VELOCITY, 0.8)]),
        // An open major shape strummed down the strings.
        shifts(
            "Strummed Guitar",
            &[7.0, 12.0, 16.0, 19.0],
            &[(STRUM, 18.0), (VELOCITY, 0.85)],
        ),
        shifts("Soft Fifth Pad", &[7.0, 12.0], &[(VELOCITY, 0.6)]),
        // One finger plays the key's chords.
        preset("Scale Triads (Key Track)", &[(MODE, 1.0)]),
        preset(
            "Scale Sevenths (Key Track)",
            &[(MODE, 2.0), (VELOCITY, 0.85)],
        ),
        preset("Chord Track Voicing", &[(MODE, 3.0)]),
    ]
}

pub(super) fn scale() -> Vec<FactoryPreset> {
    use crate::devices::scale::id::*;
    // Scale indexes (theory::Scale::ALL).
    const MINOR: f64 = 1.0;
    const HARMONIC_MINOR: f64 = 2.0;
    const DORIAN: f64 = 4.0;
    const MINOR_PENTATONIC: f64 = 10.0;
    const BLUES: f64 = 11.0;
    let manual = |name: &'static str, root: f64, scale: f64| {
        preset(name, &[(FOLLOW_KEY, 0.0), (ROOT, root), (SCALE, scale)])
    };
    vec![
        preset("Key Track", &[]),
        preset("Key Track, Up", &[(MODE, 1.0)]),
        // Only notes of the key get through.
        preset("Key Track, Wrong Notes Out", &[(MODE, 3.0)]),
        manual("C Major", 0.0, 0.0),
        manual("A Minor", 9.0, MINOR),
        manual("A Harmonic Minor", 9.0, HARMONIC_MINOR),
        manual("D Dorian", 2.0, DORIAN),
        manual("A Minor Pentatonic", 9.0, MINOR_PENTATONIC),
        manual("E Blues", 4.0, BLUES),
        // Diatonic transposers: a harmony line in the key.
        preset("A Third Up in Key", &[(DEGREES, 2.0)]),
        preset("A Sixth Below in Key", &[(DEGREES, 2.0), (OCTAVE, -1.0)]),
        preset("An Octave Down", &[(OCTAVE, -1.0)]),
    ]
}

pub(super) fn note_echo() -> Vec<FactoryPreset> {
    use crate::devices::note_echo::id::*;
    vec![
        preset(
            "Eighth Echoes",
            &[(DIVISION, D8), (REPEATS, 3.0), (FEEDBACK, 0.7)],
        ),
        preset(
            "Dotted Eighth",
            &[(DIVISION, D8D), (REPEATS, 4.0), (FEEDBACK, 0.65)],
        ),
        preset(
            "Quarter Fade",
            &[(DIVISION, D4), (REPEATS, 3.0), (FEEDBACK, 0.6)],
        ),
        preset(
            "Triplet Bounce",
            &[(DIVISION, D8T), (REPEATS, 5.0), (FEEDBACK, 0.7)],
        ),
        preset(
            "Rising Octaves",
            &[
                (DIVISION, D8),
                (REPEATS, 3.0),
                (FEEDBACK, 0.8),
                (PITCH, 12.0),
            ],
        ),
        preset(
            "Falling Fifths",
            &[
                (DIVISION, D8),
                (REPEATS, 4.0),
                (FEEDBACK, 0.75),
                (PITCH, -7.0),
            ],
        ),
        preset(
            "Minor Third Climb",
            &[
                (DIVISION, D16),
                (REPEATS, 6.0),
                (FEEDBACK, 0.85),
                (PITCH, 3.0),
            ],
        ),
        // Rolls: thirty-seconds, barely fading.
        preset(
            "Ratchet Roll",
            &[(DIVISION, D32), (REPEATS, 7.0), (FEEDBACK, 0.9)],
        ),
        preset(
            "Long Tail",
            &[(DIVISION, D4D), (REPEATS, 8.0), (FEEDBACK, 0.85)],
        ),
        // The echoes without the note: a shadow behind the beat.
        preset(
            "Shadow (Echoes Only)",
            &[(DIVISION, D8), (REPEATS, 4.0), (FEEDBACK, 0.8), (DRY, 0.0)],
        ),
        preset(
            "Slapback",
            &[(SYNC, 0.0), (TIME, 60.0), (REPEATS, 1.0), (FEEDBACK, 0.7)],
        ),
        preset(
            "Ambient Drift",
            &[
                (SYNC, 0.0),
                (TIME, 380.0),
                (REPEATS, 12.0),
                (FEEDBACK, 0.88),
            ],
        ),
    ]
}

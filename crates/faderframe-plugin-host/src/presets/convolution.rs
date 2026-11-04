//! The Convolution Reverb's presets: the built-in spaces, as they are and
//! shaped (shortened, reversed, stretched), voiced for inserts with a
//! musical mix. None names a file: a preset leaves the device's impulse
//! file as it is (choosing a built-in space).

use super::{FactoryPreset, preset};

pub(super) fn convolution_reverb() -> Vec<FactoryPreset> {
    use crate::devices::convolution::id::*;
    // Spaces (0 is the file; `devices::convolution::spaces::SPACES` + 1).
    const BOOTH: f64 = 1.0;
    const SMALL_ROOM: f64 = 2.0;
    const STUDIO: f64 = 3.0;
    const CHAMBER: f64 = 4.0;
    const HALL: f64 = 5.0;
    const LARGE_HALL: f64 = 6.0;
    const CATHEDRAL: f64 = 7.0;
    const PLATE: f64 = 8.0;
    vec![
        preset(
            "Vocal Booth",
            &[(SPACE, BOOTH), (MIX, 0.25), (LOW_CUT, 120.0)],
        ),
        preset(
            "Drum Room",
            &[
                (SPACE, SMALL_ROOM),
                (MIX, 0.25),
                (LOW_CUT, 80.0),
                (HIGH_CUT, 12_000.0),
            ],
        ),
        preset(
            "Studio Live Room",
            &[(SPACE, STUDIO), (MIX, 0.22), (PRE_DELAY, 5.0)],
        ),
        preset(
            "Echo Chamber",
            &[
                (SPACE, CHAMBER),
                (MIX, 0.25),
                (PRE_DELAY, 15.0),
                (LOW_CUT, 150.0),
            ],
        ),
        preset(
            "Concert Hall",
            &[(SPACE, HALL), (MIX, 0.25), (PRE_DELAY, 20.0)],
        ),
        preset(
            "Large Hall",
            &[
                (SPACE, LARGE_HALL),
                (MIX, 0.22),
                (PRE_DELAY, 30.0),
                (LOW_CUT, 90.0),
            ],
        ),
        preset(
            "Cathedral",
            &[
                (SPACE, CATHEDRAL),
                (MIX, 0.2),
                (PRE_DELAY, 40.0),
                (HIGH_CUT, 9_000.0),
            ],
        ),
        // Pre-delay keeps the words clear of the bloom.
        preset(
            "Vocal Plate",
            &[
                (SPACE, PLATE),
                (MIX, 0.22),
                (PRE_DELAY, 60.0),
                (LOW_CUT, 200.0),
            ],
        ),
        preset(
            "Dark Plate",
            &[
                (SPACE, PLATE),
                (MIX, 0.25),
                (HIGH_CUT, 5_000.0),
                (LENGTH, 0.7),
                (FADE_OUT, 0.4),
            ],
        ),
        // The hall's first fifth, faded: air without a tail.
        preset(
            "Short Ambience",
            &[
                (SPACE, HALL),
                (MIX, 0.3),
                (LENGTH, 0.15),
                (FADE_OUT, 0.6),
                (WIDTH, 1.3),
            ],
        ),
        // Played backwards: the swell into each note.
        preset(
            "Reverse Swell",
            &[
                (SPACE, PLATE),
                (MIX, 0.25),
                (REVERSE, 1.0),
                (LENGTH, 0.5),
                (FADE_IN, 80.0),
                (LOW_CUT, 150.0),
            ],
        ),
        // The large hall at one and a half times its size: darker, longer.
        preset(
            "Huge Space",
            &[
                (SPACE, LARGE_HALL),
                (MIX, 0.2),
                (STRETCH, 1.5),
                (LOW_CUT, 120.0),
                (HIGH_CUT, 8_000.0),
            ],
        ),
        // The studio at half its size: a tight, bright box.
        preset(
            "Tiny Box",
            &[(SPACE, STUDIO), (MIX, 0.3), (STRETCH, 0.5), (WIDTH, 0.6)],
        ),
    ]
}

//! Multiband Dynamics presets: mastering glue, mix fixes (low end, mud,
//! harshness, sibilance), upward compression for detail and expansion to
//! tighten. Band settings are listed band by band.

use super::{FactoryPreset, preset};

/// Band `b`'s fields.
fn on_band(b: usize, fields: &[(u32, f64)]) -> Vec<(u32, f64)> {
    use crate::devices::multiband::id;
    fields.iter().map(|(f, v)| (id::band(b, *f), *v)).collect()
}

fn with(name: &'static str, globals: &[(u32, f64)], bands: &[Vec<(u32, f64)>]) -> FactoryPreset {
    let mut set = globals.to_vec();
    for b in bands {
        set.extend_from_slice(b);
    }
    preset(name, &set)
}

pub(super) fn multiband_dynamics() -> Vec<FactoryPreset> {
    use crate::devices::multiband::id::*;
    const UP_COMP: f64 = 2.0;
    const EXPAND: f64 = 1.0;
    const UP_EXP: f64 = 3.0;
    const RMS: f64 = 1.0;
    const LINEAR: f64 = 1.0;
    let gentle = |t: f64, r: f64| {
        vec![
            (THRESHOLD, t),
            (RATIO, r),
            (KNEE, 9.0),
            (ATTACK, 20.0),
            (RELEASE, 200.0),
        ]
    };
    let bypass = || vec![(BYPASS, 1.0)];
    vec![
        // Four bands, two or three decibels each.
        with(
            "Master Glue",
            &[(BANDS, 4.0), (DETECTOR, RMS)],
            &[
                on_band(0, &gentle(-24.0, 1.8)),
                on_band(1, &gentle(-26.0, 1.6)),
                on_band(2, &gentle(-28.0, 1.6)),
                on_band(3, &gentle(-30.0, 1.5)),
            ],
        ),
        with(
            "Linear-Phase Master",
            &[
                (BANDS, 4.0),
                (PHASE, LINEAR),
                (DETECTOR, RMS),
                (LOOKAHEAD, 2.0),
            ],
            &[
                on_band(0, &gentle(-22.0, 2.0)),
                on_band(1, &gentle(-24.0, 1.7)),
                on_band(2, &gentle(-26.0, 1.7)),
                on_band(3, &gentle(-28.0, 1.6)),
            ],
        ),
        // Only the lows held steady.
        with(
            "Tame the Low End",
            &[(BANDS, 2.0), (CROSSOVER, 150.0)],
            &[
                on_band(
                    0,
                    &[
                        (THRESHOLD, -26.0),
                        (RATIO, 3.0),
                        (ATTACK, 15.0),
                        (RELEASE, 120.0),
                    ],
                ),
                on_band(1, &bypass()),
            ],
        ),
        // The low mids down when they pile up.
        with(
            "De-Mud",
            &[(BANDS, 3.0), (CROSSOVER, 200.0), (CROSSOVER + 1, 600.0)],
            &[
                on_band(0, &bypass()),
                on_band(1, &[(THRESHOLD, -30.0), (RATIO, 3.0), (RANGE, 6.0)]),
                on_band(2, &bypass()),
            ],
        ),
        with(
            "De-Harsh",
            &[(BANDS, 3.0), (CROSSOVER, 2_000.0), (CROSSOVER + 1, 5_000.0)],
            &[
                on_band(0, &bypass()),
                on_band(
                    1,
                    &[
                        (THRESHOLD, -32.0),
                        (RATIO, 4.0),
                        (ATTACK, 2.0),
                        (RELEASE, 60.0),
                        (RANGE, 6.0),
                    ],
                ),
                on_band(2, &bypass()),
            ],
        ),
        // A de-esser's job across the top band.
        with(
            "Sibilance Control",
            &[(BANDS, 2.0), (CROSSOVER, 5_500.0)],
            &[
                on_band(0, &bypass()),
                on_band(
                    1,
                    &[
                        (THRESHOLD, -34.0),
                        (RATIO, 6.0),
                        (ATTACK, 0.5),
                        (RELEASE, 40.0),
                        (RANGE, 9.0),
                    ],
                ),
            ],
        ),
        with(
            "Vocal Presence",
            &[(BANDS, 3.0), (CROSSOVER, 250.0), (CROSSOVER + 1, 4_000.0)],
            &[
                on_band(0, &[(THRESHOLD, -28.0), (RATIO, 2.5)]),
                on_band(1, &[(THRESHOLD, -24.0), (RATIO, 2.0), (GAIN, 1.5)]),
                on_band(
                    2,
                    &[
                        (MODE, UP_COMP),
                        (THRESHOLD, -40.0),
                        (RATIO, 1.5),
                        (RANGE, 4.0),
                    ],
                ),
            ],
        ),
        // Hard and fast on the lows, the snap kept above.
        with(
            "Drum Bus Punch",
            &[(BANDS, 3.0), (CROSSOVER, 120.0), (CROSSOVER + 1, 3_000.0)],
            &[
                on_band(
                    0,
                    &[
                        (THRESHOLD, -20.0),
                        (RATIO, 4.0),
                        (ATTACK, 25.0),
                        (RELEASE, 90.0),
                        (GAIN, 2.0),
                    ],
                ),
                on_band(
                    1,
                    &[
                        (THRESHOLD, -22.0),
                        (RATIO, 3.0),
                        (ATTACK, 10.0),
                        (RELEASE, 80.0),
                    ],
                ),
                on_band(
                    2,
                    &[
                        (MODE, UP_EXP),
                        (THRESHOLD, -20.0),
                        (RATIO, 1.3),
                        (RANGE, 4.0),
                    ],
                ),
            ],
        ),
        // Everything up from underneath: detail without squashing peaks.
        with(
            "Upward Detail",
            &[(BANDS, 4.0), (DETECTOR, RMS)],
            &[
                on_band(
                    0,
                    &[
                        (MODE, UP_COMP),
                        (THRESHOLD, -36.0),
                        (RATIO, 1.6),
                        (RANGE, 6.0),
                    ],
                ),
                on_band(
                    1,
                    &[
                        (MODE, UP_COMP),
                        (THRESHOLD, -38.0),
                        (RATIO, 1.6),
                        (RANGE, 6.0),
                    ],
                ),
                on_band(
                    2,
                    &[
                        (MODE, UP_COMP),
                        (THRESHOLD, -40.0),
                        (RATIO, 1.8),
                        (RANGE, 8.0),
                    ],
                ),
                on_band(
                    3,
                    &[
                        (MODE, UP_COMP),
                        (THRESHOLD, -42.0),
                        (RATIO, 1.8),
                        (RANGE, 8.0),
                    ],
                ),
            ],
        ),
        // The low end's rumble and room between notes taken down.
        with(
            "Tighten Lows",
            &[(BANDS, 2.0), (CROSSOVER, 180.0)],
            &[
                on_band(
                    0,
                    &[
                        (MODE, EXPAND),
                        (THRESHOLD, -40.0),
                        (RATIO, 2.0),
                        (RANGE, 12.0),
                        (RELEASE, 80.0),
                    ],
                ),
                on_band(1, &bypass()),
            ],
        ),
        // Five bands, firm, for loudness.
        with(
            "Broadcast",
            &[(BANDS, 5.0), (DETECTOR, RMS), (OUTPUT, 2.0)],
            &[
                on_band(0, &[(THRESHOLD, -28.0), (RATIO, 3.0), (KNEE, 6.0)]),
                on_band(1, &[(THRESHOLD, -28.0), (RATIO, 3.0), (KNEE, 6.0)]),
                on_band(2, &[(THRESHOLD, -30.0), (RATIO, 3.0), (KNEE, 6.0)]),
                on_band(3, &[(THRESHOLD, -32.0), (RATIO, 3.0), (KNEE, 6.0)]),
                on_band(4, &[(THRESHOLD, -34.0), (RATIO, 3.0), (KNEE, 6.0)]),
            ],
        ),
        // The bass held under the kick (keyed): route the kick to the
        // sidechain.
        with(
            "Bass Ducked by Kick",
            &[(BANDS, 2.0), (CROSSOVER, 150.0), (EXTERNAL, 1.0)],
            &[
                on_band(
                    0,
                    &[
                        (THRESHOLD, -30.0),
                        (RATIO, 8.0),
                        (ATTACK, 2.0),
                        (RELEASE, 120.0),
                        (RANGE, 8.0),
                    ],
                ),
                on_band(1, &bypass()),
            ],
        ),
    ]
}

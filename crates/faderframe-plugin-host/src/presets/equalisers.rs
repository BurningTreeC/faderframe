//! EQ and Program EQ presets.

use super::{FactoryPreset, preset};
use crate::eq::design::BandType;
use crate::eq::{Field, Placement, band_id, global, global_id};

/// One band of an EQ preset.
#[derive(Clone, Copy)]
struct Band {
    ty: BandType,
    freq: f64,
    gain: f64,
    q: f64,
    /// dB/oct (cuts).
    slope: f64,
    placement: Placement,
    /// Dynamic range (dB; 0 static).
    range: f64,
    /// Keyed from the sidechain.
    key: bool,
}

const fn band(ty: BandType, freq: f64, gain: f64, q: f64) -> Band {
    Band {
        ty,
        freq,
        gain,
        q,
        slope: 12.0,
        placement: Placement::Stereo,
        range: 0.0,
        key: false,
    }
}

const fn bell(freq: f64, gain: f64, q: f64) -> Band {
    band(BandType::Bell, freq, gain, q)
}

const fn low_shelf(freq: f64, gain: f64) -> Band {
    band(BandType::LowShelf, freq, gain, 0.7)
}

const fn high_shelf(freq: f64, gain: f64) -> Band {
    band(BandType::HighShelf, freq, gain, 0.7)
}

const fn low_cut(freq: f64, slope: f64) -> Band {
    Band {
        slope,
        ..band(BandType::LowCut, freq, 0.0, std::f64::consts::FRAC_1_SQRT_2)
    }
}

const fn high_cut(freq: f64, slope: f64) -> Band {
    Band {
        slope,
        ..band(
            BandType::HighCut,
            freq,
            0.0,
            std::f64::consts::FRAC_1_SQRT_2,
        )
    }
}

impl Band {
    /// Moving by up to `range` dB as its region gets loud.
    const fn dynamic(self, range: f64) -> Self {
        Band { range, ..self }
    }

    const fn on(self, placement: Placement) -> Self {
        Band { placement, ..self }
    }

    const fn keyed(self) -> Self {
        Band { key: true, ..self }
    }
}

/// An EQ preset from its bands (in band slots 0, 1, …) and globals.
fn eq_preset(name: &'static str, bands: &[Band], globals: &[(usize, f64)]) -> FactoryPreset {
    let mut set = Vec::new();
    for (g, v) in globals {
        set.push((global_id(*g).0, *v));
    }
    for (b, band) in bands.iter().enumerate() {
        let mut f = |field: Field, v: f64| set.push((band_id(b, field).0, v));
        f(Field::Enabled, 1.0);
        f(Field::Type, band.ty.index() as f64);
        f(Field::Freq, band.freq);
        f(Field::Gain, band.gain);
        f(Field::Q, band.q);
        f(Field::Slope, band.slope);
        f(Field::Placement, band.placement.index() as f64);
        f(Field::Range, band.range);
        if band.key {
            f(Field::Key, 1.0);
            f(Field::Dynamics, 1.0);
        }
    }
    preset(name, &set)
}

pub(super) fn eq() -> Vec<FactoryPreset> {
    let linear = [(global::PHASE, 1.0), (global::QUALITY, 2.0)];
    vec![
        eq_preset(
            "Vocal Presence",
            &[
                low_cut(90.0, 18.0),
                bell(250.0, -2.0, 1.2),
                bell(3_500.0, 2.5, 1.0),
                high_shelf(10_000.0, 2.0),
            ],
            &[],
        ),
        // Warmth, less box, presence, and the esses held down dynamically.
        eq_preset(
            "Podcast Voice",
            &[
                low_cut(80.0, 24.0),
                bell(180.0, 1.5, 1.0),
                bell(400.0, -3.0, 1.5),
                bell(4_000.0, 2.0, 1.2),
                bell(6_500.0, 0.0, 2.0).dynamic(-6.0),
                high_cut(16_000.0, 12.0),
            ],
            &[],
        ),
        eq_preset(
            "Kick Punch",
            &[
                low_cut(25.0, 24.0),
                bell(60.0, 3.0, 1.4),
                bell(350.0, -4.0, 1.8),
                bell(3_500.0, 3.0, 1.4),
            ],
            &[],
        ),
        eq_preset(
            "Snare Body & Crack",
            &[
                low_cut(70.0, 12.0),
                bell(200.0, 2.5, 1.4),
                bell(900.0, -2.0, 2.0),
                bell(5_000.0, 3.0, 1.0),
                high_shelf(12_000.0, 1.5),
            ],
            &[],
        ),
        eq_preset(
            "Bass Definition",
            &[
                low_cut(30.0, 18.0),
                bell(80.0, 2.0, 1.0),
                bell(250.0, -2.5, 1.4),
                bell(800.0, 2.0, 1.5),
                high_cut(6_000.0, 12.0),
            ],
            &[],
        ),
        eq_preset(
            "Acoustic Guitar Sparkle",
            &[
                low_cut(80.0, 18.0),
                bell(200.0, -3.0, 1.2),
                bell(5_000.0, 1.5, 1.0),
                high_shelf(8_000.0, 3.0),
            ],
            &[],
        ),
        // Room in the mix for the voice and the bass.
        eq_preset(
            "Electric Guitar Mix Fit",
            &[
                low_cut(100.0, 24.0),
                bell(400.0, -2.0, 1.0),
                bell(2_500.0, 2.0, 1.0),
                high_cut(9_000.0, 18.0),
            ],
            &[],
        ),
        eq_preset(
            "Piano Clarity",
            &[
                low_cut(40.0, 12.0),
                bell(300.0, -2.0, 1.0),
                bell(3_000.0, 1.5, 0.8),
                high_shelf(10_000.0, 1.5),
            ],
            &[],
        ),
        // The harshness comes down only when the cymbals get loud.
        eq_preset(
            "Drum Overheads",
            &[
                low_cut(150.0, 18.0),
                bell(500.0, -2.0, 1.2),
                bell(3_500.0, 0.0, 1.5).dynamic(-4.0),
                high_shelf(10_000.0, 2.5),
            ],
            &[],
        ),
        eq_preset(
            "Dynamic Mud Control",
            &[
                low_cut(30.0, 12.0),
                bell(250.0, 0.0, 1.4).dynamic(-5.0),
                bell(500.0, 0.0, 1.6).dynamic(-3.0),
            ],
            &[],
        ),
        // On the music: 1–4 kHz make way while the vocal on the sidechain
        // sings, and only then.
        eq_preset(
            "Vocal Pocket (Sidechain)",
            &[bell(2_200.0, 0.0, 0.9).dynamic(-4.0).keyed()],
            &[],
        ),
        eq_preset(
            "Gentle Smile",
            &[
                low_shelf(80.0, 1.5),
                bell(400.0, -1.0, 0.6),
                high_shelf(12_000.0, 1.5),
            ],
            &[],
        ),
        eq_preset(
            "Master Polish (Linear Phase)",
            &[
                low_cut(20.0, 48.0),
                low_shelf(60.0, 1.0),
                bell(300.0, -0.8, 0.7),
                high_shelf(14_000.0, 1.5),
            ],
            &linear,
        ),
        // The sides lose their lows (a cutting lathe's need), and sibilance
        // is held back.
        eq_preset(
            "Vinyl Prep (Mono Lows)",
            &[
                low_cut(150.0, 24.0).on(Placement::Side),
                low_cut(20.0, 24.0),
                bell(7_000.0, 0.0, 2.0).dynamic(-3.0),
            ],
            &[],
        ),
        eq_preset(
            "Telephone",
            &[
                low_cut(400.0, 24.0),
                bell(1_500.0, 4.0, 1.0),
                high_cut(3_400.0, 24.0),
            ],
            &[],
        ),
    ]
}

pub(super) fn program_eq() -> Vec<FactoryPreset> {
    use crate::program_eq::param::*;
    let id = |i: usize| i as u32;
    // Selector positions.
    const LOW_20: f64 = 0.0;
    const LOW_30: f64 = 1.0;
    const LOW_60: f64 = 2.0;
    const LOW_100: f64 = 3.0;
    const HB_3K: f64 = 0.0;
    const HB_4K: f64 = 1.0;
    const HB_5K: f64 = 2.0;
    const HB_8K: f64 = 3.0;
    const HB_10K: f64 = 4.0;
    const HB_12K: f64 = 5.0;
    const HB_16K: f64 = 6.0;
    const HA_5K: f64 = 0.0;
    const HA_10K: f64 = 1.0;
    const HA_20K: f64 = 2.0;
    let p = |name: &'static str, values: &[(usize, f64)]| {
        let set: Vec<(u32, f64)> = values.iter().map(|(i, v)| (id(*i), *v)).collect();
        preset(name, &set)
    };
    vec![
        // Keep the original preset and its program index for existing users.
        p(
            crate::program_eq::PRESETS[0].0,
            crate::program_eq::PRESETS[0].1,
        ),
        // Boost and cut the same low frequency: the famous bump with the
        // dip above it.
        p(
            "Kick Low-End Trick",
            &[(LOW_FREQ, LOW_60), (LOW_BOOST, 5.0), (LOW_ATTEN, 4.0)],
        ),
        p(
            "Sub Weight (30 Hz Trick)",
            &[(LOW_FREQ, LOW_30), (LOW_BOOST, 4.0), (LOW_ATTEN, 3.0)],
        ),
        p(
            "Bass Growl",
            &[
                (LOW_FREQ, LOW_60),
                (LOW_BOOST, 4.0),
                (LOW_ATTEN, 2.0),
                (HIGH_BOOST_FREQ, HB_3K),
                (HIGH_BOOST, 3.0),
                (BANDWIDTH, 4.0),
            ],
        ),
        p(
            "Snare Snap",
            &[
                (LOW_FREQ, LOW_100),
                (LOW_BOOST, 2.0),
                (HIGH_BOOST_FREQ, HB_8K),
                (HIGH_BOOST, 4.0),
                (BANDWIDTH, 5.0),
            ],
        ),
        p(
            "Vocal Air",
            &[
                (LOW_FREQ, LOW_100),
                (LOW_ATTEN, 2.0),
                (HIGH_BOOST_FREQ, HB_12K),
                (HIGH_BOOST, 4.0),
                (BANDWIDTH, 6.0),
                (HIGH_ATTEN_FREQ, HA_20K),
                (HIGH_ATTEN, 2.0),
            ],
        ),
        p(
            "Vocal Forward",
            &[
                (HIGH_BOOST_FREQ, HB_5K),
                (HIGH_BOOST, 3.0),
                (BANDWIDTH, 4.0),
                (HIGH_ATTEN_FREQ, HA_10K),
                (HIGH_ATTEN, 2.0),
            ],
        ),
        p(
            "Acoustic Shine",
            &[
                (LOW_FREQ, LOW_100),
                (LOW_ATTEN, 3.0),
                (HIGH_BOOST_FREQ, HB_10K),
                (HIGH_BOOST, 3.0),
                (BANDWIDTH, 7.0),
            ],
        ),
        p(
            "Piano Warmth",
            &[
                (LOW_FREQ, LOW_100),
                (LOW_BOOST, 2.5),
                (HIGH_BOOST_FREQ, HB_4K),
                (HIGH_BOOST, 2.0),
                (BANDWIDTH, 4.0),
            ],
        ),
        p(
            "Soften Harsh Highs",
            &[
                (HIGH_BOOST, 0.0),
                (HIGH_ATTEN_FREQ, HA_10K),
                (HIGH_ATTEN, 4.0),
            ],
        ),
        p(
            "Mix Bus Sheen",
            &[
                (LOW_FREQ, LOW_30),
                (LOW_BOOST, 2.0),
                (LOW_ATTEN, 1.0),
                (HIGH_BOOST_FREQ, HB_16K),
                (HIGH_BOOST, 2.0),
                (BANDWIDTH, 8.0),
                (HIGH_ATTEN_FREQ, HA_20K),
                (HIGH_ATTEN, 1.0),
                (DRIVE, 2.0),
                (OUTPUT, -2.0),
            ],
        ),
        p(
            "Master Weight & Air",
            &[
                (LOW_FREQ, LOW_20),
                (LOW_BOOST, 2.5),
                (LOW_ATTEN, 1.5),
                (HIGH_BOOST_FREQ, HB_12K),
                (HIGH_BOOST, 2.0),
                (BANDWIDTH, 7.0),
                (HIGH_ATTEN_FREQ, HA_20K),
                (HIGH_ATTEN, 1.0),
            ],
        ),
        p(
            "Dark Vintage",
            &[
                (LOW_FREQ, LOW_60),
                (LOW_BOOST, 3.0),
                (HIGH_ATTEN_FREQ, HA_5K),
                (HIGH_ATTEN, 5.0),
                (DRIVE, 6.0),
                (OUTPUT, -6.0),
            ],
        ),
        // The knobs at zero: only the transformers and tubes.
        p("Tube Colour (Flat)", &[(DRIVE, 8.0), (OUTPUT, -8.0)]),
        p(
            "Driven Tubes",
            &[
                (LOW_FREQ, LOW_100),
                (LOW_BOOST, 2.0),
                (HIGH_BOOST_FREQ, HB_8K),
                (HIGH_BOOST, 2.0),
                (DRIVE, 14.0),
                (OUTPUT, -14.0),
            ],
        ),
    ]
}

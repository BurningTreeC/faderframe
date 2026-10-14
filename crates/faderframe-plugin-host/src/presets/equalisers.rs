//! EQ and Program EQ presets.
//!
//! The EQ's are grouped (submenus): Vocals, Drums, Bass, Guitars, Keys &
//! Synths, Mix & Master, Surgical and Creative. The first fifteen keep
//! their names, settings and order from the first version.

use super::{FactoryPreset, preset};
use crate::eq::character::Character;
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
    /// Spectral dynamics.
    spectral: bool,
    /// A threshold of its own (dBFS) instead of the automatic one.
    threshold: Option<f64>,
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
        spectral: false,
        threshold: None,
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

const fn notch(freq: f64, q: f64) -> Band {
    band(BandType::Notch, freq, 0.0, q)
}

/// A tilt round `freq`: `gain` dB up above it (and as much down below).
const fn tilt(freq: f64, gain: f64) -> Band {
    band(BandType::TiltShelf, freq, gain, 0.7)
}

const fn band_pass(freq: f64, q: f64) -> Band {
    band(BandType::BandPass, freq, 0.0, q)
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

    /// Per frequency instead of as a whole.
    const fn spectral(self) -> Self {
        Band {
            spectral: true,
            ..self
        }
    }

    /// With a threshold of its own.
    const fn at(self, threshold: f64) -> Self {
        Band {
            threshold: Some(threshold),
            ..self
        }
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
        if band.key || band.threshold.is_some() {
            f(Field::Dynamics, 1.0);
        }
        if band.key {
            f(Field::Key, 1.0);
        }
        if let Some(t) = band.threshold {
            f(Field::Threshold, t);
        }
        if band.spectral {
            f(Field::Spectral, 1.0);
        }
    }
    preset(name, &set)
}

/// The groups of the EQ's presets.
pub mod group {
    pub const VOCALS: &str = "Vocals";
    pub const DRUMS: &str = "Drums";
    pub const BASS: &str = "Bass";
    pub const GUITARS: &str = "Guitars";
    pub const KEYS: &str = "Keys & Synths";
    pub const MIX: &str = "Mix & Master";
    pub const SURGICAL: &str = "Surgical";
    pub const CREATIVE: &str = "Creative";
    pub const ALL: [&str; 8] = [VOCALS, DRUMS, BASS, GUITARS, KEYS, MIX, SURGICAL, CREATIVE];
}

pub(super) fn eq() -> Vec<FactoryPreset> {
    use group::*;
    let linear = [(global::PHASE, 1.0), (global::QUALITY, 2.0)];
    let character = |c: Character, drive: f64| {
        [
            (global::CHARACTER, c.index() as f64),
            (global::CHARACTER_DRIVE, drive),
        ]
    };
    let mut out = vec![
        // --- the first version's fifteen, in their order ---------------------
        eq_preset(
            "Vocal Presence",
            &[
                low_cut(90.0, 18.0),
                bell(250.0, -2.0, 1.2),
                bell(3_500.0, 2.5, 1.0),
                high_shelf(10_000.0, 2.0),
            ],
            &[],
        )
        .in_group(VOCALS),
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
        )
        .in_group(VOCALS),
        eq_preset(
            "Kick Punch",
            &[
                low_cut(25.0, 24.0),
                bell(60.0, 3.0, 1.4),
                bell(350.0, -4.0, 1.8),
                bell(3_500.0, 3.0, 1.4),
            ],
            &[],
        )
        .in_group(DRUMS),
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
        )
        .in_group(DRUMS),
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
        )
        .in_group(BASS),
        eq_preset(
            "Acoustic Guitar Sparkle",
            &[
                low_cut(80.0, 18.0),
                bell(200.0, -3.0, 1.2),
                bell(5_000.0, 1.5, 1.0),
                high_shelf(8_000.0, 3.0),
            ],
            &[],
        )
        .in_group(GUITARS),
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
        )
        .in_group(GUITARS),
        eq_preset(
            "Piano Clarity",
            &[
                low_cut(40.0, 12.0),
                bell(300.0, -2.0, 1.0),
                bell(3_000.0, 1.5, 0.8),
                high_shelf(10_000.0, 1.5),
            ],
            &[],
        )
        .in_group(KEYS),
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
        )
        .in_group(DRUMS),
        eq_preset(
            "Dynamic Mud Control",
            &[
                low_cut(30.0, 12.0),
                bell(250.0, 0.0, 1.4).dynamic(-5.0),
                bell(500.0, 0.0, 1.6).dynamic(-3.0),
            ],
            &[],
        )
        .in_group(SURGICAL),
        // On the music: 1–4 kHz make way while the vocal on the sidechain
        // sings, and only then.
        eq_preset(
            "Vocal Pocket (Sidechain)",
            &[bell(2_200.0, 0.0, 0.9).dynamic(-4.0).keyed()],
            &[],
        )
        .in_group(MIX),
        eq_preset(
            "Gentle Smile",
            &[
                low_shelf(80.0, 1.5),
                bell(400.0, -1.0, 0.6),
                high_shelf(12_000.0, 1.5),
            ],
            &[],
        )
        .in_group(MIX),
        eq_preset(
            "Master Polish (Linear Phase)",
            &[
                low_cut(20.0, 48.0),
                low_shelf(60.0, 1.0),
                bell(300.0, -0.8, 0.7),
                high_shelf(14_000.0, 1.5),
            ],
            &linear,
        )
        .in_group(MIX),
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
        )
        .in_group(MIX),
        eq_preset(
            "Telephone",
            &[
                low_cut(400.0, 24.0),
                bell(1_500.0, 4.0, 1.0),
                high_cut(3_400.0, 24.0),
            ],
            &[],
        )
        .in_group(CREATIVE),
    ];
    // --- Vocals ---------------------------------------------------------------
    out.extend([
        // Harshness and sibilance come down only where and when they
        // stand out: the esses per frequency.
        eq_preset(
            "Vocal De-Harsh (Spectral)",
            &[
                low_cut(100.0, 18.0),
                bell(3_000.0, 0.0, 1.5).dynamic(-4.0),
                bell(7_500.0, 0.0, 1.2).dynamic(-6.0).spectral(),
            ],
            &[],
        )
        .in_group(VOCALS),
        // Backing vocals behind the lead: less low mid and presence,
        // softer top, the lead's range left to the lead.
        eq_preset(
            "Backing Vocals Tuck",
            &[
                low_cut(150.0, 24.0),
                bell(2_500.0, -2.0, 1.0),
                high_shelf(10_000.0, -1.5),
                high_cut(13_000.0, 12.0),
            ],
            &[],
        )
        .in_group(VOCALS),
        // Forward and bright, with the console's edge on the peaks.
        eq_preset(
            "Rap Vocal Bright",
            &[
                low_cut(90.0, 18.0),
                bell(200.0, -2.0, 1.2),
                bell(1_200.0, -1.5, 1.4),
                bell(5_000.0, 3.0, 1.0),
                high_shelf(11_000.0, 2.5),
            ],
            &character(Character::Console, 1.2),
        )
        .in_group(VOCALS),
        // Intimate and close: body, a touch of tube, no air.
        eq_preset(
            "Vocal Warm & Close",
            &[
                low_cut(70.0, 12.0),
                bell(160.0, 2.0, 1.0),
                bell(3_200.0, -1.5, 1.2),
                high_shelf(9_000.0, -2.0),
            ],
            &character(Character::Warm, 1.0),
        )
        .in_group(VOCALS),
    ]);
    // --- Drums ----------------------------------------------------------------
    out.extend([
        eq_preset(
            "Kick Tight",
            &[
                low_cut(35.0, 24.0),
                bell(55.0, 3.0, 1.2),
                bell(200.0, -3.0, 1.4),
                bell(4_000.0, 2.0, 1.2),
                high_cut(10_000.0, 12.0),
            ],
            &[],
        )
        .in_group(DRUMS),
        // The ring notched out, body and snap back in.
        eq_preset(
            "Snare Ring Notch",
            &[
                low_cut(60.0, 12.0),
                notch(850.0, 8.0),
                bell(180.0, 2.0, 1.4),
                bell(7_000.0, 2.0, 1.0),
            ],
            &[],
        )
        .in_group(DRUMS),
        // The kit as a whole: weight, less box, air, and the console
        // rounding the peaks.
        eq_preset(
            "Drum Bus Glue",
            &[
                low_shelf(80.0, 1.5),
                bell(350.0, -1.5, 1.0),
                high_shelf(8_000.0, 2.0),
            ],
            &character(Character::Console, 1.3),
        )
        .in_group(DRUMS),
        // Nothing below the hat, its harshness held down as it gets loud.
        eq_preset(
            "Hi-Hat Clean",
            &[
                low_cut(300.0, 24.0),
                bell(4_000.0, 0.0, 1.5).dynamic(-3.0),
                bell(8_000.0, 2.0, 1.0),
            ],
            &[],
        )
        .in_group(DRUMS),
        // Toms: boom shaped, the attack up.
        eq_preset(
            "Toms Attack",
            &[
                low_cut(50.0, 12.0),
                bell(100.0, 2.5, 1.2),
                bell(450.0, -3.5, 1.5),
                bell(5_000.0, 3.0, 1.2),
            ],
            &[],
        )
        .in_group(DRUMS),
    ]);
    // --- Bass -----------------------------------------------------------------
    out.extend([
        // The sides lose the lows; a little weight and growl in the middle.
        eq_preset(
            "Bass Mono Lows",
            &[
                low_cut(120.0, 24.0).on(Placement::Side),
                low_shelf(80.0, 1.0).on(Placement::Mid),
                bell(700.0, 1.5, 1.2),
            ],
            &[],
        )
        .in_group(BASS),
        // The sub comes under control as it gets loud, the top rolled
        // off, tape softening the rest.
        eq_preset(
            "808 Sub Control",
            &[
                low_shelf(50.0, 2.0),
                bell(100.0, 0.0, 1.0).dynamic(-4.0),
                high_cut(5_000.0, 18.0),
            ],
            &character(Character::Tape, 1.2),
        )
        .in_group(BASS),
        // A DI made to sound like an amplifier: cabinet roll-off, tube.
        eq_preset(
            "Bass Amp Tone",
            &[
                low_cut(40.0, 12.0),
                bell(120.0, 2.0, 1.0),
                bell(400.0, -2.0, 1.2),
                bell(1_800.0, 1.5, 1.5),
                high_cut(4_000.0, 24.0),
            ],
            &character(Character::Warm, 1.4),
        )
        .in_group(BASS),
    ]);
    // --- Guitars --------------------------------------------------------------
    out.extend([
        // The fizz over the amplifier's top rolled off, the harshness
        // held down when it gets loud.
        eq_preset(
            "Guitar Amp Fizz Tamer",
            &[
                low_cut(90.0, 18.0),
                bell(4_500.0, 0.0, 1.5).dynamic(-3.0),
                high_cut(7_500.0, 18.0),
            ],
            &[],
        )
        .in_group(GUITARS),
        // Doubled rhythm guitars: the middle cleared, the sides brighter.
        eq_preset(
            "Guitar Stereo Spread",
            &[
                low_cut(100.0, 18.0),
                bell(300.0, -2.0, 1.0).on(Placement::Mid),
                bell(3_000.0, 1.5, 1.0).on(Placement::Side),
                high_shelf(8_000.0, 1.0).on(Placement::Side),
            ],
            &[],
        )
        .in_group(GUITARS),
        // A clean electric: body, chime, a little tape.
        eq_preset(
            "Clean Guitar Chime",
            &[
                low_cut(80.0, 12.0),
                bell(250.0, 1.5, 1.0),
                bell(1_000.0, -1.5, 1.2),
                bell(3_500.0, 2.0, 1.0),
                high_shelf(9_000.0, 1.5),
            ],
            &character(Character::Tape, 1.0),
        )
        .in_group(GUITARS),
    ]);
    // --- Keys & Synths --------------------------------------------------------
    out.extend([
        // A pad behind everything: no lows, no top, the vocal's range out.
        eq_preset(
            "Synth Pad Bed",
            &[
                low_cut(200.0, 18.0),
                bell(2_500.0, -2.0, 0.8),
                high_cut(9_000.0, 12.0),
            ],
            &[],
        )
        .in_group(KEYS),
        eq_preset(
            "Rhodes Warmth",
            &[
                low_shelf(150.0, 2.0),
                bell(2_000.0, -1.5, 1.0),
                high_shelf(8_000.0, 1.5),
            ],
            &character(Character::Warm, 1.0),
        )
        .in_group(KEYS),
        eq_preset(
            "Synth Lead Cut-Through",
            &[
                low_cut(150.0, 18.0),
                bell(2_800.0, 3.0, 1.2),
                high_shelf(12_000.0, 1.0),
            ],
            &[],
        )
        .in_group(KEYS),
    ]);
    // --- Mix & Master ---------------------------------------------------------
    out.extend([
        // A gentle tilt towards the top with the console's rounding.
        eq_preset(
            "Mix Bus Tilt & Console",
            &[tilt(650.0, 1.0)],
            &character(Character::Console, 0.8),
        )
        .in_group(MIX),
        // Air at the very top, zero phase.
        eq_preset(
            "Mastering Air (Linear Phase)",
            &[
                low_cut(18.0, 24.0),
                bell(60.0, 0.5, 0.8),
                high_shelf(16_000.0, 1.5),
            ],
            &[(global::PHASE, 1.0), (global::QUALITY, 3.0)],
        )
        .in_group(MIX),
        // Width where it is wanted: the sides brighter and without lows.
        eq_preset(
            "Side Brightness",
            &[
                low_cut(120.0, 12.0).on(Placement::Side),
                high_shelf(8_000.0, 2.0).on(Placement::Side),
                bell(250.0, -1.0, 0.8).on(Placement::Mid),
            ],
            &[],
        )
        .in_group(MIX),
        // No bands: tape, oversampled.
        eq_preset(
            "Tape Warmth (Flat)",
            &[],
            &[
                (global::CHARACTER, Character::Tape.index() as f64),
                (global::CHARACTER_DRIVE, 1.3),
                (global::OVERSAMPLING, 1.0),
            ],
        )
        .in_group(MIX),
        // Parallel: a big low shelf heard at half strength.
        eq_preset(
            "Parallel Low Lift (50 % Mix)",
            &[low_shelf(100.0, 8.0), bell(300.0, -2.0, 1.0)],
            &[(global::MIX, 0.5)],
        )
        .in_group(MIX),
    ]);
    // --- Surgical -------------------------------------------------------------
    out.extend([
        eq_preset(
            "Hum Removal 50 Hz",
            &[notch(50.0, 30.0), notch(100.0, 30.0), notch(150.0, 30.0)],
            &[],
        )
        .in_group(SURGICAL),
        eq_preset(
            "Hum Removal 60 Hz",
            &[notch(60.0, 30.0), notch(120.0, 30.0), notch(180.0, 30.0)],
            &[],
        )
        .in_group(SURGICAL),
        // A wide spectral band over the mids: only resonances standing
        // out of their surroundings are pulled down.
        eq_preset(
            "Resonance Suppressor (Spectral)",
            &[bell(1_500.0, 0.0, 0.3).dynamic(-6.0).spectral()],
            &[],
        )
        .in_group(SURGICAL),
        eq_preset(
            "Rumble & Hiss",
            &[low_cut(40.0, 48.0), high_cut(16_000.0, 48.0)],
            &[],
        )
        .in_group(SURGICAL),
        // Boominess held down only when it builds up, with a threshold
        // of its own.
        eq_preset(
            "Boom Tamer",
            &[bell(120.0, 0.0, 1.2).dynamic(-6.0).at(-24.0)],
            &[],
        )
        .in_group(SURGICAL),
    ]);
    // --- Creative -------------------------------------------------------------
    out.extend([
        eq_preset(
            "Lo-Fi Radio",
            &[
                low_cut(300.0, 24.0),
                bell(2_000.0, 3.0, 1.0),
                high_cut(4_000.0, 24.0),
            ],
            &character(Character::Tape, 1.8),
        )
        .in_group(CREATIVE),
        eq_preset(
            "Underwater",
            &[low_shelf(120.0, 3.0), high_cut(700.0, 36.0)],
            &[],
        )
        .in_group(CREATIVE),
        eq_preset(
            "Megaphone",
            &[band_pass(1_800.0, 0.6), bell(2_500.0, 4.0, 1.2)],
            &[
                (global::CHARACTER, Character::Console.index() as f64),
                (global::CHARACTER_DRIVE, 2.0),
                (global::OUTPUT, 8.0),
            ],
        )
        .in_group(CREATIVE),
        // The top opened only where it is loud: a brightener per
        // frequency.
        eq_preset(
            "Spectral Exciter",
            &[high_shelf(6_000.0, 0.0).dynamic(5.0).spectral()],
            &[],
        )
        .in_group(CREATIVE),
    ]);
    out
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

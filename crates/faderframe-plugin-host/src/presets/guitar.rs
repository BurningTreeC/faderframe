//! Guitar Station rigs: an amplifier with its cabinet and microphones, and
//! the pedals that go with it, each trimmed (Output) to play at about the
//! level of the guitar going in (`preset_levels`).

use super::{FactoryPreset, preset};
use crate::devices::guitar::id::{self, *};
use faderframe_guitar::circuits::wah;
use faderframe_guitar::pedal::Stomp;
use faderframe_guitar::voice::Pedal;

// Amplifiers (`faderframe_guitar::chain::AMPS`).
const PLEXI: f64 = 0.0;
const BRIT45: f64 = 1.0;
const BRIT800: f64 = 3.0;
const BRIT2205: f64 = 4.0;
const AC30: f64 = 5.0;
const TWIN: f64 = 8.0;
const DELUXE: f64 = 9.0;
const JAZZ: f64 = 11.0;
const CALI_IIC: f64 = 12.0;
const RECTIFIER: f64 = 13.0;
const AMERICAN_5150: f64 = 14.0;
const SVT: f64 = 17.0;

// Cabinets (`faderframe_guitar::lists::CABINETS`).
const BRIT_1960: f64 = 0.0;
const CALI_OVERSIZED: f64 = 1.0;
const BRIT_GREEN: f64 = 3.0;
const BRIT_V30: f64 = 4.0;
const OVERSIZED: f64 = 5.0;
const AMERICAN_OPEN_212: f64 = 6.0;
const AMERICAN_OPEN_112: f64 = 7.0;
const JAZZ_OPEN_212: f64 = 10.0;
const AMERICAN_810: f64 = 12.0;

// Microphones (`faderframe_guitar::lists::MICS`).
const DYNAMIC_57: f64 = 2.0;
const DYNAMIC_421: f64 = 3.0;
const DYNAMIC_906: f64 = 4.0;
const RIBBON_121: f64 = 6.0;
const CONDENSER_87: f64 = 9.0;

fn stomp(s: Stomp) -> f64 {
    s.index() as f64
}

fn pedal(p: Pedal) -> f64 {
    stomp(Stomp::Pedal(p))
}

/// Place `s`'s field.
fn at(s: usize, field: u32) -> u32 {
    id::slot(s, field)
}

pub(super) fn guitar() -> Vec<FactoryPreset> {
    vec![
        preset(
            "Blackface Clean",
            &[
                (OUTPUT, 4.0),
                (AMP, TWIN),
                (DRIVE, 0.32),
                (BASS, 0.45),
                (TREBLE, 0.62),
                (REVERB, 0.3),
                (CABINET, AMERICAN_OPEN_212),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.25),
                (MIC_B, RIBBON_121),
                (B_POSITION, 0.5),
                (B_DISTANCE, 0.12),
                (BLEND, 0.4),
            ],
        ),
        preset(
            "Plexi Crunch",
            &[
                (OUTPUT, 8.0),
                (AMP, PLEXI),
                (DRIVE, 0.75),
                (MIDDLE, 0.65),
                (TREBLE, 0.6),
                (PRESENCE, 0.55),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
            ],
        ),
        // A screamer pushing the 800's preamp, drive down and level up.
        preset(
            "Boosted 800",
            &[
                (OUTPUT, 3.5),
                (at(0, STOMP), pedal(Pedal::Green808)),
                (at(0, P_DRIVE), 0.15),
                (at(0, P_LEVEL), 0.8),
                (at(0, TONE), 0.55),
                (AMP, BRIT800),
                (DRIVE, 0.65),
                (CABINET, BRIT_V30),
                (MIC_A, DYNAMIC_57),
                (MIC_B, RIBBON_121),
                (B_DISTANCE, 0.08),
                (BLEND, 0.45),
            ],
        ),
        preset(
            "Cali Lead",
            &[
                (OUTPUT, 12.5),
                (AMP, CALI_IIC),
                (DRIVE, 0.7),
                (GRAPHIC, 0.66),
                (GRAPHIC + 1, 0.46),
                (GRAPHIC + 2, 0.36),
                (GRAPHIC + 3, 0.52),
                (GRAPHIC + 4, 0.64),
                (CABINET, CALI_OVERSIZED),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.3),
                (MIC_B, CONDENSER_87),
                (B_DISTANCE, 0.3),
                (BLEND, 0.3),
            ],
        ),
        preset(
            "Modern High Gain",
            &[
                (OUTPUT, 14.5),
                (at(0, STOMP), pedal(Pedal::Green808)),
                (at(0, P_DRIVE), 0.0),
                (at(0, P_LEVEL), 0.7),
                (at(0, TONE), 0.6),
                (AMP, RECTIFIER),
                (DRIVE, 0.7),
                (CABINET, CALI_OVERSIZED),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.2),
                (MIC_B, DYNAMIC_906),
                (B_POSITION, 0.55),
                (BLEND, 0.45),
            ],
        ),
        preset(
            "5150 Rhythm",
            &[
                (OUTPUT, 15.0),
                (at(0, STOMP), pedal(Pedal::Green9)),
                (at(0, P_DRIVE), 0.1),
                (at(0, P_LEVEL), 0.7),
                (AMP, AMERICAN_5150),
                (DRIVE, 0.6),
                (PRESENCE, 0.6),
                (CABINET, OVERSIZED),
                (MIC_A, DYNAMIC_57),
                (MIC_B, DYNAMIC_421),
                (B_POSITION, 0.45),
                (BLEND, 0.4),
            ],
        ),
        preset(
            "Brit Chime",
            &[
                (OUTPUT, -3.5),
                (AMP, AC30),
                (DRIVE, 0.55),
                (TREBLE, 0.65),
                (CABINET, AMERICAN_OPEN_212),
                (MIC_A, RIBBON_121),
                (A_DISTANCE, 0.05),
            ],
        ),
        // Both speakers, one of them delayed, a microphone on each side.
        preset(
            "Jazz Chorus Clean",
            &[
                (OUTPUT, 15.5),
                (AMP, JAZZ),
                (DRIVE, 0.4),
                (CHORUS, 1.0),
                (CABINET, JAZZ_OPEN_212),
                (MIC_A, DYNAMIC_57),
                (A_PAN, -0.7),
                (MIC_B, CONDENSER_87),
                (B_PAN, 0.7),
            ],
        ),
        preset(
            "Fuzz and Wah",
            &[
                (OUTPUT, -4.0),
                (at(0, STOMP), stomp(Stomp::Wah(wah::Build::CryBaby))),
                (at(0, AUTO), 1.0),
                (at(0, SENSE), 0.6),
                (at(0, TREADLE), 0.25),
                (at(1, STOMP), pedal(Pedal::RoundFuzz)),
                (at(1, P_DRIVE), 0.85),
                (AMP, BRIT45),
                (DRIVE, 0.5),
                (CABINET, BRIT_GREEN),
                (MIC_A, RIBBON_121),
            ],
        ),
        preset(
            "Deluxe Shimmer",
            &[
                (OUTPUT, 2.0),
                (at(0, STOMP), pedal(Pedal::GoldDrive)),
                (at(0, P_DRIVE), 0.25),
                (at(0, P_LEVEL), 0.6),
                (at(1, STOMP), pedal(Pedal::BlueChorus)),
                (at(1, P_DRIVE), 0.3),
                (at(1, TONE), 0.55),
                (AMP, DELUXE),
                (DRIVE, 0.35),
                (REVERB, 0.25),
                (CABINET, AMERICAN_OPEN_112),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.3),
            ],
        ),
        preset(
            "Metal Zone",
            &[
                (OUTPUT, -5.0),
                (at(0, STOMP), pedal(Pedal::MetalZone)),
                (at(0, P_DRIVE), 0.7),
                (at(0, TONE), 0.7),
                (at(0, TONE + 1), 0.35),
                (at(0, TONE + 3), 0.65),
                (at(0, P_LEVEL), 0.55),
                (AMP, BRIT2205),
                (DRIVE, 0.45),
                (CABINET, BRIT_1960),
                (MIC_A, DYNAMIC_57),
            ],
        ),
        // The DI pedal into a big bass rig, the clean DI on output 2.
        preset(
            "Bass Rig",
            &[
                (OUTPUT, 14.0),
                (at(0, STOMP), pedal(Pedal::BassDriver)),
                (at(0, P_DRIVE), 0.3),
                (at(0, TONE + 4), 0.6),
                (AMP, SVT),
                (DRIVE, 0.5),
                (CABINET, AMERICAN_810),
                (MIC_A, DYNAMIC_421),
                (A_DISTANCE, 0.1),
            ],
        ),
    ]
}

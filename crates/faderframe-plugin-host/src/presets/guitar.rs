//! Guitar Station rigs: an amplifier with its cabinet and microphones, and
//! the pedals that go with it, each trimmed (Output) to play at about the
//! level of the guitar going in (`preset_levels`). The menu lists the rigs,
//! then "Amplifiers" (each amplifier's own presets) and "Sounds" (records'
//! guitar sounds, named for the record: the names say which record a
//! rig's research came from, not that anyone involved endorses it).

use super::{FactoryPreset, preset};
use crate::devices::guitar::id::{self, *};
use faderframe_guitar::circuits::wah;
use faderframe_guitar::pedal::Stomp;
use faderframe_guitar::voice::Pedal;

// Amplifiers (`faderframe_guitar::chain::AMPS`).
const PLEXI: f64 = 0.0;
const BRIT45: f64 = 1.0;
const PLEXI_BASS: f64 = 2.0;
const BRIT800: f64 = 3.0;
const BRIT2205: f64 = 4.0;
const AC30: f64 = 5.0;
const DR103: f64 = 6.0;
const BRUM100: f64 = 7.0;
const TWIN: f64 = 8.0;
const DELUXE: f64 = 9.0;
const DELUXE_NORMAL: f64 = 10.0;
const JAZZ: f64 = 11.0;
const CALI_IIC: f64 = 12.0;
const RECTIFIER: f64 = 13.0;
const AMERICAN_5150: f64 = 14.0;
const OREGON: f64 = 15.0;
const VT40: f64 = 16.0;
const SVT: f64 = 17.0;
const V4B: f64 = 18.0;
const RB800: f64 = 19.0;

// Power stages (`PowerAmp::ALL`): the amplifier's own is the default.
const BYPASS: f64 = 1.0;

// Cabinets (`faderframe_guitar::lists::CABINETS`).
const BRIT_1960: f64 = 0.0;
const CALI_OVERSIZED: f64 = 1.0;
const BRIT_CLOSED: f64 = 2.0;
const BRIT_GREEN: f64 = 3.0;
const BRIT_V30: f64 = 4.0;
const OVERSIZED: f64 = 5.0;
const AMERICAN_OPEN_212: f64 = 6.0;
const AMERICAN_OPEN_112: f64 = 7.0;
const CLOSED_112: f64 = 8.0;
const JAZZ_OPEN_212: f64 = 10.0;
const AMERICAN_CLOSED_412: f64 = 11.0;
const AMERICAN_810: f64 = 12.0;
const AMERICAN_410: f64 = 13.0;

// Speakers (`faderframe_guitar::lists::SPEAKERS`): the cabinet's own is
// the default.
const AMERICAN_VINTAGE_10: f64 = 6.0;
const AMERICAN_CERAMIC: f64 = 7.0;
const AMERICAN_ALNICO: f64 = 8.0;

// Microphones (`faderframe_guitar::lists::MICS`).
const DYNAMIC_57: f64 = 2.0;
const DYNAMIC_421: f64 = 3.0;
const DYNAMIC_906: f64 = 4.0;
const RIBBON_121: f64 = 6.0;
const RIBBON_160: f64 = 7.0;
const CONDENSER_87: f64 = 9.0;
const TUBE_CONDENSER_67: f64 = 11.0;

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
        // Each amplifier's own presets, after GainStageFx's catalogue (its
        // PRESETS.md and docs/models hold the research behind them).
        preset(
            "Boutique Lead",
            &[
                (OUTPUT, 6.6),
                (AMP, CALI_IIC),
                (DRIVE, 0.6),
                (BASS, 0.55),
                (MIDDLE, 0.35),
                (TREBLE, 0.7),
                (GRAPHIC, 0.62),
                (GRAPHIC + 1, 0.55),
                (GRAPHIC + 2, 0.35),
                (GRAPHIC + 3, 0.6),
                (GRAPHIC + 4, 0.58),
                (CABINET, CLOSED_112),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Boutique Rhythm",
            &[
                (OUTPUT, 7.6),
                (AMP, CALI_IIC),
                (DRIVE, 0.7),
                (MASTER, 0.55),
                (BASS, 0.6),
                (MIDDLE, 0.25),
                (TREBLE, 0.75),
                (GRAPHIC, 0.75),
                (GRAPHIC + 2, 0.1),
                (GRAPHIC + 3, 0.68),
                (GRAPHIC + 4, 0.7),
                (CABINET, CALI_OVERSIZED),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.25),
                (A_ANGLE, 10.0),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Blackface Clean",
            &[
                (OUTPUT, 7.5),
                (AMP, TWIN),
                (DRIVE, 0.24),
                (BASS, 0.6),
                (MIDDLE, 0.35),
                (TREBLE, 0.65),
                (REVERB, 0.3),
                (CABINET, AMERICAN_OPEN_212),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
                (MIC_B, RIBBON_121),
                (B_POSITION, 0.4),
                (B_DISTANCE, 0.3),
                (BLEND, 0.3),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Blackface Deluxe",
            &[
                (OUTPUT, 4.8),
                (AMP, DELUXE),
                (DRIVE, 0.3),
                (BASS, 0.55),
                (TREBLE, 0.65),
                (REVERB, 0.32),
                (CABINET, AMERICAN_OPEN_112),
                (SPEAKER, AMERICAN_CERAMIC),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Deluxe Breakup",
            &[
                (OUTPUT, 2.3),
                (AMP, DELUXE),
                (DRIVE, 0.62),
                (TREBLE, 0.6),
                (REVERB, 0.25),
                (INTENSITY, 0.8),
                (SPEED, 0.42),
                (CABINET, AMERICAN_OPEN_112),
                (SPEAKER, AMERICAN_CERAMIC),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.04),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Jazz Clean",
            &[
                (OUTPUT, 22.6),
                (AMP, JAZZ),
                (DRIVE, 0.16),
                (BRIGHT, 0.0),
                (CABINET, JAZZ_OPEN_212),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.05),
                (MIC_B, RIBBON_121),
                (B_DISTANCE, 0.25),
                (BLEND, 0.25),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Jazz Chorus",
            &[
                (OUTPUT, 22.6),
                (AMP, JAZZ),
                (DRIVE, 0.16),
                (CHORUS, 1.0),
                (BRIGHT, 0.0),
                (CABINET, JAZZ_OPEN_212),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.05),
                (MIC_B, RIBBON_121),
                (B_DISTANCE, 0.25),
                (BLEND, 0.25),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Blackface Normal",
            &[
                (OUTPUT, 6.5),
                (AMP, DELUXE_NORMAL),
                (DRIVE, 0.45),
                (BASS, 0.55),
                (TREBLE, 0.7),
                (CABINET, AMERICAN_OPEN_112),
                (SPEAKER, AMERICAN_CERAMIC),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.04),
            ],
        )
        .in_group("Amplifiers"),
        // Levelled by its peaks: the tremolo's dips pull the RMS down where the ear follows the swells.
        preset(
            "Blackface Throb",
            &[
                (OUTPUT, 4.7),
                (AMP, TWIN),
                (DRIVE, 0.17),
                (BASS, 0.55),
                (MIDDLE, 0.4),
                (TREBLE, 0.6),
                (REVERB, 0.38),
                (INTENSITY, 0.94),
                (CABINET, AMERICAN_OPEN_212),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.04),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Ultra Lead",
            &[
                (OUTPUT, 8.4),
                (AMP, AMERICAN_5150),
                (DRIVE, 0.9),
                (BASS, 0.7),
                (MIDDLE, 0.15),
                (TREBLE, 0.75),
                (CABINET, CALI_OVERSIZED),
                (MIC_A, DYNAMIC_57),
                (A_ANGLE, 15.0),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Ultra Rhythm",
            &[
                (OUTPUT, 9.4),
                (AMP, AMERICAN_5150),
                (DRIVE, 0.75),
                (BASS, 0.45),
                (TREBLE, 0.65),
                (CABINET, OVERSIZED),
                (MIC_A, DYNAMIC_57),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Ultra, Modern 33",
            &[
                (OUTPUT, 15.0),
                (at(0, STOMP), pedal(Pedal::Modern33)),
                (at(0, P_LEVEL), 0.8),
                (AMP, AMERICAN_5150),
                (DRIVE, 0.6),
                (BASS, 0.55),
                (MIDDLE, 0.45),
                (TREBLE, 0.6),
                (CABINET, BRIT_V30),
                (MIC_A, DYNAMIC_57),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Twin, Modern Purple",
            &[
                (OUTPUT, 8.9),
                (at(0, STOMP), pedal(Pedal::ModernPurple)),
                (at(0, P_DRIVE), 0.55),
                (at(0, TONE), 0.55),
                (at(0, TONE + 1), 0.45),
                (at(0, TONE + 2), 0.55),
                (at(0, TONE + 3), 0.9),
                (AMP, TWIN),
                (DRIVE, 0.3),
                (CABINET, BRIT_V30),
                (MIC_A, DYNAMIC_57),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Brit Crunch",
            &[
                (OUTPUT, 2.0),
                (AMP, BRIT800),
                (DRIVE, 0.45),
                (MIDDLE, 0.65),
                (TREBLE, 0.6),
                (CABINET, BRIT_1960),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Brit Lead",
            &[
                (OUTPUT, 10.5),
                (AMP, BRIT800),
                (DRIVE, 0.9),
                (BASS, 0.45),
                (MIDDLE, 0.8),
                (TREBLE, 0.7),
                (CABINET, BRIT_V30),
                (MIC_A, DYNAMIC_57),
                (A_ANGLE, 10.0),
                (MIC_B, RIBBON_121),
                (B_DISTANCE, 0.2),
                (BLEND, 0.3),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Plexi Crunch",
            &[
                (OUTPUT, 6.7),
                (AMP, PLEXI),
                (DRIVE, 0.4),
                (BASS, 0.45),
                (MIDDLE, 0.7),
                (TREBLE, 0.6),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Plexi Cranked",
            &[
                (OUTPUT, 10.9),
                (AMP, PLEXI),
                (DRIVE, 0.85),
                (BASS, 0.4),
                (MIDDLE, 0.75),
                (TREBLE, 0.55),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (MIC_B, RIBBON_121),
                (B_DISTANCE, 0.3),
                (BLEND, 0.3),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Plexi, Orange Phase",
            &[
                (OUTPUT, 6.4),
                (at(0, STOMP), pedal(Pedal::OrangePhase)),
                (at(0, P_DRIVE), 0.3),
                (at(0, TONE), 0.0),
                (AMP, PLEXI),
                (DRIVE, 0.8),
                (BASS, 0.4),
                (MIDDLE, 0.75),
                (TREBLE, 0.55),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Plexi, Cocked Wah",
            &[
                (OUTPUT, 12.7),
                (at(0, STOMP), stomp(Stomp::Wah(wah::Build::CryBaby))),
                (at(0, TREADLE), 0.6),
                (AMP, PLEXI),
                (DRIVE, 0.75),
                (BASS, 0.45),
                (MIDDLE, 0.6),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Twin, Blue Chorus",
            &[
                (OUTPUT, 5.0),
                (at(0, STOMP), pedal(Pedal::BlueChorus)),
                (at(0, P_DRIVE), 0.25),
                (AMP, TWIN),
                (DRIVE, 0.35),
                (TREBLE, 0.55),
                (CABINET, AMERICAN_OPEN_212),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Chime Clean",
            &[
                (OUTPUT, 14.9),
                (AMP, AC30),
                (DRIVE, 0.35),
                (TREBLE, 0.6),
                (CABINET, AMERICAN_OPEN_212),
                (SPEAKER, AMERICAN_ALNICO),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.4),
                (A_DISTANCE, 0.04),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Chime Edge",
            &[
                (OUTPUT, 13.9),
                (AMP, AC30),
                (DRIVE, 0.85),
                (BASS, 0.45),
                (TREBLE, 0.55),
                (CABINET, AMERICAN_OPEN_212),
                (SPEAKER, AMERICAN_ALNICO),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
                (MIC_B, RIBBON_160),
                (B_DISTANCE, 0.25),
                (BLEND, 0.3),
            ],
        )
        .in_group("Amplifiers"),
        // Levelled by its peaks: the wah opens on every attack.
        preset(
            "Chime, Auto Wah",
            &[
                (OUTPUT, 16.5),
                (at(0, STOMP), stomp(Stomp::Wah(wah::Build::V847))),
                (at(0, AUTO), 1.0),
                (at(0, TREADLE), 0.1),
                (AMP, AC30),
                (TREBLE, 0.55),
                (CABINET, AMERICAN_OPEN_212),
                (SPEAKER, AMERICAN_ALNICO),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.04),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Hi-Headroom Clean",
            &[
                (OUTPUT, 11.8),
                (AMP, DR103),
                (DRIVE, 0.45),
                (MASTER, 0.45),
                (BASS, 0.6),
                (MIDDLE, 0.55),
                (TREBLE, 0.6),
                (CABINET, BRIT_CLOSED),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.4),
                (A_DISTANCE, 0.03),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Hi-Headroom Pushed",
            &[
                (OUTPUT, 11.9),
                (AMP, DR103),
                (DRIVE, 0.8),
                (MASTER, 0.6),
                (BASS, 0.55),
                (MIDDLE, 0.6),
                (TREBLE, 0.65),
                (CABINET, BRIT_CLOSED),
                (MIC_A, DYNAMIC_57),
                (MIC_B, CONDENSER_87),
                (B_DISTANCE, 0.4),
                (BLEND, 0.25),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Recto Rhythm",
            &[
                (OUTPUT, 12.8),
                (AMP, RECTIFIER),
                (DRIVE, 0.55),
                (BASS, 0.55),
                (MIDDLE, 0.35),
                (TREBLE, 0.6),
                (CABINET, CALI_OVERSIZED),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.02),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Recto Lead",
            &[
                (OUTPUT, 13.1),
                (AMP, RECTIFIER),
                (DRIVE, 0.85),
                (MASTER, 0.55),
                (MIDDLE, 0.45),
                (TREBLE, 0.65),
                (CABINET, CALI_OVERSIZED),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.25),
                (MIC_B, DYNAMIC_421),
                (B_POSITION, 0.45),
                (B_DISTANCE, 0.03),
                (BLEND, 0.4),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Bass Head Crunch",
            &[
                (OUTPUT, 5.8),
                (AMP, PLEXI_BASS),
                (DRIVE, 0.7),
                (BASS, 0.35),
                (MIDDLE, 0.7),
                (TREBLE, 0.65),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Brit 45 Blues",
            &[
                (OUTPUT, 13.4),
                (AMP, BRIT45),
                (DRIVE, 0.7),
                (BASS, 0.4),
                (MIDDLE, 0.6),
                (TREBLE, 0.6),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Brum Crunch",
            &[
                (OUTPUT, 12.2),
                (AMP, BRUM100),
                (DRIVE, 0.6),
                (PRESENCE, 0.6),
                (BASS, 0.4),
                (MIDDLE, 0.7),
                (TREBLE, 0.65),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "Oregon Drive",
            &[
                (OUTPUT, 12.1),
                (AMP, OREGON),
                (DRIVE, 0.75),
                (MASTER, 0.65),
                (MIDDLE, 0.6),
                (TREBLE, 0.6),
                (CABINET, BRIT_CLOSED),
                (MIC_A, DYNAMIC_57),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "American SVT Grind",
            &[
                (OUTPUT, 10.2),
                (AMP, SVT),
                (DRIVE, 0.55),
                (BASS, 0.6),
                (MIDDLE, 0.6),
                (BRIGHT, 0.0),
                (CABINET, AMERICAN_810),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.05),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "American SVT, Bass Driver DI",
            &[
                (OUTPUT, 2.7),
                (at(0, STOMP), pedal(Pedal::BassDriver)),
                (at(0, P_DRIVE), 0.45),
                (at(0, TONE + 4), 1.0),
                (AMP, SVT),
                (DRIVE, 0.45),
                (BASS, 0.55),
                (BRIGHT, 0.0),
                (CABINET, AMERICAN_810),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.05),
                (DI_SOURCE, 1.0),
                (MIX, 0.5),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "American 800RB Clank",
            &[
                (OUTPUT, 5.3),
                (AMP, RB800),
                (DRIVE, 0.45),
                (BASS, 0.6),
                (TREBLE, 0.55),
                (SWEEP, 0.6),
                (MID_SWITCH, 0.0),
                (CABINET, AMERICAN_410),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.05),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "American VT-40 Crunch",
            &[
                (OUTPUT, 8.6),
                (AMP, VT40),
                (DRIVE, 0.7),
                (MIDDLE, 0.65),
                (TREBLE, 0.6),
                (BRIGHT, 0.0),
                (CABINET, AMERICAN_410),
                (SPEAKER, AMERICAN_VINTAGE_10),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
                (HORN, 0.0),
            ],
        )
        .in_group("Amplifiers"),
        preset(
            "American V-4B Growl",
            &[
                (OUTPUT, 13.4),
                (AMP, V4B),
                (DRIVE, 0.6),
                (BASS, 0.6),
                (MIDDLE, 0.6),
                (BRIGHT, 0.0),
                (CABINET, AMERICAN_810),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.05),
            ],
        )
        .in_group("Amplifiers"),
        // Records' guitar sounds, rebuilt from what their engineers and
        // players said they used (GainStageFx's PRESETS.md: the evidence for
        // each stage, and what is approximated). Oldest first.
        // A Fuzz Face into a Marshall Super 100 and 4x12s; a Beyer M160 on the cone
        // with a U67 (Eddie Kramer).
        preset(
            "The Jimi Hendrix Experience – Are You Experienced (1967)",
            &[
                (OUTPUT, -1.1),
                (at(0, STOMP), pedal(Pedal::RoundFuzz)),
                (at(0, P_DRIVE), 0.85),
                (at(0, P_LEVEL), 0.6),
                (AMP, PLEXI),
                (DRIVE, 0.7),
                (MIDDLE, 0.6),
                (TREBLE, 0.6),
                (CABINET, BRIT_GREEN),
                (MIC_A, RIBBON_160),
                (A_DISTANCE, 0.05),
                (MIC_B, TUBE_CONDENSER_67),
                (B_DISTANCE, 0.5),
                (BLEND, 0.35),
            ],
        )
        .in_group("Sounds"),
        // A Rangemaster treble booster turned up into a 100 W Laney Supergroup,
        // "presence, middle and treble on 10 with no bass whatsoever" (Iommi). The
        // booster's modification is not documented: this is the stock unit.
        preset(
            "Black Sabbath – Paranoid (1970)",
            &[
                (OUTPUT, 14.0),
                (at(0, STOMP), pedal(Pedal::TrebleBoost)),
                (at(0, P_LEVEL), 1.0),
                (AMP, BRUM100),
                (DRIVE, 0.8),
                (PRESENCE, 1.0),
                (BASS, 0.0),
                (MIDDLE, 1.0),
                (TREBLE, 1.0),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.03),
            ],
        )
        .in_group("Sounds"),
        // A stock 1967/68 Marshall 1959 Super Lead with everything up, run from a
        // Variac at 80-85 V (Donn Landee): Mains 70 %.
        preset(
            "Van Halen – Van Halen (1978)",
            &[
                (OUTPUT, 13.4),
                (AMP, PLEXI),
                (DRIVE, 1.0),
                (BASS, 0.55),
                (MIDDLE, 0.65),
                (TREBLE, 0.7),
                (MAINS, 3.0),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.02),
                (MIC_B, CONDENSER_87),
                (B_DISTANCE, 0.9),
                (B_ANGLE, 30.0),
                (BLEND, 0.3),
            ],
        )
        .in_group("Sounds"),
        // A Big Muff with the sustain well down into a Hiwatt DR103 (the Alembic
        // preamp in front of it is not modelled).
        preset(
            "Pink Floyd – The Wall (1979)",
            &[
                (OUTPUT, 8.9),
                (at(0, STOMP), pedal(Pedal::BigMuff)),
                (at(0, P_DRIVE), 0.35),
                (at(0, P_LEVEL), 0.75),
                (at(0, TONE), 0.55),
                (AMP, DR103),
                (DRIVE, 0.45),
                (BASS, 0.6),
                (MIDDLE, 0.55),
                (TREBLE, 0.6),
                (CABINET, BRIT_CLOSED),
                (MIC_A, DYNAMIC_421),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.04),
                (MIC_B, CONDENSER_87),
                (B_DISTANCE, 0.5),
                (BLEND, 0.3),
            ],
        )
        .in_group("Sounds"),
        // Marshall heads only as loud as needed, no pedal on the rhythm tracks, a
        // U67 and a U87 on different speakers (Tony Platt).
        preset(
            "AC/DC – Back in Black (1980)",
            &[
                (OUTPUT, 8.5),
                (AMP, PLEXI),
                (DRIVE, 0.55),
                (BASS, 0.4),
                (MIDDLE, 0.7),
                (TREBLE, 0.6),
                (CABINET, BRIT_1960),
                (MIC_A, TUBE_CONDENSER_67),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.1),
                (MIC_B, CONDENSER_87),
                (B_POSITION, 0.6),
                (B_DISTANCE, 0.15),
            ],
        )
        .in_group("Sounds"),
        // An MXR Distortion+ that was nearly always on, into a rented mid-70s 1959
        // Super Lead with the voltage turned down: Mains 80 %.
        preset(
            "Ozzy Osbourne – Blizzard of Ozz (1980)",
            &[
                (OUTPUT, 11.8),
                (at(0, STOMP), pedal(Pedal::YellowDist)),
                (at(0, P_DRIVE), 0.6),
                (at(0, P_LEVEL), 0.7),
                (AMP, PLEXI),
                (DRIVE, 0.85),
                (MIDDLE, 0.6),
                (TREBLE, 0.7),
                (MAINS, 2.0),
                (CABINET, BRIT_CLOSED),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.02),
                (MIC_B, CONDENSER_87),
                (B_DISTANCE, 0.6),
                (BLEND, 0.3),
            ],
        )
        .in_group("Sounds"),
        // A Tube Screamer with low drive and high level into a blackface Fender (a
        // Vibroverb; the Twin is the same AB763 family), an SM57.
        preset(
            "Stevie Ray Vaughan – Texas Flood (1983)",
            &[
                (OUTPUT, -11.2),
                (at(0, STOMP), pedal(Pedal::Green808)),
                (at(0, P_DRIVE), 0.25),
                (at(0, P_LEVEL), 0.85),
                (at(0, TONE), 0.55),
                (AMP, TWIN),
                (DRIVE, 0.55),
                (BASS, 0.45),
                (MIDDLE, 0.6),
                (TREBLE, 0.55),
                (REVERB, 0.12),
                (CABINET, AMERICAN_OPEN_112),
                (SPEAKER, AMERICAN_CERAMIC),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.25),
                (A_DISTANCE, 0.03),
            ],
        )
        .in_group("Sounds"),
        // The same Super Lead on the Variac; a ribbon further out this time.
        preset(
            "Van Halen – 1984 (1984)",
            &[
                (OUTPUT, 14.9),
                (AMP, PLEXI),
                (DRIVE, 1.0),
                (MIDDLE, 0.7),
                (TREBLE, 0.75),
                (MAINS, 3.0),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.03),
                (MIC_B, RIBBON_121),
                (B_DISTANCE, 0.4),
                (BLEND, 0.35),
            ],
        )
        .in_group("Sounds"),
        // A Mesa/Boogie Mark IIC+ lead channel through its own power stage (Flemming
        // Rasmussen), a V on the graphic, two SM57s in the cone and a tube condenser
        // out at 45°. The 808 ahead of it only tightens.
        preset(
            "Metallica – Master of Puppets (1986)",
            &[
                (OUTPUT, 8.7),
                (at(0, STOMP), pedal(Pedal::Green808)),
                (at(0, P_DRIVE), 0.15),
                (at(0, P_LEVEL), 0.85),
                (AMP, CALI_IIC),
                (DRIVE, 0.8),
                (MASTER, 0.55),
                (BASS, 0.2),
                (MIDDLE, 0.35),
                (TREBLE, 0.75),
                (GRAPHIC, 0.65),
                (GRAPHIC + 1, 0.45),
                (GRAPHIC + 2, 0.25),
                (GRAPHIC + 3, 0.65),
                (GRAPHIC + 4, 0.7),
                (CABINET, BRIT_CLOSED),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.2),
                (MIC_B, TUBE_CONDENSER_67),
                (B_DISTANCE, 1.0),
                (B_ANGLE, 45.0),
                (BLEND, 0.25),
                (INPUT, 3.0),
            ],
        )
        .in_group("Sounds"),
        // A Boss HM-2 with every knob at ten into a small clean combo (a solid-state
        // Peavey, not modelled: the blackface channel with its power stage
        // bypassed), tuned to B.
        preset(
            "Entombed – Left Hand Path (1990)",
            &[
                (OUTPUT, -1.0),
                (at(0, STOMP), pedal(Pedal::HeavyMetal)),
                (at(0, P_DRIVE), 1.0),
                (at(0, TONE), 1.0),
                (at(0, TONE + 1), 1.0),
                (AMP, TWIN),
                (POWER, BYPASS),
                (DRIVE, 0.3),
                (BASS, 0.45),
                (TREBLE, 0.55),
                (CABINET, CLOSED_112),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.25),
                (A_DISTANCE, 0.03),
                (INPUT, 2.0),
            ],
        )
        .in_group("Sounds"),
        // A JCM800 2205's boost channel with the settings Tom Morello never changed
        // (Gain 9, Treble 7, Middle 10, Bass 10, Presence 7) into a Peavey 4x12 with
        // G12K-85s, EMG pickups.
        preset(
            "Rage Against the Machine – Rage Against the Machine (1992)",
            &[
                (OUTPUT, 1.8),
                (AMP, BRIT2205),
                (DRIVE, 0.9),
                (PRESENCE, 0.7),
                (BASS, 1.0),
                (MIDDLE, 1.0),
                (TREBLE, 0.7),
                (CABINET, AMERICAN_CLOSED_412),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.25),
                (A_DISTANCE, 0.02),
                (MIC_B, DYNAMIC_421),
                (B_POSITION, 0.4),
                (B_DISTANCE, 0.03),
                (BLEND, 0.35),
                (INPUT, 3.0),
            ],
        )
        .in_group("Sounds"),
        // A Big Muff into the low input of a JCM800 2203 with the master all the way
        // up and the preamp gain doing the adjusting (the op-amp Muff is
        // approximated by the Ram's Head).
        preset(
            "The Smashing Pumpkins – Siamese Dream (1993)",
            &[
                (OUTPUT, -15.5),
                (at(0, STOMP), pedal(Pedal::BigMuff)),
                (at(0, P_DRIVE), 0.7),
                (at(0, P_LEVEL), 0.55),
                (at(0, TONE), 0.45),
                (AMP, BRIT800),
                (DRIVE, 0.35),
                (MASTER, 1.0),
                (MIDDLE, 0.6),
                (TREBLE, 0.6),
                (CABINET, BRIT_1960),
                (MIC_A, DYNAMIC_57),
                (A_POSITION, 0.35),
                (A_DISTANCE, 0.02),
                (MIC_B, RIBBON_121),
                (B_POSITION, 0.45),
                (B_DISTANCE, 0.15),
                (BLEND, 0.35),
            ],
        )
        .in_group("Sounds"),
        // An HM-2 into an MT-2, the second "to tighten up the sound"; the amplifier
        // is not documented: the blackface channel, power stage bypassed, as clean
        // as it goes.
        preset(
            "At the Gates – Slaughter of the Soul (1995)",
            &[
                (OUTPUT, 3.9),
                (at(0, STOMP), pedal(Pedal::HeavyMetal)),
                (at(0, P_DRIVE), 0.85),
                (at(0, P_LEVEL), 0.25),
                (at(0, TONE), 0.75),
                (at(0, TONE + 1), 0.8),
                (at(1, STOMP), pedal(Pedal::MetalZone)),
                (at(1, P_DRIVE), 0.55),
                (at(1, TONE), 0.45),
                (at(1, TONE + 1), 0.6),
                (at(1, TONE + 3), 0.65),
                (AMP, TWIN),
                (POWER, BYPASS),
                (DRIVE, 0.3),
                (BASS, 0.45),
                (TREBLE, 0.55),
                (CABINET, BRIT_CLOSED),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
                (MIC_B, DYNAMIC_421),
                (B_POSITION, 0.45),
                (B_DISTANCE, 0.08),
                (BLEND, 0.3),
            ],
        )
        .in_group("Sounds"),
        // John Frusciante's '65 Marshall (a JTM45) for most of it; the 200 W bass
        // head he blended with it is not represented. Two SM57s and two U87s went
        // down on one track (Jim Scott): a 57 close, an 87 at 30 cm, half and half.
        preset(
            "Red Hot Chili Peppers – Californication (1999)",
            &[
                (OUTPUT, 17.3),
                (AMP, BRIT45),
                (DRIVE, 0.6),
                (BASS, 0.4),
                (MIDDLE, 0.6),
                (TREBLE, 0.65),
                (CABINET, BRIT_GREEN),
                (MIC_A, DYNAMIC_57),
                (MIC_B, CONDENSER_87),
                (B_POSITION, 0.4),
                (B_DISTANCE, 0.3),
            ],
        )
        .in_group("Sounds"),
        // An Ampeg VT-40 with its "nasal, almost cocked-wah mid-range", the mids
        // pushed hard, its own four 10s (the transistor Peaveys blended in are not
        // represented).
        preset(
            "Queens of the Stone Age – Songs for the Deaf (2002)",
            &[
                (OUTPUT, 8.9),
                (AMP, VT40),
                (DRIVE, 0.6),
                (BASS, 0.45),
                (MIDDLE, 0.85),
                (TREBLE, 0.6),
                (BRIGHT, 0.0),
                (CABINET, AMERICAN_410),
                (SPEAKER, AMERICAN_VINTAGE_10),
                (MIC_A, DYNAMIC_57),
                (A_DISTANCE, 0.03),
                (HORN, 0.0),
            ],
        )
        .in_group("Sounds"),
    ]
}

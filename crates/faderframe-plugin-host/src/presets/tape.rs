//! The Tape Machine's presets: what a deck is reached for — a mix bus or
//! a master printed at 30 ips, drums and bass hit hard at 15, the darker
//! 7½, the bias turned either way, a tired machine and a lo-fi 3¾ on an
//! old tape. Auto Output is on in all but the parallel one, so pushing the
//! input saturates the tape without making it louder.

use super::{FactoryPreset, preset};

pub(super) fn tape_machine() -> Vec<FactoryPreset> {
    use crate::devices::tape::id::*;
    // Speeds, tapes, levels, formats.
    const IPS_3: f64 = 0.0;
    const IPS_7: f64 = 1.0;
    const IPS_15: f64 = 2.0;
    const IPS_30: f64 = 3.0;
    const HIGH_OUTPUT: f64 = 1.0;
    const VINTAGE: f64 = 2.0;
    const L185: f64 = 0.0;
    const L320: f64 = 2.0;
    const L370: f64 = 3.0;
    const L520: f64 = 4.0;
    const IEC: f64 = 1.0;
    const HALF_INCH: f64 = 1.0;
    vec![
        // 30 ips on ½″: the low end round, the top open, a little glue.
        preset(
            "Mix Bus Glue at 30 ips",
            &[
                (SPEED, IPS_30),
                (FORMAT, HALF_INCH),
                (INPUT, 3.0),
                (FLUTTER, 0.8),
            ],
        ),
        // Mastering: a modern high-output tape at +6, ½″, gently.
        preset(
            "Master to ½-inch",
            &[
                (SPEED, IPS_30),
                (FORMAT, HALF_INCH),
                (TAPE, HIGH_OUTPUT),
                (LEVEL, L370),
                (INPUT, 2.0),
                (FLUTTER, 0.6),
                (OVERSAMPLING, 1.0),
            ],
        ),
        // Drums driven into 15 ips: transients rounded, the room up.
        preset(
            "Drums Hot to 15 ips",
            &[(SPEED, IPS_15), (INPUT, 8.0), (FLUTTER, 1.0)],
        ),
        // Bass at 15 ips: the head bump under it, the peaks held.
        preset(
            "Bass Thickener",
            &[(SPEED, IPS_15), (INPUT, 5.0), (LEVEL, L320)],
        ),
        // A voice at 15 ips IEC: a little softer on top, smoothly limited.
        preset(
            "Vocal Warmth (IEC)",
            &[(SPEED, IPS_15), (EQ, IEC), (INPUT, 4.0), (BIAS, 0.5)],
        ),
        // Barely there: hot tape at its rated level, the input down.
        preset(
            "Gentle Colour",
            &[
                (TAPE, HIGH_OUTPUT),
                (LEVEL, L185),
                (INPUT, -2.0),
                (FLUTTER, 0.7),
            ],
        ),
        // 7½ ips pushed: the treble saturates first, darker and thicker.
        preset(
            "Pushed 7½ ips",
            &[(SPEED, IPS_7), (INPUT, 6.0), (FLUTTER, 1.0)],
        ),
        // Under-biased: crunchier, brighter, the quiet parts grainy.
        preset(
            "Under-Biased Grit",
            &[(SPEED, IPS_15), (BIAS, -3.5), (INPUT, 4.0)],
        ),
        // Over-biased: cleaner, darker, softer transients.
        preset(
            "Over-Biased Smooth",
            &[(SPEED, IPS_15), (BIAS, 2.5), (INPUT, 5.0)],
        ),
        // Inside the unit: the driven tape under the dry signal.
        preset(
            "Parallel Saturation",
            &[
                (SPEED, IPS_15),
                (INPUT, 12.0),
                (AUTO_OUTPUT, 0.0),
                (OUTPUT, -8.0),
                (MIX, 0.45),
            ],
        ),
        // A modern +9 tape printed hot at 30 ips.
        preset(
            "Hot +9 Print",
            &[
                (SPEED, IPS_30),
                (TAPE, HIGH_OUTPUT),
                (LEVEL, L520),
                (INPUT, 4.0),
            ],
        ),
        // A tired deck: more wow and flutter, the azimuth off, hiss.
        preset(
            "Tired Machine",
            &[
                (SPEED, IPS_7),
                (FLUTTER, 3.5),
                (AZIMUTH, 7.0),
                (HISS, 1.0),
                (INPUT, 3.0),
            ],
        ),
        // 3¾ ips on an old thin tape, under-biased, wobbling, hissing.
        preset(
            "Lo-Fi 3¾ ips",
            &[
                (SPEED, IPS_3),
                (TAPE, VINTAGE),
                (LEVEL, L185),
                (BIAS, -2.0),
                (FLUTTER, 2.0),
                (HISS, 1.0),
                (INPUT, 2.0),
            ],
        ),
    ]
}

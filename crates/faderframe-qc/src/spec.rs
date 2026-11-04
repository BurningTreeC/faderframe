//! Delivery specifications as presets: what a destination asks of a file
//! (loudness target and tolerance, true-peak ceiling, sample rate, bit
//! depth, channels, length). Each rule says whether missing it fails the
//! delivery or only warns: broadcast loudness is a requirement, a
//! streaming service's target is where it turns the file to.
//!
//! What a spec measures differently from what FaderFrame measures is said
//! with the rule ([`LoudnessRule::approximate`]): Netflix's loudness is
//! dialogue-gated (Dolby Dialogue Intelligence), FaderFrame's programme
//! loudness is not.

/// What a spec asks of a programme's integrated loudness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessRule {
    /// LUFS (LKFS).
    pub target: f64,
    /// ± LU around the target.
    pub tolerance: f64,
    /// Outside the tolerance fails (else warns: the destination turns it
    /// to its target).
    pub fails: bool,
    /// Why the measurement only approximates the spec's (`None`: it is
    /// the spec's measurement, BS.1770-4 gated).
    pub approximate: Option<&'static str>,
}

/// A destination's requirements (see the module).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    /// Stable id (`qc-report:<path>@<id>`, the JSON).
    pub id: &'static str,
    pub name: &'static str,
    /// A few characters for a button ("EBU R128").
    pub short: &'static str,
    /// Where the numbers come from.
    pub source: &'static str,
    pub loudness: Option<LoudnessRule>,
    /// Highest true peak (dBTP).
    pub true_peak: Option<f64>,
    /// A true peak over it fails (else warns).
    pub true_peak_fails: bool,
    /// Sample rates taken (empty: any).
    pub sample_rates: &'static [u32],
    /// Bit depths taken, 32 meaning floating point (empty: any).
    pub bits: &'static [u16],
    /// A rate or depth the spec does not take fails (else warns).
    pub format_fails: bool,
    pub channels: Option<usize>,
    /// Shortest and longest a programme may be (seconds).
    pub min_seconds: Option<f64>,
    pub max_seconds: Option<f64>,
    /// Samples at full scale fail (a master for pressing).
    pub no_full_scale: bool,
    /// Said under the spec in the report.
    pub notes: &'static [&'static str],
}

const NONE: Spec = Spec {
    id: "none",
    name: "General checks only",
    short: "General",
    source: "",
    loudness: None,
    true_peak: None,
    true_peak_fails: false,
    sample_rates: &[],
    bits: &[],
    format_fails: false,
    channels: None,
    min_seconds: None,
    max_seconds: None,
    no_full_scale: false,
    notes: &[],
};

const BROADCAST_RATES: &[u32] = &[48_000];
const BROADCAST_BITS: &[u16] = &[24, 16, 32];

/// The presets (the first: no destination, general checks only).
pub const SPECS: [Spec; 11] = [
    NONE,
    Spec {
        id: "ebu-r128",
        short: "EBU R128",
        name: "EBU R128 (broadcast, −23 LUFS)",
        source: "EBU R 128 (2020): programme loudness −23.0 LUFS ±0.5 LU, maximum true peak \
                 −1 dBTP; EBU Tech 3343",
        loudness: Some(LoudnessRule {
            target: -23.0,
            tolerance: 0.5,
            fails: true,
            approximate: None,
        }),
        true_peak: Some(-1.0),
        true_peak_fails: true,
        sample_rates: BROADCAST_RATES,
        bits: BROADCAST_BITS,
        format_fails: false,
        notes: &["Live programmes may deviate by ±1.0 LU (R 128); this check uses ±0.5 LU."],
        ..NONE
    },
    Spec {
        id: "atsc-a85",
        short: "ATSC A/85",
        name: "ATSC A/85 (US broadcast, −24 LKFS)",
        source: "ATSC A/85:2013 (CALM Act): −24 LKFS ±2 dB, true peak at most −2 dBTP",
        loudness: Some(LoudnessRule {
            target: -24.0,
            tolerance: 2.0,
            fails: true,
            approximate: None,
        }),
        true_peak: Some(-2.0),
        true_peak_fails: true,
        sample_rates: BROADCAST_RATES,
        bits: BROADCAST_BITS,
        notes: &[
            "A/85 measures the anchor element (dialogue) where there is one; this is the \
                  programme loudness.",
        ],
        ..NONE
    },
    Spec {
        id: "netflix",
        short: "Netflix",
        name: "Netflix (programme, −27 LKFS)",
        source: "Netflix Sound Mix Specifications: −27 LKFS ±2 LU dialogue-gated, true peak \
                 −2 dBTP, 48 kHz 24-bit",
        loudness: Some(LoudnessRule {
            target: -27.0,
            tolerance: 2.0,
            fails: false,
            approximate: Some(
                "Netflix measures dialogue-gated loudness (Dolby Dialogue Intelligence); \
                 FaderFrame measures programme loudness without dialogue gating, so this check \
                 warns but never fails",
            ),
        }),
        true_peak: Some(-2.0),
        true_peak_fails: true,
        sample_rates: BROADCAST_RATES,
        bits: &[24],
        format_fails: true,
        notes: &[
            "Dialogue gating is not measured: the loudness is the programme's (BS.1770-4 \
                  gated). Check dialogue loudness with a dialogue-gated meter before delivery.",
        ],
        ..NONE
    },
    Spec {
        id: "streaming",
        short: "Streaming",
        name: "Streaming · Spotify, YouTube, Tidal (−14 LUFS)",
        source: "Spotify, YouTube and Tidal normalise to −14 LUFS; Spotify asks for at most \
                 −1 dBTP (−2 dBTP above −14 LUFS)",
        loudness: Some(LoudnessRule {
            target: -14.0,
            tolerance: 1.0,
            fails: false,
            approximate: None,
        }),
        true_peak: Some(-1.0),
        true_peak_fails: false,
        notes: &[
            "A louder master is turned down to the target, a quieter one played quieter \
                  (Spotify raises it only as far as −1 dBTP allows).",
        ],
        ..NONE
    },
    Spec {
        id: "apple-music",
        short: "Apple Music",
        name: "Apple Music (−16 LUFS)",
        source: "Apple Music Sound Check normalises to −16 LUFS; Apple Digital Masters ask for \
                 24-bit, at most −1 dBTP",
        loudness: Some(LoudnessRule {
            target: -16.0,
            tolerance: 1.0,
            fails: false,
            approximate: None,
        }),
        true_peak: Some(-1.0),
        true_peak_fails: false,
        bits: &[24, 32],
        ..NONE
    },
    Spec {
        id: "amazon-music",
        short: "Amazon",
        name: "Amazon Music (−14 LUFS, −2 dBTP)",
        source: "Amazon Music normalises to −14 LUFS (−11 to −14 recommended), true peak at most \
                 −2 dBTP",
        loudness: Some(LoudnessRule {
            target: -14.0,
            tolerance: 1.0,
            fails: false,
            approximate: None,
        }),
        true_peak: Some(-2.0),
        true_peak_fails: false,
        ..NONE
    },
    Spec {
        id: "cd",
        short: "CD",
        name: "CD (Red Book: 44.1 kHz, 16-bit, stereo)",
        source: "IEC 60908 (Red Book): 44.1 kHz, 16-bit, two channels; a track at least 4 s, a \
                 disc at most 79:57",
        true_peak: Some(0.0),
        true_peak_fails: false,
        sample_rates: &[44_100],
        bits: &[16],
        format_fails: true,
        channels: Some(2),
        min_seconds: Some(4.0),
        max_seconds: Some(79.0 * 60.0 + 57.0),
        no_full_scale: true,
        notes: &[
            "Pressing plants reject masters with runs of full-scale samples; inter-sample \
                  peaks over 0 dBTP distort in some players.",
        ],
        ..NONE
    },
    Spec {
        id: "arib-tr-b32",
        short: "ARIB",
        name: "ARIB TR-B32 (Japan broadcast, −24 LKFS)",
        source: "ARIB TR-B32: −24.0 LKFS ±1 dB, true peak at most −1 dBTP",
        loudness: Some(LoudnessRule {
            target: -24.0,
            tolerance: 1.0,
            fails: true,
            approximate: None,
        }),
        true_peak: Some(-1.0),
        true_peak_fails: true,
        sample_rates: BROADCAST_RATES,
        bits: BROADCAST_BITS,
        ..NONE
    },
    Spec {
        id: "op-59",
        short: "OP-59",
        name: "OP-59 (Australian TV, −24 LKFS)",
        source: "Free TV Australia OP-59: −24 LKFS ±1 LU, true peak at most −2 dBTP",
        loudness: Some(LoudnessRule {
            target: -24.0,
            tolerance: 1.0,
            fails: true,
            approximate: None,
        }),
        true_peak: Some(-2.0),
        true_peak_fails: true,
        sample_rates: BROADCAST_RATES,
        bits: BROADCAST_BITS,
        ..NONE
    },
    Spec {
        id: "ebu-r128-s1",
        short: "R128 s1",
        name: "EBU R128 s1 (short-form, −23 LUFS)",
        source: "EBU R 128 s1: short-form content (adverts, trailers) −23.0 LUFS ±0.5 LU, true \
                 peak −1 dBTP, maximum short-term loudness −18 LUFS recommended",
        loudness: Some(LoudnessRule {
            target: -23.0,
            tolerance: 0.5,
            fails: true,
            approximate: None,
        }),
        true_peak: Some(-1.0),
        true_peak_fails: true,
        sample_rates: BROADCAST_RATES,
        bits: BROADCAST_BITS,
        notes: &[
            "The maximum short-term loudness of −18 LUFS is a recommendation: over it \
                  warns.",
        ],
        ..NONE
    },
];

/// The short-term ceiling R128 s1 recommends (LUFS).
pub const SHORT_FORM_MAX_SHORT_TERM: f64 = -18.0;

impl Spec {
    /// The preset with this id (`None` for an unknown one).
    pub fn by_id(id: &str) -> Option<&'static Spec> {
        SPECS.iter().find(|s| s.id.eq_ignore_ascii_case(id.trim()))
    }

    /// No destination: general checks only.
    pub fn none() -> &'static Spec {
        &SPECS[0]
    }

    pub fn is_none(&self) -> bool {
        self.id == "none"
    }

    /// The highest short-term loudness recommended (R128 s1 only).
    pub fn max_short_term(&self) -> Option<f64> {
        (self.id == "ebu-r128-s1").then_some(SHORT_FORM_MAX_SHORT_TERM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_found_by_their_ids() {
        let mut ids: Vec<&str> = SPECS.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SPECS.len(), "ids are unique");
        assert_eq!(Spec::by_id("EBU-R128").map(|s| s.name), Some(SPECS[1].name));
        assert!(Spec::by_id("nothing").is_none());
        assert!(Spec::none().is_none());
        // Netflix's loudness is approximate and never fails.
        let n = Spec::by_id("netflix").and_then(|s| s.loudness).unwrap();
        assert!(n.approximate.is_some() && !n.fails);
    }
}

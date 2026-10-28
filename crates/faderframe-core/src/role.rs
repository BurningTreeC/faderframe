//! What a track plays in the mix, as the user says it does.
//!
//! Mix Intelligence (`faderframe-mix-intelligence`) reads roles to know
//! which source of a pair is the one that should come through (a lead
//! vocal over its accompaniment, a kick against its bass). A role is only
//! ever assigned by the user: track names are not read as instruments.
//! Tracks without a role are analysed neutrally.

use serde::{Deserialize, Serialize};

/// A track's part in the mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackRole {
    Kick,
    Snare,
    /// The rest of a drum kit: toms, overheads, room microphones.
    Drums,
    Percussion,
    Bass,
    LeadVocal,
    BackingVocals,
    /// A lead guitar, synth or solo instrument (in front, like a voice).
    LeadInstrument,
    RhythmGuitar,
    Keys,
    /// Pads, strings, sustained beds.
    Pads,
    /// Effects, atmospheres, risers.
    Effects,
    Other,
}

/// Where a role belongs (what the masking rules weigh pairs by).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoleFamily {
    /// Parts of one drum kit (picked up by several microphones).
    DrumKit,
    /// The low end: the bass (the kick is also in the kit).
    LowEnd,
    /// What should stand in front: lead vocal, lead instrument.
    Foreground,
    /// Everything that accompanies.
    Accompaniment,
    /// Effects and atmospheres (masking is often what they are for).
    Effects,
}

impl TrackRole {
    pub const ALL: [TrackRole; 13] = [
        TrackRole::Kick,
        TrackRole::Snare,
        TrackRole::Drums,
        TrackRole::Percussion,
        TrackRole::Bass,
        TrackRole::LeadVocal,
        TrackRole::BackingVocals,
        TrackRole::LeadInstrument,
        TrackRole::RhythmGuitar,
        TrackRole::Keys,
        TrackRole::Pads,
        TrackRole::Effects,
        TrackRole::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TrackRole::Kick => "Kick",
            TrackRole::Snare => "Snare",
            TrackRole::Drums => "Drums (Kit, Overheads, Room)",
            TrackRole::Percussion => "Percussion",
            TrackRole::Bass => "Bass",
            TrackRole::LeadVocal => "Lead Vocal",
            TrackRole::BackingVocals => "Backing Vocals",
            TrackRole::LeadInstrument => "Lead Instrument",
            TrackRole::RhythmGuitar => "Rhythm Guitar",
            TrackRole::Keys => "Keys",
            TrackRole::Pads => "Pads and Strings",
            TrackRole::Effects => "Effects",
            TrackRole::Other => "Other",
        }
    }

    /// A short name (lists, menus of several tracks).
    pub fn short(self) -> &'static str {
        match self {
            TrackRole::Drums => "Drums",
            TrackRole::Pads => "Pads",
            _ => self.label(),
        }
    }

    pub fn family(self) -> RoleFamily {
        match self {
            TrackRole::Kick | TrackRole::Snare | TrackRole::Drums => RoleFamily::DrumKit,
            TrackRole::Bass => RoleFamily::LowEnd,
            TrackRole::LeadVocal | TrackRole::LeadInstrument => RoleFamily::Foreground,
            TrackRole::Effects => RoleFamily::Effects,
            TrackRole::Percussion
            | TrackRole::BackingVocals
            | TrackRole::RhythmGuitar
            | TrackRole::Keys
            | TrackRole::Pads
            | TrackRole::Other => RoleFamily::Accompaniment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_and_have_families() {
        for r in TrackRole::ALL {
            let s = serde_json::to_string(&r).unwrap_or_default();
            let back: TrackRole = serde_json::from_str(&s).unwrap_or(TrackRole::Other);
            assert_eq!(back, r, "{s}");
            assert!(!r.label().is_empty());
        }
        assert_eq!(TrackRole::Kick.family(), RoleFamily::DrumKit);
        assert_eq!(TrackRole::LeadVocal.family(), RoleFamily::Foreground);
        assert_eq!(
            serde_json::to_string(&TrackRole::LeadVocal).unwrap_or_default(),
            "\"lead_vocal\""
        );
    }
}

//! The choices the Guitar Station's selectors offer, in the order of their
//! stable parameter values (append, never reorder), with their names.

use crate::acoustics::cabinet::CabinetProfile;
use crate::acoustics::mic::MicProfile;
use crate::acoustics::speaker::SpeakerProfile;
use crate::acoustics::stage::MicSlot;
use crate::voice::{Cabinet, CabinetChoice, DrySource, SpeakerChoice, Throw};

/// What the cabinet selector puts after the power stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CabinetEntry {
    pub name: &'static str,
    pub choice: CabinetChoice,
    /// The baked filter on the legacy path.
    pub legacy: Cabinet,
}

const fn cab(p: &'static CabinetProfile) -> CabinetEntry {
    CabinetEntry {
        name: p.name,
        choice: CabinetChoice::Model(p),
        legacy: Cabinet::Off,
    }
}

pub const CABINETS: [CabinetEntry; 18] = [
    cab(&CabinetProfile::BRIT_1960),
    cab(&CabinetProfile::CALI_OVERSIZED),
    cab(&CabinetProfile::BRIT_CLOSED),
    cab(&CabinetProfile::BRIT_GREEN),
    cab(&CabinetProfile::BRIT_V30),
    cab(&CabinetProfile::OVERSIZED),
    cab(&CabinetProfile::AMERICAN_OPEN_212),
    cab(&CabinetProfile::AMERICAN_OPEN_112),
    cab(&CabinetProfile::CLOSED_112),
    cab(&CabinetProfile::CLOSED_212),
    cab(&CabinetProfile::JAZZ_OPEN_212),
    cab(&CabinetProfile::AMERICAN_CLOSED_412),
    cab(&CabinetProfile::AMERICAN_810),
    cab(&CabinetProfile::AMERICAN_410),
    // A driver on an infinite baffle: speaker and microphones, no box.
    CabinetEntry {
        name: "Open Baffle",
        choice: CabinetChoice::Bypass,
        legacy: Cabinet::Off,
    },
    // Upstream's legacy path: a resistive load and a baked filter.
    CabinetEntry {
        name: "Classic Combo",
        choice: CabinetChoice::Legacy,
        legacy: Cabinet::Combo,
    },
    CabinetEntry {
        name: "Classic Stack",
        choice: CabinetChoice::Legacy,
        legacy: Cabinet::Stack,
    },
    // The power stage into a resistor, nothing after it.
    CabinetEntry {
        name: "Power Amp DI",
        choice: CabinetChoice::Legacy,
        legacy: Cabinet::Off,
    },
];

pub fn cabinet(i: usize) -> CabinetEntry {
    CABINETS.get(i).copied().unwrap_or(CABINETS[0])
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeakerEntry {
    pub name: &'static str,
    pub choice: SpeakerChoice,
}

const fn spk(p: &'static SpeakerProfile) -> SpeakerEntry {
    SpeakerEntry {
        name: p.name,
        choice: SpeakerChoice::Model(p),
    }
}

pub const SPEAKERS: [SpeakerEntry; 13] = [
    SpeakerEntry {
        name: "Matched",
        choice: SpeakerChoice::Matched,
    },
    // A resistor and the terminal voltage: a DI of the power stage.
    SpeakerEntry {
        name: "Load Resistor",
        choice: SpeakerChoice::Bypass,
    },
    spk(&SpeakerProfile::BRIT_V30),
    spk(&SpeakerProfile::BRIT_GREEN_25),
    spk(&SpeakerProfile::BRIT_T75),
    spk(&SpeakerProfile::AMERICAN_VINTAGE_12),
    spk(&SpeakerProfile::AMERICAN_VINTAGE_10),
    spk(&SpeakerProfile::AMERICAN_CERAMIC),
    spk(&SpeakerProfile::AMERICAN_ALNICO),
    spk(&SpeakerProfile::JAZZ_12),
    spk(&SpeakerProfile::BRIT_K85),
    spk(&SpeakerProfile::AMERICAN_BASS_10),
    spk(&SpeakerProfile::CAST_BASS_10),
];

pub fn speaker(i: usize) -> SpeakerEntry {
    SPEAKERS.get(i).copied().unwrap_or(SPEAKERS[0])
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MicEntry {
    pub name: &'static str,
    pub slot: MicSlot,
}

const fn mic(p: &'static MicProfile) -> MicEntry {
    MicEntry {
        name: p.name,
        slot: MicSlot::Profile(p),
    }
}

/// Microphone B may be off; A never is (Off reads as Ideal there).
pub const MICS: [MicEntry; 13] = [
    MicEntry {
        name: "Off",
        slot: MicSlot::Off,
    },
    MicEntry {
        name: "Ideal Omni",
        slot: MicSlot::Ideal,
    },
    mic(&MicProfile::DYNAMIC_57),
    mic(&MicProfile::DYNAMIC_421),
    mic(&MicProfile::DYNAMIC_906),
    mic(&MicProfile::DYNAMIC_409),
    mic(&MicProfile::RIBBON_121),
    mic(&MicProfile::RIBBON_160),
    mic(&MicProfile::RIBBON_38),
    mic(&MicProfile::CONDENSER_87),
    mic(&MicProfile::CONDENSER_414),
    mic(&MicProfile::TUBE_CONDENSER_67),
    mic(&MicProfile::FET_CONDENSER_47),
];

pub fn mic_slot(i: usize, off_allowed: bool) -> MicSlot {
    match MICS.get(i).map(|m| m.slot) {
        Some(MicSlot::Off) if !off_allowed => MicSlot::Ideal,
        Some(slot) => slot,
        None => MicSlot::Ideal,
    }
}

pub const MAINS: [(&str, f64); 4] = [
    ("Nominal", 1.0),
    ("90 %", 0.9),
    ("80 %", 0.8),
    ("70 %", 0.7),
];

pub const QUALITY: [(&str, usize); 2] = [("1x", 1), ("2x", 2)];

pub const DI_SOURCES: [(&str, DrySource); 3] = [
    ("Input", DrySource::Input),
    ("Pedals", DrySource::Pedal),
    ("Preamp", DrySource::Preamp),
];

pub const THROWS: [Throw; 4] = [Throw::Left, Throw::Centre, Throw::Right, Throw::Far];

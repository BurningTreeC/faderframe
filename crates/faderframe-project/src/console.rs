//! Console summing: the mix through a console's circuits. Every audio and
//! instrument channel goes through the console's channel line amplifier (a
//! light model of its bus circuit, `faderframe_circuit::console`), the
//! buses and the master through its mix-bus amplifier (the circuit itself:
//! a device in their input stage, which the session places). Off, the mix
//! is in the box.

use crate::TrackKind;
use serde::{Deserialize, Serialize};

/// The families, in `faderframe_core::builtin::CONSOLE_BUSES` order (stable:
/// it is project data).
pub const FAMILIES: [&str; 3] = ["American", "British 4K", "British 73"];

/// The families' ids (menus, scripts), in [`FAMILIES`] order.
pub const IDS: [&str; 3] = ["american", "british-4k", "british-73"];

/// The family with id `id`.
pub fn family_of(id: &str) -> Option<u8> {
    IDS.iter().position(|i| *i == id).map(|i| i as u8)
}

/// The channel drive's range (dB either way).
pub const DRIVE_DB: f64 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Console {
    /// The family (an index into [`FAMILIES`]).
    pub family: u8,
    /// How hard the channels drive their line amplifiers (dB, ±[`DRIVE_DB`];
    /// taken off after them: the colour changes, not the level).
    #[serde(default)]
    pub drive_db: f64,
    /// The mixer takes the console's look (else it keeps the theme).
    #[serde(default = "yes")]
    pub look: bool,
}

fn yes() -> bool {
    true
}

impl Console {
    pub fn new(family: u8) -> Self {
        Self {
            family: family.min(FAMILIES.len() as u8 - 1),
            drive_db: 0.0,
            look: true,
        }
    }

    pub fn with_look(self, look: bool) -> Self {
        Self { look, ..self }
    }

    pub fn id(&self) -> &'static str {
        IDS[usize::from(self.family).min(IDS.len() - 1)]
    }

    pub fn name(&self) -> &'static str {
        FAMILIES[usize::from(self.family).min(FAMILIES.len() - 1)]
    }
}

/// Does a track of `kind` go through the console's channel stage? (Buses,
/// returns and the master go through its bus amplifier instead.)
pub fn has_channel_stage(kind: TrackKind) -> bool {
    matches!(kind, TrackKind::Audio | TrackKind::Instrument)
}

/// Does a track of `kind` sum through the console's bus amplifier?
pub fn has_bus_stage(kind: TrackKind) -> bool {
    matches!(kind, TrackKind::Bus | TrackKind::Aux | TrackKind::Master)
}

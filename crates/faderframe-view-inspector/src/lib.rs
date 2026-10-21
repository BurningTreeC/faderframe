//! The selected track, seen whole.
//!
//! * [`InspectorView`] — the Inspector (`ViewKind::Inspector`, left of the
//!   arranger, the "i" button / F11): the track's routing and settings,
//!   its devices and its channel strip in one column;
//! * [`ChannelOverviewView`] — the Channel Overview
//!   (`ViewKind::ChannelOverview`, in the bottom dock like Clip Effects):
//!   the track's signal path in one row, a card per device, to open,
//!   bypass, reorder and remove them.
//!
//! Both follow the selection ([`chain::inspected_track`]) and read the
//! one model there is: the project's tracks, the session's choice lists
//! and `shown_*` values, the engine's latency and gain-reduction reports.
//! Their controls send the session actions the mixer and the arranger
//! send (the menus are the mixer's own), so nothing is kept twice.

#![forbid(unsafe_code)]

pub mod chain;
mod inspector;
mod overview;

pub use inspector::{Field, Hit as InspectorHit, InspectorView, Section};
pub use overview::{ChannelOverviewView, Hit as OverviewHit};

#[cfg(test)]
mod tests;

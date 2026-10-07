//! What happens after the power amplifier: the loudspeaker as a load and a
//! radiator, the cabinet it is mounted in, and the microphone in front of it.
//!
//! Model data (profiles, geometry) is immutable and kept apart from the mutable
//! per-channel processors. See `ARCHITECTURE.md`, `SPEAKER_MODEL.md`,
//! `CABINET_MODEL.md` and `MICROPHONE_MODEL.md`.


#[rustfmt::skip]
pub mod cabinet;
#[rustfmt::skip]
pub mod diffraction;
#[rustfmt::skip]
pub mod enclosure;
#[rustfmt::skip]
pub mod filters;
#[rustfmt::skip]
pub mod mic;
#[rustfmt::skip]
pub mod radiation;
#[rustfmt::skip]
pub mod speaker;
#[rustfmt::skip]
pub mod stage;

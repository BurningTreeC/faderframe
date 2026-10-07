//! FaderFrame's solver, and the three parts of GainStageFx's engine that are
//! not circuits: the spring tank, the optical tremolo and the bucket brigade.

pub use faderframe_circuit::dsp::*;

#[rustfmt::skip]
pub mod bbd;
#[rustfmt::skip]
pub mod spring;
#[rustfmt::skip]
pub mod tremolo;

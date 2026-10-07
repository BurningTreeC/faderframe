//! The guitar half of GainStageFx (by Simon Huber, MIT OR Apache-2.0):
//! its pedals and wahs, its modelled guitar and bass amplifiers with their
//! power stages, and the physical loudspeaker, cabinet and microphone path,
//! on FaderFrame's copy of the circuit solver (`faderframe-circuit`).
//! See `UPSTREAM.md` for what was taken and what changed.
#![forbid(unsafe_code)]
#![allow(clippy::all)]

pub mod acoustics;
pub mod chain;
pub mod circuits;
pub mod dsp;
pub mod lists;
pub mod pedal;
pub mod voice;

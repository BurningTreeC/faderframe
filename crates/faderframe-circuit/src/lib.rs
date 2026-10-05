//! Reusable GainStageFx modified nodal analysis, independent of any host or GUI.
//! The imported solver retains its upstream SIMD kernels and safety comments.
#![allow(clippy::all)]
pub mod circuits;
pub mod dsp;
pub mod preamp;

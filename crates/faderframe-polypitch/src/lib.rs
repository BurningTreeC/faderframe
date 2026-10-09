//! Polyphonic pitch editing: every note of a chord (or of overlapping
//! lines) followed over time and moved, straightened or removed on its own,
//! the rest of the sound left as it is.
//!
//! [`refine::pitch_tracks`] follows the notes a transcription found (their
//! keys and spans) to their pitch frame by frame; [`render::render`] moves
//! them. Pure and single-threaded; the session runs both on workers.

#![forbid(unsafe_code)]

pub mod fft;
pub mod refine;
pub mod render;

pub use refine::{Heard, pitch_tracks};
pub use render::{Moved, frame_size, render, spans, together, window_for};

/// Hz of a MIDI pitch (A4 = 440).
pub fn hz(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

#[cfg(test)]
mod tests;

//! MIDI events and realtime-safe event buffers.
//!
//! Events inside the engine are always **sample-accurate**: every event
//! carries the frame offset inside the current processing block at which it
//! takes effect ([`TimedMidiEvent::sample_offset`]). There is no "somewhere in
//! this block" delivery anywhere in the engine.
//!
//! [`MidiBuffer`] has a fixed capacity chosen on the control thread and never
//! reallocates; overflowing events are dropped and counted, which is the only
//! acceptable failure mode on the audio thread.

#![forbid(unsafe_code)]

mod buffer;
mod event;
mod input;
mod output;
mod tracker;

pub use buffer::{MidiBuffer, MidiBufferFull};
pub use event::{MidiEvent, TimedMidiEvent};
pub use input::{
    MAX_MIDI_PORTS, MidiClock, MidiControlFeed, MidiInputEvent, MidiInputQueue, MidiInputSender,
    midi_input_queue,
};
pub use output::{MidiOutputEvent, MidiOutputQueue, midi_output_queue};
pub use tracker::NoteTracker;

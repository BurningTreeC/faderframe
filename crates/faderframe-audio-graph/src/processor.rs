use crate::AudioBuffer;
use faderframe_midi::MidiBuffer;

/// Static processing configuration, fixed for the lifetime of a compiled graph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrepareConfig {
    pub sample_rate: f64,
    /// Largest block the graph will ever be asked to process. The engine
    /// splits larger device callbacks into chunks of at most this size.
    pub max_block_size: usize,
    /// Capacity of every event port buffer.
    pub event_capacity: usize,
    /// Measure per-node processing time (two clock reads per node).
    pub measure_nodes: bool,
}

impl PrepareConfig {
    pub fn new(sample_rate: f64, max_block_size: usize) -> Self {
        Self {
            sample_rate,
            max_block_size,
            event_capacity: faderframe_midi::MidiBuffer::DEFAULT_CAPACITY,
            measure_nodes: true,
        }
    }
}

/// Per-block information handed to every processor.
pub struct ProcessContext<'a, C> {
    /// Frames in this block (`<= max_block_size`).
    pub frames: usize,
    pub sample_rate: f64,
    /// Engine-defined shared block state (transport, parameter table ...).
    pub data: &'a C,
}

/// A node's buffers for one block.
///
/// Input buffers already contain the (latency-compensated) sum of all
/// incoming edges. Processors must write **every** frame of every audio
/// output; event outputs start empty.
pub struct NodeIo<'a> {
    pub frames: usize,
    pub audio_in: &'a [AudioBuffer],
    pub audio_out: &'a mut [AudioBuffer],
    pub events_in: &'a [MidiBuffer],
    pub events_out: &'a mut [MidiBuffer],
}

/// A unit of DSP work in the graph.
///
/// `prepare` runs on the control thread and may allocate; `process` and
/// `reset` run on the audio thread and must be realtime-safe.
pub trait Processor<C>: Send {
    /// Prepare for processing (control thread, before the graph goes live).
    fn prepare(&mut self, _config: &PrepareConfig) {}

    /// Latency this processor adds, in samples. Queried at compile time;
    /// a change requires a graph rebuild.
    fn latency(&self) -> u32 {
        0
    }

    /// Process one block (audio thread).
    fn process(&mut self, cx: &ProcessContext<'_, C>, io: &mut NodeIo<'_>);

    /// Drop internal state such as tails or held notes (audio thread).
    fn reset(&mut self) {}
}

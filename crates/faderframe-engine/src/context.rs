use crate::snapshot::TimelineSnapshot;
use faderframe_realtime::{MeterBank, ParamTable};
use faderframe_transport::TransportInfo;
use std::sync::Arc;

/// Block-level state shared with every graph processor (the graph's `C`).
///
/// Owned by the realtime processor; processors only read it.
pub struct EngineContext {
    /// Underruns concealed by asynchronous plugins, collected after the graph.
    pub worker_underruns: std::sync::atomic::AtomicU64,
    /// Bounded device callback budget, unchanged across internal graph chunks.
    pub callback_deadline: Option<std::time::Instant>,
    pub transport: TransportInfo,
    /// Playback jumped (stop, locate or loop wrap) at the start of this
    /// block; note generators must release sounding notes.
    pub discontinuity: bool,
    /// Shared with the anticipator (see [`crate::ahead`]); dropped only on
    /// the control thread.
    pub timeline: Arc<TimelineSnapshot>,
    /// The tracks' modulators (see [`crate::modulation`]); swapped like
    /// the timeline.
    pub modulation: Arc<crate::modulation::ModulationSet>,
    pub params: Arc<ParamTable>,
    /// Automated values of strip/send parameters (same slots as `params`),
    /// written by the processors for the UI.
    pub readback: Arc<ParamTable>,
    pub meters: Arc<MeterBank>,
    /// Post-fader audio of the analysed track (the Tools view).
    pub scope: Arc<faderframe_realtime::ScopeRing>,
    /// Live MIDI input of this chunk (see [`crate::midi`]).
    pub midi_input: crate::midi::MidiInputBlock,
    /// The render-ahead sequence this block belongs to (0 without
    /// anticipation).
    pub ahead_seq: u64,
    /// The album plays instead of the project: the strips do not feed the
    /// scope.
    pub preview_active: bool,
    /// The clip launcher (see [`crate::launch`]).
    pub launch: crate::launch::LaunchState,
}

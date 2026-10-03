use crate::snapshot::TimelineSnapshot;
use faderframe_realtime::{MeterBank, ParamTable};
use faderframe_transport::TransportInfo;
use std::sync::Arc;

/// Block-level state shared with every graph processor (the graph's `C`).
///
/// Owned by the realtime processor; processors only read it.
pub struct EngineContext {
    pub transport: TransportInfo,
    /// Playback jumped (stop, locate or loop wrap) at the start of this
    /// block; note generators must release sounding notes.
    pub discontinuity: bool,
    pub timeline: Box<TimelineSnapshot>,
    pub params: Arc<ParamTable>,
    pub meters: Arc<MeterBank>,
}

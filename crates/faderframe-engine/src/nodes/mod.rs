//! Engine-specific graph processors.

mod chain;
mod clip_player;
mod crosstalk;
mod io;
mod listen;
mod midi_player;
mod plugin;
mod psola;
mod send;
mod strip;

pub use chain::{ChainMix, ChainNotes};
pub use clip_player::{AudioClipPlayer, StretchVoices};
pub use crosstalk::Crosstalk;
pub use io::{DeviceInputTap, DeviceOutputSink, FoldDown, MonitorGate};
pub use listen::ListenOut;
pub use midi_player::MidiClipPlayer;
pub use plugin::PluginNode;
pub use send::SendNode;
pub use strip::{ChannelStrip, ObjectRenderer, StripEcho};

/// Maximum channels a strip/send processes individually.
pub(crate) const MAX_CHANNELS: usize = 16;

/// Frames between automation evaluations (and plugin parameter events)
/// while a curve changes: 0.7 ms at 48 kHz.
pub(crate) const AUTOMATION_STEP: usize = 32;

/// Timeline sample at which automation is read for block offset `offset`:
/// while stopped the playhead does not move, so every offset reads it.
#[inline]
pub(crate) fn automation_at(
    cx: &faderframe_audio_graph::ProcessContext<'_, crate::context::EngineContext>,
    offset: usize,
) -> i64 {
    let t = &cx.data.transport;
    if t.playing {
        t.sample_position + offset as i64
    } else {
        t.sample_position
    }
}

/// Linear ramp helper: per-sample increments from `from` to `to` over `n`.
#[inline]
pub(crate) fn ramp_step(from: f32, to: f32, n: usize) -> f32 {
    if n == 0 { 0.0 } else { (to - from) / n as f32 }
}

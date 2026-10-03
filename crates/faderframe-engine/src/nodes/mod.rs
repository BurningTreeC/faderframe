//! Engine-specific graph processors.

mod clip_player;
mod io;
mod midi_player;
mod plugin;
mod send;
mod strip;

pub use clip_player::AudioClipPlayer;
pub use io::{DeviceInputTap, DeviceOutputSink, MonitorGate};
pub use midi_player::MidiClipPlayer;
pub use plugin::PluginNode;
pub use send::SendNode;
pub use strip::ChannelStrip;

/// Maximum channels a strip/send processes individually.
pub(crate) const MAX_CHANNELS: usize = 8;

/// Linear ramp helper: per-sample increments from `from` to `to` over `n`.
#[inline]
pub(crate) fn ramp_step(from: f32, to: f32, n: usize) -> f32 {
    if n == 0 { 0.0 } else { (to - from) / n as f32 }
}

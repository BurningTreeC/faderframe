//! Mono → Stereo inserts: a mono track keeps a plugin's stereo output and
//! is stereo from that insert on (`PluginSlot::mono_to_stereo`, set with
//! `Command::SetPluginMonoToStereo`). Off, the graph folds the plugin's
//! stereo output into the mono track, `(L + R) / 2`.

use crate::Session;
use faderframe_core::{ChannelLayout, PluginInstanceId, builtin};
use faderframe_project::PluginFormat;

impl Session {
    /// Whether insert `plugin`'s menu offers Mono → Stereo: a mono signal
    /// reaches it on its track and it can put out stereo (its main output
    /// has two channels or more) — or it is set already (to turn it off).
    /// Not for devices that widen by themselves (the Guitar Station), have
    /// a switch of their own (the Hardware Insert), hold chains (a
    /// container) or sit in the input stage.
    pub fn mono_to_stereo_offered(&self, plugin: PluginInstanceId) -> bool {
        let Some((track, slot)) = self.plugin_slot(plugin) else {
            return false;
        };
        if slot.mono_to_stereo {
            return true;
        }
        if track.layout_into(plugin) != Some(ChannelLayout::Mono)
            || slot.plugin.widens_mono()
            || slot.plugin.is_container()
        {
            return false;
        }
        if slot.plugin.format == PluginFormat::Builtin
            && (slot.plugin.id == builtin::HARDWARE_INSERT
                || builtin::is_input_stage(&slot.plugin.id))
        {
            return false;
        }
        self.engine
            .plugin_outputs(plugin)
            .and_then(|o| o.first())
            .is_some_and(|main| main.channels >= 2)
    }
}

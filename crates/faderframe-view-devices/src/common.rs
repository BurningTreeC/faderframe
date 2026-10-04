//! What every device editor shares: reading the plugin's live values and
//! sending edits as undoable parameter commands.

use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_project::Command;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::EventCx;
use std::sync::Arc;

/// An editor's plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
    pub plugin: PluginInstanceId,
}

impl Device {
    pub fn new(plugin: PluginInstanceId) -> Self {
        Self { plugin }
    }

    /// The plugin's tap (it changes when the plugin is loaded again).
    pub fn tap(&self, model: &Session) -> Option<Arc<AnalysisTap>> {
        model.plugin_tap(self.plugin)
    }

    /// A parameter's live value by index (automation included).
    pub fn value(&self, model: &Session, index: usize) -> f64 {
        self.tap(model)
            .map_or(0.0, |t| f64::from(t.params.get(index)))
    }

    /// Set a parameter (an undo step of its own unless inside a gesture).
    pub fn set(&self, model: &Session, cx: &mut EventCx<'_, Action>, id: ParameterId, value: f64) {
        let Some((track, _)) = model.plugin_owner(self.plugin) else {
            return;
        };
        cx.emit(Action::Edit(Command::SetPluginParameter {
            track,
            plugin: self.plugin,
            parameter: id,
            value: Some(value),
        }));
        cx.redraw();
    }

    /// Several edits as one undo step.
    pub fn begin(&self, cx: &mut EventCx<'_, Action>, label: &str) {
        cx.emit(Action::BeginGesture(label.into()));
    }

    pub fn end(&self, cx: &mut EventCx<'_, Action>) {
        cx.emit(Action::EndGesture);
    }
}

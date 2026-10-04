//! Editors of FaderFrame's built-in devices, GTK-free like every view: the
//! EQ ([`eq::EqView`]) and the Program EQ's hardware panel
//! ([`program_eq::ProgramEqView`]). The shell opens one in a window of its
//! own for a plugin whose id [`editor_for`] knows.

mod common;
pub mod eq;
pub mod program_eq;

use faderframe_core::{PluginInstanceId, builtin};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{CanvasView, Theme};

/// The editor of a built-in plugin, if it has one of its own.
pub fn editor_for(
    plugin_id: &str,
    plugin: PluginInstanceId,
    theme: &Theme,
) -> Option<Box<dyn CanvasView<Session, Action>>> {
    match plugin_id {
        builtin::EQ => Some(Box::new(eq::EqView::new(plugin, theme))),
        builtin::PROGRAM_EQ => Some(Box::new(program_eq::ProgramEqView::new(plugin, theme))),
        _ => None,
    }
}

/// Height of the editor window's header bar (logical pixels).
const HEADER_BAR: i32 = 46;

/// The editor window's size when it opens (logical pixels).
pub fn editor_size(plugin_id: &str) -> Option<(i32, i32)> {
    match plugin_id {
        builtin::EQ => Some((1180, 700)),
        // The panel at 1.2 times its size, under the window's header bar.
        builtin::PROGRAM_EQ => Some((
            (program_eq::PANEL_W * 1.2) as i32,
            (program_eq::TOTAL_H * 1.2) as i32 + HEADER_BAR,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests;

//! Editors of FaderFrame's built-in devices, GTK-free like every view: the
//! EQ ([`eq::EqView`]) and the Program EQ's hardware panel
//! ([`program_eq::ProgramEqView`]). The shell opens one in a window of its
//! own for a plugin whose id [`editor_for`] knows.

mod common;
mod compressor;
mod deesser;
mod delay;
pub mod eq;
mod gate;
mod kit;
mod limiter;
mod modulation;
pub mod program_eq;
mod reverb;
mod saturator;
mod tuner;
mod utility;
mod values;

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
        builtin::COMPRESSOR => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            compressor::CompressorFace::new(theme),
        ))),
        builtin::LIMITER => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            limiter::LimiterFace::new(theme),
        ))),
        builtin::TUNER => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            tuner::TunerFace::new(theme),
        ))),
        builtin::MODULATION => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            modulation::ModulationFace::new(theme),
        ))),
        builtin::REVERB => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            reverb::ReverbFace::new(theme),
        ))),
        builtin::ECHO => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            delay::DelayFace::new(theme),
        ))),
        builtin::GAIN => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            utility::UtilityFace::new(theme),
        ))),
        builtin::SATURATOR => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            saturator::SaturatorFace::new(theme),
        ))),
        builtin::DEESSER => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            deesser::DeesserFace::new(theme),
        ))),
        builtin::GATE => Some(Box::new(kit::DeviceView::new(
            plugin,
            theme,
            gate::GateFace::new(theme),
        ))),
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
        builtin::COMPRESSOR => Some((1040, 520 + HEADER_BAR)),
        builtin::LIMITER => Some((900, 480 + HEADER_BAR)),
        builtin::TUNER => Some((620, 440 + HEADER_BAR)),
        builtin::MODULATION => Some((1060, 520 + HEADER_BAR)),
        builtin::REVERB => Some((1120, 520 + HEADER_BAR)),
        builtin::ECHO => Some((1120, 520 + HEADER_BAR)),
        builtin::GAIN => Some((900, 500 + HEADER_BAR)),
        builtin::SATURATOR => Some((960, 520 + HEADER_BAR)),
        builtin::DEESSER => Some((1000, 520 + HEADER_BAR)),
        builtin::GATE => Some((1000, 520 + HEADER_BAR)),
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

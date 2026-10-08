//! Toolkit-independent framework for FaderFrame's custom work surfaces.
//!
//! The arranger, mixer, piano roll, meters etc. are not built from toolkit
//! widgets (one widget per clip/note/knob does not scale to thousands of
//! objects). Instead each surface is a [`CanvasView`] that
//!
//! * paints through the [`Painter`] trait — implemented on GTK 4 snapshots
//!   (GPU-accelerated GSK render nodes) by `faderframe-ui`, and replaceable by
//!   a wgpu painter for very dense views later;
//! * receives toolkit-neutral [`ViewEvent`]s in logical pixels;
//! * emits actions and [`HostRequest`]s (menus, text entry) through an
//!   [`EventCx`].
//!
//! All styling comes from a [`Theme`]; [`controls`] contains the shared
//! analogue-console drawing primitives.

#![forbid(unsafe_code)]

pub mod access;
mod color;
pub mod controls;
mod event;
pub mod flow;
mod geometry;
mod paint;
mod painter;
mod text;
mod theme;
mod themes;
mod view;

pub use access::{AccessNode, AccessRole, AccessValue, access_id, access_id_str};
pub use color::Color;
pub use event::{Cursor, Key, Modifiers, PointerButton, ViewEvent};
pub use flow::{Flow, FlowMetrics};
pub use geometry::{Point, Rect, Size};
pub use paint::{External, Image, Paint, Path, PathCmd, Pixels};
pub use painter::{DrawOp, Painter, RecordingPainter};
pub use text::{Align, FontFamily, FontWeight, TextStyle};
pub use theme::{
    Accent, ArrangerTheme, ConsoleLook, ConsoleTheme, DeviceTheme, FaderStyle, KnobStyle, LedStyle,
    MeterKind, MeterStyle, PerformanceTheme, PianoRollTheme, Theme, ToolsTheme, Typography,
    UiPalette,
};
pub use view::{
    CanvasView, EventCx, FileChoice, FilesCommit, HostRequest, MenuItem, ScrollAxis, ScrollInfo,
    TextCommit,
};

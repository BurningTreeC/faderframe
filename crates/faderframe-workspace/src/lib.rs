//! Docking / workspace layout model.
//!
//! This crate describes *where* editor views live — split ratios, tab
//! groups, detached windows — independently of any GUI toolkit. The GTK
//! shell realises a [`WorkspaceLayout`] into widgets and writes user changes
//! (dragging a divider, switching tabs, detaching) back into it. Because the
//! model only stores [`ViewId`]s, moving a view between the main window and
//! a floating window never copies view or project state.
//!
//! Structure:
//!
//! * every window (main or floating) has a [`DockNode`] tree of
//!   [`DockNode::Split`]s and [`TabGroup`]s;
//! * tab groups may carry a [`DockAreaId`] ("main", "bottom", ...). Named
//!   areas persist even when empty (they simply hide), which gives detached
//!   views a well-defined home to return to;
//! * a [`WorkspaceSet`] holds several named layouts (screensets) such as
//!   Recording, Editing, Mixing, MIDI and Mastering.

#![forbid(unsafe_code)]

mod layout;
mod presets;

pub use layout::{
    Axis, DockAreaId, DockNode, FloatingWindow, LayoutError, TabBar, TabGroup, ViewKind,
    ViewLocation, WindowGeometry, WindowId, WindowRef, WorkspaceLayout,
};
pub use presets::Preset;

use serde::{Deserialize, Serialize};
use std::fmt;

/// Identifier of an editor view instance ("arranger", "mixer", ...).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewId(pub String);

impl ViewId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn arranger() -> Self {
        Self::new("arranger")
    }

    pub fn mixer() -> Self {
        Self::new("mixer")
    }

    pub fn piano_roll() -> Self {
        Self::new("piano-roll")
    }

    pub fn automation() -> Self {
        Self::new("automation")
    }

    pub fn performance() -> Self {
        Self::new("performance")
    }

    pub fn history() -> Self {
        Self::new("history")
    }

    pub fn modulators() -> Self {
        Self::new("modulators")
    }

    pub fn tools() -> Self {
        Self::new("tools")
    }

    pub fn album() -> Self {
        Self::new("album")
    }
}

impl fmt::Debug for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "view:{}", self.0)
    }
}

impl fmt::Display for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named layout (screenset).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub layout: WorkspaceLayout,
}

/// All workspaces of a project plus the active one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSet {
    pub active: usize,
    pub workspaces: Vec<Workspace>,
    /// Arranger track height for all tracks (logical pixels; `None` = theme
    /// default) and per-track overrides.
    #[serde(default)]
    pub track_height: Option<f32>,
    #[serde(default)]
    pub track_heights: std::collections::BTreeMap<faderframe_core::TrackId, f32>,
    /// Automation lanes shown in the arranger.
    #[serde(default)]
    pub automation_shown: std::collections::BTreeSet<faderframe_core::AutomationLaneId>,
    /// Width of the arranger's track header column (`None` = theme default).
    #[serde(default)]
    pub header_width: Option<f32>,
    /// Where each plugin's editor window was last (screen coordinates of
    /// its top-left corner); new editors open centred.
    #[serde(default)]
    pub plugin_windows: std::collections::BTreeMap<faderframe_core::PluginInstanceId, (i32, i32)>,
    /// Insert slots per mixer strip (`None` = [`DEFAULT_INSERT_SLOTS`]).
    #[serde(default)]
    pub mixer_insert_slots: Option<u16>,
    /// Mixer strip width for every channel strip (`None` = theme default)
    /// and per-track overrides.
    #[serde(default)]
    pub strip_width: Option<f32>,
    #[serde(default)]
    pub strip_widths: std::collections::BTreeMap<faderframe_core::TrackId, f32>,
    /// Folder tracks shown closed (their tracks hidden).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub closed_folders: std::collections::BTreeSet<faderframe_core::TrackId>,
}

/// Narrowest and widest mixer channel strips.
pub const STRIP_WIDTH_RANGE: (f32, f32) = (64.0, 240.0);

/// Insert slots a mixer strip shows by default, and the range the user can
/// drag the section to.
pub const DEFAULT_INSERT_SLOTS: u16 = 5;
pub const INSERT_SLOTS_RANGE: (u16, u16) = (1, 24);

/// Narrowest and widest arranger track header column.
pub const HEADER_WIDTH_RANGE: (f32, f32) = (190.0, 640.0);

/// Smallest and largest arranger track heights.
pub const TRACK_HEIGHT_RANGE: (f32, f32) = (40.0, 480.0);

impl Default for WorkspaceSet {
    fn default() -> Self {
        Self {
            active: 0,
            workspaces: Preset::ALL
                .iter()
                .map(|p| Workspace {
                    name: p.name().to_string(),
                    layout: p.layout(),
                })
                .collect(),
            track_height: None,
            track_heights: Default::default(),
            automation_shown: Default::default(),
            header_width: None,
            plugin_windows: Default::default(),
            mixer_insert_slots: None,
            strip_width: None,
            strip_widths: Default::default(),
            closed_folders: Default::default(),
        }
    }
}

impl WorkspaceSet {
    /// Height of a track in the arranger, if not the theme default.
    pub fn track_height(&self, track: faderframe_core::TrackId) -> Option<f32> {
        self.track_heights
            .get(&track)
            .copied()
            .or(self.track_height)
    }

    /// Set one track's height, or (with `None`) every track's.
    pub fn set_track_height(&mut self, track: Option<faderframe_core::TrackId>, height: f32) {
        let h = height.clamp(TRACK_HEIGHT_RANGE.0, TRACK_HEIGHT_RANGE.1);
        match track {
            Some(t) => {
                self.track_heights.insert(t, h);
            }
            None => {
                self.track_height = Some(h);
                self.track_heights.clear();
            }
        }
    }

    /// Width of a track's mixer strip, if not the theme default.
    pub fn strip_width(&self, track: faderframe_core::TrackId) -> Option<f32> {
        self.strip_widths.get(&track).copied().or(self.strip_width)
    }

    /// Set one strip's width, or (with `None`) every strip's; a `None`
    /// width goes back to the default.
    pub fn set_strip_width(&mut self, track: Option<faderframe_core::TrackId>, width: Option<f32>) {
        let w = width.map(|w| w.clamp(STRIP_WIDTH_RANGE.0, STRIP_WIDTH_RANGE.1));
        match (track, w) {
            (Some(t), Some(w)) => {
                self.strip_widths.insert(t, w);
            }
            (Some(t), None) => {
                self.strip_widths.remove(&t);
            }
            (None, w) => {
                self.strip_width = w;
                self.strip_widths.clear();
            }
        }
    }

    pub fn active(&self) -> &Workspace {
        &self.workspaces[self.active.min(self.workspaces.len().saturating_sub(1))]
    }

    pub fn active_mut(&mut self) -> &mut Workspace {
        let i = self.active.min(self.workspaces.len().saturating_sub(1));
        &mut self.workspaces[i]
    }

    pub fn active_layout(&self) -> &WorkspaceLayout {
        &self.active().layout
    }

    pub fn active_layout_mut(&mut self) -> &mut WorkspaceLayout {
        &mut self.active_mut().layout
    }

    /// Switch to workspace `index` (no-op if out of range).
    pub fn switch_to(&mut self, index: usize) -> bool {
        if index < self.workspaces.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Reset the active workspace to its preset (if it is one).
    pub fn reset_active(&mut self) {
        let ws = self.active_mut();
        if let Some(p) = Preset::ALL.iter().find(|p| p.name() == ws.name) {
            ws.layout = p.layout();
        }
    }

    /// Repair invariants after loading (empty set, bad indices, invalid layouts).
    pub fn sanitise(&mut self) {
        if self.workspaces.is_empty() {
            *self = Self::default();
        }
        self.active = self.active.min(self.workspaces.len() - 1);
        for ws in &mut self.workspaces {
            if ws.layout.validate().is_err() {
                ws.layout = Preset::ALL
                    .iter()
                    .find(|p| p.name() == ws.name)
                    .map(|p| p.layout())
                    .unwrap_or_else(|| Preset::Recording.layout());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_set_contains_all_presets_and_is_valid() {
        let set = WorkspaceSet::default();
        assert_eq!(set.workspaces.len(), Preset::ALL.len());
        for ws in &set.workspaces {
            ws.layout.validate().unwrap();
        }
        assert_eq!(set.active().name, "Recording");
    }

    #[test]
    fn serde_round_trip() {
        let mut set = WorkspaceSet::default();
        set.active_layout_mut()
            .detach(&ViewId::mixer(), WindowGeometry::default())
            .unwrap();
        let json = serde_json::to_string_pretty(&set).unwrap();
        let back: WorkspaceSet = serde_json::from_str(&json).unwrap();
        assert_eq!(set, back);
    }

    #[test]
    fn sanitise_repairs_broken_layouts() {
        let mut set = WorkspaceSet {
            active: 99,
            ..WorkspaceSet::default()
        };
        set.workspaces[0].layout.main =
            DockNode::Tabs(TabGroup::new(None, vec![ViewId::mixer(), ViewId::mixer()]));
        set.sanitise();
        assert_eq!(set.active, set.workspaces.len() - 1);
        set.workspaces[0].layout.validate().unwrap();
    }
}

#[cfg(test)]
mod track_height_tests {
    use super::*;
    use faderframe_core::TrackId;

    #[test]
    fn heights_clamp_override_and_round_trip() {
        let mut ws = WorkspaceSet::default();
        assert_eq!(ws.track_height(TrackId(3)), None);
        ws.set_track_height(None, 100.0);
        ws.set_track_height(Some(TrackId(3)), 9999.0);
        assert_eq!(ws.track_height(TrackId(3)), Some(TRACK_HEIGHT_RANGE.1));
        assert_eq!(ws.track_height(TrackId(4)), Some(100.0));
        let json = serde_json::to_string(&ws).unwrap();
        let back: WorkspaceSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ws);
        // Setting all heights clears overrides.
        ws.set_track_height(None, 10.0);
        assert_eq!(ws.track_height(TrackId(3)), Some(TRACK_HEIGHT_RANGE.0));
    }
}

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

    pub fn video() -> Self {
        Self::new("video")
    }

    pub fn ddp() -> Self {
        Self::new("ddp")
    }

    pub fn surround() -> Self {
        Self::new("surround")
    }

    pub fn adr() -> Self {
        Self::new("adr")
    }

    pub fn modulators() -> Self {
        Self::new("modulators")
    }

    pub fn pitch() -> Self {
        Self::new("pitch")
    }

    pub fn clip_fx() -> Self {
        Self::new("clip-fx")
    }

    pub fn launcher() -> Self {
        Self::new("launcher")
    }

    pub fn tools() -> Self {
        Self::new("tools")
    }

    pub fn album() -> Self {
        Self::new("album")
    }

    pub fn lead_sheet() -> Self {
        Self::new("lead-sheet")
    }

    pub fn events() -> Self {
        Self::new("events")
    }

    pub fn spectral() -> Self {
        Self::new("spectral")
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
    /// What every strip's meter shows (`None` = [`MeterMode::Peak`]) and
    /// per-track overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter_mode: Option<MeterMode>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub meter_modes: std::collections::BTreeMap<faderframe_core::TrackId, MeterMode>,
    /// The level (dBFS RMS) that reads 0 VU (`None` = [`VU_REFERENCE`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vu_reference: Option<f32>,
    /// The mixer's meter bridge: a moving-coil VU meter over every strip.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub meter_bridge: bool,
}

/// What a level meter shows and how it moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterMode {
    /// Sample peaks: instant rise, falling 26 dB/s, a held peak.
    #[default]
    Peak,
    /// Peaks with the RMS level (300 ms) inside the bar.
    PeakRms,
    /// A moving-coil VU meter: the RMS level through the needle's 300 ms
    /// movement, 0 VU at the reference level.
    Vu,
    /// The EBU quasi-peak programme meter: 10 ms integration, 24 dB fall in
    /// 2.8 s, alignment (TEST) at −18 dBFS.
    Ppm,
    /// Bob Katz's K-System: RMS on a scale whose 0 is −20, −14 or −12
    /// dBFS, peaks above it.
    K20,
    K14,
    K12,
}

impl MeterMode {
    pub const ALL: [MeterMode; 7] = [
        MeterMode::Peak,
        MeterMode::PeakRms,
        MeterMode::Vu,
        MeterMode::Ppm,
        MeterMode::K20,
        MeterMode::K14,
        MeterMode::K12,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MeterMode::Peak => "Peak",
            MeterMode::PeakRms => "Peak + RMS",
            MeterMode::Vu => "VU",
            MeterMode::Ppm => "PPM (EBU)",
            MeterMode::K20 => "K-20",
            MeterMode::K14 => "K-14",
            MeterMode::K12 => "K-12",
        }
    }

    /// A K-System meter's 0 (dBFS).
    pub fn k_zero(self) -> Option<f32> {
        match self {
            MeterMode::K20 => Some(-20.0),
            MeterMode::K14 => Some(-14.0),
            MeterMode::K12 => Some(-12.0),
            _ => None,
        }
    }
}

/// The level that reads 0 VU unless set: −18 dBFS RMS (EBU R68).
pub const VU_REFERENCE: f32 = -18.0;

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
            meter_mode: None,
            meter_modes: Default::default(),
            vu_reference: None,
            meter_bridge: false,
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

    /// What a strip's meter shows.
    pub fn meter_mode(&self, track: faderframe_core::TrackId) -> MeterMode {
        self.meter_modes
            .get(&track)
            .copied()
            .or(self.meter_mode)
            .unwrap_or_default()
    }

    /// Set one strip's meter, or (with `None`) every strip's; a `None`
    /// mode goes back to the default.
    pub fn set_meter_mode(
        &mut self,
        track: Option<faderframe_core::TrackId>,
        mode: Option<MeterMode>,
    ) {
        match (track, mode) {
            (Some(t), Some(m)) => {
                self.meter_modes.insert(t, m);
            }
            (Some(t), None) => {
                self.meter_modes.remove(&t);
            }
            (None, m) => {
                self.meter_mode = m.filter(|m| *m != MeterMode::Peak);
                self.meter_modes.clear();
            }
        }
    }

    /// The level that reads 0 VU (dBFS RMS).
    pub fn vu_reference(&self) -> f32 {
        self.vu_reference.unwrap_or(VU_REFERENCE)
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
            // Layouts saved before the tabs kept one order.
            ws.layout.order_tabs();
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

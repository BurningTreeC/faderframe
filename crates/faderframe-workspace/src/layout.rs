use crate::ViewId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The kind of editor a view is (decides which implementation the shell
/// instantiates for a [`ViewId`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    Arranger,
    Mixer,
    PianoRoll,
    Automation,
    Performance,
    /// Mastering meters: loudness, level, phase, spectrum.
    Tools,
    /// Songs in release order, their loudness and delivery.
    Album,
    /// The undo history.
    History,
    /// The selected track's modulators.
    Modulators,
    /// An audio clip's notes, for pitch editing.
    Pitch,
    /// An audio clip's effects, rendered offline.
    ClipFx,
    /// Scenes of clips launched while the song plays.
    Launcher,
    /// A CD master's DDP fileset, checked and played.
    Ddp,
}

impl ViewKind {
    pub const ALL: [ViewKind; 13] = [
        ViewKind::Arranger,
        ViewKind::Mixer,
        ViewKind::PianoRoll,
        ViewKind::Automation,
        ViewKind::Performance,
        ViewKind::Tools,
        ViewKind::Album,
        ViewKind::History,
        ViewKind::Modulators,
        ViewKind::Pitch,
        ViewKind::ClipFx,
        ViewKind::Launcher,
        ViewKind::Ddp,
    ];

    /// The kind whose default view has this id (views added after a layout
    /// was saved are registered on first use).
    pub fn of_default_id(id: &ViewId) -> Option<ViewKind> {
        Self::ALL.into_iter().find(|k| k.default_id() == *id)
    }

    pub fn title(self) -> &'static str {
        match self {
            ViewKind::Arranger => "Arranger",
            ViewKind::Mixer => "Mixer",
            ViewKind::PianoRoll => "Piano Roll",
            ViewKind::Automation => "Automation",
            ViewKind::Performance => "Performance",
            ViewKind::Tools => "Tools",
            ViewKind::Album => "Album",
            ViewKind::History => "History",
            ViewKind::Modulators => "Modulators",
            ViewKind::Pitch => "Pitch",
            ViewKind::ClipFx => "Clip Effects",
            ViewKind::Launcher => "Clip Launcher",
            ViewKind::Ddp => "DDP Player",
        }
    }

    pub fn default_id(self) -> ViewId {
        match self {
            ViewKind::Arranger => ViewId::arranger(),
            ViewKind::Mixer => ViewId::mixer(),
            ViewKind::PianoRoll => ViewId::piano_roll(),
            ViewKind::Automation => ViewId::automation(),
            ViewKind::Performance => ViewId::performance(),
            ViewKind::Tools => ViewId::tools(),
            ViewKind::Album => ViewId::album(),
            ViewKind::History => ViewId::history(),
            ViewKind::Modulators => ViewId::modulators(),
            ViewKind::Pitch => ViewId::pitch(),
            ViewKind::ClipFx => ViewId::clip_fx(),
            ViewKind::Launcher => ViewId::launcher(),
            ViewKind::Ddp => ViewId::ddp(),
        }
    }
}

/// Named dock area; tab groups with an area id survive becoming empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DockAreaId(pub String);

impl DockAreaId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn main() -> Self {
        Self::new("main")
    }

    pub fn bottom() -> Self {
        Self::new("bottom")
    }
}

/// Split direction. `Horizontal` places children side by side,
/// `Vertical` stacks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// When to show a tab strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabBar {
    #[default]
    Always,
    /// Only when the group holds more than one view.
    Auto,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabGroup {
    #[serde(default)]
    pub area: Option<DockAreaId>,
    pub views: Vec<ViewId>,
    #[serde(default)]
    pub active: usize,
    /// Collapsed by the user (hide/show dock).
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub tab_bar: TabBar,
}

impl TabGroup {
    pub fn new(area: Option<DockAreaId>, views: Vec<ViewId>) -> Self {
        Self {
            area,
            views,
            active: 0,
            hidden: false,
            tab_bar: TabBar::Always,
        }
    }

    pub fn active_view(&self) -> Option<&ViewId> {
        self.views.get(self.active).or(self.views.first())
    }

    /// Whether the group occupies space on screen.
    pub fn is_visible(&self) -> bool {
        !self.hidden && !self.views.is_empty()
    }

    fn remove(&mut self, view: &ViewId) -> Option<usize> {
        let i = self.views.iter().position(|v| v == view)?;
        self.views.remove(i);
        if self.active > i || self.active >= self.views.len() {
            self.active = self.active.saturating_sub(1);
        }
        Some(i)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockNode {
    Split {
        axis: Axis,
        /// Fraction of the space given to `first` (0..1).
        ratio: f32,
        first: Box<DockNode>,
        second: Box<DockNode>,
    },
    Tabs(TabGroup),
}

impl DockNode {
    pub fn split(axis: Axis, ratio: f32, first: DockNode, second: DockNode) -> Self {
        DockNode::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Visit every tab group with its path (0 = first, 1 = second child).
    pub fn for_each_group<'a>(
        &'a self,
        path: &mut Vec<u8>,
        f: &mut impl FnMut(&[u8], &'a TabGroup),
    ) {
        match self {
            DockNode::Tabs(g) => f(path, g),
            DockNode::Split { first, second, .. } => {
                path.push(0);
                first.for_each_group(path, f);
                path.pop();
                path.push(1);
                second.for_each_group(path, f);
                path.pop();
            }
        }
    }

    pub fn node_at(&self, path: &[u8]) -> Option<&DockNode> {
        match (path.split_first(), self) {
            (None, n) => Some(n),
            (Some((&0, rest)), DockNode::Split { first, .. }) => first.node_at(rest),
            (Some((&1, rest)), DockNode::Split { second, .. }) => second.node_at(rest),
            _ => None,
        }
    }

    pub fn node_at_mut(&mut self, path: &[u8]) -> Option<&mut DockNode> {
        match (path.split_first(), self) {
            (None, n) => Some(n),
            (Some((&0, rest)), DockNode::Split { first, .. }) => first.node_at_mut(rest),
            (Some((&1, rest)), DockNode::Split { second, .. }) => second.node_at_mut(rest),
            _ => None,
        }
    }

    fn views(&self) -> Vec<ViewId> {
        let mut out = Vec::new();
        self.for_each_group(&mut Vec::new(), &mut |_, g| {
            out.extend(g.views.iter().cloned())
        });
        out
    }

    /// Whether anything in this subtree is visible.
    pub fn is_visible(&self) -> bool {
        match self {
            DockNode::Tabs(g) => g.is_visible(),
            DockNode::Split { first, second, .. } => first.is_visible() || second.is_visible(),
        }
    }

    /// Is this subtree disposable (no views and no named areas)?
    fn is_disposable(&self) -> bool {
        match self {
            DockNode::Tabs(g) => g.views.is_empty() && g.area.is_none(),
            DockNode::Split { first, second, .. } => {
                first.is_disposable() && second.is_disposable()
            }
        }
    }

    /// Collapse splits whose child became disposable.
    fn prune(&mut self) {
        if let DockNode::Split { first, second, .. } = self {
            first.prune();
            second.prune();
            if first.is_disposable() {
                let keep =
                    std::mem::replace(second.as_mut(), DockNode::Tabs(TabGroup::new(None, vec![])));
                *self = keep;
            } else if second.is_disposable() {
                let keep =
                    std::mem::replace(first.as_mut(), DockNode::Tabs(TabGroup::new(None, vec![])));
                *self = keep;
            }
        }
    }

    fn group_with_area_mut(&mut self, area: &DockAreaId) -> Option<&mut TabGroup> {
        match self {
            DockNode::Tabs(g) => (g.area.as_ref() == Some(area)).then_some(g),
            DockNode::Split { first, second, .. } => first
                .group_with_area_mut(area)
                .or_else(|| second.group_with_area_mut(area)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub width: i32,
    pub height: i32,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub fullscreen: bool,
    /// Monitor connector/model name. Informational on Wayland, where clients
    /// cannot position windows; used where the platform allows it.
    #[serde(default)]
    pub monitor: Option<String>,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            width: 1100,
            height: 520,
            maximized: false,
            fullscreen: false,
            monitor: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowId(pub u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FloatingWindow {
    pub id: WindowId,
    pub root: DockNode,
    pub geometry: WindowGeometry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowRef {
    Main,
    Floating(WindowId),
}

/// Where a view currently is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewLocation {
    pub window: WindowRef,
    /// Path of the containing tab group.
    pub group_path: Vec<u8>,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("view {0} is not part of this layout")]
    UnknownView(ViewId),
    #[error("view {0} appears more than once")]
    DuplicateView(ViewId),
    #[error("no dock area named {0:?}")]
    UnknownArea(DockAreaId),
    #[error("no window {0:?}")]
    UnknownWindow(WindowId),
    #[error("invalid layout: {0}")]
    Invalid(&'static str),
}

/// Complete docking state of one workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    /// Every view instance known to the workspace, placed or not.
    pub views: BTreeMap<ViewId, ViewKind>,
    pub main: DockNode,
    #[serde(default)]
    pub main_geometry: Option<WindowGeometry>,
    #[serde(default)]
    pub floating: Vec<FloatingWindow>,
    /// Where each view returns when re-attached.
    #[serde(default)]
    pub home: BTreeMap<ViewId, DockAreaId>,
    #[serde(default)]
    next_window: u32,
    /// The master strip sits at the window's right edge, beside every view
    /// (the mixer then leaves its own master out).
    #[serde(default)]
    pub master_panel: bool,
}

impl WorkspaceLayout {
    pub fn new(views: BTreeMap<ViewId, ViewKind>, main: DockNode) -> Self {
        let mut layout = Self {
            views,
            main,
            main_geometry: None,
            floating: Vec::new(),
            home: BTreeMap::new(),
            next_window: 1,
            master_panel: false,
        };
        // Initial homes: wherever a view starts out.
        let mut homes = Vec::new();
        layout.main.for_each_group(&mut Vec::new(), &mut |_, g| {
            if let Some(area) = &g.area {
                for v in &g.views {
                    homes.push((v.clone(), area.clone()));
                }
            }
        });
        layout.home.extend(homes);
        layout
    }

    pub fn kind_of(&self, view: &ViewId) -> Option<ViewKind> {
        self.views.get(view).copied()
    }

    pub fn root(&self, window: WindowRef) -> Option<&DockNode> {
        match window {
            WindowRef::Main => Some(&self.main),
            WindowRef::Floating(id) => self.floating.iter().find(|w| w.id == id).map(|w| &w.root),
        }
    }

    pub fn root_mut(&mut self, window: WindowRef) -> Option<&mut DockNode> {
        match window {
            WindowRef::Main => Some(&mut self.main),
            WindowRef::Floating(id) => self
                .floating
                .iter_mut()
                .find(|w| w.id == id)
                .map(|w| &mut w.root),
        }
    }

    fn windows(&self) -> impl Iterator<Item = (WindowRef, &DockNode)> {
        std::iter::once((WindowRef::Main, &self.main)).chain(
            self.floating
                .iter()
                .map(|w| (WindowRef::Floating(w.id), &w.root)),
        )
    }

    pub fn locate(&self, view: &ViewId) -> Option<ViewLocation> {
        for (window, root) in self.windows() {
            let mut found = None;
            root.for_each_group(&mut Vec::new(), &mut |path, g| {
                if found.is_none()
                    && let Some(i) = g.views.iter().position(|v| v == view)
                {
                    found = Some(ViewLocation {
                        window,
                        group_path: path.to_vec(),
                        index: i,
                    });
                }
            });
            if found.is_some() {
                return found;
            }
        }
        None
    }

    pub fn is_placed(&self, view: &ViewId) -> bool {
        self.locate(view).is_some()
    }

    /// Is the view placed *and* currently on screen (its group visible and it
    /// is the active tab)?
    pub fn is_showing(&self, view: &ViewId) -> bool {
        let Some(loc) = self.locate(view) else {
            return false;
        };
        match self.group(loc.window, &loc.group_path) {
            Some(g) => g.is_visible() && g.active_view() == Some(view),
            None => false,
        }
    }

    pub fn group(&self, window: WindowRef, path: &[u8]) -> Option<&TabGroup> {
        match self.root(window)?.node_at(path)? {
            DockNode::Tabs(g) => Some(g),
            DockNode::Split { .. } => None,
        }
    }

    pub fn group_mut(&mut self, window: WindowRef, path: &[u8]) -> Option<&mut TabGroup> {
        match self.root_mut(window)?.node_at_mut(path)? {
            DockNode::Tabs(g) => Some(g),
            DockNode::Split { .. } => None,
        }
    }

    pub fn area_mut(&mut self, area: &DockAreaId) -> Option<&mut TabGroup> {
        if let Some(g) = self.main.group_with_area_mut(area) {
            return Some(g);
        }
        self.floating
            .iter_mut()
            .find_map(|w| w.root.group_with_area_mut(area))
    }

    pub fn area(&self, area: &DockAreaId) -> Option<&TabGroup> {
        let mut found = None;
        for (_, root) in self.windows() {
            root.for_each_group(&mut Vec::new(), &mut |_, g| {
                if found.is_none() && g.area.as_ref() == Some(area) {
                    found = Some(g);
                }
            });
        }
        found
    }

    /// Remove a view from wherever it is; prunes empty splits and windows.
    /// Returns the area the view was in (if that group had one).
    pub fn remove_view(&mut self, view: &ViewId) -> Option<Option<DockAreaId>> {
        let loc = self.locate(view)?;
        let group = self.group_mut(loc.window, &loc.group_path)?;
        let area = group.area.clone();
        group.remove(view);
        match loc.window {
            WindowRef::Main => self.main.prune(),
            WindowRef::Floating(id) => {
                if let Some(w) = self.floating.iter_mut().find(|w| w.id == id) {
                    w.root.prune();
                    if w.root.is_disposable() {
                        self.floating.retain(|w| w.id != id);
                    }
                }
            }
        }
        Some(area)
    }

    /// Detach a view into a new floating window and return its id.
    ///
    /// If the view already is the only view of a floating window, that
    /// window is returned unchanged.
    pub fn detach(
        &mut self,
        view: &ViewId,
        geometry: WindowGeometry,
    ) -> Result<WindowId, LayoutError> {
        if !self.views.contains_key(view) {
            return Err(LayoutError::UnknownView(view.clone()));
        }
        if let Some(ViewLocation {
            window: WindowRef::Floating(id),
            ..
        }) = self.locate(view)
            && self
                .floating
                .iter()
                .find(|w| w.id == id)
                .is_some_and(|w| w.root.views().len() == 1)
        {
            return Ok(id);
        }
        if let Some(Some(area)) = self.remove_view(view) {
            self.home.insert(view.clone(), area);
        }
        let id = WindowId(self.next_window.max(1));
        self.next_window = id.0 + 1;
        self.floating.push(FloatingWindow {
            id,
            root: DockNode::Tabs(TabGroup {
                tab_bar: TabBar::Auto,
                ..TabGroup::new(None, vec![view.clone()])
            }),
            geometry,
        });
        Ok(id)
    }

    /// Dock a view into `area` (or its home area, or "bottom") and make it
    /// the active, visible tab there.
    pub fn attach(&mut self, view: &ViewId, area: Option<DockAreaId>) -> Result<(), LayoutError> {
        if !self.views.contains_key(view) {
            return Err(LayoutError::UnknownView(view.clone()));
        }
        let target = area
            .or_else(|| self.home.get(view).cloned())
            .unwrap_or_else(DockAreaId::bottom);
        if self.area(&target).is_none() {
            return Err(LayoutError::UnknownArea(target));
        }
        self.remove_view(view);
        let group = self
            .area_mut(&target)
            .ok_or_else(|| LayoutError::UnknownArea(target.clone()))?;
        group.views.push(view.clone());
        group.active = group.views.len() - 1;
        group.hidden = false;
        self.home.insert(view.clone(), target);
        Ok(())
    }

    /// Close a floating window, returning its views to their home areas.
    pub fn close_window(&mut self, id: WindowId) -> Result<Vec<ViewId>, LayoutError> {
        let w = self
            .floating
            .iter()
            .find(|w| w.id == id)
            .ok_or(LayoutError::UnknownWindow(id))?;
        let views = w.root.views();
        for v in &views {
            self.attach(v, None)?;
        }
        self.floating.retain(|w| w.id != id);
        Ok(views)
    }

    /// Bring a view on screen: place it if needed, select its tab and
    /// un-hide its group. Returns its window.
    pub fn activate(&mut self, view: &ViewId) -> Result<WindowRef, LayoutError> {
        if !self.is_placed(view) {
            self.attach(view, None)?;
        }
        let loc = self
            .locate(view)
            .ok_or_else(|| LayoutError::UnknownView(view.clone()))?;
        if let Some(g) = self.group_mut(loc.window, &loc.group_path) {
            g.active = loc.index;
            g.hidden = false;
        }
        Ok(loc.window)
    }

    /// Hide (`true`) or show a named area.
    pub fn set_area_hidden(&mut self, area: &DockAreaId, hidden: bool) -> Result<(), LayoutError> {
        let g = self
            .area_mut(area)
            .ok_or_else(|| LayoutError::UnknownArea(area.clone()))?;
        g.hidden = hidden;
        Ok(())
    }

    pub fn toggle_area(&mut self, area: &DockAreaId) -> Result<bool, LayoutError> {
        let g = self
            .area_mut(area)
            .ok_or_else(|| LayoutError::UnknownArea(area.clone()))?;
        g.hidden = !g.hidden;
        Ok(!g.hidden)
    }

    /// Update a split ratio after the user dragged a divider.
    pub fn set_ratio(&mut self, window: WindowRef, path: &[u8], value: f32) -> bool {
        match self.root_mut(window).and_then(|r| r.node_at_mut(path)) {
            Some(DockNode::Split { ratio, .. }) => {
                *ratio = value.clamp(0.05, 0.95);
                true
            }
            _ => false,
        }
    }

    pub fn set_active(&mut self, window: WindowRef, path: &[u8], index: usize) -> bool {
        match self.group_mut(window, path) {
            Some(g) if index < g.views.len() => {
                g.active = index;
                true
            }
            _ => false,
        }
    }

    pub fn floating_window_mut(&mut self, id: WindowId) -> Option<&mut FloatingWindow> {
        self.floating.iter_mut().find(|w| w.id == id)
    }

    /// Check structural invariants.
    pub fn validate(&self) -> Result<(), LayoutError> {
        let mut seen = BTreeSet::new();
        let mut areas = BTreeSet::new();
        let mut result = Ok(());
        for (_, root) in self.windows() {
            root.for_each_group(&mut Vec::new(), &mut |_, g| {
                for v in &g.views {
                    if !self.views.contains_key(v) {
                        result = Err(LayoutError::UnknownView(v.clone()));
                    }
                    if !seen.insert(v.clone()) {
                        result = Err(LayoutError::DuplicateView(v.clone()));
                    }
                }
                if !g.views.is_empty() && g.active >= g.views.len() {
                    result = Err(LayoutError::Invalid("active tab out of range"));
                }
                if let Some(a) = &g.area
                    && !areas.insert(a.clone())
                {
                    result = Err(LayoutError::Invalid("duplicate dock area"));
                }
            });
            check_ratios(root, &mut result);
        }
        result
    }
}

fn check_ratios(node: &DockNode, result: &mut Result<(), LayoutError>) {
    if let DockNode::Split {
        ratio,
        first,
        second,
        ..
    } = node
    {
        if !(*ratio > 0.0 && *ratio < 1.0) {
            *result = Err(LayoutError::Invalid("split ratio outside 0..1"));
        }
        check_ratios(first, result);
        check_ratios(second, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Preset;

    fn layout() -> WorkspaceLayout {
        Preset::Recording.layout()
    }

    #[test]
    fn detach_and_reattach_mixer() {
        let mut l = layout();
        let mixer = ViewId::mixer();
        let before = l.locate(&mixer).unwrap();
        assert_eq!(before.window, WindowRef::Main);

        let id = l.detach(&mixer, WindowGeometry::default()).unwrap();
        let loc = l.locate(&mixer).unwrap();
        assert_eq!(loc.window, WindowRef::Floating(id));
        assert!(
            l.area(&DockAreaId::bottom()).is_some(),
            "bottom area persists"
        );
        l.validate().unwrap();
        // Detaching again is idempotent.
        assert_eq!(l.detach(&mixer, WindowGeometry::default()).unwrap(), id);

        l.attach(&mixer, None).unwrap();
        assert!(l.floating.is_empty(), "empty floating window removed");
        let after = l.locate(&mixer).unwrap();
        assert_eq!(after.window, WindowRef::Main);
        assert!(l.is_showing(&mixer));
        l.validate().unwrap();
    }

    #[test]
    fn close_window_returns_views_home() {
        let mut l = layout();
        let pr = ViewId::piano_roll();
        let id = l.detach(&pr, WindowGeometry::default()).unwrap();
        assert_eq!(l.close_window(id).unwrap(), vec![pr.clone()]);
        assert_eq!(l.locate(&pr).unwrap().window, WindowRef::Main);
        assert_eq!(
            l.close_window(WindowId(77)),
            Err(LayoutError::UnknownWindow(WindowId(77)))
        );
    }

    #[test]
    fn emptying_the_bottom_dock_hides_it_but_keeps_it() {
        let mut l = layout();
        let bottom_views = [
            ViewId::mixer(),
            ViewId::tools(),
            ViewId::album(),
            ViewId::piano_roll(),
            ViewId::automation(),
            ViewId::performance(),
        ];
        for v in &bottom_views {
            l.detach(v, WindowGeometry::default()).unwrap();
        }
        let bottom = l.area(&DockAreaId::bottom()).unwrap();
        assert!(bottom.views.is_empty());
        assert!(!bottom.is_visible());
        assert_eq!(l.floating.len(), bottom_views.len());
        l.validate().unwrap();
        l.attach(&ViewId::automation(), None).unwrap();
        assert!(l.area(&DockAreaId::bottom()).unwrap().is_visible());
    }

    #[test]
    fn activate_shows_hidden_dock_and_selects_tab() {
        let mut l = layout();
        l.set_area_hidden(&DockAreaId::bottom(), true).unwrap();
        assert!(!l.is_showing(&ViewId::piano_roll()));
        assert_eq!(l.activate(&ViewId::piano_roll()).unwrap(), WindowRef::Main);
        assert!(l.is_showing(&ViewId::piano_roll()));
        assert!(!l.is_showing(&ViewId::mixer()));
    }

    #[test]
    fn removing_a_view_without_area_prunes_split() {
        let mut views = BTreeMap::new();
        views.insert(ViewId::arranger(), ViewKind::Arranger);
        views.insert(ViewId::mixer(), ViewKind::Mixer);
        let mut l = WorkspaceLayout::new(
            views,
            DockNode::split(
                Axis::Horizontal,
                0.5,
                DockNode::Tabs(TabGroup::new(
                    Some(DockAreaId::main()),
                    vec![ViewId::arranger()],
                )),
                DockNode::Tabs(TabGroup::new(None, vec![ViewId::mixer()])),
            ),
        );
        l.remove_view(&ViewId::mixer());
        assert!(matches!(l.main, DockNode::Tabs(_)));
    }

    #[test]
    fn ratio_and_active_updates() {
        let mut l = layout();
        assert!(l.set_ratio(WindowRef::Main, &[], 1.5));
        match &l.main {
            DockNode::Split { ratio, .. } => assert_eq!(*ratio, 0.95),
            _ => panic!("expected split"),
        }
        assert!(!l.set_ratio(WindowRef::Main, &[0], 0.5));
        assert!(l.set_active(WindowRef::Main, &[1], 1));
        assert!(!l.set_active(WindowRef::Main, &[1], 10));
    }

    #[test]
    fn validate_detects_duplicates_and_unknown_views() {
        let mut l = layout();
        if let DockNode::Split { first, .. } = &mut l.main
            && let DockNode::Tabs(g) = first.as_mut()
        {
            g.views.push(ViewId::mixer());
        }
        assert_eq!(
            l.validate(),
            Err(LayoutError::DuplicateView(ViewId::mixer()))
        );
        let mut l = layout();
        assert_eq!(
            l.attach(&ViewId::new("nope"), None),
            Err(LayoutError::UnknownView(ViewId::new("nope")))
        );
    }
}

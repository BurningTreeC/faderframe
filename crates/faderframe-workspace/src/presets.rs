use crate::{Axis, DockAreaId, DockNode, TabBar, TabGroup, ViewId, ViewKind, WorkspaceLayout};
use std::collections::BTreeMap;

/// Built-in workspace presets (screensets).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Recording,
    Editing,
    Mixing,
    Midi,
    Mastering,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Recording,
        Preset::Editing,
        Preset::Mixing,
        Preset::Midi,
        Preset::Mastering,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Recording => "Recording",
            Preset::Editing => "Editing",
            Preset::Mixing => "Mixing",
            Preset::Midi => "MIDI",
            Preset::Mastering => "Mastering",
        }
    }

    pub fn layout(self) -> WorkspaceLayout {
        let mut views = BTreeMap::new();
        for kind in ViewKind::ALL {
            views.insert(kind.default_id(), kind);
        }
        let (ratio, active, hidden) = match self {
            // Large arranger, small mixer.
            Preset::Recording => (0.60, ViewId::mixer(), false),
            // Arranger nearly full screen; dock collapsed but available.
            Preset::Editing => (0.80, ViewId::piano_roll(), true),
            // Mixer dominates.
            Preset::Mixing => (0.22, ViewId::mixer(), false),
            // Arranger top, piano roll bottom.
            Preset::Midi => (0.42, ViewId::piano_roll(), false),
            // The meters (loudness, level, phase, spectrum) dominate.
            Preset::Mastering => (0.42, ViewId::tools(), false),
        };
        let mut bottom_views = vec![
            ViewId::mixer(),
            ViewId::tools(),
            ViewId::album(),
            ViewId::piano_roll(),
            ViewId::automation(),
            ViewId::performance(),
        ];
        // Mixing keeps the mix scenes beside the mixer.
        if self == Preset::Mixing {
            bottom_views.insert(1, ViewId::mix_scenes());
        }
        // Mixing and Mastering keep the reference tracks beside the
        // meters (a tab behind them): A/B with a released record.
        if matches!(self, Preset::Mixing | Preset::Mastering)
            && let Some(i) = bottom_views.iter().position(|v| *v == ViewId::tools())
        {
            bottom_views.insert(i + 1, ViewId::reference());
        }
        let active_idx = bottom_views.iter().position(|v| *v == active).unwrap_or(0);
        let main = DockNode::split(
            Axis::Vertical,
            ratio,
            DockNode::Tabs(TabGroup {
                tab_bar: TabBar::Auto,
                ..TabGroup::new(Some(DockAreaId::main()), vec![ViewId::arranger()])
            }),
            DockNode::Tabs(TabGroup {
                active: active_idx,
                hidden,
                ..TabGroup::new(Some(DockAreaId::bottom()), bottom_views)
            }),
        );
        let mut layout = WorkspaceLayout::new(views, main);
        layout.order_tabs();
        // Mastering keeps the master fader in view whatever is shown.
        layout.master_panel = self == Preset::Mastering;
        layout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tabs keep one order: the mixer first, then as the View menu lists
    /// the views; a view closed and shown again goes back to its place.
    #[test]
    fn tabs_keep_their_order() {
        let order = |l: &WorkspaceLayout| {
            l.area(&DockAreaId::bottom())
                .map(|g| g.views.clone())
                .unwrap_or_default()
        };
        let ranks = |views: &[ViewId], l: &WorkspaceLayout| {
            views
                .iter()
                .map(|v| l.kind_of(v).map_or(usize::MAX, |k| k.rank()))
                .collect::<Vec<_>>()
        };
        for p in Preset::ALL {
            let mut l = p.layout();
            let before = order(&l);
            assert_eq!(before.first(), Some(&ViewId::mixer()), "{p:?}");
            let r = ranks(&before, &l);
            assert!(r.windows(2).all(|w| w[0] < w[1]), "{p:?}: {before:?}");
            // The mixer closed and shown again: first again, and active.
            l.remove_view(&ViewId::mixer());
            l.activate(&ViewId::mixer()).unwrap();
            assert_eq!(order(&l), before);
            assert!(l.is_showing(&ViewId::mixer()));
            // A view that was not there joins in its place.
            l.activate(&ViewId::history()).unwrap();
            let after = order(&l);
            let r = ranks(&after, &l);
            assert!(r.windows(2).all(|w| w[0] < w[1]), "{after:?}");
            assert!(l.is_showing(&ViewId::history()));
        }
    }

    #[test]
    fn presets_are_valid_and_distinct() {
        let layouts: Vec<_> = Preset::ALL.iter().map(|p| p.layout()).collect();
        for l in &layouts {
            l.validate().unwrap();
            assert_eq!(l.views.len(), ViewKind::ALL.len());
            assert!(l.views.contains_key(&ViewId::performance()));
        }
        assert!(
            layouts[4].is_showing(&ViewId::tools()),
            "mastering shows the meters"
        );
        assert!(layouts[0].is_showing(&ViewId::mixer()));
        assert!(!layouts[1].is_showing(&ViewId::piano_roll()));
        assert!(layouts[3].is_showing(&ViewId::piano_roll()));
        // The mix scenes beside the mixer when mixing (a tab behind it).
        assert!(layouts[2].is_placed(&ViewId::mix_scenes()));
        assert!(!layouts[1].is_placed(&ViewId::mix_scenes()));
        // The reference tracks beside the meters when mixing and
        // mastering (a tab behind them, the meters showing).
        let placed: Vec<bool> = layouts
            .iter()
            .map(|l| l.is_placed(&ViewId::reference()))
            .collect();
        assert_eq!(placed, [false, false, true, false, true]);
        assert!(!layouts[4].is_showing(&ViewId::reference()));
        assert_ne!(layouts[0], layouts[2]);
        // The master strip at the side: Mastering only, by default.
        let panel: Vec<bool> = layouts.iter().map(|l| l.master_panel).collect();
        assert_eq!(panel, [false, false, false, false, true]);
    }

    /// A layout saved before the presets placed the Reference view loads
    /// as it was saved; the view joins the bottom dock in its place once
    /// shown.
    #[test]
    fn saved_layouts_without_the_reference_view_stay_as_saved() {
        let mut old = Preset::Mastering.layout();
        if let Some(g) = old.area_mut(&DockAreaId::bottom()) {
            g.views.retain(|v| *v != ViewId::reference());
            g.active = g
                .views
                .iter()
                .position(|v| *v == ViewId::tools())
                .unwrap_or(0);
        }
        old.validate().unwrap();
        let json = serde_json::to_string(&old).unwrap();
        let mut set: crate::WorkspaceSet =
            serde_json::from_str(&serde_json::to_string(&crate::WorkspaceSet::default()).unwrap())
                .unwrap();
        set.workspaces[4].layout = serde_json::from_str(&json).unwrap();
        set.sanitise();
        let loaded = &set.workspaces[4].layout;
        assert_eq!(*loaded, old, "unchanged by loading");
        assert!(!loaded.is_placed(&ViewId::reference()));
        assert!(loaded.is_showing(&ViewId::tools()));
        // Shown, it goes beside the meters; resetting gives the preset.
        let mut l = loaded.clone();
        l.activate(&ViewId::reference()).unwrap();
        let bottom = l.area(&DockAreaId::bottom()).unwrap();
        let at = |v: ViewId| bottom.views.iter().position(|x| *x == v);
        assert_eq!(at(ViewId::reference()), at(ViewId::tools()).map(|i| i + 1));
        set.switch_to(4);
        set.reset_active();
        assert_eq!(set.active_layout(), &Preset::Mastering.layout());
    }

    #[test]
    fn views_are_found_by_default_id() {
        for kind in ViewKind::ALL {
            assert_eq!(ViewKind::of_default_id(&kind.default_id()), Some(kind));
        }
        assert_eq!(ViewKind::of_default_id(&ViewId::new("nope")), None);
    }
}

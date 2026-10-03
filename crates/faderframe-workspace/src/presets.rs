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
        for kind in [
            ViewKind::Arranger,
            ViewKind::Mixer,
            ViewKind::PianoRoll,
            ViewKind::Automation,
        ] {
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
            // Mixer + meters (analyzers to come).
            Preset::Mastering => (0.30, ViewId::mixer(), false),
        };
        let bottom_views = vec![ViewId::mixer(), ViewId::piano_roll(), ViewId::automation()];
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
        WorkspaceLayout::new(views, main)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_and_distinct() {
        let layouts: Vec<_> = Preset::ALL.iter().map(|p| p.layout()).collect();
        for l in &layouts {
            l.validate().unwrap();
            assert_eq!(l.views.len(), 4);
        }
        assert!(layouts[0].is_showing(&ViewId::mixer()));
        assert!(!layouts[1].is_showing(&ViewId::piano_roll()));
        assert!(layouts[3].is_showing(&ViewId::piano_roll()));
        assert_ne!(layouts[0], layouts[2]);
    }
}

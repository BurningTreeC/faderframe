#![allow(clippy::unwrap_used)]
//! Favorite plugins (issue #15) are the user's, not the project's:
//! changing them leaves the project, its undo history, its saved state and
//! every plugin's state alone, and they outlive the project.

use faderframe_engine::EngineConfig;
use faderframe_session::plugin_favorites::{FavoriteOp, FavoritePlugin, PluginFavorites};
use faderframe_session::{Action, Session};

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

#[test]
fn changing_favorites_never_touches_the_project() {
    let mut s = session();
    let eq = s
        .available_plugins()
        .into_iter()
        .find(|p| p.plugin.id == faderframe_core::builtin::EQ)
        .unwrap();
    // A plugin on a track, to see its state stays.
    let track = s.project().tracks[0].id;
    let inserts = s.project().tracks[0].inserts.len();
    s.dispatch(Action::InsertPlugin {
        track,
        index: 0,
        plugin: eq.plugin.clone(),
    })
    .unwrap();
    s.capture_plugin_states();
    let project = serde_json::to_string(s.project()).unwrap();
    let undo = s.history_steps();
    let dirty = s.is_dirty();
    let revision = s.favorites_revision();

    let plugin = FavoritePlugin::of(&eq);
    s.dispatch(Action::PluginFavorites(FavoriteOp::NewCategory {
        name: "Mixing".into(),
        with: Some(plugin.clone()),
    }))
    .unwrap();
    assert!(s.plugin_favorites().is_favorite(&eq.plugin));
    let mixing = s.plugin_favorites().categories()[0].id;
    s.dispatch(Action::PluginFavorites(FavoriteOp::RenameCategory {
        category: mixing,
        name: "Mix".into(),
    }))
    .unwrap();
    s.dispatch(Action::PluginFavorites(FavoriteOp::Set {
        plugin,
        favorite: false,
    }))
    .unwrap();
    s.dispatch(Action::PluginFavorites(FavoriteOp::RemoveCategory(mixing)))
        .unwrap();
    assert!(
        s.favorites_revision() >= revision + 4,
        "the shell saves them"
    );

    s.capture_plugin_states();
    assert_eq!(serde_json::to_string(s.project()).unwrap(), project);
    assert_eq!(s.history_steps(), undo, "no undo steps");
    assert_eq!(s.is_dirty(), dirty, "nothing to save in the project");
    // Undo takes back the insert, not a favorite.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks[0].inserts.len(), inserts);
    assert!(s.plugin_favorites().favorites().is_empty());
}

#[test]
fn a_refused_change_is_a_notice_and_favorites_outlive_the_project() {
    let mut s = session();
    let before = s.notices().count();
    s.dispatch(Action::PluginFavorites(FavoriteOp::NewCategory {
        name: "   ".into(),
        with: None,
    }))
    .unwrap();
    assert!(s.notices().count() > before, "told why");
    assert!(s.plugin_favorites().categories().is_empty());

    let mut favorites = PluginFavorites::default();
    let first = s.available_plugins().into_iter().next().unwrap();
    favorites
        .apply(FavoriteOp::Set {
            plugin: FavoritePlugin::of(&first),
            favorite: true,
        })
        .unwrap();
    s.set_plugin_favorites(favorites.clone());
    // Another project: the favorites stay.
    s.new_project(false).unwrap();
    assert_eq!(s.plugin_favorites(), &favorites);
}

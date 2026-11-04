//! Development aids for show-mode gear changes and articulation and drum
//! maps (`FADERFRAME_STARTUP_ACTIONS`):
//!
//! * `show-gear:<song n>=<program>[/<bank>]` — the song's program change
//!   (1–128, a bank 0–16383) on the default output's channel 1;
//! * `articulation-map:<track>=<name>` — the track plays by the map named:
//!   the project's, a template (`strings`, `brass`), a user file's, a
//!   file's path (FaderFrame's JSON or a Cubase `.expressionmap`), `none`;
//! * `drum-map:<track>=<gm|pads|name|path|none>` — likewise for drum maps
//!   (`pads`: the track's Drum Sampler's);
//! * `edit-map:<articulation|drum>=<track>` — the map editor on the
//!   track's map of that kind (an empty articulation map or the General
//!   MIDI drum map made first when it has none).

use crate::state::AppState;
use faderframe_core::TrackId;
use faderframe_session::Action;
use faderframe_session::maps::{MapKind, MapOp};
use faderframe_session::setlist::SetlistOp;
use std::rc::Rc;

fn track_named(a: &Rc<AppState>, name: &str) -> Option<TrackId> {
    a.session
        .borrow()
        .project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.id)
}

pub(crate) fn show_gear(a: &Rc<AppState>, arg: &str) {
    match faderframe_session::show_gear::parse_show_gear(arg) {
        Some((song, program, bank)) => a.dispatch(Action::Setlist(SetlistOp::GearProgram {
            song,
            program,
            bank,
        })),
        None => tracing::warn!("show-gear: '{arg}' is not <song n>=<program>[/<bank>]"),
    }
}

pub(crate) fn articulation_map(a: &Rc<AppState>, arg: &str) {
    let Some((name, map)) = arg.split_once('=') else {
        return tracing::warn!("articulation-map: '{arg}' is not <track>=<name>");
    };
    let Some(track) = track_named(a, name) else {
        return tracing::warn!("articulation-map: no track '{name}'");
    };
    let done = a
        .session
        .borrow_mut()
        .use_articulation_map_named(track, map);
    match done {
        Ok(()) => a.after_change(),
        Err(e) => a.report(e, false),
    }
}

pub(crate) fn drum_map(a: &Rc<AppState>, arg: &str) {
    let Some((name, map)) = arg.split_once('=') else {
        return tracing::warn!("drum-map: '{arg}' is not <track>=<gm|pads|file>");
    };
    let Some(track) = track_named(a, name) else {
        return tracing::warn!("drum-map: no track '{name}'");
    };
    let done = a.session.borrow_mut().use_drum_map_named(track, map);
    match done {
        Ok(()) => a.after_change(),
        Err(e) => a.report(e, false),
    }
}

pub(crate) fn edit_map(a: &Rc<AppState>, arg: &str) {
    let Some((kind, name)) = arg.split_once('=') else {
        return tracing::warn!("edit-map: '{arg}' is not <articulation|drum>=<track>");
    };
    let kind = match kind.trim() {
        "articulation" | "articulations" => MapKind::Articulation,
        "drum" | "drums" => MapKind::Drum,
        other => return tracing::warn!("edit-map: '{other}' is not articulation or drum"),
    };
    let Some(track) = track_named(a, name.trim()) else {
        return tracing::warn!("edit-map: no track '{name}'");
    };
    a.dispatch(Action::Maps(MapOp::EditTrackMap { track, kind }));
}

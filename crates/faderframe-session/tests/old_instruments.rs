//! Older projects and track presets kept an instrument track's instrument
//! apart from its inserts; opened now, it is an insert like any other.
#![allow(clippy::unwrap_used)]

use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{PluginRef, PluginSlot, Project, TrackKind};
use faderframe_session::{Action, Session};

fn slot(p: &mut Project, id: &str) -> PluginSlot {
    PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    }
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn ids(s: &Session, t: TrackId) -> Vec<String> {
    s.project()
        .track(t)
        .unwrap()
        .inserts
        .iter()
        .map(|p| p.plugin.id.clone())
        .collect()
}

#[test]
fn old_style_instruments_become_inserts_when_a_project_opens() {
    let dir = std::env::temp_dir().join(format!("ff-old-instruments-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // An old project: the demo's Lead Synth with its synth kept apart and an
    // arpeggiator before it; a second track with an old-style synth under
    // a synth insert (never heard).
    let mut p = faderframe_project::demo::demo_project(48_000);
    let lead = p
        .tracks
        .iter()
        .position(|t| t.name == "Lead Synth")
        .unwrap();
    let arp = slot(&mut p, builtin::ARPEGGIATOR);
    let t = &mut p.tracks[lead];
    let synth = t.inserts.remove(0);
    let synth_id = synth.id;
    t.instrument = Some(synth);
    t.inserts = vec![arp];
    let mut two = p.tracks[lead].clone();
    two.id = p.ids.allocate();
    two.name = "Two Synths".into();
    two.clips.clear();
    two.sends.clear();
    let old = slot(&mut p, builtin::SYNTH);
    let new = slot(&mut p, builtin::SYNTH);
    let kept = new.id;
    two.instrument = Some(old);
    two.inserts = vec![new];
    p.tracks.insert(lead + 1, two);
    let path = dir.join("old.ffproj");
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    s.save_as(&path).unwrap();

    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let notes = s.open(&path).unwrap();
    let lead = track(&s, "Lead Synth");
    let t = s.project().track(lead).unwrap();
    assert!(t.instrument.is_none());
    assert_eq!(ids(&s, lead), [builtin::ARPEGGIATOR, builtin::SYNTH]);
    assert_eq!(
        t.inserts[1].id, synth_id,
        "the same instrument, after the effect"
    );
    let two = track(&s, "Two Synths");
    let t = s.project().track(two).unwrap();
    assert!(t.instrument.is_none());
    assert_eq!(t.inserts.len(), 1);
    assert_eq!(t.inserts[0].id, kept, "the one that was heard stays");
    assert!(
        notes
            .iter()
            .any(|n| n.contains("'Two Synths'") && n.contains("removed")),
        "{notes:?}"
    );
    // It plays: the instrument track is the Lead Synth's instrument.
    assert_eq!(s.instrument_slot(t).unwrap().id, kept);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_track_preset_with_an_old_style_instrument_gives_an_insert() {
    let dir = std::env::temp_dir().join(format!("ff-old-preset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut p = faderframe_project::demo::demo_project(48_000);
    let lead = p
        .tracks
        .iter()
        .position(|t| t.name == "Lead Synth")
        .unwrap();
    let synth = p.tracks[lead].inserts.remove(0);
    p.tracks[lead].instrument = Some(synth);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    let path = dir.join("Lead.fftrack");
    s.export_track_preset(lead, &path).unwrap();
    // A new track from it.
    let new = s.add_track_from_preset(&path).unwrap();
    let t = s.project().track(new).unwrap();
    assert_eq!(t.kind, TrackKind::Instrument);
    assert!(t.instrument.is_none());
    assert_eq!(ids(&s, new), [builtin::SYNTH]);
    // Applied to an instrument track that has a synth insert already.
    let target = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::ApplyTrackPreset {
        track: target,
        path: path.clone(),
    })
    .unwrap();
    let t = s.project().track(target).unwrap();
    assert!(t.instrument.is_none());
    assert_eq!(ids(&s, target), [builtin::SYNTH]);
    let _ = std::fs::remove_dir_all(&dir);
}

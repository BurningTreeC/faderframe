//! Merges of project versions.

use super::*;
use crate::{AudioClip, MidiNote, TrackColor, TrackKind};
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::{ClipId, NoteId, TrackId};
use faderframe_timeline::MusicalTime;

fn demo() -> Project {
    crate::demo::demo_project(48_000)
}

fn track(p: &Project, name: &str) -> TrackId {
    p.tracks.iter().find(|t| t.name == name).unwrap().id
}

fn clip(p: &Project, name: &str) -> ClipId {
    p.clips.values().find(|c| c.name == name).unwrap().id
}

fn audio_mut<'a>(p: &'a mut Project, name: &str) -> &'a mut AudioClip {
    let id = clip(p, name);
    match &mut p.clips.get_mut(&id).unwrap().content {
        ClipContent::Audio(a) => a,
        _ => panic!("not audio"),
    }
}

fn plain(base: &Project, ours: &Project, theirs: &Project) -> Merged {
    merge(base, ours, theirs, &MergeOptions::default()).unwrap()
}

fn resolved(base: &Project, ours: &Project, theirs: &Project, key: &str, r: Resolution) -> Merged {
    let mut options = MergeOptions::default();
    options.resolutions.insert(key.to_string(), r);
    merge(base, ours, theirs, &options).unwrap()
}

fn q(quarters: f64) -> MusicalTime {
    MusicalTime::from_quarters(quarters)
}

/// A new track with a clip of its own (ids from the allocator).
fn add_track_with_clip(p: &mut Project, name: &str) -> (TrackId, ClipId) {
    let id: TrackId = p.ids.allocate();
    let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(2));
    let clip_id: ClipId = p.ids.allocate();
    let mut c = p.clip(clip(p, "Pad")).unwrap().clone();
    c.id = clip_id;
    c.track = id;
    c.start = q(8.0);
    c.name = format!("{name} clip");
    t.clips.push(clip_id);
    p.clips.insert(clip_id, c);
    let at = p.tracks.len() - 1;
    p.tracks.insert(at, t);
    (id, clip_id)
}

#[test]
fn the_same_versions_merge_to_themselves() {
    let p = demo();
    let m = plain(&p, &p, &p);
    assert!(m.conflicts.is_empty());
    assert!(m.changes.is_empty(), "{:#?}", m.changes);
    assert_eq!(m.project, p);
}

#[test]
fn disjoint_edits_on_both_sides_all_arrive() {
    let base = demo();
    let mut ours = base.clone();
    let mut theirs = base.clone();
    let bass = track(&base, "Bass");
    let drums = track(&base, "Drums");
    ours.track_mut(bass).unwrap().volume_db = -9.0;
    theirs.track_mut(drums).unwrap().name = "Kit".into();
    theirs.track_mut(bass).unwrap().pan = 0.25;
    theirs.loop_enabled = !base.loop_enabled;
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let p = &m.project;
    assert_eq!(p.track(bass).unwrap().volume_db, -9.0);
    assert_eq!(p.track(bass).unwrap().pan, 0.25);
    assert_eq!(p.track(drums).unwrap().name, "Kit");
    assert_eq!(p.loop_enabled, !base.loop_enabled);
    assert!(
        m.changes.iter().any(|c| c == "‘Kit’: renamed from ‘Drums’"),
        "{:#?}",
        m.changes
    );
}

#[test]
fn the_same_change_on_both_sides_is_no_conflict() {
    let base = demo();
    let bass = track(&base, "Bass");
    let mut ours = base.clone();
    ours.track_mut(bass).unwrap().mute = true;
    let theirs = ours.clone();
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty());
    assert!(m.project.track(bass).unwrap().mute);
}

#[test]
fn a_field_both_changed_is_a_conflict_ours_by_default() {
    let base = demo();
    let bass = track(&base, "Bass");
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    ours.track_mut(bass).unwrap().volume_db = -6.0;
    theirs.track_mut(bass).unwrap().volume_db = -12.0;
    // Theirs also changed something nobody else did.
    theirs.track_mut(bass).unwrap().solo = true;
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1, "{:#?}", m.conflicts);
    let c = &m.conflicts[0];
    assert_eq!(c.what, "Track ‘Bass’");
    assert_eq!(c.kind, ConflictKind::BothChanged);
    assert_eq!(c.chosen, Resolution::Ours);
    assert_eq!(c.choices, [Resolution::Ours, Resolution::Theirs]);
    assert_eq!(c.differences[0].field, "volume");
    assert!(c.differences[0].ours.contains("-6.0") || c.differences[0].ours.contains("−6.0"));
    assert!(
        c.summary().starts_with("Track ‘Bass’ — volume:"),
        "{}",
        c.summary()
    );
    assert_eq!(m.project.track(bass).unwrap().volume_db, -6.0);
    assert!(m.project.track(bass).unwrap().solo);
    // Theirs chosen.
    let m = resolved(&base, &ours, &theirs, &c.key, Resolution::Theirs);
    assert_eq!(m.project.track(bass).unwrap().volume_db, -12.0);
    assert_eq!(m.conflicts[0].chosen, Resolution::Theirs);
    // "Both" is not a choice for a track: ours.
    let m = resolved(&base, &ours, &theirs, &c.key, Resolution::Both);
    assert_eq!(m.project.track(bass).unwrap().volume_db, -6.0);
    assert_eq!(m.conflicts[0].chosen, Resolution::Ours);
}

#[test]
fn removed_on_one_side_and_changed_on_the_other_is_a_conflict() {
    let base = demo();
    let bassline = clip(&base, "Bassline");
    let bass = track(&base, "Bass");
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    ours.clips.remove(&bassline);
    ours.track_mut(bass)
        .unwrap()
        .clips
        .retain(|c| *c != bassline);
    audio_mut(&mut theirs, "Bassline").gain_db = -4.0;
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1, "{:#?}", m.conflicts);
    let c = &m.conflicts[0];
    assert_eq!(c.kind, ConflictKind::RemovedHere);
    assert!(
        c.what.starts_with("Clip ‘Bassline’ on ‘Bass’"),
        "{}",
        c.what
    );
    assert_eq!(c.choices, [Resolution::Ours, Resolution::Theirs]);
    assert!(m.project.clip(bassline).is_none());
    assert!(!m.project.track(bass).unwrap().clips.contains(&bassline));
    // Theirs: the clip stays, changed, listed on its track again.
    let m = resolved(&base, &ours, &theirs, &c.key, Resolution::Theirs);
    let kept = m.project.clip(bassline).unwrap();
    assert_eq!(kept.as_audio().unwrap().gain_db, -4.0);
    assert!(m.project.track(bass).unwrap().clips.contains(&bassline));
}

#[test]
fn a_clip_moved_on_one_side_and_edited_on_the_other_takes_both() {
    let base = demo();
    let id = clip(&base, "Pad");
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    let start = base.clip(id).unwrap().start;
    ours.clips.get_mut(&id).unwrap().start = start + q(8.0);
    let a = audio_mut(&mut theirs, "Pad");
    a.gain_db = 3.0;
    a.fades.fade_in = 4_800;
    a.length -= 48_000;
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let c = m.project.clip(id).unwrap();
    assert_eq!(c.start, start + q(8.0));
    let a = c.as_audio().unwrap();
    assert_eq!((a.gain_db, a.fades.fade_in), (3.0, 4_800));
    assert_eq!(
        a.length,
        base.clip(id).unwrap().as_audio().unwrap().length - 48_000
    );
    // Both moving it: a conflict over the position, the edits still in.
    theirs.clips.get_mut(&id).unwrap().start = start + q(4.0);
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1);
    assert_eq!(m.conflicts[0].differences[0].field, "position");
    assert_eq!(m.conflicts[0].choices.len(), 3, "clips may be kept twice");
    assert_eq!(m.project.clip(id).unwrap().start, start + q(8.0));
    assert_eq!(m.project.clip(id).unwrap().as_audio().unwrap().gain_db, 3.0);
    // Both: theirs' clip beside ours, with an id of its own.
    let m = resolved(&base, &ours, &theirs, &m.conflicts[0].key, Resolution::Both);
    let pads: Vec<&Clip> = m
        .project
        .clips
        .values()
        .filter(|c| c.name == "Pad")
        .collect();
    assert_eq!(pads.len(), 2);
    assert!(pads.iter().any(|c| c.start == start + q(4.0)));
    let track = m.project.track(pads[0].track).unwrap();
    assert!(pads.iter().all(|c| track.clips.contains(&c.id)));
}

fn notes_mut<'a>(p: &'a mut Project, name: &str) -> &'a mut Vec<MidiNote> {
    let id = clip(p, name);
    &mut p.clips.get_mut(&id).unwrap().as_midi_mut().unwrap().notes
}

#[test]
fn notes_merge_one_by_one() {
    let base = demo();
    let melody = clip(&base, "Melody");
    let notes: Vec<NoteId> = base
        .clip(melody)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .map(|n| n.id)
        .collect();
    assert!(notes.len() >= 4);
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    // Ours: a velocity changed, a note added.
    let added: NoteId = ours.ids.allocate();
    {
        let n = notes_mut(&mut ours, "Melody");
        n.iter_mut().find(|n| n.id == notes[0]).unwrap().velocity = 33;
        n.push(MidiNote {
            fine: Default::default(),
            id: added,
            start: q(0.5),
            length: q(0.25),
            key: 90,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
            time_locked: false,
            articulation: None,
        });
    }
    // Theirs: one removed, one moved, one added (the same id ours used).
    let theirs_added: NoteId = theirs.ids.allocate();
    assert_eq!(theirs_added, added);
    {
        let n = notes_mut(&mut theirs, "Melody");
        n.retain(|n| n.id != notes[1]);
        n.iter_mut().find(|n| n.id == notes[2]).unwrap().start += q(1.0);
        n.push(MidiNote {
            fine: Default::default(),
            id: theirs_added,
            start: q(1.5),
            length: q(0.25),
            key: 30,
            velocity: 90,
            channel: 0,
            muted: false,
            release: None,
            time_locked: false,
            articulation: None,
        });
    }
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let merged = m.project.clip(melody).unwrap().as_midi().unwrap();
    assert_eq!(merged.note(notes[0]).unwrap().velocity, 33);
    assert!(merged.note(notes[1]).is_none());
    let moved = base
        .clip(melody)
        .unwrap()
        .as_midi()
        .unwrap()
        .note(notes[2])
        .unwrap()
        .start
        + q(1.0);
    assert_eq!(merged.note(notes[2]).unwrap().start, moved);
    assert!(merged.notes.iter().any(|n| n.key == 90 && n.id == added));
    let theirs_note = merged.notes.iter().find(|n| n.key == 30).unwrap();
    assert_ne!(
        theirs_note.id, added,
        "theirs' new note got an id of its own"
    );
    assert!(m.project.ids.peek() > theirs_note.id.raw());
    // Both moving one note: a conflict about that note; both = two notes.
    let ours_start = {
        let n = notes_mut(&mut ours, "Melody");
        let n = n.iter_mut().find(|n| n.id == notes[3]).unwrap();
        n.key += 1;
        n.start
    };
    notes_mut(&mut theirs, "Melody")
        .iter_mut()
        .find(|n| n.id == notes[3])
        .unwrap()
        .key += 2;
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1, "{:#?}", m.conflicts);
    assert!(
        m.conflicts[0].what.contains("Note"),
        "{}",
        m.conflicts[0].what
    );
    let m = resolved(&base, &ours, &theirs, &m.conflicts[0].key, Resolution::Both);
    let at: Vec<u8> = m
        .project
        .clip(melody)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .filter(|n| n.start == ours_start)
        .map(|n| n.key)
        .collect();
    let k = base
        .clip(melody)
        .unwrap()
        .as_midi()
        .unwrap()
        .note(notes[3])
        .unwrap()
        .key;
    assert!(at.contains(&(k + 1)) && at.contains(&(k + 2)), "{at:?}");
}

fn lane(id: faderframe_core::AutomationLaneId, points: &[(f64, f64)]) -> AutomationLane {
    AutomationLane {
        id,
        target: AutomationTarget::TrackVolume,
        curve: AutomationCurve::from_points(
            points
                .iter()
                .map(|&(t, v)| AutomationPoint {
                    time: q(t),
                    value: v,
                    shape: CurveShape::default(),
                })
                .collect(),
        ),
        mode: AutomationMode::default(),
        visible: true,
    }
}

#[test]
fn automation_merges_by_the_stretches_each_side_changed() {
    let mut base = demo();
    let bass = track(&base, "Bass");
    let id = base.ids.allocate();
    let points: Vec<(f64, f64)> = (0..17).map(|i| (i as f64 * 4.0, 0.5)).collect();
    base.track_mut(bass)
        .unwrap()
        .automation
        .lanes
        .push(lane(id, &points));
    let edit = |p: &mut Project, from: f64, to: f64, value: f64| {
        let l = &mut p.track_mut(bass).unwrap().automation.lanes[0];
        let pts: Vec<(f64, f64)> = l
            .curve
            .points()
            .iter()
            .map(|pt| {
                let t = pt.time.quarters();
                (
                    t,
                    if t >= from && t <= to {
                        value
                    } else {
                        pt.value
                    },
                )
            })
            .collect();
        *l = lane(l.id, &pts);
    };
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    edit(&mut ours, 4.0, 8.0, 0.9);
    edit(&mut theirs, 40.0, 48.0, 0.1);
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let merged = &m.project.track(bass).unwrap().automation.lanes[0];
    let value = |t: f64| {
        merged
            .curve
            .points()
            .iter()
            .find(|p| p.time == q(t))
            .unwrap()
            .value
    };
    assert_eq!(
        (value(4.0), value(8.0), value(44.0), value(20.0)),
        (0.9, 0.9, 0.1, 0.5)
    );
    assert_eq!(merged.curve.points().len(), 17);
    // Overlapping stretches: a conflict over the lane, ours by default,
    // theirs' stretch where chosen (and ours' other stretch kept).
    edit(&mut theirs, 6.0, 12.0, 0.2);
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1, "{:#?}", m.conflicts);
    let c = &m.conflicts[0];
    assert_eq!(c.kind, ConflictKind::SamePlace);
    assert!(c.what.contains("Automation of volume"), "{}", c.what);
    assert!(
        c.differences[0].field.starts_with("points from"),
        "{:?}",
        c.differences
    );
    let m = resolved(&base, &ours, &theirs, &c.key, Resolution::Theirs);
    let merged = &m.project.track(bass).unwrap().automation.lanes[0];
    let value = |t: f64| {
        merged
            .curve
            .points()
            .iter()
            .find(|p| p.time == q(t))
            .unwrap()
            .value
    };
    assert_eq!(
        (value(4.0), value(8.0), value(12.0), value(44.0)),
        (0.5, 0.2, 0.2, 0.1)
    );
}

#[test]
fn both_sides_adding_things_keep_them_apart() {
    let base = demo();
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    let (ot, oc) = add_track_with_clip(&mut ours, "Vocals");
    let (tt, tc) = add_track_with_clip(&mut theirs, "Guitar");
    // The same numbers on both sides, for different things.
    assert_eq!((ot, oc), (tt, tc));
    let scene = |p: &mut Project, name: &str| {
        let id = p.ids.allocate();
        p.mix_scenes.scenes.push(crate::scenes::MixScene {
            id,
            name: name.into(),
            mix: Default::default(),
            scope: Default::default(),
            safe: Default::default(),
            fade: None,
        });
    };
    scene(&mut ours, "Verse mix");
    scene(&mut theirs, "Chorus mix");
    let map = |p: &mut Project, name: &str| {
        let id = p.ids.allocate();
        p.articulation_maps
            .push(crate::articulation::ArticulationMap::template(id, name).unwrap());
    };
    map(&mut ours, "strings");
    map(&mut theirs, "brass");
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let p = &m.project;
    let vocals = track(p, "Vocals");
    let guitar = track(p, "Guitar");
    assert_eq!(vocals, ot);
    assert_ne!(guitar, ot);
    let guitar_clip = p.clips.values().find(|c| c.name == "Guitar clip").unwrap();
    assert_eq!(guitar_clip.track, guitar);
    assert_eq!(p.track(guitar).unwrap().clips, [guitar_clip.id]);
    assert_eq!(p.track(vocals).unwrap().clips, [oc]);
    let names: Vec<&str> = p
        .mix_scenes
        .scenes
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["Verse mix", "Chorus mix"]);
    assert_eq!(p.articulation_maps.len(), 2);
    let ids: BTreeSet<u64> = p.articulation_maps.iter().map(|m| m.id.raw()).collect();
    assert_eq!(ids.len(), 2);
    // The allocator is past everything; theirs' move is reported.
    assert!(p.ids.peek() > guitar_clip.id.raw());
    let (start, end, by) = m.moved.unwrap();
    assert_eq!(start, base.ids.peek());
    assert_eq!(end, theirs.ids.peek());
    assert_eq!(by as u64, ours.ids.peek() - base.ids.peek());
    // Theirs' track keeps its place in the order: before the master.
    let order: Vec<&str> = p.tracks.iter().map(|t| t.name.as_str()).collect();
    let master = order.iter().position(|n| *n == "Master").unwrap();
    assert!(
        order.iter().position(|n| *n == "Guitar").unwrap() < master,
        "{order:?}"
    );
}

#[test]
fn plugin_settings_merge_by_value_until_a_state_is_loaded() {
    let base = demo();
    let lead = track(&base, "Lead Synth");
    let slot_index = 0;
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    let set = |p: &mut Project, id: u32, v: f64| {
        let slot = &mut p.track_mut(lead).unwrap().inserts[slot_index];
        match slot.parameters.iter_mut().find(|s| s.id.0 == id) {
            Some(s) => s.value = v,
            None => slot.parameters.push(crate::SavedParameter {
                id: faderframe_core::ParameterId(id),
                value: v,
            }),
        }
    };
    set(&mut ours, 101, 0.25);
    set(&mut theirs, 102, 0.75);
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let slot = &m.project.track(lead).unwrap().inserts[slot_index];
    assert_eq!(slot.saved(faderframe_core::ParameterId(101)), Some(0.25));
    assert_eq!(slot.saved(faderframe_core::ParameterId(102)), Some(0.75));
    // A state loaded on one side, a value on the other: one conflict over
    // the settings, taken whole.
    ours.track_mut(lead).unwrap().inserts[slot_index].state = Some("AAAA".into());
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1, "{:#?}", m.conflicts);
    assert_eq!(m.conflicts[0].differences[0].field, "settings");
    let slot = &m.project.track(lead).unwrap().inserts[slot_index];
    assert_eq!(slot.state.as_deref(), Some("AAAA"));
    assert_eq!(slot.saved(faderframe_core::ParameterId(102)), None);
    let m = resolved(
        &base,
        &ours,
        &theirs,
        &m.conflicts[0].key,
        Resolution::Theirs,
    );
    let slot = &m.project.track(lead).unwrap().inserts[slot_index];
    assert_eq!(
        slot.state,
        base.track(lead).unwrap().inserts[slot_index].state
    );
    assert_eq!(slot.saved(faderframe_core::ParameterId(102)), Some(0.75));
}

#[test]
fn the_order_of_tracks_and_plugins_merges() {
    let base = demo();
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    // Ours moves the first track to the end (before the master); theirs
    // adds a track after the second.
    let first = ours.tracks.remove(0);
    let at = ours.tracks.len() - 1;
    ours.tracks.insert(at, first.clone());
    let id: TrackId = theirs.ids.allocate();
    theirs.tracks.insert(
        2,
        Track::new(id, TrackKind::Bus, "New Bus", TrackColor::palette(1)),
    );
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let names: Vec<&str> = m.project.tracks.iter().map(|t| t.name.as_str()).collect();
    let after = names
        .iter()
        .position(|n| *n == base.tracks[1].name)
        .unwrap();
    assert_eq!(names[after + 1], "New Bus", "{names:?}");
    assert_eq!(names[names.len() - 2], first.name, "{names:?}");
    // Both reordering: a conflict over the order.
    theirs.tracks.swap(0, 1);
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1, "{:#?}", m.conflicts);
    assert_eq!(m.conflicts[0].kind, ConflictKind::Order);
    assert_eq!(m.conflicts[0].what, "Order of Tracks");
}

#[test]
fn tempo_and_markers_merge() {
    let base = demo();
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    let tempo = |p: &mut Project, at: f64, bpm: f64| {
        p.timeline.tempo.set_point(faderframe_timeline::TempoPoint {
            position: q(at),
            bpm,
            curve: Default::default(),
        });
    };
    tempo(&mut ours, 64.0, 100.0);
    tempo(&mut theirs, 128.0, 140.0);
    let marker = |p: &mut Project, at: f64, name: &str| {
        let id = p.ids.allocate();
        p.markers.push(crate::Marker {
            id,
            position: q(at),
            name: name.into(),
            time_base: Default::default(),
        });
    };
    marker(&mut ours, 16.0, "Bridge");
    marker(&mut theirs, 32.0, "Solo");
    let m = plain(&base, &ours, &theirs);
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let tempo = &m.project.timeline.tempo;
    assert_eq!(tempo.bpm_at(q(64.0)), 100.0);
    assert_eq!(tempo.bpm_at(q(128.0)), 140.0);
    let names: BTreeSet<&str> = m.project.markers.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains("Bridge") && names.contains("Solo"));
    // The same marker renamed apart: both keeps two.
    let base = m.project.clone();
    let id = base.markers.iter().find(|m| m.name == "Solo").unwrap().id;
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    ours.markers.iter_mut().find(|m| m.id == id).unwrap().name = "Guitar Solo".into();
    theirs.markers.iter_mut().find(|m| m.id == id).unwrap().name = "Sax Solo".into();
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts[0].what, "Marker ‘Guitar Solo’");
    let m = resolved(&base, &ours, &theirs, &m.conflicts[0].key, Resolution::Both);
    let names: Vec<&str> = m.project.markers.iter().map(|m| m.name.as_str()).collect();
    assert!(
        names.contains(&"Guitar Solo") && names.contains(&"Sax Solo"),
        "{names:?}"
    );
}

#[test]
fn a_track_removed_on_one_side_takes_the_others_clip_edits_with_it() {
    let base = demo();
    let bass = track(&base, "Bass");
    let bassline = clip(&base, "Bassline");
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    ours.tracks.retain(|t| t.id != bass);
    ours.clips.retain(|_, c| c.track != bass);
    audio_mut(&mut theirs, "Bassline").gain_db = 2.0;
    let m = plain(&base, &ours, &theirs);
    assert!(m.project.track(bass).is_none());
    assert!(m.project.clip(bassline).is_none());
    assert_eq!(m.conflicts.len(), 1);
    // Keeping their clip without its track: it goes, and the notes say so.
    let m = resolved(
        &base,
        &ours,
        &theirs,
        &m.conflicts[0].key,
        Resolution::Theirs,
    );
    assert!(m.project.clip(bassline).is_none());
    assert!(
        m.notes.iter().any(|n| n.contains("missing track")),
        "{:#?}",
        m.notes
    );
    // With the track changed there too, taking theirs for both brings
    // them back together.
    theirs.track_mut(bass).unwrap().volume_db = -1.0;
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 2, "{:#?}", m.conflicts);
    let mut options = MergeOptions::default();
    for c in &m.conflicts {
        options
            .resolutions
            .insert(c.key.clone(), Resolution::Theirs);
    }
    let m = merge(&base, &ours, &theirs, &options).unwrap();
    assert!(m.project.track(bass).is_some());
    assert_eq!(
        m.project
            .clip(bassline)
            .unwrap()
            .as_audio()
            .unwrap()
            .gain_db,
        2.0
    );
}

#[test]
fn ranges_known_from_an_earlier_merge_read_as_what_they_already_are() {
    let base = demo();
    let next = base.ids.peek();
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    add_track_with_clip(&mut ours, "Vocals");
    let (tt, _) = add_track_with_clip(&mut theirs, "Guitar");
    let first = plain(&base, &ours, &theirs);
    let (start, end, by) = first.moved.unwrap();
    // They go on from their state and change their own track; ours is the
    // merge. Merged again with the known range: one guitar, renamed.
    let mut theirs2 = theirs.clone();
    theirs2.track_mut(tt).unwrap().name = "Guitar DI".into();
    let options = MergeOptions {
        known: vec![IdRange { start, end, by }],
        branch_ids: Some(next),
        ..Default::default()
    };
    // The base for the second merge: theirs as merged the first time.
    let m = merge(&first.theirs, &first.project, &theirs2, &options).unwrap();
    assert!(m.conflicts.is_empty(), "{:#?}", m.conflicts);
    let guitars: Vec<&str> = m
        .project
        .tracks
        .iter()
        .filter(|t| t.name.starts_with("Guitar"))
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(guitars, ["Guitar DI"]);
    assert!(m.project.tracks.iter().any(|t| t.name == "Vocals"));
}

#[test]
fn a_take_folders_comp_is_taken_whole() {
    let mut base = demo();
    let pad = clip(&base, "Pad");
    let a = base.clip(pad).unwrap().as_audio().unwrap().clone();
    let mut folder = crate::TakeFolder::new(a.length);
    for i in 0..3 {
        folder.add_take(crate::Take {
            name: format!("Take {}", i + 1),
            source: a.source,
            source_offset: 0,
            start: 0,
            end: a.length,
            gain_db: 0.0,
            rating: 0,
        });
    }
    base.clips.get_mut(&pad).unwrap().content = ClipContent::Takes(folder);
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    let comp = |p: &mut Project, take: usize, from: i64| {
        let ClipContent::Takes(f) = &mut p.clips.get_mut(&pad).unwrap().content else {
            panic!()
        };
        f.set_comp(from, from + 48_000, Some(take));
    };
    comp(&mut ours, 1, 0);
    comp(&mut theirs, 2, 96_000);
    let m = plain(&base, &ours, &theirs);
    assert_eq!(m.conflicts.len(), 1, "{:#?}", m.conflicts);
    assert_eq!(
        m.project.clip(pad).unwrap().content,
        ours.clip(pad).unwrap().content
    );
    let m = resolved(
        &base,
        &ours,
        &theirs,
        &m.conflicts[0].key,
        Resolution::Theirs,
    );
    assert_eq!(
        m.project.clip(pad).unwrap().content,
        theirs.clip(pad).unwrap().content
    );
}

#[test]
fn resolutions_read_from_words() {
    assert_eq!(Resolution::parse("ours"), Some(Resolution::Ours));
    assert_eq!(Resolution::parse(" Theirs "), Some(Resolution::Theirs));
    assert_eq!(Resolution::parse("both"), Some(Resolution::Both));
    assert_eq!(Resolution::parse("all"), None);
}

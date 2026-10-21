//! Folder groups (issue #5): a folder's Group switch makes what it holds
//! one for arming, selecting and editing — a dry and a wet guitar recorded
//! on two tracks handled as one.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, MidiClip, MidiNote, Project,
    SourceSpec, StretchSettings, TrackKind,
};
use faderframe_session::clipboard::ClipboardKind;
use faderframe_session::{
    Action, ClipEdge, EditMode, EditRange, SelectMode, Session, TransportAction,
};
use faderframe_timeline::MusicalTime;
use std::collections::BTreeSet;

const SR: u32 = 48_000;
/// Frames per bar at 120 BPM in 4/4.
const BAR: i64 = 2 * SR as i64;

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

/// Guitars (a folder): Dry, Wet (audio), Keys (instrument); Other
/// (audio) outside it.
struct Rig {
    s: Session,
    folder: TrackId,
    dry: TrackId,
    wet: TrackId,
    keys: TrackId,
    other: TrackId,
    source: AudioSourceId,
}

fn rig() -> Rig {
    let mut s = Session::new(Project::new("Groups", SR), None, EngineConfig::default()).unwrap();
    let dry = s.add_track(TrackKind::Audio).unwrap();
    let wet = s.add_track(TrackKind::Audio).unwrap();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    let other = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::NewFolder {
        tracks: vec![dry, wet, keys],
    })
    .unwrap();
    let folder = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Folder)
        .unwrap()
        .id;
    let source = AudioSourceId(9000);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "guitar".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 32,
                    seed: 3,
                },
            },
        }),
    }))
    .unwrap();
    Rig {
        s,
        folder,
        dry,
        wet,
        keys,
        other,
        source,
    }
}

fn audio(t: TrackId, source: AudioSourceId, start: f64, bars: f64, id: u64) -> Clip {
    Clip {
        id: ClipId(id),
        track: t,
        name: format!("clip {id}"),
        color: None,
        start: bar(start),
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source,
            source_offset: (start * BAR as f64) as i64,
            length: (bars * BAR as f64) as i64,
            gain_db: 0.0,
            fades: ClipFades::default(),
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
        }),
    }
}

fn midi(t: TrackId, start: f64, bars: f64, id: u64) -> Clip {
    Clip {
        id: ClipId(id),
        track: t,
        name: "keys".into(),
        color: None,
        start: bar(start),
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: bar(bars),
            notes: vec![MidiNote {
                id: NoteId(id * 10),
                start: MusicalTime::ZERO,
                length: bar(bars),
                key: 60,
                velocity: 90,
                channel: 0,
                muted: false,
                release: None,
            }],
            ..MidiClip::default()
        }),
    }
}

fn add(s: &mut Session, clip: Clip) -> ClipId {
    let id = clip.id;
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(clip),
    }))
    .unwrap();
    id
}

fn group(s: &mut Session, folder: TrackId, on: bool) {
    s.dispatch(Action::SetFolderGroup { folder, on }).unwrap();
}

fn select_clip(s: &mut Session, clip: ClipId, mode: SelectMode) {
    s.dispatch(Action::SelectClips {
        clips: vec![clip],
        mode,
    })
    .unwrap();
}

fn selected(s: &Session) -> BTreeSet<ClipId> {
    s.selection.clips.clone()
}

fn set(ids: &[ClipId]) -> BTreeSet<ClipId> {
    ids.iter().copied().collect()
}

fn armed(s: &Session, t: TrackId) -> bool {
    s.project().track(t).unwrap().record_arm
}

fn start(s: &Session, c: ClipId) -> MusicalTime {
    s.project().clip(c).unwrap().start
}

fn steps(s: &Session) -> usize {
    s.history_steps().0.len()
}

fn clips_on(s: &Session, t: TrackId) -> Vec<(MusicalTime, MusicalTime)> {
    let p = s.project();
    p.clips_of(t)
        .into_iter()
        .map(|c| (c.start, c.end(&p.timeline, p.sample_rate)))
        .collect()
}

fn locate(s: &mut Session, at: MusicalTime) {
    s.dispatch(Action::Transport(TransportAction::Locate(at)))
        .unwrap();
}

/// Arming one member arms the others (however it is armed: a button, a
/// batch of the selected tracks), disarming disarms them; one undo step.
/// Tracks outside the folder never follow; off, nothing follows.
#[test]
fn arming_one_member_arms_the_group_and_back() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        keys,
        other,
        ..
    } = rig();
    let arm = |s: &mut Session, track, on| {
        s.dispatch(Action::Edit(Command::SetTrackRecordArm { track, on }))
            .unwrap();
    };
    arm(&mut s, dry, true);
    assert!(armed(&s, dry) && !armed(&s, wet), "no group yet");
    arm(&mut s, dry, false);

    group(&mut s, folder, true);
    assert!(s.project().track(folder).unwrap().folder_group);
    let before = steps(&s);
    arm(&mut s, wet, true);
    assert!(armed(&s, dry) && armed(&s, wet) && armed(&s, keys));
    assert!(!armed(&s, other));
    assert_eq!(steps(&s), before + 1, "one undo step");
    s.dispatch(Action::Undo).unwrap();
    assert!(!armed(&s, dry) && !armed(&s, wet) && !armed(&s, keys));
    s.dispatch(Action::Redo).unwrap();
    assert!(armed(&s, dry) && armed(&s, keys));
    // Disarming one disarms the rest.
    arm(&mut s, keys, false);
    assert!(!armed(&s, dry) && !armed(&s, wet));
    // Shift+R's batch of the selected tracks reaches them too.
    s.dispatch(Action::SelectTracks {
        tracks: vec![dry],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::ToggleArmSelected).unwrap();
    assert!(armed(&s, dry) && armed(&s, wet) && armed(&s, keys) && !armed(&s, other));
    s.dispatch(Action::ToggleArmSelected).unwrap();
    assert!(!armed(&s, wet));
    // Off again: one track at a time.
    group(&mut s, folder, false);
    arm(&mut s, dry, true);
    assert!(armed(&s, dry) && !armed(&s, wet));
}

/// Selecting a clip selects the clips at its place on the other members
/// (any kind of clip), not elsewhere and not on other tracks; Ctrl toggles
/// them together; switching the group on extends the selection.
#[test]
fn selecting_a_clip_selects_the_clips_at_its_place() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        keys,
        other,
        source,
    } = rig();
    let d1 = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let w1 = add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    let k1 = add(&mut s, midi(keys, 0.0, 2.0, 10_003));
    let d2 = add(&mut s, audio(dry, source, 4.0, 2.0, 10_004));
    let w2 = add(&mut s, audio(wet, source, 4.0, 2.0, 10_005));
    let o1 = add(&mut s, audio(other, source, 0.0, 2.0, 10_006));

    select_clip(&mut s, d1, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[d1]), "no group yet");
    group(&mut s, folder, true);
    assert_eq!(
        selected(&s),
        set(&[d1, w1, k1]),
        "switched on, the selection extends"
    );
    select_clip(&mut s, w2, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[d2, w2]));
    assert_eq!(s.selection.anchor_clip, Some(w2), "the clicked clip");
    // Ctrl adds a place and takes it away again.
    select_clip(&mut s, k1, SelectMode::Toggle);
    assert_eq!(selected(&s), set(&[d1, w1, k1, d2, w2]));
    select_clip(&mut s, w1, SelectMode::Toggle);
    assert_eq!(selected(&s), set(&[d2, w2]));
    // Outside the folder: only itself.
    select_clip(&mut s, o1, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[o1]));
    group(&mut s, folder, false);
    select_clip(&mut s, d1, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[d1]));
}

/// A range on one member covers all of them (and is shown there):
/// separating it cuts every member's clips, not the other track's, in one
/// undo step; a copy of it holds every member.
#[test]
fn a_range_on_one_member_covers_the_group() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        keys,
        other,
        source,
    } = rig();
    add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    add(&mut s, midi(keys, 0.0, 2.0, 10_003));
    add(&mut s, audio(other, source, 0.0, 2.0, 10_004));
    group(&mut s, folder, true);
    s.dispatch(Action::SelectTracks {
        tracks: vec![wet],
        mode: SelectMode::Replace,
    })
    .unwrap();
    assert_eq!(
        s.selection.tracks,
        [wet].into_iter().collect(),
        "the track selection stays as clicked (faders are not linked)"
    );
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(0.5),
        bar(1.5),
    ))))
    .unwrap();
    assert_eq!(s.edit_tracks(), [wet, dry, keys]);
    let before = steps(&s);
    s.dispatch(Action::Separate).unwrap();
    assert_eq!(steps(&s), before + 1);
    for t in [dry, wet, keys] {
        assert_eq!(
            clips_on(&s, t),
            [
                (bar(0.0), bar(0.5)),
                (bar(0.5), bar(1.5)),
                (bar(1.5), bar(2.0))
            ],
            "{t}"
        );
    }
    assert_eq!(clips_on(&s, other).len(), 1, "not in the group");
    s.dispatch(Action::Copy).unwrap();
    assert_eq!(s.clipboard().kind(), ClipboardKind::Range);
    assert_eq!(s.clipboard().lanes().len(), 3);
    s.dispatch(Action::Undo).unwrap();
    for t in [dry, wet, keys] {
        assert_eq!(clips_on(&s, t).len(), 1, "{t}");
    }
}

/// Split, move (a drag: from where the gesture found the clips), trim,
/// fade, clip gain, slip, delete, cut and paste, duplicate: each reaches
/// the clips at the same place on the other members as one undo step;
/// MIDI clips skip what is for audio.
#[test]
fn edits_of_a_clip_reach_the_group_as_one_undo_step() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        keys,
        other,
        source,
    } = rig();
    let d1 = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let w1 = add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    let k1 = add(&mut s, midi(keys, 0.0, 2.0, 10_003));
    let d2 = add(&mut s, audio(dry, source, 4.0, 2.0, 10_004));
    let w2 = add(&mut s, audio(wet, source, 4.0, 2.0, 10_005));
    let o2 = add(&mut s, audio(other, source, 4.0, 2.0, 10_006));
    // Right after the second place on the wet track only.
    let w3 = add(&mut s, audio(wet, source, 6.0, 2.0, 10_007));
    group(&mut s, folder, true);

    // Split at the playhead: every member's clip is cut, every piece is
    // selected.
    select_clip(&mut s, d1, SelectMode::Replace);
    locate(&mut s, bar(1.0));
    let before = steps(&s);
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    assert_eq!(steps(&s), before + 1);
    for t in [dry, wet, keys] {
        assert_eq!(
            clips_on(&s, t)[..2],
            [(bar(0.0), bar(1.0)), (bar(1.0), bar(2.0))],
            "{t}"
        );
    }
    assert_eq!(s.selection.clips.len(), 6, "every piece stays selected");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(clips_on(&s, wet)[0], (bar(0.0), bar(2.0)));

    // A drag of an unselected clip (the view names only it): its place
    // moves, every event from where the gesture started (half-way the
    // dry clip is over the next wet one: that one stays).
    s.dispatch(Action::BeginGesture("Move Clip".into()))
        .unwrap();
    for by in [bar(1.0), bar(2.0)] {
        s.dispatch(Action::MoveClips {
            clips: vec![d2],
            by: by.ticks(),
            tracks: 0,
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!((start(&s, d2), start(&s, w2)), (bar(6.0), bar(6.0)));
    assert_eq!(start(&s, w3), bar(6.0), "where it was");
    assert_eq!(start(&s, o2), bar(4.0), "not in the group");
    assert_eq!(start(&s, d1), bar(0.0), "not at that place");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!((start(&s, d2), start(&s, w2)), (bar(4.0), bar(4.0)));

    // Trim, fades and clip gain (MIDI clips have none).
    s.dispatch(Action::TrimClips {
        clips: vec![w1],
        edge: ClipEdge::End,
        by: -bar(0.5).ticks(),
        stretch: false,
    })
    .unwrap();
    for t in [dry, wet, keys] {
        assert_eq!(clips_on(&s, t)[0], (bar(0.0), bar(1.5)), "{t}");
    }
    let before = steps(&s);
    s.dispatch(Action::ClipGain {
        clips: vec![d1],
        delta_db: -3.0,
    })
    .unwrap();
    s.dispatch(Action::SetFade {
        clips: vec![d1],
        edge: ClipEdge::Start,
        length: Some(4_800),
        shape: None,
        bend: None,
    })
    .unwrap();
    assert_eq!(steps(&s), before + 2);
    let au = |s: &Session, c| s.project().clip(c).unwrap().as_audio().cloned().unwrap();
    for c in [d1, w1] {
        assert_eq!((au(&s, c).gain_db, au(&s, c).fades.fade_in), (-3.0, 4_800));
    }
    assert!(s.project().clip(k1).unwrap().as_midi().is_some());
    // Slip: the audio moves inside both.
    let offset = |s: &Session, c| au(s, c).source_offset;
    let (od, ow) = (offset(&s, d2), offset(&s, w2));
    s.dispatch(Action::SlipClips {
        clips: vec![w2],
        by: bar(0.25).ticks(),
    })
    .unwrap();
    assert_eq!(offset(&s, d2) - od, offset(&s, w2) - ow);
    assert_ne!(offset(&s, d2), od);

    // Delete the selected place.
    select_clip(&mut s, w2, SelectMode::Replace);
    s.dispatch(Action::DeleteSelection).unwrap();
    assert!(s.project().clip(d2).is_none() && s.project().clip(w2).is_none());
    assert!(s.project().clip(o2).is_some());
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().clip(d2).is_some() && s.project().clip(w2).is_some());

    // Cut the place, paste it at the playhead on the dry track: both
    // tracks get it back (the clipboard holds a lane per member).
    select_clip(&mut s, d2, SelectMode::Replace);
    s.dispatch(Action::Cut).unwrap();
    assert!(s.project().clip(d2).is_none() && s.project().clip(w2).is_none());
    assert_eq!(s.clipboard().lanes().len(), 2);
    s.dispatch(Action::SelectTracks {
        tracks: vec![dry],
        mode: SelectMode::Replace,
    })
    .unwrap();
    locate(&mut s, bar(8.0));
    let before = steps(&s);
    s.dispatch(Action::Paste).unwrap();
    assert_eq!(steps(&s), before + 1);
    assert_eq!(clips_on(&s, dry).last().unwrap().0, bar(8.0));
    assert_eq!(clips_on(&s, wet).last().unwrap().0, bar(8.0));
    // Duplicate: the pasted place again right after it, on both.
    s.dispatch(Action::Repeat(1)).unwrap();
    assert_eq!(clips_on(&s, dry).last().unwrap().0, bar(10.0));
    assert_eq!(clips_on(&s, wet).last().unwrap().0, bar(10.0));
    assert_eq!(clips_on(&s, other).len(), 1);
}

/// Different boundaries: the pieces of a clip cut up on one member go
/// with the whole clip on the other (whichever is clicked), a neighbour
/// touching it in a crossfade does not; a split cuts every member's clip
/// under the playhead.
#[test]
fn different_clip_boundaries_on_the_members() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        source,
        ..
    } = rig();
    group(&mut s, folder, true);
    // Dry in two pieces, wet one clip over both.
    let a = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let b = add(&mut s, audio(dry, source, 2.0, 2.0, 10_002));
    let whole = add(&mut s, audio(wet, source, 0.0, 4.0, 10_003));
    select_clip(&mut s, a, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[a, whole, b]));
    select_clip(&mut s, whole, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[a, whole, b]), "the same set");
    // Moving one of them moves the place, aligned.
    s.dispatch(Action::MoveClips {
        clips: vec![b],
        by: bar(1.0).ticks(),
        tracks: 0,
    })
    .unwrap();
    assert_eq!(
        (start(&s, a), start(&s, b), start(&s, whole)),
        (bar(1.0), bar(3.0), bar(1.0))
    );
    s.dispatch(Action::Undo).unwrap();

    // A crossfade's overlap is not a shared place.
    let c = add(&mut s, audio(dry, source, 8.0, 2.0, 10_004));
    let near = add(&mut s, audio(wet, source, 6.25, 2.0, 10_005));
    let far = add(&mut s, audio(wet, source, 8.25, 2.0, 10_006));
    select_clip(&mut s, c, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[c, far]));
    // A split where only the neighbour is under the playhead on the other
    // member: it is cut there too, and its pieces, now at the places of
    // the selected pieces, are selected with them.
    locate(&mut s, bar(8.125));
    let before = steps(&s);
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    assert_eq!(steps(&s), before + 1);
    let cut_at = |s: &Session, t| clips_on(s, t).iter().filter(|x| x.0 == bar(8.125)).count();
    assert_eq!((cut_at(&s, dry), cut_at(&s, wet)), (1, 1));
    assert_eq!(start(&s, near), bar(6.25), "its left piece keeps its id");
    assert_eq!(start(&s, far), bar(8.25), "not under the playhead");
    assert!(s.selection.clips.contains(&near) && s.selection.clips.contains(&far));
}

/// Nested groups: a grouped folder in a grouped folder is part of the
/// outer group (expanding goes no further than that one group, and twice
/// changes nothing); the outer one off, the inner group is its own.
#[test]
fn a_grouped_folder_in_a_grouped_folder() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        keys,
        source,
        ..
    } = rig();
    // Wet and Keys into an inner folder (inside Guitars).
    s.dispatch(Action::NewFolder {
        tracks: vec![wet, keys],
    })
    .unwrap();
    let inner = s
        .project()
        .tracks
        .iter()
        .find(|t| t.is_folder() && t.id != folder)
        .unwrap()
        .id;
    assert_eq!(s.project().track(inner).unwrap().folder, Some(folder));
    let d = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let w = add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    let k = add(&mut s, midi(keys, 0.0, 2.0, 10_003));
    group(&mut s, inner, true);
    select_clip(&mut s, w, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[w, k]));
    select_clip(&mut s, d, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[d]), "dry is in no group yet");
    group(&mut s, folder, true);
    assert_eq!(s.edit_group_of(wet), Some(folder));
    assert_eq!(s.edit_group_of(inner), Some(folder));
    select_clip(&mut s, w, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[d, w, k]));
    let once = s.grouped_clips(&[w]);
    assert_eq!(s.grouped_clips(&once).len(), once.len(), "a fixed point");
    // Arming in the inner folder arms the outer group.
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: keys,
        on: true,
    }))
    .unwrap();
    assert!(armed(&s, dry) && armed(&s, wet));
    // The outer one off: the inner group again.
    group(&mut s, folder, false);
    assert_eq!(s.edit_group_of(wet), Some(inner));
    select_clip(&mut s, d, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[d]));
    select_clip(&mut s, k, SelectMode::Replace);
    assert_eq!(selected(&s), set(&[w, k]));
}

/// Folders, buses, auxes and VCAs inside a grouped folder are no members:
/// they are never armed or edited with it; a folder bus groups like a
/// plain folder.
#[test]
fn incompatible_tracks_are_skipped() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        source,
        ..
    } = rig();
    let aux = s.add_track(TrackKind::Aux).unwrap();
    let vca = s.add_track(TrackKind::Vca).unwrap();
    s.dispatch(Action::MoveToFolder {
        tracks: vec![aux, vca],
        folder: Some(folder),
    })
    .unwrap();
    s.dispatch(Action::SetFolderBus { folder, on: true })
        .unwrap();
    group(&mut s, folder, true);
    assert!(s.folder_grouped(folder));
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: dry,
        on: true,
    }))
    .unwrap();
    assert!(armed(&s, wet));
    for t in [aux, vca, folder] {
        assert!(!armed(&s, t), "{t}");
    }
    s.dispatch(Action::SelectTracks {
        tracks: vec![dry],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let covered = s.edit_tracks();
    assert!(covered.contains(&wet) && !covered.contains(&aux) && !covered.contains(&vca));
    let d = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let w = add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    assert_eq!(s.grouped_clips(&[d]), [d, w]);
}

/// A frozen member refuses the group's clip edits as it refuses its own:
/// nothing moves or goes, so the members never drift apart.
#[test]
fn a_frozen_member_holds_the_group() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        source,
        ..
    } = rig();
    let d = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let w = add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    group(&mut s, folder, true);
    s.dispatch(Action::Edit(Command::SetTrackFreeze {
        track: wet,
        freeze: Some(faderframe_project::Freeze {
            source,
            start: MusicalTime::ZERO,
            length: 2 * BAR,
            latency: 0,
        }),
    }))
    .unwrap();
    let moved = s.dispatch(Action::MoveClips {
        clips: vec![d],
        by: bar(1.0).ticks(),
        tracks: 0,
    });
    assert!(moved.unwrap_err().to_string().contains("frozen"));
    assert_eq!((start(&s, d), start(&s, w)), (bar(0.0), bar(0.0)));
    select_clip(&mut s, d, SelectMode::Replace);
    assert!(s.dispatch(Action::DeleteSelection).is_err());
    assert!(s.project().clip(d).is_some() && s.project().clip(w).is_some());
}

/// The switch is saved, shown on reopening, and works there; undo turns
/// it off again; older files open with it off.
#[test]
fn the_group_switch_is_saved_and_undoable() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        source,
        ..
    } = rig();
    let d = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let w = add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    let dir = std::env::temp_dir().join(format!("ff-folder-group-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let plain = dir.join("plain.ffproj");
    s.save_as(&plain).unwrap();
    assert!(
        !std::fs::read_to_string(&plain)
            .unwrap()
            .contains("folder_group")
    );
    group(&mut s, folder, true);
    assert_eq!(s.history_steps().0.last().unwrap(), "Group Folder");
    let path = dir.join("grouped.ffproj");
    s.save_as(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("folder_group").count(), 1, "written once");
    s.dispatch(Action::Undo).unwrap();
    assert!(!s.folder_grouped(folder));

    let mut t = Session::new(Project::new("Other", SR), None, EngineConfig::default()).unwrap();
    t.open(&path).unwrap();
    assert!(t.folder_grouped(folder));
    t.dispatch(Action::SelectClips {
        clips: vec![w],
        mode: SelectMode::Replace,
    })
    .unwrap();
    assert_eq!(selected(&t), set(&[d, w]));
    t.open(&plain).unwrap();
    assert!(!t.folder_grouped(folder));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Spot and Shuffle moves of a grouped clip take the others along on
/// their own tracks, one undo step each.
#[test]
fn spot_and_shuffle_move_the_group() {
    let Rig {
        mut s,
        folder,
        dry,
        wet,
        source,
        ..
    } = rig();
    let d1 = add(&mut s, audio(dry, source, 0.0, 2.0, 10_001));
    let w1 = add(&mut s, audio(wet, source, 0.0, 2.0, 10_002));
    let d2 = add(&mut s, audio(dry, source, 2.0, 2.0, 10_003));
    let w2 = add(&mut s, audio(wet, source, 2.0, 2.0, 10_004));
    group(&mut s, folder, true);
    let before = steps(&s);
    s.dispatch(Action::SpotClip {
        clip: d1,
        start: bar(8.0),
    })
    .unwrap();
    assert_eq!((start(&s, d1), start(&s, w1)), (bar(8.0), bar(8.0)));
    assert_eq!(steps(&s), before + 1);
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::SetEditMode(EditMode::Shuffle)).unwrap();
    let before = steps(&s);
    // The first clip after the second: the gap closes, both tracks alike.
    s.dispatch(Action::ShuffleClip {
        clip: d1,
        track: dry,
        at: bar(4.0),
    })
    .unwrap();
    assert_eq!(steps(&s), before + 1);
    assert_eq!((start(&s, d2), start(&s, w2)), (bar(0.0), bar(0.0)));
    assert_eq!((start(&s, d1), start(&s, w1)), (bar(2.0), bar(2.0)));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!((start(&s, d1), start(&s, w1)), (bar(0.0), bar(0.0)));
}

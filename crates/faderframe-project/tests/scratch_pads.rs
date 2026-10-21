//! Scratch pads at the project level: the arrangement swap, its commands
//! and their inverses, track removal, the file form, used media and
//! repairs, and the span operations material moves with.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ClipId, ScratchPadId, TrackId};
use faderframe_project::arrange;
use faderframe_project::demo::demo_project;
use faderframe_project::scratch::ScratchPad;
use faderframe_project::{
    Clip, ClipContent, Command, EditError, History, MidiClip, Project, TrackKind, file,
};
use faderframe_timeline::MusicalTime;

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

/// The demo with one empty pad added (not shown).
fn with_pad() -> (Project, History, ScratchPadId) {
    let mut p = demo_project(48_000);
    let mut h = History::default();
    let id: ScratchPadId = p.ids.allocate();
    let pad = ScratchPad::empty(id, "Pad", &p.timeline);
    h.apply(
        &mut p,
        Command::SetScratchPad {
            pad: id,
            value: Some(Box::new(pad)),
            index: 0,
        },
    )
    .unwrap();
    (p, h, id)
}

fn midi_track(p: &Project) -> TrackId {
    p.tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap()
        .id
}

fn midi_clip(p: &mut Project, track: TrackId, start: MusicalTime) -> Clip {
    let id: ClipId = p.ids.allocate();
    Clip {
        id,
        track,
        name: "Idea".into(),
        color: None,
        start,
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: bar(1.0),
            ..Default::default()
        }),
    }
}

/// Every track lists exactly the clips that name it (launcher clips
/// aside).
fn consistent(p: &Project) {
    let launcher = p.launcher_clips();
    for t in &p.tracks {
        let mut listed = t.clips.clone();
        listed.sort();
        let mut owned: Vec<ClipId> = p
            .clips
            .values()
            .filter(|c| c.track == t.id && !launcher.contains(&c.id))
            .map(|c| c.id)
            .collect();
        owned.sort();
        assert_eq!(listed, owned, "{}'s clip list", t.name);
    }
}

#[test]
fn showing_a_pad_puts_the_song_aside_and_back_exactly() {
    let (mut p, mut h, pad) = with_pad();
    let song = p.clone();
    assert!(!song.sections.is_empty() && song.loop_range.is_some());
    let launcher = p.launcher_clips();
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    assert_eq!(p.shown_pad(), Some(pad));
    // An empty arrangement: no clips (but the launcher's), no automation,
    // sections, markers or loop; the song's tempo and metre at its start.
    assert!(p.clips.keys().all(|c| launcher.contains(c)));
    assert!(
        p.tracks
            .iter()
            .all(|t| t.clips.is_empty() && t.automation.is_empty())
    );
    assert!(p.sections.is_empty() && p.markers.is_empty() && p.loop_range.is_none());
    assert_eq!(
        p.timeline.tempo.bpm_at(MusicalTime::ZERO),
        song.timeline.tempo.bpm_at(MusicalTime::ZERO)
    );
    // The pad's own clip.
    let track = midi_track(&p);
    let clip = midi_clip(&mut p, track, bar(1.0));
    let id = clip.id;
    h.apply(
        &mut p,
        Command::AddClip {
            clip: Box::new(clip),
        },
    )
    .unwrap();
    consistent(&p);
    // Back to the song: as it was (the pad holds its clip aside).
    h.apply(&mut p, Command::ShowArrangement { pad: None })
        .unwrap();
    let mut expected = song.clone();
    expected.scratch_pads = p.scratch_pads.clone();
    expected.ids = p.ids.clone();
    assert_eq!(p, expected);
    assert!(p.clip(id).is_none());
    assert_eq!(p.stashed_clips().map(|c| c.id).collect::<Vec<_>>(), [id]);
    // The pad again: its clip.
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    assert!(p.clip(id).is_some());
    assert!(p.track(track).unwrap().clips.contains(&id));
    // Undo walks back through every switch: each edit undone where made.
    while h.can_undo() {
        h.undo(&mut p).unwrap();
    }
    let mut first = demo_project(48_000);
    first.ids = p.ids.clone();
    assert_eq!(p, first);
    while h.can_redo() {
        h.redo(&mut p).unwrap();
    }
    assert_eq!(p.shown_pad(), Some(pad));
    assert!(p.clip(id).is_some());
}

#[test]
fn the_pad_shown_cannot_be_removed_and_pad_commands_invert() {
    let (mut p, mut h, pad) = with_pad();
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    let err = h
        .apply(
            &mut p,
            Command::SetScratchPad {
                pad,
                value: None,
                index: 0,
            },
        )
        .unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)));
    // Renaming the shown pad: its value holds no stash.
    let mut renamed = p.scratch_pads.pad(pad).unwrap().clone();
    assert!(renamed.stash.is_none());
    renamed.name = "Chorus Idea".into();
    h.apply(
        &mut p,
        Command::SetScratchPad {
            pad,
            value: Some(Box::new(renamed)),
            index: 0,
        },
    )
    .unwrap();
    assert_eq!(p.scratch_pads.name_of(Some(pad)), "Chorus Idea");
    h.undo(&mut p).unwrap();
    assert_eq!(p.scratch_pads.name_of(Some(pad)), "Pad");
    // A second pad, then its removal and back.
    h.apply(&mut p, Command::ShowArrangement { pad: None })
        .unwrap();
    let other: ScratchPadId = p.ids.allocate();
    let b = ScratchPad::empty(other, "B", &p.timeline);
    h.apply(
        &mut p,
        Command::SetScratchPad {
            pad: other,
            value: Some(Box::new(b)),
            index: 0,
        },
    )
    .unwrap();
    assert_eq!(p.scratch_pads.pads[0].id, other);
    let before = p.clone();
    h.apply(
        &mut p,
        Command::SetScratchPad {
            pad: other,
            value: None,
            index: 0,
        },
    )
    .unwrap();
    assert!(p.scratch_pads.pad(other).is_none());
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
    // Unknown pads are refused.
    let err = h
        .apply(
            &mut p,
            Command::ShowArrangement {
                pad: Some(ScratchPadId(999_999)),
            },
        )
        .unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)));
}

#[test]
fn removing_a_track_takes_its_parts_of_every_arrangement_along() {
    let (mut p, mut h, pad) = with_pad();
    let track = midi_track(&p);
    let song_clips = p.track(track).unwrap().clips.clone();
    assert!(!song_clips.is_empty());
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    let clip = midi_clip(&mut p, track, MusicalTime::ZERO);
    let pad_clip = clip.id;
    h.apply(
        &mut p,
        Command::AddClip {
            clip: Box::new(clip),
        },
    )
    .unwrap();
    h.apply(&mut p, Command::ShowArrangement { pad: None })
        .unwrap();
    let before = p.clone();
    h.apply(&mut p, Command::RemoveTrack { track }).unwrap();
    // Gone from the song and from the pad put aside.
    assert!(p.stashed_clips().all(|c| c.track != track));
    assert!(
        p.scratch_pads
            .stashes()
            .all(|(_, s)| !s.tracks.contains_key(&track))
    );
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
    assert!(p.stashed_clips().any(|c| c.id == pad_clip));
    // Removed while the pad is shown: the song's part goes and comes back.
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    let shown = p.clone();
    h.apply(&mut p, Command::RemoveTrack { track }).unwrap();
    assert!(p.stashed_clips().all(|c| c.track != track));
    h.undo(&mut p).unwrap();
    assert_eq!(p, shown);
}

#[test]
fn a_file_holds_the_song_in_place_and_opens_the_pad_shown() {
    let (mut p, mut h, pad) = with_pad();
    let song_lists: Vec<Vec<ClipId>> = p.tracks.iter().map(|t| t.clips.clone()).collect();
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    let track = midi_track(&p);
    let clip = midi_clip(&mut p, track, bar(2.0));
    h.apply(
        &mut p,
        Command::AddClip {
            clip: Box::new(clip),
        },
    )
    .unwrap();
    let text = file::to_string(&p, None).unwrap();
    // In the file: the song in the tracks, the pad aside, named to open.
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let tracks = doc["project"]["tracks"].as_array().unwrap();
    for (t, want) in tracks.iter().zip(&song_lists) {
        let got: Vec<ClipId> = t
            .get("clips")
            .map(|c| serde_json::from_value(c.clone()).unwrap())
            .unwrap_or_default();
        assert_eq!(&got, want);
    }
    let pads = &doc["project"]["scratch_pads"];
    assert_eq!(pads["open"], serde_json::json!(pad.raw()));
    assert!(pads.get("shown").is_none() && pads.get("song").is_none());
    // Loading shows the pad again: the same project.
    let loaded = file::from_str(&text).unwrap();
    assert!(loaded.notes.is_empty(), "{:?}", loaded.notes);
    assert_eq!(loaded.project, p);
    // The song shown: what the file holds is what the project holds.
    h.apply(&mut p, Command::ShowArrangement { pad: None })
        .unwrap();
    let back = file::from_str(&file::to_string(&p, None).unwrap()).unwrap();
    assert_eq!(back.project, p);
}

#[test]
fn files_without_pads_load_and_write_none() {
    let p = demo_project(48_000);
    let text = file::to_string(&p, None).unwrap();
    assert!(!text.contains("scratch_pads"));
    // An older file (no such field) loads with none.
    let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc["project"]
        .as_object_mut()
        .unwrap()
        .remove("scratch_pads");
    let loaded = file::from_str(&doc.to_string()).unwrap();
    assert!(loaded.project.scratch_pads.is_empty());
    assert_eq!(loaded.project, p);
}

#[test]
fn media_of_a_pad_count_as_used() {
    let (mut p, mut h, pad) = with_pad();
    let audio = p
        .clips
        .values()
        .find(|c| c.as_audio().is_some())
        .unwrap()
        .clone();
    let source = audio.as_audio().unwrap().source;
    // Into the pad by value (as a copy goes), then out of the song.
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    let id: ClipId = p.ids.allocate();
    h.apply(
        &mut p,
        Command::AddClip {
            clip: Box::new(Clip {
                id,
                ..audio.clone()
            }),
        },
    )
    .unwrap();
    h.apply(&mut p, Command::ShowArrangement { pad: None })
        .unwrap();
    let users: Vec<ClipId> = p
        .clips
        .values()
        .filter(|c| c.content.sources().contains(&source))
        .map(|c| c.id)
        .collect();
    for clip in users {
        h.apply(&mut p, Command::RemoveClip { clip }).unwrap();
    }
    assert!(p.used_sources().contains(&source));
    let err = h
        .apply(&mut p, Command::RemoveSource { source })
        .unwrap_err();
    assert_eq!(err, EditError::SourceInUse(source));
}

#[test]
fn repair_keeps_pads_consistent_with_the_tracks() {
    let (mut p, mut h, pad) = with_pad();
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    let track = midi_track(&p);
    let clip = midi_clip(&mut p, track, MusicalTime::ZERO);
    let kept = clip.id;
    h.apply(
        &mut p,
        Command::AddClip {
            clip: Box::new(clip),
        },
    )
    .unwrap();
    h.apply(&mut p, Command::ShowArrangement { pad: None })
        .unwrap();
    // A stray clip of a track that is gone, listed nowhere, with a high id.
    let mut stray = midi_clip(&mut p, track, MusicalTime::ZERO);
    stray.id = ClipId(1_000_000);
    stray.track = TrackId(999_999);
    let stash = p.scratch_pads.stash_mut(Some(pad)).unwrap();
    stash.clips.insert(stray.id, stray);
    // And its own clip left out of the track's list.
    stash.tracks.get_mut(&track).unwrap().clips.clear();
    let notes = p.repair();
    assert!(
        notes.iter().any(|n| n.contains("missing tracks")),
        "{notes:?}"
    );
    let stash = p.scratch_pads.pad(pad).unwrap().stash.as_deref().unwrap();
    assert!(!stash.clips.contains_key(&ClipId(1_000_000)));
    assert_eq!(stash.tracks[&track].clips, vec![kept]);
    // New ids never meet a pad's.
    let next: ClipId = p.ids.allocate();
    assert!(next.raw() > kept.raw());
}

#[test]
fn the_song_is_what_renders_and_templates_take() {
    let (mut p, mut h, pad) = with_pad();
    let song = p.clone();
    h.apply(&mut p, Command::ShowArrangement { pad: Some(pad) })
        .unwrap();
    let as_song = p.song().into_owned();
    assert_eq!(as_song.shown_pad(), None);
    assert_eq!(as_song.clips, song.clips);
    assert_eq!(as_song.sections, song.sections);
    let template = faderframe_project::template::without_content(&p);
    assert!(template.scratch_pads.is_empty());
    assert_eq!(template.sections, song.sections);
    // Showing a pad is no difference between versions; a pad is.
    assert!(faderframe_project::compare::differences(&song, &p).is_empty());
    let lines = faderframe_project::compare::differences(&demo_project(48_000), &p);
    assert_eq!(lines, vec!["Scratch pad ‘Pad’ added".to_string()]);
}

/// Copying a section into a pad and bringing it back: the span operations
/// a pad's material moves with.
#[test]
fn material_moves_between_arrangements_by_span() {
    let (p, _, pad) = with_pad();
    let chorus = p.sections[1].clone();
    let len = chorus.end - chorus.start;
    let song_end = p.arrangement_span().unwrap().1;
    // Into the pad: the section with its clips and automation.
    let mut slice = arrange::copy_span(&p, chorus.start, chorus.end);
    let in_section = slice.clip_count();
    assert!(in_section > 0);
    let mut q = p.clone();
    q.show_arrangement(Some(pad)).unwrap();
    slice.adopt_lanes(&mut q);
    arrange::insert_span(&mut q, MusicalTime::ZERO, &slice, false);
    consistent(&q);
    assert_eq!(q.sections.len(), 1);
    assert_eq!(q.sections[0].name, chorus.name);
    assert_eq!(
        (q.sections[0].start, q.sections[0].end),
        (MusicalTime::ZERO, len)
    );
    assert!(
        q.tracks.iter().any(|t| !t.automation.is_empty()),
        "lanes came along"
    );
    assert_eq!(q.arrangement_span(), Some((MusicalTime::ZERO, len)));
    // Half of it back over the song's first section, keeping its length.
    let first = p.sections[0].clone();
    let first_len = first.end - first.start;
    let half = arrange::copy_span(&q, MusicalTime::ZERO, first_len);
    q.show_arrangement(None).unwrap();
    let mut song = q.clone();
    arrange::replace_span(
        &mut song,
        first.start,
        first.end,
        &half,
        Some(first.clone()),
    );
    consistent(&song);
    assert_eq!(song.arrangement_span().unwrap().1, song_end, "length kept");
    // The whole section as a replacement: the song grows by the difference.
    let whole = {
        let mut r = q.clone();
        r.show_arrangement(Some(pad)).unwrap();
        arrange::copy_span(&r, MusicalTime::ZERO, len)
    };
    let mut longer = q.clone();
    arrange::replace_span(
        &mut longer,
        first.start,
        first.end,
        &whole,
        Some(first.clone()),
    );
    consistent(&longer);
    assert_eq!(
        longer.arrangement_span().unwrap().1,
        song_end + len - first_len
    );
    // Laid over the song: time does not move, what was under goes.
    let mut over = q.clone();
    arrange::overlay_span(&mut over, first.start, &whole);
    consistent(&over);
    assert_eq!(over.arrangement_span().unwrap().1, song_end);
    assert_eq!(over.sections, q.sections);
    let tracks = whole.clip_tracks();
    for c in over.clips.values().filter(|c| tracks.contains(&c.track)) {
        let end = c.end(&over.timeline, over.sample_rate);
        assert!(
            end <= first.start || c.start >= first.start,
            "nothing under the material keeps playing into it"
        );
    }
    // Inserted: what follows moves later by its length.
    let mut ins = q.clone();
    arrange::insert_span(&mut ins, first.end, &whole, false);
    consistent(&ins);
    assert_eq!(ins.arrangement_span().unwrap().1, song_end + len);
}

#[test]
fn bar_lines_round_out_to_whole_bars() {
    let p = demo_project(48_000);
    let m = &p.timeline.meter;
    assert_eq!(
        arrange::bar_lines(m, bar(1.0), bar(2.0)),
        (bar(1.0), bar(2.0))
    );
    assert_eq!(
        arrange::bar_lines(m, bar(1.25), bar(2.5)),
        (bar(1.0), bar(3.0))
    );
    // A hair past a bar line counts as on it; nothing is at least a bar.
    let hair = MusicalTime::from_ticks(2);
    assert_eq!(
        arrange::bar_lines(m, bar(1.0), bar(2.0) + hair),
        (bar(1.0), bar(2.0))
    );
    assert_eq!(
        arrange::bar_lines(m, bar(1.0), bar(1.0)),
        (bar(1.0), bar(2.0))
    );
}

//! Scratch pads end to end: a section copied into a pad, edited there
//! without touching the song, played (the engine plays the pad's clips and
//! not the song's), brought back by inserting, overwriting and replacing
//! (both answers to a replacement of another length), every step undone
//! and redone, the clipboard across song and pads, renames, duplicates and
//! deletes, save and reopen, unused media and renders of the song.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_automation::{AutomationCurve, AutomationLane, AutomationPoint, AutomationTarget};
use faderframe_core::{ClipId, ScratchPadId, SectionId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, MidiClip, MidiNote, Project,
    Section, SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_session::scratch::{PadMaterial, ScratchOp, SongPlacement};
use faderframe_session::{Action, EditRange, SelectMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};

const SR: u32 = 48_000;
/// Samples a bar at 120 BPM in 4/4.
const BAR: i64 = 2 * SR as i64;

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-scratch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(path: &Path, seconds: f32) {
    let ch: Vec<f32> = (0..(SR as f32 * seconds) as usize)
        .map(|i| (i as f32 * 0.05).sin() * 0.5)
        .collect();
    write_wav(path, &[ch.clone(), ch], SR, WavFormat::Float32, false).unwrap();
}

struct Song {
    s: Session,
    keys: TrackId,
    drums: TrackId,
    /// Intro, Verse, Chorus: two bars each.
    sections: Vec<SectionId>,
    _dir: PathBuf,
}

/// Keys (an instrument track: a MIDI clip a section, one note each, and
/// volume automation) and Drums (an audio track: a two-bar clip of a
/// file a section) over three two-bar sections.
fn song(name: &str) -> Song {
    let dir = tmp(name);
    let wav = dir.join("drums.wav");
    tone(&wav, 4.0);
    let mut p = Project::new("Pads", SR);
    let keys: TrackId = p.ids.allocate();
    let drums: TrackId = p.ids.allocate();
    let mut k = Track::new(keys, TrackKind::Instrument, "Keys", TrackColor::palette(1));
    k.automation.lanes.push(AutomationLane {
        id: p.ids.allocate(),
        target: AutomationTarget::TrackVolume,
        curve: AutomationCurve::from_points(vec![
            AutomationPoint {
                time: MusicalTime::ZERO,
                value: -10.0,
                shape: Default::default(),
            },
            AutomationPoint {
                time: bar(4.0),
                value: -20.0,
                shape: Default::default(),
            },
        ]),
        mode: Default::default(),
        visible: true,
    });
    p.tracks.insert(0, k);
    p.tracks.insert(
        1,
        Track::new(drums, TrackKind::Audio, "Drums", TrackColor::palette(2))
            .with_layout(faderframe_core::ChannelLayout::Stereo),
    );
    let source = p.ids.allocate();
    p.sources.insert(
        source,
        AudioSource {
            id: source,
            name: "drums".into(),
            spec: SourceSpec::File {
                path: wav,
                channels: 2,
                frames: 4 * i64::from(SR),
                sample_rate: SR,
            },
        },
    );
    let mut sections = Vec::new();
    for (i, name) in ["Intro", "Verse", "Chorus"].iter().enumerate() {
        let at = bar(2.0 * i as f64);
        let id: SectionId = p.ids.allocate();
        p.sections.push(Section {
            id,
            name: (*name).into(),
            start: at,
            end: at + bar(2.0),
            color: TrackColor::palette(i),
        });
        sections.push(id);
        let clip: ClipId = p.ids.allocate();
        let note = MidiNote {
            id: p.ids.allocate(),
            start: MusicalTime::ZERO,
            length: MusicalTime::QUARTER,
            key: 60 + 2 * i as u8,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
        };
        p.clips.insert(
            clip,
            Clip {
                id: clip,
                track: keys,
                name: format!("Keys {name}"),
                color: None,
                start: at,
                muted: false,
                content: ClipContent::Midi(MidiClip {
                    length: bar(2.0),
                    notes: vec![note],
                    ..Default::default()
                }),
            },
        );
        p.track_mut(keys).unwrap().clips.push(clip);
        let clip: ClipId = p.ids.allocate();
        p.clips.insert(
            clip,
            Clip {
                id: clip,
                track: drums,
                name: format!("Drums {name}"),
                color: None,
                start: at,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source,
                    source_offset: 0,
                    length: 2 * BAR,
                    gain_db: 0.0,
                    fades: ClipFades::default(),
                    stretch: StretchSettings::Off,
                    reversed: false,
                    warp: None,
                    pitch: None,
                    effects: None,
                    spectral: None,
                }),
            },
        );
        p.track_mut(drums).unwrap().clips.push(clip);
    }
    let s = Session::new(p, None, EngineConfig::default()).unwrap();
    Song {
        s,
        keys,
        drums,
        sections,
        _dir: dir,
    }
}

fn op(s: &mut Session, op: ScratchOp) {
    s.dispatch(Action::ScratchPad(op)).unwrap();
}

/// The song alone (whichever arrangement is shown), without its pads and
/// ids, to compare.
fn song_of(p: &Project) -> Project {
    let mut s = p.song().into_owned();
    s.scratch_pads = Default::default();
    s.ids = Default::default();
    s
}

/// The same project (the id allocator aside: undo keeps ids used).
fn assert_same(a: &Project, b: &Project) {
    let mut a = a.clone();
    a.ids = b.ids.clone();
    assert_eq!(&a, b);
}

/// The notes' keys of a track's MIDI clips, by start.
fn keys_on(p: &Project, track: TrackId) -> Vec<(MusicalTime, u8)> {
    p.clips_of(track)
        .into_iter()
        .filter_map(|c| {
            c.as_midi()
                .and_then(|m| m.notes.first())
                .map(|n| (c.start, n.key))
        })
        .collect()
}

/// Where the engine's streamed audio regions start (samples).
fn streamed(s: &Session) -> Vec<i64> {
    let mut v: Vec<i64> = s
        .engine()
        .stream_plan()
        .regions()
        .iter()
        .filter(|r| !r.pinned)
        .map(|r| r.start)
        .collect();
    v.sort();
    v
}

/// A new pad holding the chorus, shown.
fn chorus_pad(f: &mut Song) -> ScratchPadId {
    op(
        &mut f.s,
        ScratchOp::New {
            name: None,
            from: Some(PadMaterial::Section(f.sections[2])),
        },
    );
    f.s.shown_pad().unwrap()
}

#[test]
fn a_section_copied_into_a_pad_plays_there_and_leaves_the_song() {
    let mut f = song("copy");
    let before = f.s.project().clone();
    assert_eq!(streamed(&f.s), [0, 2 * BAR, 4 * BAR]);
    let pad = chorus_pad(&mut f);
    let s = &f.s;
    let p = s.project();
    assert_eq!(s.shown_arrangement_name(), "Chorus Idea");
    // The chorus from bar 1: its clips, its section, its automation.
    assert_eq!(keys_on(p, f.keys), [(MusicalTime::ZERO, 64)]);
    assert_eq!(p.clips_of(f.drums).len(), 1);
    assert_eq!(p.sections.len(), 1);
    assert_eq!(
        (
            p.sections[0].name.as_str(),
            p.sections[0].start,
            p.sections[0].end
        ),
        ("Chorus", MusicalTime::ZERO, bar(2.0))
    );
    let lane = &p.track(f.keys).unwrap().automation.lanes[0];
    assert_eq!(lane.curve.value_at(MusicalTime::ZERO), Some(-20.0));
    // The song is put aside as it was; the pad plays from bar 1.
    assert_eq!(song_of(p), song_of(&before));
    assert_eq!(s.playhead(), MusicalTime::ZERO);
    // The engine plays the pad's clips, not the song's.
    assert_eq!(streamed(s), [0]);
    // Back in the song: its own again, the pad aside.
    op(&mut f.s, ScratchOp::Show(None));
    assert_eq!(streamed(&f.s), [0, 2 * BAR, 4 * BAR]);
    assert_eq!(f.s.scratch_pads().len(), 1);
    assert_eq!(f.s.scratch_pads()[0].id, pad);
    assert_same(&song_of(f.s.project()), &song_of(&before));
}

#[test]
fn edits_in_a_pad_stay_in_the_pad() {
    let mut f = song("edit");
    let before = f.s.project().clone();
    let pad = chorus_pad(&mut f);
    let p = f.s.project();
    let midi = p.clips_of(f.keys)[0].id;
    let audio = p.clips_of(f.drums)[0].id;
    // Move the keys a bar later, drop the drums, add a MIDI clip.
    f.s.dispatch(Action::Edit(Command::MoveClip {
        clip: midi,
        track: f.keys,
        start: bar(1.0),
    }))
    .unwrap();
    f.s.dispatch(Action::Edit(Command::RemoveClip { clip: audio }))
        .unwrap();
    f.s.dispatch(Action::CreateMidiClip {
        track: f.keys,
        start: bar(4.0),
        length: bar(1.0),
    })
    .unwrap();
    assert_eq!(f.s.project().clips_of(f.keys).len(), 2);
    assert!(streamed(&f.s).is_empty());
    op(&mut f.s, ScratchOp::Show(None));
    assert_same(&song_of(f.s.project()), &song_of(&before));
    op(&mut f.s, ScratchOp::Show(Some(pad)));
    let p = f.s.project();
    assert_eq!(p.clips_of(f.keys)[0].start, bar(1.0));
    assert!(p.clips_of(f.drums).is_empty());
}

#[test]
fn a_pad_comes_back_inserted_or_laid_over_the_song() {
    let mut f = song("back");
    let pad = chorus_pad(&mut f);
    let in_pad = f.s.project().clone();
    // Insert before the verse: verse and chorus move two bars later.
    op(
        &mut f.s,
        ScratchOp::ToSong {
            pad,
            what: PadMaterial::All,
            place: SongPlacement::Insert { at: bar(2.0) },
        },
    );
    let p = f.s.project();
    assert_eq!(f.s.shown_pad(), None, "the song is shown");
    assert_eq!(
        keys_on(p, f.keys),
        [
            (MusicalTime::ZERO, 60),
            (bar(2.0), 64),
            (bar(4.0), 62),
            (bar(6.0), 64)
        ]
    );
    let names: Vec<&str> = p.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Intro", "Chorus", "Verse", "Chorus"]);
    assert_eq!(streamed(&f.s), [0, 2 * BAR, 4 * BAR, 6 * BAR]);
    // The material brought is selected, the playhead on it.
    assert_eq!(f.s.selection.clips.len(), 2);
    assert_eq!(f.s.playhead(), bar(2.0));
    // One step: undo shows the pad as it was.
    f.s.dispatch(Action::Undo).unwrap();
    assert_same(f.s.project(), &in_pad);
    // Laid over the intro: time does not move, the intro's clips go.
    op(
        &mut f.s,
        ScratchOp::ToSong {
            pad,
            what: PadMaterial::All,
            place: SongPlacement::Overwrite {
                at: MusicalTime::ZERO,
            },
        },
    );
    let p = f.s.project();
    assert_eq!(
        keys_on(p, f.keys),
        [(MusicalTime::ZERO, 64), (bar(2.0), 62), (bar(4.0), 64)]
    );
    let names: Vec<&str> = p.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Intro", "Verse", "Chorus"], "sections stay");
    assert_eq!(p.clips_of(f.drums).len(), 3);
    f.s.dispatch(Action::Undo).unwrap();
    assert_same(f.s.project(), &in_pad);
    f.s.dispatch(Action::Redo).unwrap();
    assert_eq!(keys_on(f.s.project(), f.keys)[0], (MusicalTime::ZERO, 64));
}

#[test]
fn replacing_a_section_says_what_happens_to_the_length() {
    let mut f = song("replace");
    let pad = chorus_pad(&mut f);
    // Make the pad a bar longer: a MIDI clip in bar 3.
    f.s.dispatch(Action::CreateMidiClip {
        track: f.keys,
        start: bar(2.0),
        length: bar(1.0),
    })
    .unwrap();
    assert_eq!(
        f.s.pad_material_length(pad, &PadMaterial::All),
        Some(bar(3.0))
    );
    let in_pad = f.s.project().clone();
    let song_end = |s: &Session| s.project().arrangement_span().unwrap().1;
    let verse = f.sections[1];
    // The verse takes the pad's length: what follows moves a bar later.
    op(
        &mut f.s,
        ScratchOp::ToSong {
            pad,
            what: PadMaterial::All,
            place: SongPlacement::Replace {
                section: verse,
                keep_length: false,
            },
        },
    );
    let p = f.s.project();
    assert_eq!(song_end(&f.s), bar(7.0));
    assert_eq!(
        keys_on(p, f.keys),
        [(MusicalTime::ZERO, 60), (bar(2.0), 64), (bar(5.0), 64)]
    );
    assert_eq!(p.clips_of(f.keys).len(), 4, "the pad's extra bar came too");
    let chorus = p.sections.iter().find(|s| s.start == bar(2.0)).unwrap();
    assert_eq!((chorus.name.as_str(), chorus.end), ("Chorus", bar(4.0)));
    f.s.dispatch(Action::Undo).unwrap();
    assert_same(f.s.project(), &in_pad);
    // The verse keeps its two bars: the pad's third bar is left out.
    op(
        &mut f.s,
        ScratchOp::ToSong {
            pad,
            what: PadMaterial::All,
            place: SongPlacement::Replace {
                section: verse,
                keep_length: true,
            },
        },
    );
    let p = f.s.project();
    assert_eq!(song_end(&f.s), bar(6.0));
    assert_eq!(
        keys_on(p, f.keys),
        [(MusicalTime::ZERO, 60), (bar(2.0), 64), (bar(4.0), 64)]
    );
    assert_eq!(p.clips_of(f.keys).len(), 3);
    assert_eq!(p.clips_of(f.drums).len(), 3);
    f.s.dispatch(Action::Undo).unwrap();
    assert_same(f.s.project(), &in_pad);
}

#[test]
fn the_selection_goes_into_a_pad_and_back() {
    let mut f = song("selection");
    // The verse's keys clip only: a pad holding it, in its place in the
    // bar, without automation.
    let verse_keys = f.s.project().clips_of(f.keys)[1].id;
    f.s.dispatch(Action::SelectClips {
        clips: vec![verse_keys],
        mode: SelectMode::Replace,
    })
    .unwrap();
    op(
        &mut f.s,
        ScratchOp::New {
            name: Some("Keys idea".into()),
            from: Some(PadMaterial::Selection),
        },
    );
    let pad = f.s.shown_pad().unwrap();
    let p = f.s.project();
    assert_eq!(f.s.shown_arrangement_name(), "Keys idea");
    assert_eq!(keys_on(p, f.keys), [(MusicalTime::ZERO, 62)]);
    assert!(p.clips_of(f.drums).is_empty());
    assert!(p.track(f.keys).unwrap().automation.is_empty());
    assert!(p.sections.is_empty());
    // A range on the drums from beat 2 of bar 1 to bar 2: copied to the
    // end of the pad, still on beat 2.
    op(&mut f.s, ScratchOp::Show(None));
    f.s.dispatch(Action::SelectTracks {
        tracks: vec![f.drums],
        mode: SelectMode::Replace,
    })
    .unwrap();
    f.s.dispatch(Action::SetEditRange(Some(EditRange::new(
        MusicalTime::QUARTER,
        bar(1.0),
    ))))
    .unwrap();
    op(
        &mut f.s,
        ScratchOp::CopyTo {
            pad,
            what: PadMaterial::Selection,
        },
    );
    assert_eq!(f.s.shown_pad(), None, "copying keeps the song shown");
    op(&mut f.s, ScratchOp::Show(Some(pad)));
    let p = f.s.project();
    let drums = p.clips_of(f.drums);
    assert_eq!(drums.len(), 1);
    assert_eq!(drums[0].start, bar(2.0) + MusicalTime::QUARTER);
    // The pad's keys clip, selected, back over the song's chorus.
    let pad_keys = p.clips_of(f.keys)[0].id;
    f.s.dispatch(Action::SelectClips {
        clips: vec![pad_keys],
        mode: SelectMode::Replace,
    })
    .unwrap();
    op(
        &mut f.s,
        ScratchOp::ToSong {
            pad,
            what: PadMaterial::Selection,
            place: SongPlacement::Overwrite { at: bar(4.0) },
        },
    );
    let p = f.s.project();
    assert_eq!(
        keys_on(p, f.keys),
        [(MusicalTime::ZERO, 60), (bar(2.0), 62), (bar(4.0), 62)]
    );
    assert_eq!(p.clips_of(f.drums).len(), 3, "only the keys were laid over");
}

#[test]
fn the_clipboard_works_across_song_and_pads() {
    let mut f = song("clipboard");
    let before = f.s.project().clone();
    let intro_drums = f.s.project().clips_of(f.drums)[0].id;
    f.s.dispatch(Action::SelectClips {
        clips: vec![intro_drums],
        mode: SelectMode::Replace,
    })
    .unwrap();
    f.s.dispatch(Action::Copy).unwrap();
    op(
        &mut f.s,
        ScratchOp::New {
            name: None,
            from: None,
        },
    );
    assert_eq!(f.s.shown_arrangement_name(), "Scratch Pad 1");
    assert!(f.s.project().clips_of(f.drums).is_empty());
    f.s.dispatch(Action::Transport(TransportAction::Locate(bar(1.0))))
        .unwrap();
    f.s.dispatch(Action::Paste).unwrap();
    let p = f.s.project();
    assert_eq!(p.clips_of(f.drums).len(), 1);
    assert_eq!(p.clips_of(f.drums)[0].start, bar(1.0));
    assert_eq!(streamed(&f.s), [BAR]);
    // And from the pad into the song.
    let pasted = p.clips_of(f.drums)[0].id;
    f.s.dispatch(Action::SelectClips {
        clips: vec![pasted],
        mode: SelectMode::Replace,
    })
    .unwrap();
    f.s.dispatch(Action::Copy).unwrap();
    op(&mut f.s, ScratchOp::Show(None));
    assert_same(&song_of(f.s.project()), &song_of(&before));
    f.s.dispatch(Action::Transport(TransportAction::Locate(bar(6.0))))
        .unwrap();
    f.s.dispatch(Action::Paste).unwrap();
    assert_eq!(f.s.project().clips_of(f.drums).len(), 4);
    // The pad's playhead was kept: back there, where pasting left it (the
    // end of the two bars pasted at bar 1).
    let pad = f.s.scratch_pads().first().map(|p| p.id);
    op(&mut f.s, ScratchOp::Show(pad));
    assert_eq!(f.s.playhead(), bar(3.0));
}

#[test]
fn every_step_undoes_and_redoes() {
    let mut f = song("undo");
    let mut states = vec![f.s.project().clone()];
    let step = |s: &mut Session, o: ScratchOp, states: &mut Vec<Project>| {
        op(s, o);
        states.push(s.project().clone());
    };
    let chorus = f.sections[2];
    step(
        &mut f.s,
        ScratchOp::New {
            name: None,
            from: Some(PadMaterial::Section(chorus)),
        },
        &mut states,
    );
    let pad = f.s.shown_pad().unwrap();
    step(
        &mut f.s,
        ScratchOp::Rename {
            pad,
            name: "B".into(),
        },
        &mut states,
    );
    step(&mut f.s, ScratchOp::Duplicate(pad), &mut states);
    let copy = f.s.scratch_pads()[1].id;
    assert_eq!(f.s.scratch_pads()[1].name, "B Copy");
    step(&mut f.s, ScratchOp::Show(None), &mut states);
    step(
        &mut f.s,
        ScratchOp::CopyTo {
            pad: copy,
            what: PadMaterial::Section(f.sections[0]),
        },
        &mut states,
    );
    step(
        &mut f.s,
        ScratchOp::ToSong {
            pad: copy,
            what: PadMaterial::All,
            place: SongPlacement::Insert { at: bar(6.0) },
        },
        &mut states,
    );
    step(&mut f.s, ScratchOp::Show(Some(pad)), &mut states);
    step(&mut f.s, ScratchOp::Delete(pad), &mut states);
    assert_eq!(f.s.shown_pad(), None);
    assert_eq!(f.s.scratch_pads().len(), 1);
    // The duplicate holds its own clips (new ids) and the copied intro.
    let dup = f.s.scratch_pads()[0].stash.as_deref().unwrap();
    assert_eq!(dup.clips.len(), 4);
    assert_eq!(f.s.history_steps().0.len(), states.len() - 1);
    for want in states.iter().rev().skip(1) {
        f.s.dispatch(Action::Undo).unwrap();
        assert_same(f.s.project(), want);
    }
    for want in states.iter().skip(1) {
        f.s.dispatch(Action::Redo).unwrap();
        assert_same(f.s.project(), want);
    }
}

#[test]
fn pads_are_saved_and_reopened() {
    let mut f = song("save");
    let pad = chorus_pad(&mut f);
    op(
        &mut f.s,
        ScratchOp::New {
            name: Some("Empty".into()),
            from: None,
        },
    );
    op(&mut f.s, ScratchOp::Show(Some(pad)));
    let shown = f.s.project().clone();
    let dir = tmp("save-project");
    let path = dir.join("Pads.ffproj");
    f.s.save_as(&path).unwrap();
    // The file holds the song in place.
    let text = std::fs::read_to_string(&path).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["project"]["sections"].as_array().unwrap().len(), 3);
    let mut s = Session::new(Project::new("Other", SR), None, EngineConfig::default()).unwrap();
    s.open(&path).unwrap();
    assert_eq!(s.shown_pad(), Some(pad), "opens on the pad shown");
    let names: Vec<&str> = s.scratch_pads().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Chorus Idea", "Empty"]);
    assert_eq!(
        keys_on(s.project(), f.keys),
        keys_on(&shown, f.keys),
        "the pad's content"
    );
    assert_eq!(streamed(&s), [0]);
    s.dispatch(Action::ScratchPad(ScratchOp::Show(None)))
        .unwrap();
    assert_eq!(s.project().sections.len(), 3);
    assert_eq!(keys_on(s.project(), f.keys).len(), 3);
}

#[test]
fn media_only_a_pad_plays_are_kept() {
    let dir = tmp("cleanup");
    let file = dir.join("Kick.wav");
    tone(&file, 1.0);
    let mut s = Session::new(Project::new("Cleanup", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let path = project.join("Cleanup.ffproj");
    s.save_as(&path).unwrap();
    // A pad holds a copy; the song's clip goes.
    s.dispatch(Action::ScratchPad(ScratchOp::New {
        name: None,
        from: Some(PadMaterial::Clips(vec![clip])),
    }))
    .unwrap();
    s.dispatch(Action::ScratchPad(ScratchOp::Show(None)))
        .unwrap();
    s.dispatch(Action::Edit(Command::RemoveClip { clip }))
        .unwrap();
    assert!(s.project().clips.is_empty());
    s.save().unwrap();
    let unused = s.unused_media().unwrap();
    assert!(unused.is_empty(), "the pad plays it: {unused:?}");
    // Without the pad nothing does.
    let pad = s.scratch_pads()[0].id;
    s.dispatch(Action::ScratchPad(ScratchOp::Delete(pad)))
        .unwrap();
    s.save().unwrap();
    let unused = s.unused_media().unwrap();
    assert!(
        unused
            .files
            .iter()
            .any(|f| f.extension().is_some_and(|e| e == "wav")),
        "{unused:?}"
    );
}

#[test]
fn renders_take_the_song_whatever_is_shown() {
    use faderframe_session::render::{RenderRange, RenderSettings};
    let mut f = song("render");
    chorus_pad(&mut f);
    let out = f._dir.join("song.wav");
    let settings = RenderSettings {
        range: RenderRange::Project,
        tail_seconds: 0.0,
        ..RenderSettings::defaults_for(f.s.project(), out.clone())
    };
    f.s.render(settings).unwrap().join().unwrap();
    let wav = faderframe_audio_files::read_wav(&out).unwrap();
    // Six bars of the song (12 s), not the pad's two.
    let seconds = wav.channels[0].len() as f64 / f64::from(wav.sample_rate);
    assert!((seconds - 12.0).abs() < 0.05, "{seconds}");
    assert_eq!(f.s.shown_pad(), f.s.scratch_pads().first().map(|p| p.id));
}

#[test]
fn switching_waits_for_a_gesture_to_end() {
    let mut f = song("gesture");
    f.s.dispatch(Action::BeginGesture("Drag".into())).unwrap();
    let r = f.s.dispatch(Action::ScratchPad(ScratchOp::New {
        name: None,
        from: None,
    }));
    assert!(r.is_err());
    f.s.dispatch(Action::EndGesture).unwrap();
    assert!(f.s.scratch_pads().is_empty());
}

#[test]
fn a_frozen_track_plays_its_freeze_in_the_song_only() {
    let mut f = song("freeze");
    let source = *f.s.project().sources.keys().next().unwrap();
    f.s.dispatch(Action::Edit(Command::SetTrackFreeze {
        track: f.drums,
        freeze: Some(faderframe_project::Freeze {
            source,
            start: MusicalTime::ZERO,
            length: 4 * i64::from(SR),
            latency: 0,
        }),
    }))
    .unwrap();
    // The song: the freeze instead of the clips.
    assert_eq!(streamed(&f.s), [0]);
    let intro = f.sections[0];
    op(
        &mut f.s,
        ScratchOp::New {
            name: None,
            from: Some(PadMaterial::Section(intro)),
        },
    );
    // In a pad the song's freeze is not heard, nor the frozen track's clips.
    assert!(streamed(&f.s).is_empty());
    assert_eq!(f.s.project().clips_of(f.drums).len(), 1, "they are there");
    op(&mut f.s, ScratchOp::Show(None));
    assert_eq!(streamed(&f.s), [0]);
}

/// A freeze is the song's. A frozen track's clips are copied into a pad
/// and edited there (the song's freeze stays right); bringing material
/// back that would rearrange the frozen track in the song is refused, and
/// the song stays as it was.
#[test]
fn a_frozen_tracks_clips_are_edited_in_a_pad_but_not_brought_over_its_freeze() {
    let mut f = song("freeze-edit");
    let source = *f.s.project().sources.keys().next().unwrap();
    f.s.dispatch(Action::Edit(Command::SetTrackFreeze {
        track: f.drums,
        freeze: Some(faderframe_project::Freeze {
            source,
            start: MusicalTime::ZERO,
            length: 4 * i64::from(SR),
            latency: 0,
        }),
    }))
    .unwrap();
    let song_before = song_of(f.s.project());
    op(
        &mut f.s,
        ScratchOp::New {
            name: None,
            from: Some(PadMaterial::Section(f.sections[0])),
        },
    );
    let pad = f.s.project().scratch_pads.shown.unwrap();
    // In the pad the frozen track's clip is the pad's: it moves.
    let clip = f.s.project().clips_of(f.drums)[0].id;
    f.s.dispatch(Action::Edit(Command::MoveClip {
        clip,
        track: f.drums,
        start: bar(1.0),
    }))
    .unwrap();
    assert_eq!(f.s.project().clip(clip).unwrap().start, bar(1.0));
    assert_eq!(song_of(f.s.project()), song_before, "the song untouched");
    // Laid over the song it would move the frozen track's clips under
    // its freeze: refused, and the song is as it was.
    let refused = f.s.dispatch(Action::ScratchPad(ScratchOp::ToSong {
        pad,
        what: PadMaterial::All,
        place: SongPlacement::Overwrite {
            at: MusicalTime::ZERO,
        },
    }));
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(song_of(f.s.project()), song_before);
    // Unfrozen, it goes through.
    op(&mut f.s, ScratchOp::Show(None));
    f.s.dispatch(Action::Edit(Command::SetTrackFreeze {
        track: f.drums,
        freeze: None,
    }))
    .unwrap();
    op(
        &mut f.s,
        ScratchOp::ToSong {
            pad,
            what: PadMaterial::All,
            place: SongPlacement::Overwrite {
                at: MusicalTime::ZERO,
            },
        },
    );
    assert_ne!(song_of(f.s.project()), song_before);
}

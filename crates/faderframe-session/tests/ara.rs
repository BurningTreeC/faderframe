//! ARA in a live session, against the mock ARA plug-in: a clip opened
//! with ARA (one undo step), the plug-in's edits saved with the project
//! and back on opening it, its notes as MIDI, its transport requests.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::ara::PlaybackRequest;
use faderframe_plugin_host::ara_mock::{self, AraMockFactory};
use faderframe_project::{PluginFormat, PluginRef, Project, TrackKind};
use faderframe_session::{Action, Session, UiRequest};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn registry() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(AraMockFactory));
        r
    });
}

fn mock() -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: ara_mock::ID.into(),
        name: ara_mock::NAME.into(),
    }
}

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-ara-{}-{name}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

/// A session named `name` (its ARA document's name: the test finds its
/// mock instance by it) with a second of A440 at quarter 4.
fn session(name: &str) -> (Session, ClipId) {
    registry();
    let file = dir(name).join("tone.wav");
    let tone: Vec<f32> = (0..SR as usize)
        .map(|i| 0.5 * (std::f64::consts::TAU * 440.0 * i as f64 / f64::from(SR)).sin() as f32)
        .collect();
    write_wav(&file, &[tone], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new(name, SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::from_quarters(4.0),
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    (s, clip)
}

fn open(s: &mut Session, clip: ClipId) {
    s.dispatch(Action::OpenWithAra {
        clip,
        plugin: Some(mock()),
    })
    .unwrap();
}

#[test]
fn a_clip_opened_with_ara_is_rendered_by_the_plugin() {
    let (mut s, clip) = session("ARA open");
    assert_eq!(s.ara_plugins(), vec![mock()]);
    let menu = s.ara_menu(clip);
    assert_eq!(menu.len(), 1);
    assert_eq!(menu[0].label, "Open with ARA Mock (ARA)");
    let track = s.project().clip(clip).unwrap().track;
    s.take_ui_requests();
    open(&mut s, clip);
    let slot = s.project().track(track).unwrap().inserts[0].clone();
    assert_eq!(slot.plugin, mock());
    assert!(s.project().is_ara_slot(slot.id));
    assert_eq!(s.ara_rendered_tracks(), vec![track]);
    assert!(s.take_ui_requests().iter().any(|r| matches!(
        r,
        UiRequest::PluginEditor { plugin, generic: false, .. } if *plugin == slot.id
    )));
    let probe = ara_mock::probe_of("ARA open").unwrap();
    let summary = probe.summary();
    assert_eq!(summary.regions, 1);
    assert_eq!(summary.sequences.len(), 1);
    assert_eq!(probe.selected(), 1, "the editor shows the clip");
    assert!(probe.violations().is_empty(), "{:?}", probe.violations());
    let labels: Vec<String> = s.ara_menu(clip).into_iter().map(|e| e.label).collect();
    assert_eq!(
        labels,
        ["Edit in ARA Mock (ARA)", "Notes Found by ARA Mock to MIDI"]
    );
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().track(track).unwrap().inserts.is_empty());
    assert!(!s.project().is_ara_slot(slot.id));
    assert!(s.ara_rendered_tracks().is_empty());
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.ara_rendered_tracks(), vec![track]);
    // Moving the clip moves its region (no second region, no breach).
    let at = MusicalTime::from_quarters(8.0);
    s.dispatch(Action::Edit(faderframe_project::Command::MoveClip {
        clip,
        track,
        start: at,
    }))
    .unwrap();
    let probe = ara_mock::probe_of("ARA open").unwrap();
    assert_eq!(probe.summary().regions, 1);
    assert!(probe.violations().is_empty(), "{:?}", probe.violations());
}

#[test]
fn the_plugins_edits_are_saved_and_come_back() {
    let (mut s, clip) = session("ARA saved");
    open(&mut s, clip);
    // The analysis it reports changes its document too.
    s.tick(0.016);
    assert!(s.is_dirty());
    let path = dir("ARA saved").join("song.ffproj");
    s.save_as(&path).unwrap();
    s.tick(0.016);
    assert!(!s.is_dirty(), "saved with its archive");
    let probe = ara_mock::probe_of("ARA saved").unwrap();
    let modification = probe.summary().modifications[0].0.clone();
    assert!(probe.edit(&modification, 0.5));
    s.tick(0.016);
    assert!(s.is_dirty(), "an edit in the plug-in is unsaved");
    s.save_as(&path).unwrap();
    assert!(!s.is_dirty());
    assert_eq!(s.project().ara.archives.len(), 1);
    let archive = s.project().ara.archives[0].clone();
    assert_eq!(archive.plugin, "ARA Mock");
    drop(s);
    let mut s = Session::new(Project::new("other", SR), None, EngineConfig::default()).unwrap();
    s.open(&path).unwrap();
    let probe = ara_mock::probe_of("ARA saved").unwrap();
    let summary = probe.summary();
    assert_eq!(summary.restored, 1, "the document came back from the file");
    assert_eq!(summary.modifications[0].1, 0.5);
    assert!(probe.violations().is_empty(), "{:?}", probe.violations());
}

#[test]
fn the_notes_the_plugin_found_become_midi() {
    let (mut s, clip) = session("ARA notes");
    open(&mut s, clip);
    let tracks = s.project().tracks.len();
    s.dispatch(Action::AraNotesToMidi(clip)).unwrap();
    let p = s.project();
    assert_eq!(p.tracks.len(), tracks + 1);
    let t = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap();
    let c = p.clip(t.clips[0]).unwrap();
    assert_eq!(c.start, MusicalTime::from_quarters(4.0));
    let m = c.as_midi().unwrap();
    assert_eq!(m.notes.len(), 1);
    assert_eq!(m.notes[0].key, 69);
    assert!(m.notes[0].start.quarters().abs() < 0.01);
    assert!(
        (m.notes[0].length.quarters() - 2.0).abs() < 0.05,
        "a second at 120 BPM"
    );
}

#[test]
fn the_plugin_drives_the_transport() {
    let (mut s, clip) = session("ARA transport");
    open(&mut s, clip);
    let probe = ara_mock::probe_of("ARA transport").unwrap();
    probe.request(PlaybackRequest::CycleRange {
        start: 1.0,
        duration: 2.0,
    });
    probe.request(PlaybackRequest::EnableCycle(true));
    probe.request(PlaybackRequest::Locate(1.5));
    s.tick(0.016);
    s.tick(0.016);
    let p = s.project();
    let range = p.loop_range.unwrap();
    assert!((range.start.quarters() - 2.0).abs() < 1e-6);
    assert!((range.end.quarters() - 6.0).abs() < 1e-6);
    assert!(p.loop_enabled);
}

//! Tunings in the session: Scala files read into the project and tracks
//! (undoable), the menus' choices, the piano roll's degree labels, MTS to
//! external MIDI 1.0 devices and per-note pitch to MIDI 2.0 ones.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_midi::tuning::Tuning;
use faderframe_midi::tuning::mts::{self, Mts};
use faderframe_midi::ump::{Message, Voice2, packets};
use faderframe_project::tuning::Retune;
use faderframe_project::{Command, MidiOutputRouting, MpeConfig, Project, TrackKind};
use faderframe_session::tuning::TuningFile;
use faderframe_session::{Action, SelectMode, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SCALE: &str = "! ptolemy.scl
!
Ptolemy's intense diatonic
 7
!
 9/8
 5/4
 4/3
 3/2
 5/3
 15/8
 2/1
";

const WHITE: &str = "! white.kbm
12
0
127
60
69
440.0
7
0
x
1
x
2
3
x
4
x
5
x
6
";

/// A folder of this test's own with the files.
fn files(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ff-tuning-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let scl = dir.join("ptolemy.scl");
    let kbm = dir.join("white.kbm");
    std::fs::write(&scl, SCALE).unwrap();
    std::fs::write(&kbm, WHITE).unwrap();
    (scl, kbm)
}

fn session() -> Session {
    Session::new(
        Project::new("Tuning", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap()
}

fn choose(
    s: &mut Session,
    choices: impl FnOnce(&Session) -> Vec<faderframe_session::InputChoice>,
    label: &str,
) {
    let c = choices(s)
        .into_iter()
        .find(|c| c.label == label)
        .unwrap_or_else(|| panic!("no choice '{label}'"));
    s.dispatch(c.action).unwrap();
}

fn run(s: &mut Session, millis: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(millis) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

#[test]
fn scala_files_tune_the_project_and_a_mapping_follows_undoably() {
    let (scl, kbm) = files("load");
    let mut s = session();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::LoadTuning {
        track: None,
        scale: scl.clone(),
        mapping: None,
    })
    .unwrap();
    let t = s.project().tuning.clone().unwrap();
    assert_eq!(t.name, "ptolemy");
    assert_eq!(t.description(), "Ptolemy's intense diatonic");
    assert!(s.project().is_retuned(keys), "the track follows");
    s.dispatch(Action::LoadKeyboardMapping {
        track: None,
        mapping: Some(kbm.clone()),
    })
    .unwrap();
    let t = s.project().tuning.clone().unwrap();
    assert_eq!(t.name, "ptolemy (white)");
    assert!(t.mapping.is_some());
    let table = s.project().tuning_table(keys);
    assert!(
        (table.frequency(60).unwrap() - 264.0).abs() < 1e-9,
        "just C"
    );
    assert!(!table.is_mapped(61));
    // Again: the name does not pile up.
    s.dispatch(Action::LoadKeyboardMapping {
        track: None,
        mapping: Some(kbm),
    })
    .unwrap();
    assert_eq!(s.project().tuning.as_ref().unwrap().name, "ptolemy (white)");
    // Each was one undo step.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().tuning.as_ref().unwrap().mapping.is_none());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tuning, None);
    // A broken file says why and changes nothing.
    let bad = scl.with_file_name("broken.scl");
    std::fs::write(&bad, "desc\n3\n9/8\n").unwrap();
    assert!(
        s.dispatch(Action::LoadTuning {
            track: Some(keys),
            scale: bad,
            mapping: None,
        })
        .is_err()
    );
    assert!(s.notices().any(|n| n.text.contains("3 notes announced")));
    assert_eq!(s.project().track(keys).unwrap().tuning, None);
    // The choosers are the shell's.
    s.dispatch(Action::PromptTuning {
        track: Some(keys),
        file: TuningFile::Mapping,
    })
    .unwrap();
    assert!(s.take_ui_requests().contains(&UiRequest::ChooseTuning {
        track: Some(keys),
        file: TuningFile::Mapping,
    }));
}

#[test]
fn menus_choose_tunings_retuning_and_the_reference_pitch() {
    let mut s = session();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    let audio = s.add_track(TrackKind::Audio).unwrap();
    assert!(
        s.tuning_choices(Some(audio)).is_empty(),
        "no notes, no tuning"
    );
    assert!(s.retune_choices(audio).is_empty());
    // The project: 12-TET is checked, 31-EDO chosen.
    let project = s.tuning_choices(None);
    assert!(
        project
            .iter()
            .any(|c| c.label == "12-TET (Standard)" && c.checked)
    );
    choose(&mut s, |s| s.tuning_choices(None), "31-EDO");
    assert_eq!(s.project().tuning, Some(Tuning::equal(31)));
    choose(&mut s, |s| s.tuning_choices(None), "12-TET (Standard)");
    assert_eq!(s.project().tuning, None, "12-TET is no tuning of its own");
    // A track: its own, then the project's again.
    choose(&mut s, |s| s.more_tuning_choices(Some(keys)), "53-EDO");
    assert_eq!(
        s.project().track(keys).unwrap().tuning,
        Some(Tuning::equal(53))
    );
    assert!(
        s.tuning_choices(Some(keys))[0]
            .label
            .starts_with("Project Tuning")
    );
    assert!(!s.tuning_choices(Some(keys))[0].checked);
    choose(
        &mut s,
        |s| s.tuning_choices(Some(keys)),
        "Project Tuning (12-TET)",
    );
    assert_eq!(s.project().track(keys).unwrap().tuning, None);
    // Retuning: MPE only on MPE tracks.
    let labels = |s: &Session| -> Vec<String> {
        s.retune_choices(keys)
            .into_iter()
            .map(|c| c.label)
            .collect()
    };
    assert!(!labels(&s).iter().any(|l| l.contains("MPE")));
    s.dispatch(Action::Edit(Command::SetTrackMpe {
        track: keys,
        mpe: Some(MpeConfig::default()),
    }))
    .unwrap();
    assert!(labels(&s).iter().any(|l| l.contains("MPE")));
    choose(&mut s, |s| s.retune_choices(keys), Retune::Mts.label());
    assert_eq!(s.project().track(keys).unwrap().retune, Retune::Mts);
    // The reference pitch.
    assert_eq!(s.project_tuning_name(), None);
    choose(&mut s, |s| s.reference_choices(), "A = 432 Hz");
    assert_eq!(s.project().reference_hz(), 432.0);
    assert_eq!(
        s.project_tuning_name().as_deref(),
        Some("12-TET · A = 432 Hz")
    );
}

#[test]
fn the_piano_roll_labels_rows_with_degrees_and_cents() {
    let (scl, kbm) = files("piano");
    let mut s = session();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: keys,
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters(4.0),
    })
    .unwrap();
    let clip = s.project().track(keys).unwrap().clips[0];
    assert_eq!(s.piano_tuning(clip), None, "12-TET keeps note names");
    s.dispatch(Action::Edit(Command::SetReferencePitch { hz: 432.0 }))
        .unwrap();
    assert_eq!(s.piano_tuning(clip), None, "at any reference pitch");
    s.dispatch(Action::LoadTuning {
        track: Some(keys),
        scale: scl,
        mapping: Some(kbm),
    })
    .unwrap();
    let p = s.piano_tuning(clip).unwrap();
    assert_eq!(p.notes, 7);
    assert_eq!(p.label(60).as_deref(), Some("0 · 0¢"));
    assert_eq!(p.label(64).as_deref(), Some("2 · 386¢"));
    assert_eq!(p.label(67).as_deref(), Some("4 · 702¢"));
    assert_eq!(p.label(61), None, "unmapped");
    assert!(p.is_tonic(72) && !p.in_scale(70));
}

/// A MIDI 1.0 device is told by MTS once per table, a MIDI 2.0 device gets
/// each note's pitch with the note.
#[test]
fn external_devices_are_told_by_mts_or_get_per_note_pitch() {
    use faderframe_audio::dummy::DummyBackend;
    use faderframe_session::AudioPreferences;
    let mut s = session();
    let midi1 = s.add_virtual_midi_output("Synth 1");
    let midi2 = s.add_virtual_ump_output("Synth 2");
    let mut tracks = Vec::new();
    for port in ["virtual:Synth 1", "virtual:Synth 2"] {
        let t = s.add_track(TrackKind::Midi).unwrap();
        s.dispatch(Action::Edit(Command::SetTrackMidiOutput {
            track: t,
            output: Some(MidiOutputRouting {
                port: port.into(),
                channel: None,
            }),
        }))
        .unwrap();
        tracks.push(t);
    }
    let sysex = |c: &faderframe_midi_io::Captured| -> Vec<Vec<u8>> {
        c.lock()
            .unwrap()
            .iter()
            .filter(|(_, b)| b.first() == Some(&0xF0))
            .map(|(_, b)| b.clone())
            .collect()
    };
    s.tick(0.01);
    assert!(sysex(&midi1).is_empty(), "12-TET: nothing to tell");
    s.dispatch(Action::Edit(Command::SetTuning {
        track: None,
        tuning: Some(Tuning::equal(19)),
    }))
    .unwrap();
    s.tick(0.01);
    let told = sysex(&midi1);
    assert_eq!(told.len(), 3, "a bulk dump and two real-time changes");
    let Some(Mts::Bulk { name, pitches, .. }) = mts::parse(&told[0]) else {
        panic!("not a bulk dump")
    };
    assert_eq!(name, "19-EDO");
    let table = Tuning::equal(19).table(440.0);
    assert!((pitches[61].unwrap() - table.pitch(61).unwrap()).abs() < 1e-4);
    assert!(matches!(
        mts::parse(&told[1]),
        Some(Mts::NoteChange { realtime: true, .. })
    ));
    s.tick(0.01);
    assert_eq!(sysex(&midi1).len(), 3, "told once");
    // Retuning off: not told again (the next table either).
    s.dispatch(Action::Edit(Command::SetTrackRetune {
        track: tracks[0],
        retune: Retune::Off,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetReferencePitch { hz: 432.0 }))
        .unwrap();
    s.tick(0.01);
    assert_eq!(sysex(&midi1).len(), 3);
    // The MIDI 2.0 port got no MTS.
    let words: Vec<Message> = midi2
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, w)| packets(w).map(|p| Message::parse(&p)).collect::<Vec<_>>())
        .collect();
    assert!(words.is_empty(), "{words:?}");
    // A key played live on it goes out with its pitch.
    s.dispatch(Action::SelectTracks {
        tracks: vec![tracks[1]],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    run(&mut s, 100);
    s.midi_keyboard().send(&[0x90, 61, 100]);
    run(&mut s, 200);
    let got: Vec<Message> = midi2
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, w)| packets(w).map(|p| Message::parse(&p)).collect::<Vec<_>>())
        .collect();
    s.stop_audio();
    let on = got
        .iter()
        .position(|m| {
            matches!(
                m,
                Message::Midi2 {
                    voice: Voice2::NoteOn { note: 61, .. },
                    ..
                }
            )
        })
        .unwrap_or_else(|| panic!("no note: {got:?}"));
    let bend = got[on..].iter().find_map(|m| match m {
        Message::Midi2 {
            voice: Voice2::PerNotePitchBend { note: 61, value },
            ..
        } => Some(faderframe_midi::ump::bend_of(*value) * 48.0),
        _ => None,
    });
    let want = Tuning::equal(19).table(432.0).offset(61);
    let bend = bend.expect("the note's pitch went out");
    assert!((bend - want).abs() < 1e-3, "{bend} for {want}");
}

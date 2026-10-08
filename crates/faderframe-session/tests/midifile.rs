//! Standard MIDI File import and export.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_project::{MidiController, Project, TrackKind};
use faderframe_session::{Action, Session};
use faderframe_timeline::{MusicalTime, TimeSignature};
use midly::num::{u4, u7, u14, u15, u24, u28};
use midly::{
    Format, Header, MetaMessage, MidiMessage, PitchBend, Smf, Timing, TrackEvent, TrackEventKind,
};

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ff-midifile-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn empty() -> Session {
    Session::new(Project::new("Empty", 48_000), None, EngineConfig::default()).unwrap()
}

#[test]
fn export_then_import_keeps_notes_and_tempo() {
    let demo = Session::demo(EngineConfig::default()).unwrap();
    let path = dir().join("demo.mid");
    let tracks = demo.export_midi_file(&path, None).unwrap();
    assert_eq!(tracks, 2, "the demo's instrument track and MIDI track");
    let original = demo
        .project()
        .clips
        .values()
        .find(|c| c.name == "Melody")
        .unwrap()
        .clone();

    let mut s = empty();
    let created = s.import_midi_file(&path, MusicalTime::ZERO, true).unwrap();
    assert_eq!(created.len(), 2);
    let t = s.project().track(created[0]).unwrap();
    assert_eq!(
        (t.kind, t.name.as_str()),
        (TrackKind::Instrument, "Lead Synth")
    );
    assert!(t.instrument.is_none(), "instruments are chosen afterwards");
    assert_eq!(s.project().timeline.tempo.points()[0].bpm.round(), 112.0);
    let clip = s.project().clips_of(created[0])[0].clone();
    let (a, b) = (original.as_midi().unwrap(), clip.as_midi().unwrap());
    assert_eq!(a.notes.len(), b.notes.len());
    // The export starts at the clip's position: the import puts it at 0,
    // so compare absolute times.
    for (x, y) in a.notes.iter().zip(&b.notes) {
        assert_eq!(x.key, y.key);
        assert_eq!(x.velocity, y.velocity);
        assert_eq!(original.start + x.start, clip.start + y.start);
        assert_eq!(x.length, y.length);
    }
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().clips.is_empty());
    assert_eq!(s.project().timeline.tempo.points()[0].bpm, 120.0);
}

fn ev(delta: u32, kind: TrackEventKind<'static>) -> TrackEvent<'static> {
    TrackEvent {
        delta: u28::new(delta),
        kind,
    }
}

fn midi(ch: u8, message: MidiMessage) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: u4::new(ch),
        message,
    }
}

#[test]
fn format_0_files_split_by_channel_with_tempo_meter_controllers_and_sysex() {
    // 96 ticks per quarter; 100 BPM, 140 BPM from beat 8; 3/4 from bar 3.
    let on = |key: u8| MidiMessage::NoteOn {
        key: u7::new(key),
        vel: u7::new(90),
    };
    let off = |key: u8| MidiMessage::NoteOn {
        key: u7::new(key),
        vel: u7::new(0),
    };
    let mut smf = Smf::new(Header::new(
        Format::SingleTrack,
        Timing::Metrical(u15::new(96)),
    ));
    smf.tracks.push(vec![
        ev(
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(600_000))),
        ),
        ev(
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
        ),
        ev(0, midi(0, on(60))),
        ev(0, midi(9, on(36))),
        ev(
            0,
            midi(
                0,
                MidiMessage::Controller {
                    controller: u7::new(1),
                    value: u7::new(40),
                },
            ),
        ),
        ev(48, midi(9, off(36))),
        ev(48, midi(0, off(60))),
        ev(
            0,
            midi(
                0,
                MidiMessage::PitchBend {
                    bend: PitchBend(u14::new(12_000)),
                },
            ),
        ),
        ev(0, TrackEventKind::SysEx(&[0x41, 0x10])),
        ev(0, TrackEventKind::Escape(&[0x42, 0xF7])),
        ev(
            672,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(428_571))),
        ),
        ev(
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
        ),
        ev(0, midi(0, on(64))),
        // Never released: lasts to the end.
        ev(
            96,
            midi(9, MidiMessage::ChannelAftertouch { vel: u7::new(70) }),
        ),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);
    let path = dir().join("type0.mid");
    smf.save(&path).unwrap();

    let mut s = empty();
    s.dispatch(Action::ImportFiles {
        files: vec![path.clone()],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    let tracks: Vec<_> = s
        .project()
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Instrument)
        .cloned()
        .collect();
    let names: Vec<&str> = tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["type0 Ch 1", "type0 Drums"]);
    let tl = &s.project().timeline;
    let bpms: Vec<(f64, i64)> = tl
        .tempo
        .points()
        .iter()
        .map(|p| ((p.bpm * 10.0).round() / 10.0, p.position.ticks() / 960_000))
        .collect();
    assert_eq!(bpms, [(100.0, 0), (140.0, 8)]);
    assert_eq!(
        tl.meter.signature_of_bar(2),
        TimeSignature::new(3, 4).unwrap()
    );
    assert_eq!(tl.meter.signature_of_bar(1), TimeSignature::FOUR_FOUR);

    let lead = s.project().clips_of(tracks[0].id)[0]
        .as_midi()
        .unwrap()
        .clone();
    assert_eq!(lead.notes.len(), 2);
    assert_eq!(lead.notes[0].length, MusicalTime::from_quarters(1.0));
    assert_eq!(lead.notes[1].start, MusicalTime::from_quarters(8.0));
    let mods = lead.lane(MidiController::MOD_WHEEL, 0).unwrap();
    assert_eq!(mods.points[0].value, 40);
    let bend = lead.lane(MidiController::PitchBend, 0).unwrap();
    assert_eq!(bend.points[0].value, 12_000);
    assert_eq!(lead.sysex.len(), 1);
    assert_eq!(lead.sysex[0].data, [0xF0, 0x41, 0x10, 0x42, 0xF7]);
    // Clips end on a bar line after their last event.
    assert!(lead.length >= MusicalTime::from_quarters(9.0));
    let drums = s.project().clips_of(tracks[1].id)[0]
        .as_midi()
        .unwrap()
        .clone();
    assert_eq!(drums.notes.len(), 1);
    assert_eq!(drums.notes[0].channel, 9);
    assert!(drums.lane(MidiController::ChannelPressure, 9).is_some());
    assert!(faderframe_session::is_midi_file(&path));
}

#[test]
fn exporting_only_selected_clips_and_nothing_to_export() {
    let demo = Session::demo(EngineConfig::default()).unwrap();
    let melody = demo
        .project()
        .clips
        .values()
        .find(|c| c.name == "Melody")
        .unwrap()
        .id;
    let path = dir().join("sel.mid");
    assert_eq!(demo.export_midi_file(&path, Some(&[melody])).unwrap(), 1);
    let drum_clip = demo
        .project()
        .clips
        .values()
        .find(|c| c.name == "Drum Loop")
        .unwrap()
        .id;
    assert!(demo.export_midi_file(&path, Some(&[drum_clip])).is_err());
    assert!(empty().export_midi_file(&path, None).is_err());
}

/// Program changes (after their bank select), polyphonic aftertouch and
/// note-off velocities come in, play from the clip and go out again.
#[test]
fn programs_poly_pressure_and_release_velocities_survive_import_and_export() {
    let cc = |n: u8, v: u8| MidiMessage::Controller {
        controller: u7::new(n),
        value: u7::new(v),
    };
    let mut smf = Smf::new(Header::new(
        Format::SingleTrack,
        Timing::Metrical(u15::new(96)),
    ));
    smf.tracks.push(vec![
        ev(0, midi(0, cc(0, 1))),
        ev(0, midi(0, cc(32, 0))),
        ev(
            0,
            midi(
                0,
                MidiMessage::ProgramChange {
                    program: u7::new(40),
                },
            ),
        ),
        ev(
            0,
            midi(
                0,
                MidiMessage::NoteOn {
                    key: u7::new(60),
                    vel: u7::new(100),
                },
            ),
        ),
        ev(
            24,
            midi(
                0,
                MidiMessage::Aftertouch {
                    key: u7::new(60),
                    vel: u7::new(77),
                },
            ),
        ),
        ev(
            72,
            midi(
                0,
                MidiMessage::NoteOff {
                    key: u7::new(60),
                    vel: u7::new(55),
                },
            ),
        ),
        ev(
            0,
            midi(
                0,
                MidiMessage::NoteOn {
                    key: u7::new(62),
                    vel: u7::new(90),
                },
            ),
        ),
        ev(
            96,
            midi(
                0,
                MidiMessage::NoteOn {
                    key: u7::new(62),
                    vel: u7::new(0),
                },
            ),
        ),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);
    let path = dir().join("programs.mid");
    smf.save(&path).unwrap();
    let mut s = empty();
    let tracks = s.import_midi_file(&path, MusicalTime::ZERO, false).unwrap();
    let clip_of = |s: &Session, t| {
        let id = s.project().track(t).unwrap().clips[0];
        s.project().clip(id).unwrap().as_midi().unwrap().clone()
    };
    let m = clip_of(&s, tracks[0]);
    let q = MusicalTime::from_quarters;
    let program = m.lane(MidiController::Program, 0).unwrap();
    assert_eq!(
        (program.points[0].time, program.points[0].value),
        (q(0.0), 40)
    );
    assert_eq!(
        m.lane(MidiController::Cc { number: 0 }, 0).unwrap().points[0].value,
        1
    );
    let poly = m.lane(MidiController::PolyPressure { key: 60 }, 0).unwrap();
    assert_eq!((poly.points[0].time, poly.points[0].value), (q(0.25), 77));
    let note = |k: u8| *m.notes.iter().find(|n| n.key == k).unwrap();
    assert_eq!(note(60).release, Some(55), "a note-off's velocity");
    assert_eq!(note(62).release, None, "a note-on of velocity 0 has none");
    assert!(
        !s.notices().any(|n| n.text.contains("not imported")),
        "nothing dropped"
    );

    // Out again: the bank before the program, poly pressure, the release.
    let out = dir().join("programs-out.mid");
    s.export_midi_file(&out, None).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    let back = Smf::parse(&bytes).unwrap();
    let messages: Vec<MidiMessage> = back
        .tracks
        .iter()
        .flatten()
        .filter_map(|e| match e.kind {
            TrackEventKind::Midi { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    let at = |m: &MidiMessage| messages.iter().position(|x| x == m);
    let program = at(&MidiMessage::ProgramChange {
        program: u7::new(40),
    })
    .expect("the program change");
    assert!(at(&cc(0, 1)).unwrap() < program && at(&cc(32, 0)).unwrap() < program);
    assert!(
        at(&MidiMessage::Aftertouch {
            key: u7::new(60),
            vel: u7::new(77),
        })
        .is_some()
    );
    assert!(
        at(&MidiMessage::NoteOff {
            key: u7::new(60),
            vel: u7::new(55),
        })
        .is_some()
    );
    // And in once more: the same clip.
    let mut again = empty();
    let t = again
        .import_midi_file(&out, MusicalTime::ZERO, false)
        .unwrap();
    let m2 = clip_of(&again, t[0]);
    assert_eq!(m2.controllers, m.controllers);
    let strip = |m: &faderframe_project::MidiClip| {
        m.notes
            .iter()
            .map(|n| (n.start, n.length, n.key, n.velocity, n.release))
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&m2), strip(&m));
}

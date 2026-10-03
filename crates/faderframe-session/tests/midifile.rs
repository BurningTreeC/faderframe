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
    assert_eq!(tracks, 1, "the demo's one instrument track");
    let original = demo
        .project()
        .clips
        .values()
        .find(|c| c.name == "Melody")
        .unwrap()
        .clone();

    let mut s = empty();
    let created = s.import_midi_file(&path, MusicalTime::ZERO, true).unwrap();
    assert_eq!(created.len(), 1);
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

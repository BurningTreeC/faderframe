//! Clips at MIDI 2.0's resolution: what a MIDI 2.0 port plays is recorded
//! with its 16-bit velocities and 32-bit controllers, played back to MIDI
//! 2.0 devices at that resolution (MIDI 1.0 devices get the top bits),
//! kept by `.midi2` files, rounded by Standard MIDI Files, and edited at
//! full resolution in the event list and with the velocity operations.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_midi::ump::{Attribute, Message, Voice2, packets, scale_up};
use faderframe_project::{
    ClipContent, Command, ControllerLane, ControllerPoint, MidiClip, MidiController, MidiNote,
    MidiOutputRouting, PluginRef, Project, TrackKind,
};
use faderframe_session::midi_events::{EventField, EventRef, EventValue};
use faderframe_session::{Action, AudioPreferences, NoteOp, SelectMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn run(s: &mut Session, millis: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(millis) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

fn send(s: &Session, channel: u8, voice: Voice2) {
    let p = Message::Midi2 {
        group: 0,
        channel,
        voice,
    }
    .to_ump();
    assert!(s.midi_keyboard().send_ump(p.words()));
}

fn start_audio(s: &mut Session) {
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
}

fn synth_track(s: &mut Session) -> TrackId {
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::SetInstrumentPlugin {
        track: t,
        plugin: Some(PluginRef::builtin(builtin::SYNTH, "Synth")),
    })
    .unwrap();
    t
}

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

fn note(id: u64, start: f64, key: u8) -> MidiNote {
    MidiNote {
        id: faderframe_core::NoteId(id),
        start: q(start),
        length: q(0.5),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: false,
        articulation: None,
        fine: None,
    }
}

/// A MIDI clip on `track` (one note at 16 bits, a 32-bit mod wheel and a
/// pitch bend); its id.
fn fine_clip(s: &mut Session, track: TrackId) -> faderframe_core::ClipId {
    let mut n = note(1, 0.25, 60);
    n.set_velocity16(0x1234);
    n.set_release16(Some(0x4321));
    n.set_attribute(Attribute {
        kind: 1,
        data: 0x0BAD,
    });
    let mut wheel = ControllerLane::new(MidiController::MOD_WHEEL, 0);
    wheel.points = vec![ControllerPoint::at32(
        q(0.125),
        MidiController::MOD_WHEEL,
        0x8765_4321,
    )];
    let mut bend = ControllerLane::new(MidiController::PitchBend, 0);
    bend.points = vec![ControllerPoint::at32(
        q(0.125),
        MidiController::PitchBend,
        0x9000_0001,
    )];
    let content = MidiClip {
        length: q(4.0),
        notes: vec![n],
        controllers: vec![wheel, bend],
        expressions: Vec::new(),
        sysex: Vec::new(),
        articulations: Vec::new(),
    };
    s.dispatch(Action::CreateMidiClip {
        track,
        start: MusicalTime::ZERO,
        length: q(4.0),
    })
    .unwrap();
    let id = s.project().clips_of(track)[0].id;
    s.dispatch(Action::Edit(Command::SetClipContent {
        clip: id,
        start: MusicalTime::ZERO,
        content: Box::new(ClipContent::Midi(content)),
    }))
    .unwrap();
    id
}

#[test]
fn a_take_from_a_midi2_port_keeps_its_resolution() {
    let mut s = Session::new(Project::new("Take", 48_000), None, EngineConfig::default()).unwrap();
    let t = synth_track(&mut s);
    start_audio(&mut s);
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: t,
        on: true,
    }))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 200);
    send(
        &s,
        0,
        Voice2::ControlChange {
            index: 1,
            value: 0x1357_9BDF,
        },
    );
    send(&s, 0, Voice2::PitchBend(0x8000_4000));
    send(
        &s,
        0,
        Voice2::NoteOn {
            note: 64,
            velocity: 0xA5A5,
            attribute: Attribute { kind: 2, data: 77 },
        },
    );
    run(&mut s, 150);
    send(
        &s,
        0,
        Voice2::NoteOff {
            note: 64,
            velocity: 0x0FF1,
            attribute: Attribute::default(),
        },
    );
    run(&mut s, 100);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    s.stop_audio();
    let clips = s.project().clips_of(t);
    assert_eq!(clips.len(), 1);
    let m = clips[0].as_midi().unwrap();
    assert_eq!(m.notes.len(), 1);
    let n = m.notes[0];
    assert_eq!((n.key, n.velocity), (64, (0xA5A5u16 >> 9) as u8));
    assert_eq!(n.velocity16(), 0xA5A5, "16 bits, as played");
    assert_eq!(n.release16(), Some(0x0FF1));
    assert_eq!(n.attribute(), Attribute { kind: 2, data: 77 });
    let wheel = m.lane(MidiController::MOD_WHEEL, 0).unwrap();
    assert_eq!(
        wheel.points[0].value32(MidiController::MOD_WHEEL),
        0x1357_9BDF
    );
    let bend = m.lane(MidiController::PitchBend, 0).unwrap();
    assert_eq!(
        bend.points[0].value32(MidiController::PitchBend),
        0x8000_4000
    );
    // The event list shows the fraction MIDI 2.0 adds.
    let rows = s.midi_events(clips[0].id);
    let row = rows
        .iter()
        .find(|r| matches!(r.event, EventRef::Note(_)))
        .unwrap();
    let steps = row.fine.unwrap();
    assert!(
        (steps - f64::from(0xA5A5u16) / 512.0).abs() < 1e-9,
        "{steps}"
    );
}

#[test]
fn a_captured_phrase_keeps_its_16_bit_velocities() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth")
        .unwrap()
        .id;
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    run(&mut s, 30);
    let before: Vec<_> = s.project().clips_of(lead).iter().map(|c| c.id).collect();
    for (i, v) in [0x2001u16, 0xFFFE, 0x0005].into_iter().enumerate() {
        send(
            &s,
            0,
            Voice2::NoteOn {
                note: 60 + i as u8,
                velocity: v,
                attribute: Attribute::default(),
            },
        );
        run(&mut s, 40);
        send(
            &s,
            0,
            Voice2::NoteOff {
                note: 60 + i as u8,
                velocity: 0,
                attribute: Attribute::default(),
            },
        );
        run(&mut s, 20);
    }
    s.dispatch(Action::CaptureMidi).unwrap();
    let clip = s
        .project()
        .clips_of(lead)
        .into_iter()
        .find(|c| !before.contains(&c.id))
        .unwrap();
    let mut notes = clip.as_midi().unwrap().notes.clone();
    notes.sort_by_key(|n| n.key);
    let got: Vec<(u8, u16)> = notes.iter().map(|n| (n.velocity, n.velocity16())).collect();
    // A MIDI 2.0 note-on below 1/128 plays at 1 for MIDI 1.0.
    assert_eq!(got, [(16, 0x2001), (127, 0xFFFE), (1, 0x0005)]);
}

#[test]
fn clips_play_midi2_devices_at_full_resolution_and_midi1_devices_rounded() {
    let mut s = Session::new(Project::new("Out", 48_000), None, EngineConfig::default()).unwrap();
    let ump = s.add_virtual_ump_output("Synth 2");
    let one = s.add_virtual_midi_output("Synth 1");
    let mut tracks = Vec::new();
    for port in ["virtual:Synth 2", "virtual:Synth 1"] {
        let t = s.add_track(TrackKind::Midi).unwrap();
        s.dispatch(Action::Edit(Command::SetTrackMidiOutput {
            track: t,
            output: Some(MidiOutputRouting {
                port: port.into(),
                channel: None,
            }),
        }))
        .unwrap();
        fine_clip(&mut s, t);
        tracks.push(t);
    }
    start_audio(&mut s);
    run(&mut s, 100);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    // 120 BPM: the note ends at 0.75 quarter (0.375 s).
    run(&mut s, 900);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, 100);
    s.stop_audio();
    let got: Vec<Message> = ump
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, w)| packets(w).map(|p| Message::parse(&p)).collect::<Vec<_>>())
        .collect();
    let voices: Vec<Voice2> = got
        .iter()
        .filter_map(|m| match m {
            Message::Midi2 { voice, .. } => Some(*voice),
            _ => None,
        })
        .collect();
    assert!(
        voices.contains(&Voice2::NoteOn {
            note: 60,
            velocity: 0x1234,
            attribute: Attribute {
                kind: 1,
                data: 0x0BAD
            },
        }),
        "{voices:?}"
    );
    assert!(voices.contains(&Voice2::NoteOff {
        note: 60,
        velocity: 0x4321,
        attribute: Attribute::default(),
    }));
    assert!(voices.contains(&Voice2::ControlChange {
        index: 1,
        value: 0x8765_4321
    }));
    assert!(voices.contains(&Voice2::PitchBend(0x9000_0001)));
    // MIDI 1.0: the top bits.
    let bytes: Vec<Vec<u8>> = one.lock().unwrap().iter().map(|(_, b)| b.clone()).collect();
    assert!(
        bytes.contains(&vec![0x90, 60, (0x1234 >> 9) as u8]),
        "{bytes:?}"
    );
    assert!(bytes.contains(&vec![0xB0, 1, (0x8765_4321u32 >> 25) as u8]));
    let b14 = 0x9000_0001u32 >> 18;
    assert!(bytes.contains(&vec![0xE0, (b14 & 0x7F) as u8, (b14 >> 7) as u8]));
}

#[test]
fn midi2_files_keep_the_resolution_and_standard_midi_files_round_it() {
    let mut s = Session::new(Project::new("Files", 48_000), None, EngineConfig::default()).unwrap();
    let t = synth_track(&mut s);
    let id = fine_clip(&mut s, t);
    let dir = std::env::temp_dir().join(format!("ff-midi2-res-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let midi2 = dir.join("fine.midi2");
    s.export_midi2_clip(&midi2, id).unwrap();
    let smf = dir.join("fine.mid");
    s.export_midi_file(&smf, Some(&[id])).unwrap();

    let mut back =
        Session::new(Project::new("Back", 48_000), None, EngineConfig::default()).unwrap();
    let made = back
        .import_midi_file(&midi2, MusicalTime::ZERO, false)
        .unwrap();
    let m = back.project().clips_of(made[0])[0]
        .as_midi()
        .unwrap()
        .clone();
    let n = m.notes[0];
    assert_eq!(n.velocity16(), 0x1234);
    assert_eq!(n.release16(), Some(0x4321));
    assert_eq!(
        n.attribute(),
        Attribute {
            kind: 1,
            data: 0x0BAD
        }
    );
    let wheel = m.lane(MidiController::MOD_WHEEL, 0).unwrap();
    assert_eq!(
        wheel.points[0].value32(MidiController::MOD_WHEEL),
        0x8765_4321
    );
    let bend = m.lane(MidiController::PitchBend, 0).unwrap();
    assert_eq!(
        bend.points[0].value32(MidiController::PitchBend),
        0x9000_0001
    );

    // A Standard MIDI File holds MIDI 1.0's seven bits: the top ones.
    let made = back
        .import_midi_file(&smf, MusicalTime::ZERO, false)
        .unwrap();
    let m = back.project().clips_of(made[0])[0]
        .as_midi()
        .unwrap()
        .clone();
    let n = m.notes[0];
    assert_eq!((n.velocity, n.fine), ((0x1234 >> 9) as u8, None));
    assert_eq!(
        n.velocity16(),
        scale_up(u32::from(n.velocity), 7, 16) as u16
    );
    let wheel = m.lane(MidiController::MOD_WHEEL, 0).unwrap();
    assert_eq!(
        (wheel.points[0].value, wheel.points[0].fine),
        ((0x8765_4321u32 >> 25) as u16, None)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn velocities_and_values_are_edited_at_full_resolution() {
    let mut s = Session::new(Project::new("Edit", 48_000), None, EngineConfig::default()).unwrap();
    let t = synth_track(&mut s);
    let id = fine_clip(&mut s, t);
    let note_id = faderframe_core::NoteId(1);
    let first = |s: &Session| s.project().clip(id).unwrap().as_midi().unwrap().notes[0];
    // The event list: a fraction is MIDI 2.0's resolution …
    s.dispatch(Action::EditMidiEvents {
        clip: id,
        events: vec![EventRef::Note(note_id)],
        field: EventField::Data2,
        value: EventValue::Fine(100.5),
    })
    .unwrap();
    assert_eq!(first(&s).velocity16(), 100 * 512 + 256);
    assert_eq!(first(&s).velocity_text(), "100.50");
    // … whole steps are MIDI 1.0's.
    s.dispatch(Action::EditMidiEvents {
        clip: id,
        events: vec![EventRef::Note(note_id)],
        field: EventField::Data2,
        value: EventValue::Number(90),
    })
    .unwrap();
    assert_eq!(first(&s).velocity, 90);
    assert!(!first(&s).has_fine_velocity());
    assert_eq!(first(&s).fine.and_then(|f| f.velocity), None);
    let wheel = EventRef::Controller {
        controller: MidiController::MOD_WHEEL,
        channel: 0,
        time: q(0.125),
    };
    s.dispatch(Action::EditMidiEvents {
        clip: id,
        events: vec![wheel],
        field: EventField::Data2,
        value: EventValue::Fine(64.25),
    })
    .unwrap();
    let m = s.project().clip(id).unwrap().as_midi().unwrap().clone();
    let p = m.lane(MidiController::MOD_WHEEL, 0).unwrap().points[0];
    assert_eq!(
        (p.value, p.value32(MidiController::MOD_WHEEL)),
        (64, 0x8080_0000)
    );

    // The velocity lane's operations at 16 bits, undoable.
    s.dispatch(Action::NoteOperation {
        clip: id,
        notes: vec![note_id],
        op: NoteOp::SetVelocity16(0x7777),
    })
    .unwrap();
    assert_eq!(first(&s).velocity16(), 0x7777);
    s.dispatch(Action::NoteOperation {
        clip: id,
        notes: vec![note_id],
        op: NoteOp::OffsetVelocity16(-0x0077),
    })
    .unwrap();
    assert_eq!(first(&s).velocity16(), 0x7700);
    // A MIDI 1.0 edit keeps the resolution where the note has it.
    s.dispatch(Action::NoteOperation {
        clip: id,
        notes: vec![note_id],
        op: NoteOp::ScaleVelocity {
            factor: 1.0,
            offset: 1,
        },
    })
    .unwrap();
    assert_eq!(first(&s).velocity, (0x7700u16 >> 9) as u8 + 1);
    assert_eq!(first(&s).velocity16(), 0x7700 + 512);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(first(&s).velocity16(), 0x7700);
}

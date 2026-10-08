//! MIDI 2.0 input: a phrase played on a MIDI 2.0 port — 16-bit velocity,
//! per-note pitch bend and per-note brightness — keeps its per-note
//! expression when it is captured (and recorded).
#![allow(clippy::unwrap_used)]

use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_midi::ump::{Attribute, Message, Voice2, of_bend, of_unit, per_note};
use faderframe_project::ExpressionKind;
use faderframe_session::{Action, SelectMode, Session};
use std::time::{Duration, Instant};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn run(s: &mut Session, millis: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(millis) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

fn send(s: &Session, voice: Voice2) {
    let p = Message::Midi2 {
        group: 0,
        channel: 0,
        voice,
    }
    .to_ump();
    assert!(s.midi_keyboard().send_ump(p.words()));
}

#[test]
fn a_midi2_phrase_keeps_its_per_note_expression() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    run(&mut s, 30);
    let before: Vec<_> = s.project().clips_of(lead).iter().map(|c| c.id).collect();
    send(
        &s,
        Voice2::NoteOn {
            note: 62,
            velocity: 0xC000,
            attribute: Attribute::default(),
        },
    );
    run(&mut s, 60);
    // Bend up two semitones, brighten.
    for i in 1..=8u32 {
        send(
            &s,
            Voice2::PerNotePitchBend {
                note: 62,
                value: of_bend(2.0 / 48.0 * f64::from(i) / 8.0),
            },
        );
        send(
            &s,
            Voice2::RegisteredPerNote {
                note: 62,
                index: per_note::BRIGHTNESS,
                value: of_unit(0.5 + 0.3 * f64::from(i) / 8.0),
            },
        );
        run(&mut s, 20);
    }
    run(&mut s, 60);
    send(
        &s,
        Voice2::NoteOff {
            note: 62,
            velocity: 0x8000,
            attribute: Attribute::default(),
        },
    );
    run(&mut s, 40);
    assert!(s.can_capture_midi());
    s.dispatch(Action::CaptureMidi).unwrap();
    let p = s.project();
    let clip = p
        .clips_of(lead)
        .into_iter()
        .find(|c| !before.contains(&c.id))
        .unwrap();
    let m = clip.as_midi().unwrap();
    assert_eq!(m.notes.len(), 1);
    let note = m.notes[0];
    assert_eq!(note.key, 62);
    // 0xC000 of 0xFFFF is 96 of 127.
    assert_eq!(note.velocity, 96);
    let e = m.expressions.iter().find(|e| e.note == note.id).unwrap();
    let pitch = e.curve(ExpressionKind::Pitch);
    let timbre = e.curve(ExpressionKind::Timbre);
    assert!(pitch.len() >= 2, "{pitch:?}");
    let top = pitch.last().unwrap().value;
    assert!((top - 2.0).abs() < 0.001, "the bend reached {top}");
    let bright = timbre.last().unwrap().value;
    assert!((bright - 0.8).abs() < 0.001, "brightness {bright}");
    // It plays rising: the curve is monotonic.
    assert!(pitch.windows(2).all(|w| w[1].value >= w[0].value));
}

/// A MIDI track playing a MIDI 2.0 device: the notes go out as MIDI 2.0
/// and per-note expression as per-note controllers.
#[test]
fn per_note_expression_reaches_a_midi2_device() {
    use faderframe_audio::dummy::DummyBackend;
    use faderframe_midi::ump::packets;
    use faderframe_project::{Command, MidiOutputRouting, Project, TrackKind};
    use faderframe_session::AudioPreferences;
    let mut s = Session::new(Project::new("M2", 48_000), None, EngineConfig::default()).unwrap();
    let captured = s.add_virtual_ump_output("Synth 2");
    let t = s.add_track(TrackKind::Midi).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMidiOutput {
        track: t,
        output: Some(MidiOutputRouting {
            port: "virtual:Synth 2".into(),
            channel: None,
        }),
    }))
    .unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: vec![t],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    run(&mut s, 100);
    send(
        &s,
        Voice2::NoteOn {
            note: 60,
            velocity: 0xFFFF,
            attribute: Attribute::default(),
        },
    );
    send(
        &s,
        Voice2::PerNotePitchBend {
            note: 60,
            value: of_bend(0.5 / 48.0),
        },
    );
    run(&mut s, 200);
    let got: Vec<Message> = captured
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, w)| packets(w).map(|p| Message::parse(&p)).collect::<Vec<_>>())
        .collect();
    assert!(
        got.iter().any(|m| matches!(
            m,
            Message::Midi2 {
                voice: Voice2::NoteOn {
                    note: 60,
                    velocity: 0xFFFF,
                    ..
                },
                ..
            }
        )),
        "the note went out as MIDI 2.0: {got:?}"
    );
    let bend = got.iter().find_map(|m| match m {
        Message::Midi2 {
            voice: Voice2::PerNotePitchBend { note: 60, value },
            ..
        } => Some(faderframe_midi::ump::bend_of(*value) * 48.0),
        _ => None,
    });
    let bend = bend.expect("the per-note bend went out");
    assert!((bend - 0.5).abs() < 1e-4, "{bend}");
    s.stop_audio();
}

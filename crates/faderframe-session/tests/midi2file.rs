//! MIDI 2.0 clip files: a clip written and read back keeps its notes (with
//! release velocities), per-note expression, controllers, program and bank,
//! SysEx, name, and the project's tempo, meter and key.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_project::{
    ClipContent, Command, ControllerLane, ControllerPoint, ExpressionKind, ExpressionPoint,
    MidiController, NoteExpression, Project, SysexEvent,
};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

#[test]
fn a_clip_goes_through_a_midi2_file_and_back() {
    let mut demo = Session::demo(EngineConfig::default()).unwrap();
    let (id, mut midi) = {
        let c = demo
            .project()
            .clips
            .values()
            .find(|c| c.name == "Melody")
            .unwrap();
        (c.id, c.as_midi().unwrap().clone())
    };
    // Expression on the first note, a release velocity on the second.
    let first = midi.notes[0];
    midi.notes[1].release = Some(90);
    let mut e = NoteExpression::new(first.id);
    e.curve_mut(ExpressionKind::Pitch).extend([
        ExpressionPoint {
            time: MusicalTime::ZERO,
            value: 0.0,
        },
        ExpressionPoint {
            time: first.length,
            value: 1.0,
        },
    ]);
    e.curve_mut(ExpressionKind::Pan).push(ExpressionPoint {
        time: MusicalTime::ZERO,
        value: -0.5,
    });
    midi.expressions = vec![e];
    // Controllers: a mod wheel, a bend, a program with its bank.
    let lane = |c: MidiController, points: &[(f64, u16)]| {
        let mut l = ControllerLane::new(c, 0);
        l.points = points
            .iter()
            .map(|&(t, value)| ControllerPoint { time: q(t), value })
            .collect();
        l
    };
    midi.controllers = vec![
        lane(MidiController::MOD_WHEEL, &[(0.0, 10), (1.0, 100)]),
        lane(MidiController::PitchBend, &[(0.5, 0x3000)]),
        lane(MidiController::Cc { number: 0 }, &[(0.0, 2)]),
        lane(MidiController::Cc { number: 32 }, &[(0.0, 5)]),
        lane(MidiController::Program, &[(0.0, 41)]),
    ];
    midi.sysex = vec![SysexEvent {
        time: q(2.0),
        data: vec![0xF0, 0x43, 0x10, 0x4C, 0x00, 0x00, 0x7E, 0x00, 0xF7],
    }];
    let start = demo.project().clip(id).unwrap().start;
    demo.dispatch(Action::Edit(Command::SetClipContent {
        clip: id,
        start,
        content: Box::new(ClipContent::Midi(midi.clone())),
    }))
    .unwrap();
    let path = std::env::temp_dir().join(format!("ff-midi2-{}.midi2", std::process::id()));
    demo.export_midi2_clip(&path, id).unwrap();

    let mut s = Session::new(Project::new("Empty", 48_000), None, EngineConfig::default()).unwrap();
    let created = s.import_midi_file(&path, MusicalTime::ZERO, true).unwrap();
    assert_eq!(created.len(), 1);
    let p = s.project();
    let clip = p.clips_of(created[0])[0];
    assert_eq!(clip.name, "Melody");
    let back = clip.as_midi().unwrap();
    // The notes, to the tick (files hold 960 a quarter; the demo's notes
    // sit on that grid).
    assert_eq!(back.notes.len(), midi.notes.len());
    for (a, b) in midi.notes.iter().zip(&back.notes) {
        assert_eq!(
            (a.start, a.length, a.key, a.velocity),
            (b.start, b.length, b.key, b.velocity)
        );
        assert_eq!(a.channel, b.channel);
    }
    assert_eq!(back.notes[1].release, Some(90));
    // The expression of the first note.
    let e = back
        .expressions
        .iter()
        .find(|e| e.note == back.notes[0].id)
        .unwrap();
    let rise = e.value_at(ExpressionKind::Pitch, first.length / 2);
    assert!((rise - 0.5).abs() < 0.01, "{rise}");
    assert!((e.value_at(ExpressionKind::Pan, MusicalTime::ZERO) + 0.5).abs() < 0.001);
    // Controllers, program, bank.
    let wheel = back.lane(MidiController::MOD_WHEEL, 0).unwrap();
    assert_eq!(
        wheel.points.iter().map(|p| p.value).collect::<Vec<_>>(),
        [10, 100]
    );
    assert_eq!(
        back.lane(MidiController::PitchBend, 0).unwrap().points[0].value,
        0x3000
    );
    assert_eq!(
        back.lane(MidiController::Program, 0).unwrap().points[0].value,
        41
    );
    assert_eq!(
        back.lane(MidiController::Cc { number: 0 }, 0)
            .unwrap()
            .points[0]
            .value,
        2
    );
    assert_eq!(
        back.lane(MidiController::Cc { number: 32 }, 0)
            .unwrap()
            .points[0]
            .value,
        5
    );
    assert_eq!(back.sysex, midi.sysex);
    // The project's tempo, meter and key came along.
    let tl = &p.timeline;
    let demo_tl = &demo.project().timeline;
    assert!(
        (tl.tempo.bpm_at(MusicalTime::ZERO) - demo_tl.tempo.bpm_at(MusicalTime::ZERO)).abs() < 1e-6
    );
    assert_eq!(
        tl.meter.signature_at(MusicalTime::ZERO),
        demo_tl.meter.signature_at(MusicalTime::ZERO)
    );
    let demo_clip_start = demo.project().clip(id).unwrap().start;
    if let Some(k) = demo.project().key_at(demo_clip_start) {
        assert_eq!(p.key_at(MusicalTime::ZERO), Some(k));
    }
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().clips.is_empty());
}

#![allow(clippy::unwrap_used)]
//! MPE: notes of an MPE track get member channels of their own and play
//! their expression as pitch bend, pressure and CC 74.

mod common;

use common::TestProject;
use faderframe_core::{ChannelLayout, NoteId, TrackId};
use faderframe_engine::TimelineSnapshot;
use faderframe_midi::MidiEvent;
use faderframe_project::{
    Clip, ClipContent, ExpressionPoint, MidiClip, MidiNote, MpeConfig, NoteExpression, TrackColor,
    TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn note(id: u64, start: f64, length: f64, key: u8) -> MidiNote {
    MidiNote {
        id: NoteId(id),
        start: MusicalTime::from_quarters(start),
        length: MusicalTime::from_quarters(length),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
    }
}

fn project(mpe: Option<MpeConfig>) -> (TestProject, TrackId) {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "MPE", ChannelLayout::Stereo);
    tp.project.track_mut(t).unwrap().mpe = mpe;
    // A two-note chord, then a third note after both ended; the first note
    // glides up two semitones over its quarter note.
    let mut glide = NoteExpression::new(NoteId(1));
    glide.pitch = vec![
        ExpressionPoint {
            time: MusicalTime::ZERO,
            value: 0.0,
        },
        ExpressionPoint {
            time: MusicalTime::from_quarters(1.0),
            value: 2.0,
        },
    ];
    glide.pressure = vec![ExpressionPoint {
        time: MusicalTime::ZERO,
        value: 0.5,
    }];
    let clip = Clip {
        id: tp.project.ids.allocate(),
        track: t,
        name: "mpe".into(),
        color: Some(TrackColor::palette(2)),
        start: MusicalTime::ZERO,
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: MusicalTime::from_quarters(4.0),
            notes: vec![
                note(1, 0.0, 1.0, 60),
                note(2, 0.0, 1.0, 64),
                note(3, 2.0, 1.0, 67),
            ],
            controllers: Vec::new(),
            expressions: vec![glide],
            sysex: Vec::new(),
        }),
    };
    tp.project.track_mut(t).unwrap().clips.push(clip.id);
    tp.project.clips.insert(clip.id, clip);
    (tp, t)
}

fn events(tp: &TestProject, t: TrackId) -> Vec<(i64, MidiEvent)> {
    let snap = TimelineSnapshot::build(&tp.project, &tp.sources, SR);
    snap.lane(t).unwrap().midi[0].events.clone()
}

#[test]
fn notes_get_member_channels_and_play_their_expression() {
    let (tp, t) = project(Some(MpeConfig::default()));
    let ev = events(&tp, t);
    let ons: Vec<(i64, u8, u8)> = ev
        .iter()
        .filter_map(|&(time, e)| match e {
            MidiEvent::NoteOn { channel, key, .. } => Some((time, channel, key)),
            _ => None,
        })
        .collect();
    assert_eq!(ons.len(), 3);
    // Chord notes on different member channels (never the master channel).
    assert_ne!(ons[0].1, ons[1].1);
    assert!(ons.iter().all(|&(_, ch, _)| (1..=15).contains(&ch)));
    // The later note takes the least recently used free channel.
    assert!(ons[2].1 != ons[0].1 && ons[2].1 != ons[1].1);
    let ch = ons.iter().find(|o| o.2 == 60).unwrap().1;
    // Initial values come before the note-on, at the same time.
    let first_on = ev
        .iter()
        .position(|&(_, e)| matches!(e, MidiEvent::NoteOn { key: 60, .. }))
        .unwrap();
    assert!(ev[..first_on].iter().any(|&(_, e)| e
        == MidiEvent::ChannelPressure {
            channel: ch,
            pressure: 64
        }));
    // The glide: pitch bends rising on the note's channel to 2 semitones
    // of ±48 (8192 + 2/48 · 8192 ≈ 8533).
    let bends: Vec<u16> = ev
        .iter()
        .filter_map(|&(_, e)| match e {
            MidiEvent::PitchBend { channel, value } if channel == ch => Some(value),
            _ => None,
        })
        .collect();
    assert!(bends.len() > 50, "a smooth glide: {}", bends.len());
    assert!(bends.windows(2).all(|w| w[1] >= w[0]));
    assert!(
        (*bends.last().unwrap() as i32 - 8533).abs() <= 6,
        "{bends:?}"
    );
    // The other chord note does not bend.
    let other = ons.iter().find(|o| o.2 == 64).unwrap().1;
    assert!(ev.iter().all(|&(_, e)| !matches!(e,
        MidiEvent::PitchBend { channel, value } if channel == other && value != 8192)));
    // Without MPE: notes keep their channel, expression is not played.
    let (tp, t) = project(None);
    let ev = events(&tp, t);
    assert!(ev.iter().all(|&(_, e)| e.channel() == 0));
    assert!(
        !ev.iter()
            .any(|&(_, e)| matches!(e, MidiEvent::PitchBend { .. }))
    );
    assert!(
        ev.iter()
            .filter(|(_, e)| matches!(e, MidiEvent::NoteOn { .. }))
            .count()
            == 3
    );
}

#[test]
fn without_mpe_expression_goes_to_plugins_as_note_expressions() {
    use faderframe_midi::NoteExpressionKind;
    let (mut tp, t) = project(None);
    // A volume curve too (not something MPE can carry).
    if let Some(c) = tp.project.clips.values_mut().next()
        && let ClipContent::Midi(m) = &mut c.content
    {
        m.expressions[0].volume = vec![ExpressionPoint {
            time: MusicalTime::ZERO,
            value: -6.0,
        }];
    }
    let ev = events(&tp, t);
    let on = ev
        .iter()
        .position(|&(_, e)| matches!(e, MidiEvent::NoteOn { key: 60, .. }))
        .unwrap();
    let exprs: Vec<(i64, NoteExpressionKind, f64)> = ev
        .iter()
        .filter_map(|&(time, e)| match e {
            MidiEvent::NoteExpression {
                channel: 0,
                key: 60,
                kind,
                value,
            } => Some((time, kind, value.get())),
            MidiEvent::NoteExpression { key, .. } => panic!("only note 60 has expression: {key}"),
            _ => None,
        })
        .collect();
    // Starting values right after the note-on, at its time.
    let first = ev
        .iter()
        .position(|&(_, e)| matches!(e, MidiEvent::NoteExpression { .. }))
        .unwrap();
    assert!(first > on);
    assert_eq!(ev[first].0, ev[on].0);
    let at_start = |k| exprs.iter().find(|e| e.1 == k).map(|e| (e.0, e.2)).unwrap();
    assert_eq!(at_start(NoteExpressionKind::Pressure), (ev[on].0, 0.5));
    assert_eq!(at_start(NoteExpressionKind::Volume), (ev[on].0, -6.0));
    // The glide: tuning rising to two semitones.
    let tuning: Vec<f64> = exprs
        .iter()
        .filter(|e| e.1 == NoteExpressionKind::Tuning)
        .map(|e| e.2)
        .collect();
    assert!(tuning.len() > 50, "{}", tuning.len());
    assert!(tuning.windows(2).all(|w| w[1] > w[0]));
    assert!((tuning.last().unwrap() - 2.0).abs() < 0.03);
    // MPE tracks play pitch and pressure over MPE; volume has no MPE form.
    let (mut tp, t) = project(Some(MpeConfig::default()));
    if let Some(c) = tp.project.clips.values_mut().next()
        && let ClipContent::Midi(m) = &mut c.content
    {
        m.expressions[0].volume = vec![ExpressionPoint {
            time: MusicalTime::ZERO,
            value: -6.0,
        }];
    }
    let ev = events(&tp, t);
    assert!(
        !ev.iter()
            .any(|&(_, e)| matches!(e, MidiEvent::NoteExpression { .. }))
    );
}

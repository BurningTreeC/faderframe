//! The MIDI event list's operations on a clip: its events in time order,
//! each field edited, events added and removed, the clip put on another
//! channel (its notes and controller lanes) — each one undo step.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, MidiController};
use faderframe_session::midi_events::{EventField, EventKind, EventRef, EventValue, NewEvent};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

fn melody(s: &Session) -> faderframe_core::ClipId {
    s.project()
        .clips
        .values()
        .find(|c| c.name == "Melody" && matches!(c.content, ClipContent::Midi(_)))
        .unwrap()
        .id
}

#[test]
fn the_event_list_edits_a_clips_events() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let clip = melody(&s);
    let start = s.project().clip(clip).unwrap().start;
    let rows = s.midi_events(clip);
    assert!(!rows.is_empty());
    assert!(rows.windows(2).all(|w| w[0].at <= w[1].at), "in time order");
    let steps = |s: &Session| s.history_steps().0.len();

    // A mod wheel value at the clip's start, then its value and channel.
    let before = steps(&s);
    s.dispatch(Action::AddMidiEvent {
        clip,
        what: NewEvent::Controller(MidiController::MOD_WHEEL),
        at: start,
        channel: 0,
    })
    .unwrap();
    assert_eq!(steps(&s), before + 1);
    let cc = s
        .midi_events(clip)
        .into_iter()
        .find(|r| r.kind == EventKind::Controller(MidiController::MOD_WHEEL))
        .unwrap();
    assert_eq!(
        (cc.data1, cc.data2, cc.channel),
        (Some(1), Some(64), Some(0))
    );
    s.dispatch(Action::EditMidiEvent {
        clip,
        event: cc.event,
        field: EventField::Data2,
        value: EventValue::Number(100),
    })
    .unwrap();
    s.dispatch(Action::EditMidiEvent {
        clip,
        event: EventRef::Controller {
            controller: MidiController::MOD_WHEEL,
            channel: 0,
            time: MusicalTime::ZERO,
        },
        field: EventField::Channel,
        value: EventValue::Number(3),
    })
    .unwrap();
    let cc = s
        .midi_events(clip)
        .into_iter()
        .find(|r| r.kind == EventKind::Controller(MidiController::MOD_WHEEL))
        .unwrap();
    assert_eq!(
        (cc.data2, cc.channel),
        (Some(100), Some(3)),
        "moved to channel 4"
    );
    // Its controller number: now CC 11 (another lane).
    s.dispatch(Action::EditMidiEvent {
        clip,
        event: cc.event,
        field: EventField::Data1,
        value: EventValue::Number(11),
    })
    .unwrap();
    let m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
    assert!(
        m.lane(MidiController::MOD_WHEEL, 3).is_none(),
        "the emptied lane went"
    );
    assert_eq!(
        m.lane(MidiController::Cc { number: 11 }, 3).unwrap().points[0].value,
        100
    );

    // A note: key, velocity, position, length.
    let note = s
        .midi_events(clip)
        .into_iter()
        .find(|r| r.kind == EventKind::Note)
        .unwrap();
    for (field, value) in [
        (EventField::Data1, EventValue::Number(72)),
        (EventField::Data2, EventValue::Number(33)),
        (
            EventField::Position,
            EventValue::Time(start + MusicalTime::from_quarters(0.5)),
        ),
        (
            EventField::Length,
            EventValue::Time(MusicalTime::from_quarters(2.0)),
        ),
    ] {
        let n = steps(&s);
        s.dispatch(Action::EditMidiEvent {
            clip,
            event: note.event,
            field,
            value,
        })
        .unwrap();
        assert_eq!(steps(&s), n + 1, "{field:?}: one step");
    }
    let after = s
        .midi_events(clip)
        .into_iter()
        .find(|r| r.event == note.event)
        .unwrap();
    assert_eq!(after.data1, Some(72));
    assert_eq!(after.data2, Some(33));
    assert_eq!(after.at, start + MusicalTime::from_quarters(0.5));
    assert_eq!(after.length, Some(MusicalTime::from_quarters(2.0)));

    // The whole clip on channel 10: notes and controller lanes.
    s.dispatch(Action::SetMidiChannel {
        clips: vec![clip],
        channel: 9,
    })
    .unwrap();
    assert_eq!(s.midi_channel_of(&[clip]), Some(9));
    assert!(
        s.midi_events(clip)
            .iter()
            .all(|r| r.channel.is_none_or(|c| c == 9))
    );

    // Removing: the note and the CC.
    let count = s.midi_events(clip).len();
    let cc = s
        .midi_events(clip)
        .into_iter()
        .find(|r| matches!(r.kind, EventKind::Controller(_)))
        .unwrap()
        .event;
    s.dispatch(Action::RemoveMidiEvents {
        clip,
        events: vec![note.event, cc],
    })
    .unwrap();
    assert_eq!(s.midi_events(clip).len(), count - 2);
    // Every step undoes (ten: the add, three controller edits, four note
    // edits, the channel, the removal).
    for _ in 0..10 {
        s.dispatch(Action::Undo).unwrap();
    }
    let fresh = Session::demo(EngineConfig::default()).unwrap();
    assert_eq!(
        s.project().clip(clip).unwrap().content,
        fresh.project().clip(melody(&fresh)).unwrap().content
    );
}

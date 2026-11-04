//! Material placed in time on its own, on musical tracks: locked notes
//! keep their samples through tempo changes while their clip and the other
//! notes follow the beats (one undo step with the change, undone exactly);
//! the piano roll's and the event list's locks; Quantize leaves locked
//! notes where they are; locked markers and sections keep their samples,
//! ripple in time and travel with a moved section.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ClipId, MarkerId, NoteId, SectionId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::midi_ops::QuantizeSettings;
use faderframe_project::{
    Clip, ClipContent, Command, MidiClip, MidiNote, Project, Section, TimeBase, TrackColor,
    TrackKind,
};
use faderframe_session::midi_events::{EventField, EventRef, EventValue};
use faderframe_session::{
    Action, EditRange, NoteOp, RippleOp, RippleScope, RippleSettings, SelectMode, Session,
};
use faderframe_timeline::{GridDivision, MusicalTime, TempoCurve, TempoPoint};

const SR: u32 = 48_000;

fn bar(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(4.0 * n)
}

fn sample(s: &Session, t: MusicalTime) -> i64 {
    s.project().timeline.to_samples(t, f64::from(SR))
}

fn session() -> Session {
    Session::new(Project::new("Hits", SR), None, EngineConfig::default()).unwrap()
}

fn note(id: u64, start: f64, length: f64, key: u8) -> MidiNote {
    MidiNote {
        articulation: None,
        id: NoteId(id),
        start: MusicalTime::from_quarters(start),
        length: MusicalTime::from_quarters(length),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: false,
        fine: None,
    }
}

/// A musical instrument track with a two-bar cue at bar 3: notes a hair
/// off the grid at 0.07, 1.03 and 5.11 quarters into it.
fn cue(s: &mut Session) -> (TrackId, ClipId) {
    let t = s.add_track(TrackKind::Instrument).unwrap();
    let id = ClipId(70_001);
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track: t,
            name: "cue".into(),
            color: None,
            start: bar(3.0),
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: bar(2.0),
                notes: vec![
                    note(90_001, 0.07, 0.5, 60),
                    note(90_002, 1.03, 0.5, 64),
                    note(90_003, 5.11, 1.0, 67),
                ],
                ..MidiClip::default()
            }),
        }),
    })
    .unwrap();
    (t, id)
}

fn notes(s: &Session, c: ClipId) -> Vec<MidiNote> {
    s.project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .clone()
}

/// Where a note sounds (start, end samples).
fn heard(s: &Session, c: ClipId, n: NoteId) -> (i64, i64) {
    let clip = s.project().clip(c).unwrap();
    let note = clip.as_midi().unwrap().note(n).unwrap();
    (
        sample(s, clip.start + note.start),
        sample(s, clip.start + note.end()),
    )
}

/// A marker added as the marker lane adds one; its id.
fn add_marker(s: &mut Session, at: MusicalTime) -> MarkerId {
    let before: Vec<MarkerId> = s.project().markers.iter().map(|m| m.id).collect();
    s.dispatch(Action::AddMarker(at)).unwrap();
    s.project()
        .markers
        .iter()
        .map(|m| m.id)
        .find(|id| !before.contains(id))
        .unwrap()
}

fn steps(s: &Session) -> usize {
    s.history_steps().0.len()
}

#[test]
fn a_locked_note_keeps_its_samples_through_tempo_changes() {
    let mut s = session();
    let (t, c) = cue(&mut s);
    // The piano roll's lock on the second note (as the dev action finds
    // it: the track's notes in time order).
    let (clip, hit) = s.track_note(t, 1).unwrap();
    assert_eq!((clip, hit), (c, NoteId(90_002)));
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![hit],
        op: NoteOp::ToggleTimeLocked,
    })
    .unwrap();
    assert!(notes(&s, c).iter().any(|n| n.id == hit && n.time_locked));
    assert_eq!(s.time_locked_notes(t), 1);
    let at = heard(&s, c, hit);
    let plain = heard(&s, c, NoteId(90_001));
    let project = s.project().clone();
    let n = steps(&s);
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 150.0 }))
        .unwrap();
    assert_eq!(steps(&s), n + 1, "one undo step");
    assert_eq!(heard(&s, c, hit), at, "the hit stays where it sounds");
    assert_ne!(heard(&s, c, NoteId(90_001)), plain, "the others move");
    assert_eq!(s.project().clip(c).unwrap().start, bar(3.0), "on its bar");
    // Slower: the clip starts later than the hit sounds; it grows back to
    // hold it, the rest keeping its place in the music.
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 87.0 }))
        .unwrap();
    assert_eq!(steps(&s), n + 2);
    assert_eq!(heard(&s, c, hit), at);
    let clip = s.project().clip(c).unwrap().clone();
    assert!(clip.start < bar(3.0), "grown back");
    let m = clip.as_midi().unwrap();
    assert_eq!(m.note(hit).unwrap().start, MusicalTime::ZERO);
    let other = m.note(NoteId(90_001)).unwrap();
    assert_eq!(
        clip.start + other.start,
        bar(3.0) + MusicalTime::from_quarters(0.07)
    );
    assert_eq!(clip.start + m.length, bar(5.0), "its end kept");
    // A tempo-lane drag: many maps in one gesture, no drift.
    s.dispatch(Action::AddTempoPoint(bar(2.0))).unwrap();
    s.dispatch(Action::BeginGesture("Tempo".into())).unwrap();
    for (i, bpm) in [100.0, 87.5, 141.25, 66.6, 93.0].into_iter().enumerate() {
        s.dispatch(Action::SetTempoPoint {
            index: 1,
            position: bar(1.0 + 0.25 * i as f64),
            bpm,
        })
        .unwrap();
        assert_eq!(heard(&s, c, hit), at, "{bpm}");
    }
    s.dispatch(Action::EndGesture).unwrap();
    // Undone to the very tick.
    for _ in 0..4 {
        s.dispatch(Action::Undo).unwrap();
    }
    assert_eq!(s.project().clips, project.clips);
    assert_eq!(s.project().timeline, project.timeline);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(heard(&s, c, hit), at);
    // Unlocked from the inspector's menu: it follows the music again.
    let unlock = s.unlock_track_notes(t).unwrap();
    s.dispatch(unlock).unwrap();
    assert_eq!(s.time_locked_notes(t), 0);
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 120.0 }))
        .unwrap();
    assert_ne!(heard(&s, c, hit), at);
    assert!(s.unlock_track_notes(t).is_none());
}

#[test]
fn quantize_leaves_locked_notes_where_they_are() {
    let mut s = session();
    let (_, c) = cue(&mut s);
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![NoteId(90_002)],
        op: NoteOp::SetTimeLocked(true),
    })
    .unwrap();
    let locked = heard(&s, c, NoteId(90_002));
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: Vec::new(),
        op: NoteOp::Quantize(QuantizeSettings {
            grid: GridDivision::Note(4),
            ..QuantizeSettings::default()
        }),
    })
    .unwrap();
    let after = notes(&s, c);
    assert_eq!(
        after[0].start,
        MusicalTime::ZERO,
        "the plain notes go to the grid"
    );
    assert_eq!(heard(&s, c, NoteId(90_002)), locked, "the locked one stays");
    // Moved on purpose it goes where it is put (onto a whole sample).
    s.dispatch(Action::NoteOperation {
        clip: c,
        notes: vec![NoteId(90_002)],
        op: NoteOp::Move {
            by: MusicalTime::from_quarters(0.5),
            keys: 0,
        },
    })
    .unwrap();
    let moved = heard(&s, c, NoteId(90_002));
    assert_ne!(moved, locked);
    let clip = s.project().clip(c).unwrap().clone();
    let n = clip.as_midi().unwrap().note(NoteId(90_002)).unwrap();
    let tl = &s.project().timeline;
    let rate = f64::from(SR);
    assert_eq!(tl.at_sample(moved.0, rate), clip.start + n.start);
}

#[test]
fn the_event_list_locks_a_note() {
    let mut s = session();
    let (_, c) = cue(&mut s);
    let row = |s: &Session| {
        s.midi_events(c)
            .into_iter()
            .find(|r| r.event == EventRef::Note(NoteId(90_003)))
            .unwrap()
    };
    assert!(!row(&s).time_locked);
    s.dispatch(Action::EditMidiEvents {
        clip: c,
        events: vec![EventRef::Note(NoteId(90_003))],
        field: EventField::TimeLocked,
        value: EventValue::Number(1),
    })
    .unwrap();
    assert!(row(&s).time_locked);
    let at = heard(&s, c, NoteId(90_003));
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 150.0 }))
        .unwrap();
    assert_eq!(heard(&s, c, NoteId(90_003)), at);
    s.dispatch(Action::EditMidiEvents {
        clip: c,
        events: vec![EventRef::Note(NoteId(90_003))],
        field: EventField::TimeLocked,
        value: EventValue::Number(0),
    })
    .unwrap();
    assert!(!row(&s).time_locked);
}

/// Markers and sections locked from their menus keep their samples; a
/// ripple of every track moves them by the time it takes out; a section
/// move carries a locked marker with its section.
#[test]
fn locked_markers_and_sections_keep_their_samples() {
    let mut s = session();
    let audio = s.add_track(TrackKind::Audio).unwrap();
    let hit = add_marker(&mut s, bar(6.3));
    let plain = add_marker(&mut s, bar(6.3));
    let lock = s.marker_time_lock(hit, true).unwrap();
    s.dispatch(lock).unwrap();
    assert!(s.marker_time_lock(hit, true).is_none(), "locked already");
    let pos = |s: &Session, id: MarkerId| {
        s.project()
            .markers
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.position)
            .unwrap()
    };
    let at = sample(&s, pos(&s, hit));
    let n = steps(&s);
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 95.0 }))
        .unwrap();
    assert_eq!(steps(&s), n + 1);
    assert_eq!(sample(&s, pos(&s, hit)), at, "the locked marker stays");
    assert_eq!(pos(&s, plain), bar(6.3), "the other keeps its beat");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(sample(&s, pos(&s, hit)), at);
    assert_eq!(s.project().timeline.tempo.points()[0].bpm, 120.0);
    // A ripple of every track (the tempo along): by the time taken out.
    s.dispatch(Action::SelectTracks {
        tracks: vec![audio],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Ripple(RippleOp::Settings(RippleSettings {
        scope: RippleScope::All,
        ..s.ripple_settings()
    })))
    .unwrap();
    s.dispatch(Action::SetEditRange(Some(EditRange::new(
        bar(1.0),
        bar(2.0),
    ))))
    .unwrap();
    let taken = sample(&s, bar(2.0)) - sample(&s, bar(1.0));
    s.dispatch(Action::Ripple(RippleOp::DeleteRange(RippleScope::All)))
        .unwrap();
    assert_eq!(sample(&s, pos(&s, hit)), at - taken);
    assert_eq!(pos(&s, plain), bar(5.3));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(sample(&s, pos(&s, hit)), at);
    // A locked section keeps both bounds through a tempo change.
    let scene = SectionId(880);
    s.edit(Command::AddSection {
        section: Section {
            id: scene,
            name: "Reel 1".into(),
            start: bar(4.0),
            end: bar(8.0),
            color: TrackColor::palette(2),
            time_base: TimeBase::TimeLocked,
        },
    })
    .unwrap();
    let bounds = |s: &Session| {
        let x = s.project().sections.iter().find(|x| x.id == scene).unwrap();
        (sample(s, x.start), sample(s, x.end))
    };
    let before = bounds(&s);
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 101.0 }))
        .unwrap();
    assert_eq!(bounds(&s), before);
    let unlock = s.section_time_lock(scene, false).unwrap();
    s.dispatch(unlock).unwrap();
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 120.0 }))
        .unwrap();
    assert_ne!(bounds(&s), before, "with the music again");
}

#[test]
fn a_section_move_carries_a_locked_marker_with_its_section() {
    let mut s = session();
    s.add_track(TrackKind::Audio).unwrap();
    let mut tl = s.project().timeline.clone();
    tl.tempo.set_point(TempoPoint {
        position: bar(4.0),
        bpm: 90.0,
        curve: TempoCurve::Constant,
    });
    s.edit(Command::SetTimeline {
        timeline: Box::new(tl),
    })
    .unwrap();
    for (i, (a, b)) in [(0.0, 4.0), (4.0, 8.0)].into_iter().enumerate() {
        s.edit(Command::AddSection {
            section: Section {
                id: SectionId(900 + i as u64),
                name: format!("S{i}"),
                start: bar(a),
                end: bar(b),
                color: TrackColor::palette(i),
                time_base: TimeBase::Musical,
            },
        })
        .unwrap();
    }
    let hit = add_marker(&mut s, bar(5.37));
    let lock = s.marker_time_lock(hit, true).unwrap();
    s.dispatch(lock).unwrap();
    let section_start = |s: &Session, id: u64| {
        let x = s
            .project()
            .sections
            .iter()
            .find(|x| x.id == SectionId(id))
            .unwrap();
        sample(s, x.start)
    };
    let marker = |s: &Session| {
        let m = s.project().markers.iter().find(|m| m.id == hit).unwrap();
        assert!(m.is_time_locked());
        sample(s, m.position)
    };
    let into = marker(&s) - section_start(&s, 901);
    s.dispatch(Action::MoveSection {
        section: SectionId(901),
        to: MusicalTime::ZERO,
        copy: false,
    })
    .unwrap();
    assert_eq!(section_start(&s, 901), 0);
    assert!((marker(&s) - into).abs() <= 1, "carried with its section");
}

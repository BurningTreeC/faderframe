//! Clip aliases share their content: an edit of one reaches the others.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ClipId, NoteId};
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, MidiNote};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

fn melody(s: &Session) -> ClipId {
    s.project()
        .clips
        .values()
        .find(|c| c.name == "Melody")
        .unwrap()
        .id
}

fn notes(s: &Session, clip: ClipId) -> Vec<(MusicalTime, u8)> {
    s.project()
        .clip(clip)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start, n.key))
        .collect()
}

#[test]
fn an_alias_follows_the_edits_of_its_original() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let a = melody(&s);
    s.dispatch(Action::DuplicateAsAlias(vec![a])).unwrap();
    let b = *s.selection.clips.iter().next().unwrap();
    assert_ne!(a, b);
    assert!(s.is_alias(a) && s.is_alias(b));
    let (ca, cb) = (s.project().clip(a).unwrap(), s.project().clip(b).unwrap());
    let rate = s.project().sample_rate;
    assert_eq!(
        cb.start,
        ca.end(&s.project().timeline, rate),
        "right after it"
    );
    // A note added to the alias reaches the original, one undo step.
    let note = MidiNote {
        id: NoteId(9_999_999),
        start: MusicalTime::from_quarters(1.5),
        length: MusicalTime::from_quarters(0.5),
        key: 90,
        velocity: 100,
        channel: 0,
        muted: false,
    };
    s.dispatch(Action::Edit(Command::AddNote { clip: b, note }))
        .unwrap();
    assert!(notes(&s, a).contains(&(MusicalTime::from_quarters(1.5), 90)));
    assert_eq!(notes(&s, a), notes(&s, b));
    s.dispatch(Action::Undo).unwrap();
    assert!(!notes(&s, a).iter().any(|n| n.1 == 90));
    assert!(!notes(&s, b).iter().any(|n| n.1 == 90));
    // A front trim of one moves the other's start as much.
    let (start_a, start_b) = (
        s.project().clip(a).unwrap().start,
        s.project().clip(b).unwrap().start,
    );
    let mut content = s.project().clip(a).unwrap().content.clone();
    let ClipContent::Midi(m) = &mut content else {
        panic!()
    };
    let cut = MusicalTime::from_quarters(1.0);
    m.length -= cut;
    m.notes.retain(|n| n.start >= cut);
    for n in &mut m.notes {
        n.start -= cut;
    }
    s.dispatch(Action::Edit(Command::SetClipContent {
        clip: a,
        start: start_a + cut,
        content: Box::new(content),
    }))
    .unwrap();
    assert_eq!(s.project().clip(b).unwrap().start, start_b + cut);
    assert_eq!(notes(&s, a), notes(&s, b));
    // Splitting one makes it its own; the other keeps its whole content.
    let before_b = notes(&s, b);
    let new_clip: ClipId = ClipId(9_999_998);
    s.dispatch(Action::Edit(Command::SplitClip {
        clip: a,
        at: s.project().clip(a).unwrap().start + MusicalTime::from_quarters(2.0),
        new_clip,
    }))
    .unwrap();
    assert!(!s.is_alias(a) && !s.is_alias(b));
    assert_eq!(notes(&s, b), before_b);
}

#[test]
fn unique_clips_part_ways_and_links_are_saved() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let a = melody(&s);
    s.dispatch(Action::DuplicateAsAlias(vec![a])).unwrap();
    let b = *s.selection.clips.iter().next().unwrap();
    // Saved and opened again: still aliases.
    let dir = std::env::temp_dir().join(format!("ff-aliases-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aliases.ffproj");
    s.save_as(&path).unwrap();
    let mut t = Session::demo(EngineConfig::default()).unwrap();
    t.open(&path).unwrap();
    assert!(t.is_alias(a) && t.is_alias(b));
    let _ = std::fs::remove_dir_all(&dir);
    // Made unique: an edit stays where it is made.
    s.dispatch(Action::MakeClipsUnique(vec![b])).unwrap();
    assert!(!s.is_alias(a));
    let first = s.project().clip(a).unwrap().as_midi().unwrap().notes[0].id;
    s.dispatch(Action::Edit(Command::RemoveNote {
        clip: a,
        note: first,
    }))
    .unwrap();
    assert_ne!(notes(&s, a), notes(&s, b));
}

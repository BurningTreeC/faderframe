//! Merge Clips: clips of one track become one — MIDI joined at once,
//! audio rendered as it plays into a new file — in one undo step.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_project::Clip;
use faderframe_session::{Action, SelectMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;

fn named(s: &Session, name: &str) -> Clip {
    s.project()
        .clips
        .values()
        .find(|c| c.name == name)
        .unwrap()
        .clone()
}

/// Split `clip` at `q` quarters into it; returns both halves.
fn split(s: &mut Session, clip: &Clip, q: f64) -> Vec<Clip> {
    s.dispatch(Action::SelectClips {
        clips: vec![clip.id],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Locate(
        clip.start + MusicalTime::from_quarters(q),
    )))
    .unwrap();
    s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
    let mut halves: Vec<Clip> = s
        .project()
        .clips_of(clip.track)
        .into_iter()
        .filter(|c| {
            c.start >= clip.start && c.start < clip.start + MusicalTime::from_quarters(64.0)
        })
        .cloned()
        .collect();
    halves.sort_by_key(|c| c.start);
    halves
}

#[test]
fn midi_clips_join_into_one() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let melody = named(&s, "Melody");
    let original = melody.as_midi().unwrap().clone();
    // (Where no note crosses: a split cuts those short.)
    let halves = split(&mut s, &melody, 3.99);
    assert_eq!(halves.len(), 2, "split in two");
    let count = s.project().clips_of(melody.track).len();
    s.dispatch(Action::MergeClips(halves.iter().map(|c| c.id).collect()))
        .unwrap();
    assert_eq!(s.project().clips_of(melody.track).len(), count - 1);
    let merged = s
        .project()
        .clips_of(melody.track)
        .into_iter()
        .find(|c| c.start == melody.start)
        .unwrap()
        .clone();
    let m = merged.as_midi().unwrap();
    assert_eq!(m.length, original.length);
    let shape = |notes: &[faderframe_project::MidiNote]| {
        let mut v: Vec<_> = notes
            .iter()
            .map(|n| (n.start, n.length, n.key, n.velocity))
            .collect();
        v.sort();
        v
    };
    assert_eq!(shape(&m.notes), shape(&original.notes), "the same notes");
    assert!(s.selection.clips.contains(&merged.id));
    // One step back: the two halves.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips_of(melody.track).len(), count);
}

#[test]
fn audio_clips_render_into_one() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let arp = named(&s, "Pluck Arp");
    let a = arp.as_audio().unwrap().clone();
    let halves = split(&mut s, &arp, 4.0);
    assert_eq!(halves.len(), 2);
    let count = s.project().clips_of(arp.track).len();
    s.dispatch(Action::MergeClips(halves.iter().map(|c| c.id).collect()))
        .unwrap();
    assert!(s.merging());
    s.wait_for_merges();
    let clips = s.project().clips_of(arp.track);
    assert_eq!(clips.len(), count - 1);
    let merged = clips
        .into_iter()
        .find(|c| c.start == arp.start)
        .unwrap()
        .clone();
    let m = merged.as_audio().unwrap();
    assert_ne!(m.source, a.source, "a file of its own");
    assert!(
        (m.length - a.length).abs() <= 1,
        "{} vs {}",
        m.length,
        a.length
    );
    // Away from the clip's edges (fades), the same audio.
    let at = a.length / 2;
    let gain = faderframe_core::gain::db_to_gain(a.gain_db);
    let before = s.source_frames(a.source, a.source_offset + at, 64).unwrap();
    let after = s.source_frames(m.source, at, 64).unwrap();
    for (x, y) in before[0].iter().zip(&after[0]) {
        assert!((x * gain - y).abs() < 1e-3, "{x} vs {y}");
    }
    assert!(before[0].iter().any(|x| x.abs() > 1e-3), "not silence");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().clips_of(arp.track).len(), count);
}

//! Capture MIDI: what the live tracks were played, recording or not,
//! becomes clips afterwards.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{MidiNote, TrackKind};
use faderframe_session::{Action, AudioPreferences, SelectMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// Tick the session for `millis` (the MIDI tick drains the input).
fn run(s: &mut Session, millis: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(millis) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

/// Play `key` for `millis` on the virtual keyboard.
fn play(s: &mut Session, key: u8, millis: u64) {
    s.midi_keyboard().send(&[0x90, key, 100]);
    run(s, millis);
    s.midi_keyboard().send(&[0x80, key, 0]);
    run(s, 30);
}

/// The clips on `track` now.
fn clip_ids(s: &Session, track: TrackId) -> Vec<faderframe_core::ClipId> {
    s.project().clips_of(track).iter().map(|c| c.id).collect()
}

/// The notes of the clips on `track` that are not in `before`, with their
/// clips' starts.
fn new_notes(
    s: &Session,
    track: TrackId,
    before: &[faderframe_core::ClipId],
) -> Vec<(MusicalTime, MidiNote)> {
    let p = s.project();
    let clips: Vec<_> = p
        .clips_of(track)
        .into_iter()
        .filter(|c| !before.contains(&c.id))
        .collect();
    assert!(!clips.is_empty(), "a clip was added");
    clips
        .iter()
        .filter_map(|c| c.as_midi().map(|m| (c.start, m)))
        .flat_map(|(start, m)| m.notes.iter().map(move |n| (start, *n)))
        .collect()
}

#[test]
fn a_phrase_played_while_stopped_starts_on_the_bar() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    run(&mut s, 30);
    assert!(!s.can_capture_midi());
    // The playhead in bar 3, beat 2: the phrase starts on bar 3.
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(9.0),
    )))
    .unwrap();
    run(&mut s, 30);
    play(&mut s, 60, 150);
    run(&mut s, 150);
    play(&mut s, 64, 150);
    assert!(s.can_capture_midi());
    let before = clip_ids(&s, lead);
    s.dispatch(Action::CaptureMidi).unwrap();
    let notes = new_notes(&s, lead, &before);
    assert_eq!(
        notes.iter().map(|(_, n)| n.key).collect::<Vec<_>>(),
        [60, 64]
    );
    let tempo = s.project().timeline.tempo.bpm_at(MusicalTime::ZERO);
    let quarters = |t: MusicalTime| t.quarters();
    let (start, first) = &notes[0];
    // On bar 3 (to the sample: never before it, which would start the clip
    // a bar early).
    let on = quarters(*start + first.start) - 8.0;
    assert!((0.0..0.001).contains(&on), "{on}");
    assert_eq!(quarters(*start), 8.0, "the clip starts on bar 3");
    // About 0.33 s apart (150 + 30 + 150 ms) and 0.15 s long, as played.
    let seconds = |q: f64| q * 60.0 / tempo;
    let gap = seconds(quarters(notes[1].1.start - first.start));
    assert!((gap - 0.33).abs() < 0.06, "{gap}");
    let len = seconds(quarters(first.length));
    assert!((len - 0.15).abs() < 0.06, "{len}");
    // One undo step; captured, it is not offered again.
    assert!(!s.can_capture_midi());
    s.dispatch(Action::CaptureMidi).unwrap();
    assert_eq!(s.project().clips_of(lead).len(), before.len() + 1);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(clip_ids(&s, lead), before);
}

#[test]
fn notes_played_while_playing_land_where_they_were_heard() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let midi = {
        let lead = track(&s, "Lead Synth");
        s.selection.select_tracks(&[lead], SelectMode::Replace);
        let midi = s.add_track(TrackKind::Midi).unwrap();
        s.selection.select_tracks(&[midi], SelectMode::Replace);
        midi
    };
    run(&mut s, 50);
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(16.0),
    )))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 300);
    let at = s.playhead();
    play(&mut s, 67, 200);
    run(&mut s, 100);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, 50);
    // Captured after stopping: the notes of the run that played.
    s.dispatch(Action::CaptureMidi).unwrap();
    let notes = new_notes(&s, midi, &[]);
    assert_eq!(notes.len(), 1);
    let (start, n) = &notes[0];
    let tempo = s.project().timeline.tempo.bpm_at(MusicalTime::ZERO);
    let played = (*start + n.start).quarters();
    // Where the playhead was when the key went down (within the latency
    // and the tick).
    let off = (played - at.quarters()) * 60.0 / tempo;
    assert!((-0.1..0.15).contains(&off), "{off}");
    let len = n.length.quarters() * 60.0 / tempo;
    assert!((len - 0.2).abs() < 0.08, "{len}");
}

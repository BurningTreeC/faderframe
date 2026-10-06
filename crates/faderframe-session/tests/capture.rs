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

/// Play `key` for `millis` on the virtual keyboard; returns when it went
/// down and how long it was held in seconds (the keyboard stamps events
/// as they are sent, and a busy machine oversleeps).
fn play(s: &mut Session, key: u8, millis: u64) -> (Instant, f64) {
    let down = Instant::now();
    s.midi_keyboard().send(&[0x90, key, 100]);
    run(s, millis);
    let held = down.elapsed().as_secs_f64();
    s.midi_keyboard().send(&[0x80, key, 0]);
    run(s, 30);
    (down, held)
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
    let (first_down, held) = play(&mut s, 60, 150);
    run(&mut s, 150);
    let (second_down, _) = play(&mut s, 64, 150);
    let apart = (second_down - first_down).as_secs_f64();
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
    // As far apart (about 150 + 30 + 150 ms) and as long (about 0.15 s)
    // as played.
    let seconds = |q: f64| q * 60.0 / tempo;
    let gap = seconds(quarters(notes[1].1.start - first.start));
    assert!((gap - apart).abs() < 0.02, "{gap} for {apart}");
    let len = seconds(quarters(first.length));
    assert!((len - held).abs() < 0.02, "{len} for {held}");
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
    // Where the engine is as the key goes down (extrapolated from its last
    // callback, as captured events are: a busy machine's dummy device
    // falls behind, and the playhead it last reported with it).
    let clock = s.midi_keyboard().clock();
    let at = s.engine().position_at(clock.now_ns()).unwrap();
    s.midi_keyboard().send(&[0x90, 67, 100]);
    run(&mut s, 200);
    // The same for the key coming up.
    let up = s.engine().position_at(clock.now_ns()).unwrap();
    s.midi_keyboard().send(&[0x80, 67, 0]);
    let held = (up - at) as f64 / f64::from(s.engine().sample_rate());
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
    let at = s.engine().samples_to_musical(s.project(), at).quarters();
    // Where the playhead was when the key went down, less the latency
    // (what was heard then).
    let off = (played - at) * 60.0 / tempo;
    assert!((-0.1..0.01).contains(&off), "{off}");
    // As long as it was held.
    let len = n.length.quarters() * 60.0 / tempo;
    assert!((len - held).abs() < 0.01, "{len} for {held}");
}

#![allow(clippy::unwrap_used)]
//! Aborting a recording (Ctrl+Space) and aborting and restarting it
//! (Ctrl+Shift+Space), with the dummy device's test tone in real time:
//! nothing of the pass reaches the project, the history or the media
//! folder, earlier takes stay, a restart records a new take from the same
//! start with the same pre-roll, and a project closed while recording
//! gets nothing of it.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, InputRouting, MusicalRange, Project, TrackKind};
use faderframe_session::{Action, AudioPreferences, Roll, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn session() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Abort", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Hardware { first_channel: 0 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(1_000.0))],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    (s, t)
}

fn secs(s: &Session, seconds: f64) -> MusicalTime {
    s.engine()
        .samples_to_musical(s.project(), (seconds * SR as f64) as i64)
}

fn transport(s: &mut Session, a: TransportAction) {
    s.dispatch(Action::Transport(a)).unwrap();
}

fn wait_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

fn wait_position(s: &mut Session, seconds: f64) {
    let target = (seconds * SR as f64) as i64;
    wait_until(
        s,
        |s| s.transport().playing && s.transport().position >= target,
        "the position",
    );
}

fn settle(s: &mut Session, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        s.tick(0.005);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The files in the media folder.
fn media(s: &Session) -> BTreeSet<PathBuf> {
    std::fs::read_dir(s.media_dir())
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// A take recorded from `from` until `until` seconds, stopped and placed.
fn record(s: &mut Session, from: f64, until: f64) {
    let at = secs(s, from);
    transport(s, TransportAction::Locate(at));
    settle(s, Duration::from_millis(50));
    transport(s, TransportAction::ToggleRecord);
    wait_position(s, until);
    transport(s, TransportAction::Stop);
    s.wait_for_recordings();
}

#[test]
fn an_aborted_take_leaves_the_project_and_the_media_folder_as_they_were() {
    let (mut s, t) = session();
    // An earlier take, kept.
    record(&mut s, 0.0, 0.3);
    assert_eq!(s.project().clips_of(t).len(), 1);
    let project = s.project().clone();
    let undo = s.history_steps();
    let files = media(&s);
    assert!(
        files
            .iter()
            .any(|f| f.extension().is_some_and(|e| e == "wav"))
    );

    // Recording from 1 s, well under way (its file is being written) …
    let from = secs(&s, 1.0);
    transport(&mut s, TransportAction::Locate(from));
    settle(&mut s, Duration::from_millis(50));
    transport(&mut s, TransportAction::ToggleRecord);
    wait_position(&mut s, 1.4);
    assert!(s.recording_view().is_some());
    assert!(media(&s).len() > files.len(), "the take is being written");
    // … thrown away.
    transport(&mut s, TransportAction::AbortRecording);
    assert!(s.recording_view().is_none());
    s.wait_for_discarded_recordings();
    settle(&mut s, Duration::from_millis(200));
    s.wait_for_recordings();
    assert!(!s.transport().playing, "playback stopped");
    assert!(!s.transport().recording);
    assert_eq!(
        s.processing_position(),
        48_000,
        "back where the recording began"
    );
    assert_eq!(s.project(), &project, "the project as before the pass");
    assert_eq!(s.history_steps(), undo, "nothing to undo");
    assert_eq!(media(&s), files, "the media folder as before the pass");

    // Not recording: Abort stops, Abort and Restart does nothing.
    transport(&mut s, TransportAction::Play);
    wait_until(&mut s, |s| s.transport().playing, "playing");
    transport(&mut s, TransportAction::AbortAndRestartRecording);
    settle(&mut s, Duration::from_millis(100));
    assert!(s.transport().playing && s.recording_view().is_none());
    transport(&mut s, TransportAction::AbortRecording);
    settle(&mut s, Duration::from_millis(100));
    assert!(!s.transport().playing);
    assert_eq!(s.project(), &project);
}

#[test]
fn abort_and_restart_records_a_new_take_from_the_same_start_with_its_pre_roll() {
    let (mut s, t) = session();
    record(&mut s, 0.0, 0.3);
    let earlier = s.project().clips_of(t)[0].clone();
    // Half a second of pre-roll before a take from 2 s.
    let mut r = s.record;
    r.preroll = Roll::seconds(0.5);
    r.preroll_on_bar = false;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    let from = secs(&s, 2.0);
    transport(&mut s, TransportAction::Locate(from));
    settle(&mut s, Duration::from_millis(50));
    transport(&mut s, TransportAction::ToggleRecord);
    wait_position(&mut s, 2.4);
    let files = media(&s);
    transport(&mut s, TransportAction::AbortAndRestartRecording);
    // Back to the pre-roll's start, recording again.
    let mut lowest = i64::MAX;
    wait_until(
        &mut s,
        |s| {
            if s.transport().playing {
                lowest = lowest.min(s.processing_position());
            }
            s.recording_view().is_some() && s.transport().position >= (2.3 * SR as f64) as i64
        },
        "the new take",
    );
    assert!(
        (72_000..96_000).contains(&lowest),
        "the pre-roll again: from {lowest}"
    );
    assert_eq!(s.recording_view().unwrap().from, 96_000);
    transport(&mut s, TransportAction::Stop);
    s.wait_for_recordings();
    s.wait_for_discarded_recordings();
    let clips = s.project().clips_of(t);
    assert_eq!(clips.len(), 2, "the earlier take and the new one");
    assert!(
        clips.iter().any(|c| **c == earlier),
        "the earlier take stays"
    );
    assert!(clips.iter().any(|c| c.start == from), "the new take at 2 s");
    let labels = s.history_steps().0;
    assert_eq!(labels.iter().filter(|l| *l == "Record").count(), 2);
    // The aborted take's file is gone, the new one's is there.
    let now = media(&s);
    let wav = |set: &BTreeSet<PathBuf>| {
        set.iter()
            .filter(|f| f.extension().is_some_and(|e| e == "wav"))
            .count()
    };
    assert_eq!(wav(&now), 2, "{now:?}");
    assert_eq!(
        wav(&files),
        2,
        "the earlier take and the aborted one: {files:?}"
    );
    assert_ne!(now, files);
}

#[test]
fn a_take_aborted_inside_a_punch_range_goes_back_to_where_playback_started() {
    let (mut s, t) = session();
    let range = MusicalRange::new(secs(&s, 0.5), secs(&s, 2.0)).unwrap();
    transport(&mut s, TransportAction::SetPunch(Some(range)));
    transport(&mut s, TransportAction::TogglePunch);
    let at = secs(&s, 0.25);
    transport(&mut s, TransportAction::Locate(at));
    settle(&mut s, Duration::from_millis(50));
    transport(&mut s, TransportAction::ToggleRecord);
    wait_position(&mut s, 0.8);
    transport(&mut s, TransportAction::AbortRecording);
    s.wait_for_discarded_recordings();
    settle(&mut s, Duration::from_millis(200));
    assert_eq!(s.processing_position(), 12_000);
    assert!(s.project().clips_of(t).is_empty());
}

#[test]
fn a_project_closed_while_recording_gets_nothing_of_it() {
    let (mut s, _) = session();
    let at = secs(&s, 0.0);
    transport(&mut s, TransportAction::Locate(at));
    transport(&mut s, TransportAction::ToggleRecord);
    wait_position(&mut s, 0.3);
    s.new_project(false).unwrap();
    settle(&mut s, Duration::from_millis(300));
    s.wait_for_recordings();
    assert!(s.recording_view().is_none());
    assert!(s.project().clips.is_empty(), "no take in the new project");
    assert!(s.project().sources.is_empty());
    assert!(!s.history().can_undo());
}

#[test]
fn a_midi_take_is_thrown_away_too() {
    let (mut s, audio) = session();
    s.edit(Command::SetTrackRecordArm {
        track: audio,
        on: false,
    })
    .unwrap();
    let t = s.add_track(TrackKind::Midi).unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    let take = |s: &mut Session, abort: bool| {
        let at = secs(s, 0.0);
        transport(s, TransportAction::Locate(at));
        settle(s, Duration::from_millis(50));
        transport(s, TransportAction::ToggleRecord);
        wait_position(s, 0.2);
        s.midi_keyboard().send(&[0x90, 60, 100]);
        settle(s, Duration::from_millis(100));
        s.midi_keyboard().send(&[0x80, 60, 0]);
        settle(s, Duration::from_millis(50));
        transport(
            s,
            if abort {
                TransportAction::AbortRecording
            } else {
                TransportAction::Stop
            },
        );
        s.wait_for_recordings();
        s.wait_for_discarded_recordings();
    };
    take(&mut s, true);
    assert!(s.project().clips_of(t).is_empty(), "thrown away");
    take(&mut s, false);
    assert_eq!(s.project().clips_of(t).len(), 1, "kept when stopped");
}

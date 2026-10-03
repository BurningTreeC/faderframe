#![allow(clippy::unwrap_used)]
//! Recording through the session with the dummy device's test tone:
//! takes, take folders, replace mode, punch, loop modes, undo and flatten.

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::wavstream::WavFile;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, InputRouting, MusicalRange, Project, TrackKind};
use faderframe_session::{
    Action, AudioPreferences, LoopRecordMode, RecordMode, Session, TransportAction,
};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn session() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Rec", SR), None, EngineConfig::default()).unwrap();
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

fn locate(s: &mut Session, seconds: f64) {
    let at = secs(s, seconds);
    s.dispatch(Action::Transport(TransportAction::Locate(at)))
        .unwrap();
}

/// Arm record mode, play until the playhead passes `until` seconds, stop.
fn record(s: &mut Session, until: f64) {
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    wait_position(s, until);
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
}

fn wait_position(s: &mut Session, seconds: f64) {
    let target = (seconds * SR as f64) as i64;
    wait_until(s, |s| s.transport().position >= target, "position");
}

/// Wait for `n` loop wraps.
fn wait_wraps(s: &mut Session, n: usize) {
    let mut last = s.transport().position;
    let mut wraps = 0;
    wait_until(
        s,
        |s| {
            let pos = s.transport().position;
            if pos < last {
                wraps += 1;
            }
            last = pos;
            wraps >= n
        },
        "loop wraps",
    );
}

fn wait_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        s.tick(0.005);
        if s.transport().playing && done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

fn clips(s: &Session, t: TrackId) -> Vec<faderframe_project::Clip> {
    let mut v: Vec<_> = s.project().clips_of(t).into_iter().cloned().collect();
    v.sort_by_key(|c| c.start);
    v
}

#[test]
fn records_a_take_with_the_input_signal() {
    let (mut s, t) = session();
    let before = s.history().revision();
    record(&mut s, 0.4);
    let c = clips(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    let a = c[0].as_audio().unwrap();
    assert_eq!(c[0].start, MusicalTime::ZERO);
    assert!(a.length >= (0.4 * SR as f64) as i64, "length {}", a.length);
    let src = &s.project().sources[&a.source];
    let faderframe_project::SourceSpec::File { path, .. } = &src.spec else {
        panic!()
    };
    assert!(path.starts_with(s.media_dir()));
    let f = WavFile::open(path).unwrap();
    let mut l = vec![0.0f32; 4_800];
    f.read(4_800, &mut [&mut l], &mut Vec::new()).unwrap();
    let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak - 0.5).abs() < 0.01, "test tone recorded ({peak})");
    let crossings = l.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    assert!(
        (crossings as i64 - 100).abs() <= 1,
        "1 kHz: {crossings} in 0.1 s"
    );
    assert!(s.peaks(a.source).is_some());
    assert_eq!(s.missing_sources(), 0);
    // One undo step removes the take.
    s.dispatch(Action::Undo).unwrap();
    assert!(clips(&s, t).is_empty());
    assert!(s.project().sources.is_empty());
    let _ = before;
}

#[test]
fn recording_again_makes_a_take_folder_and_comps_the_new_take() {
    let (mut s, t) = session();
    record(&mut s, 0.5);
    locate(&mut s, 0.1);
    record(&mut s, 0.3);
    let c = clips(&s, t);
    assert_eq!(c.len(), 1, "one folder, not two clips: {c:?}");
    let f = c[0].as_takes().expect("take folder");
    assert_eq!(f.takes.len(), 2);
    assert_eq!(c[0].start, MusicalTime::ZERO);
    let second = (0.1 * SR as f64) as i64;
    assert_eq!(
        f.take_at(second - 100),
        Some(0),
        "first take before the second recording"
    );
    assert_eq!(f.take_at(second + 100), Some(1), "new take comped in");
    assert_eq!(
        f.take_at((0.45 * SR as f64) as i64),
        Some(0),
        "first take after it"
    );
    assert!((f.takes[1].start - second).abs() < 64);

    // Flatten: plain clips of the comp.
    s.dispatch(Action::FlattenTakes(c[0].id)).unwrap();
    let flat = clips(&s, t);
    assert_eq!(flat.len(), 3);
    assert!(flat.iter().all(|c| c.as_audio().is_some()));
    s.dispatch(Action::Undo).unwrap();
    assert!(clips(&s, t)[0].as_takes().is_some());
}

#[test]
fn replace_mode_cuts_existing_clips_back() {
    let (mut s, t) = session();
    let mut r = s.record;
    r.mode = RecordMode::Replace;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    record(&mut s, 0.5);
    locate(&mut s, 0.1);
    record(&mut s, 0.3);
    let c = clips(&s, t);
    assert_eq!(
        c.len(),
        3,
        "head of the old take, new take, tail of the old take"
    );
    assert!(c.iter().all(|c| c.as_audio().is_some()));
    let new = &c[1];
    assert_eq!(new.start, c[0].end(&s.project().timeline, SR));
    assert_eq!(c[2].start, new.end(&s.project().timeline, SR));
}

#[test]
fn punch_records_exactly_the_punch_range() {
    let (mut s, t) = session();
    let range = MusicalRange::new(secs(&s, 0.1), secs(&s, 0.3)).unwrap();
    s.dispatch(Action::Transport(TransportAction::SetPunch(Some(range))))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::TogglePunch))
        .unwrap();
    assert!(s.project().punch_enabled);
    record(&mut s, 0.4);
    let c = clips(&s, t);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].start, range.start);
    let a = c[0].as_audio().unwrap();
    let expected = s.engine().musical_to_samples(s.project(), range.end)
        - s.engine().musical_to_samples(s.project(), range.start);
    assert_eq!(a.length, expected, "sample-accurate punch");
}

fn loop_record(mode: LoopRecordMode) -> (Session, TrackId) {
    let (mut s, t) = session();
    let mut r = s.record;
    r.loop_mode = mode;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    let range = MusicalRange::new(MusicalTime::ZERO, secs(&s, 0.25)).unwrap();
    s.dispatch(Action::Transport(TransportAction::SetLoop(Some(range))))
        .unwrap();
    assert!(s.project().loop_enabled);
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    wait_wraps(&mut s, 3);
    wait_position(&mut s, 0.2); // most of the fourth pass
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    (s, t)
}

#[test]
fn loop_recording_modes() {
    let (s, t) = loop_record(LoopRecordMode::Takes);
    let c = clips(&s, t);
    assert_eq!(c.len(), 1);
    let f = c[0].as_takes().expect("folder of passes");
    assert!(f.takes.len() >= 4, "{} takes", f.takes.len());
    let last = f.takes.len() - 1;
    assert_eq!(
        f.take_at(100),
        Some(last),
        "the last pass plays where it has material"
    );
    assert_eq!(f.length, (0.25 * SR as f64) as i64);

    let (s, t) = loop_record(LoopRecordMode::LastPass);
    let c = clips(&s, t);
    assert_eq!(c.len(), 1);
    assert!(c[0].as_audio().is_some(), "single clip of the last pass");

    let (s, t) = loop_record(LoopRecordMode::NewTracks);
    let tracks: Vec<_> = s
        .project()
        .tracks
        .iter()
        .filter(|x| x.kind == TrackKind::Audio)
        .collect();
    // Three wraps plus most of a fourth pass: the armed track holds the
    // last pass, three muted tracks the earlier ones.
    assert!(tracks.len() >= 4, "armed track + one per earlier pass");
    let extra: Vec<_> = tracks.iter().filter(|x| x.id != t).collect();
    assert!(
        extra
            .iter()
            .all(|x| x.mute && !x.record_arm && x.clips.len() == 1)
    );
    assert_eq!(clips(&s, t).len(), 1);
    assert!(matches!(clips(&s, t)[0].content, ClipContent::Audio(_)));
}

#[test]
fn mono_and_stereo_tracks_record_one_or_two_channels() {
    let (mut s, mono) = session();
    let stereo = s
        .add_track_with_layout(
            TrackKind::Audio,
            Some(faderframe_core::ChannelLayout::Stereo),
        )
        .unwrap();
    s.edit(Command::SetTrackInput {
        track: stereo,
        input: InputRouting::Hardware { first_channel: 2 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm {
        track: stereo,
        on: true,
    })
    .unwrap();
    // The mono track takes input 2 instead of 1.
    s.edit(Command::Batch {
        label: "Change Input".into(),
        commands: vec![
            Command::SetTrackLayout {
                track: mono,
                layout: faderframe_core::ChannelLayout::Mono,
            },
            Command::SetTrackInput {
                track: mono,
                input: InputRouting::Hardware { first_channel: 1 },
            },
        ],
    })
    .unwrap();
    record(&mut s, 0.3);
    let channels = |t: TrackId| {
        let c = clips(&s, t);
        let src = c[0].as_audio().unwrap().source;
        let faderframe_project::SourceSpec::File { path, channels, .. } =
            &s.project().sources[&src].spec
        else {
            panic!()
        };
        assert_eq!(WavFile::open(path).unwrap().channels() as u16, *channels);
        *channels
    };
    assert_eq!(channels(mono), 1);
    assert_eq!(channels(stereo), 2);
}

/// Tick like the UI's frame clock for `seconds`.
fn settle(s: &mut Session, seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Tick like the UI's frame clock until `done` or 5 s.
fn frames_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        s.tick(0.016);
        if done(s) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn ui_flow_record_button_then_stop_button_keeps_the_take() {
    // As the UI does it: the record button, then the stop button; takes
    // are finished by the frame tick (no wait_for_recordings).
    for stop in [TransportAction::Stop, TransportAction::TogglePlay] {
        let (mut s, t) = session();
        s.dispatch(Action::Transport(TransportAction::ToggleRecord))
            .unwrap();
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        wait_position(&mut s, 0.3);
        s.dispatch(Action::Transport(stop.clone())).unwrap();
        let ok = frames_until(&mut s, |s| !clips(s, t).is_empty());
        assert!(ok, "{stop:?}: the take never appeared");
        // And it stays.
        settle(&mut s, 0.4);
        assert_eq!(clips(&s, t).len(), 1, "{stop:?}: the take disappeared");
    }
}

#[test]
fn the_record_button_starts_playing_and_warns_when_nothing_is_armed() {
    let (mut s, t) = session();
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    assert!(
        frames_until(&mut s, |s| s.transport().playing && s.transport().recording),
        "recording and playing after one press"
    );
    wait_position(&mut s, 0.2);
    // Pressing it again punches out; playback goes on.
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    assert!(
        frames_until(&mut s, |s| !clips(s, t).is_empty()),
        "the take is placed"
    );
    assert!(s.transport().playing, "punching out keeps playing");
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    // Nothing armed: no recording, a warning.
    s.edit(Command::SetTrackRecordArm {
        track: t,
        on: false,
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    settle(&mut s, 0.3);
    assert!(!s.transport().playing && !s.transport().recording);
    let n = s.latest_notice().unwrap();
    assert_eq!(n.level, faderframe_session::NoticeLevel::Warning);
    assert!(n.text.starts_with("Nothing is armed"), "{}", n.text);
}

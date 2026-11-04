#![allow(clippy::unwrap_used)]
//! Capture Audio: what the kept tracks were played, recording or not,
//! becomes takes afterwards — played while playing, where it was heard
//! (the same frames as a recording of that run); played while stopped,
//! its first sound on the bar at the playhead — within the memory limit.

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::wavstream::WavFile;
use faderframe_core::{ChannelLayout, TrackId};
use faderframe_engine::{EngineConfig, MetronomeMode};
use faderframe_project::{
    Clip, Command, InputRouting, MonitorMode, OutputRouting, Project, SourceSpec, Take, TrackKind,
};
use faderframe_session::audio_capture::{AudioCaptureSettings, CaptureTracks};
use faderframe_session::{Action, AudioPreferences, LoopRecordMode, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const BUFFER: u32 = 64;

/// A session with one armed mono audio track on input 1 (monitoring off)
/// on the dummy device `backend`.
fn session(backend: DummyBackend, rate: u32, buffer: u32) -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Capture", rate), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    s.start_audio(
        vec![Box::new(backend)],
        &AudioPreferences {
            sample_rate: Some(rate),
            buffer_size: Some(buffer),
            ..Default::default()
        },
    )
    .unwrap();
    (s, t)
}

/// Tick the session for `millis`.
fn run(s: &mut Session, millis: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(millis) {
        std::thread::sleep(Duration::from_millis(2));
        s.tick(0.002);
    }
}

fn wait_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        s.tick(0.002);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

/// Play from the start until the playhead passes `seconds`, then stop.
fn play_to(s: &mut Session, seconds: f64) {
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::ZERO,
    )))
    .unwrap();
    run(s, 30);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let target = (seconds * f64::from(SR)) as i64;
    let mut wrapped = 0;
    let mut last = 0;
    wait_until(
        s,
        |s| {
            let p = s.transport().position;
            if p < last {
                wrapped += 1;
            }
            last = p;
            s.transport().playing && p >= target
        },
        "the playhead",
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    let _ = wrapped;
}

fn clips(s: &Session, t: TrackId) -> Vec<Clip> {
    let mut v: Vec<Clip> = s.project().clips_of(t).into_iter().cloned().collect();
    v.sort_by_key(|c| c.start);
    v
}

/// A source's first channel, whole.
fn source(s: &Session, id: faderframe_core::AudioSourceId) -> Vec<f32> {
    let SourceSpec::File { path, .. } = &s.project().sources[&id].spec else {
        panic!("a file")
    };
    let f = WavFile::open(path).unwrap();
    let mut l = vec![0.0f32; f.frames() as usize];
    f.read(0, &mut [&mut l], &mut Vec::new()).unwrap();
    l
}

/// What a take plays at folder frames `from..to`.
fn take_audio(s: &Session, t: &Take, from: i64, to: i64) -> Vec<f32> {
    let data = source(s, t.source);
    (from..to)
        .map(|r| data[(t.source_offset + r) as usize])
        .collect()
}

fn set_capture(s: &mut Session, change: impl FnOnce(&mut AudioCaptureSettings)) {
    let mut c = s.audio_capture_settings();
    change(&mut c);
    s.set_audio_capture(c);
}

#[test]
fn a_phrase_played_while_stopped_starts_on_the_bar_at_the_playhead() {
    // Input 1 sounds from one second into the stream.
    let (mut s, t) = session(DummyBackend::with_input_spread(220.0, 7.0), SR, BUFFER);
    // The playhead in bar 3, beat 2: the phrase's first sound on bar 3.
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(9.0),
    )))
    .unwrap();
    run(&mut s, 400);
    assert_eq!(s.audio_capture_status().tracks, [t]);
    assert!(!s.can_capture_audio(), "nothing played yet");
    s.dispatch(Action::CaptureAudio).unwrap();
    s.wait_for_audio_captures();
    assert!(clips(&s, t).is_empty());
    wait_until(&mut s, Session::can_capture_audio, "something to capture");
    run(&mut s, 300);
    let before = s.history().revision();
    s.dispatch(Action::CaptureAudio).unwrap();
    s.wait_for_audio_captures();
    let c = clips(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    let a = c[0].as_audio().unwrap();
    let start = s.engine().musical_to_samples(s.project(), c[0].start);
    let bar = s
        .engine()
        .musical_to_samples(s.project(), MusicalTime::from_quarters(8.0));
    // The lead-in before the bar, the first sound on it.
    let lead = (0.2 * f64::from(SR)) as i64;
    assert_eq!(start, bar - lead);
    let data = source(&s, a.source);
    let at = (a.source_offset + bar - start) as usize;
    assert!(data[at].abs() >= 0.001, "{}", data[at]);
    assert!(data[..at].iter().all(|v| *v == 0.0), "silence before it");
    assert!(
        a.length > lead + (0.3 * f64::from(SR)) as i64,
        "{}",
        a.length
    );
    // One undo step.
    assert_ne!(s.history().revision(), before);
    s.dispatch(Action::Undo).unwrap();
    assert!(clips(&s, t).is_empty());
}

/// A loopback (outputs back on the inputs a buffer and `CABLE` frames
/// later) and the metronome: a take is right when its clicks sit on the
/// beats; the latency offset is the round trip (the dummy device reports
/// none).
const CABLE: u32 = 37;

fn loopback() -> (Session, TrackId) {
    let (mut s, t) = session(DummyBackend::with_loopback(CABLE), SR, BUFFER);
    // Its takes never come back round the cable.
    s.edit(Command::SetTrackOutput {
        track: t,
        output: OutputRouting::None,
    })
    .unwrap();
    let mut r = s.record;
    r.latency_offset = i64::from(BUFFER + CABLE);
    r.metronome = MetronomeMode::Always;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    run(&mut s, 200);
    (s, t)
}

/// The first sample of a take after `from` (timeline frames) that is not
/// silent.
fn first_sound(s: &Session, t: &Take, from: i64, to: i64) -> Option<i64> {
    take_audio(s, t, from, to)
        .iter()
        .position(|v| *v != 0.0)
        .map(|i| from + i as i64)
}

#[test]
fn a_run_played_while_playing_is_what_recording_it_would_have_been() {
    let (mut s, t) = loopback();
    // Recorded first.
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    play_to(&mut s, 1.3);
    s.wait_for_recordings();
    assert_eq!(clips(&s, t).len(), 1);
    // Recorded, so not offered.
    run(&mut s, 300);
    assert!(!s.can_capture_audio());
    // The same played without recording, captured after it stopped: one
    // folder with both (record mode Takes).
    play_to(&mut s, 1.3);
    run(&mut s, 300);
    assert!(s.can_capture_audio());
    s.dispatch(Action::CaptureAudio).unwrap();
    s.wait_for_audio_captures();
    let c = clips(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0].start, MusicalTime::ZERO);
    let f = c[0]
        .as_takes()
        .expect("a folder of the take and the capture");
    assert_eq!(f.takes.len(), 2);
    let (recorded, captured) = (&f.takes[0], &f.takes[1]);
    assert!(captured.name.contains("Capture"), "{}", captured.name);
    let from = recorded.start.max(captured.start);
    let to = recorded.end.min(captured.end);
    assert!(to - from > i64::from(SR), "{from}..{to}");
    let a = take_audio(&s, recorded, from, to);
    let b = take_audio(&s, captured, from, to);
    assert!(a.iter().any(|v| v.abs() > 0.1), "clicks");
    assert_eq!(a, b, "the same frames at the same places");
    // The click of beat 2 (half a second in) where it was heard.
    let beat = i64::from(SR) / 2;
    let first = first_sound(&s, captured, beat - 2_000, beat + 2_000);
    assert!(
        first.is_some_and(|f| (beat..=beat + 1).contains(&f)),
        "{first:?} for {beat}"
    );
}

#[test]
fn loop_passes_are_captured_as_takes_in_their_places() {
    let (mut s, t) = loopback();
    let mut r = s.record;
    r.loop_mode = LoopRecordMode::Takes;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    // A loop of one second: two beats.
    let one = s.engine().samples_to_musical(s.project(), i64::from(SR));
    s.dispatch(Action::Transport(TransportAction::SetLoop(Some(
        faderframe_project::MusicalRange::new(MusicalTime::ZERO, one).unwrap(),
    ))))
    .unwrap();
    if !s.project().loop_enabled {
        s.dispatch(Action::Transport(TransportAction::ToggleLoop))
            .unwrap();
    }
    // Three passes and some.
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::ZERO,
    )))
    .unwrap();
    run(&mut s, 30);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let mut wraps = 0;
    let mut last = 0;
    wait_until(
        &mut s,
        |s| {
            let p = s.transport().position;
            if p < last {
                wraps += 1;
            }
            last = p;
            wraps >= 3 && p > i64::from(SR) / 4
        },
        "three wraps",
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, 300);
    s.dispatch(Action::CaptureAudio).unwrap();
    s.wait_for_audio_captures();
    let c = clips(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    let f = c[0].as_takes().expect("the passes in a folder");
    assert!(f.takes.len() >= 4, "{} passes", f.takes.len());
    // Every whole pass plays the loop as it was heard: the same frames,
    // the clicks on the beats.
    let whole: Vec<&Take> = f
        .takes
        .iter()
        .filter(|k| k.end - k.start == i64::from(SR))
        .collect();
    assert!(whole.len() >= 2, "{:?}", f.takes);
    let first = take_audio(&s, whole[0], 0, i64::from(SR));
    for k in &whole[1..] {
        assert_eq!(take_audio(&s, k, 0, i64::from(SR)), first, "{}", k.name);
    }
    let beat = i64::from(SR) / 2;
    let on = first_sound(&s, whole[0], beat - 2_000, beat + 2_000);
    assert!(on.is_some_and(|f| (beat..=beat + 1).contains(&f)), "{on:?}");
}

#[test]
fn the_memory_limit_holds_and_the_oldest_input_goes() {
    const RATE: u32 = 96_000;
    let (mut s, t) = session(DummyBackend::with_input_tone(440.0), RATE, 256);
    s.edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Stereo,
    })
    .unwrap();
    // 1 MiB for two channels: under 1.2 s each at 96 kHz.
    set_capture(&mut s, |c| {
        c.max_megabytes = 1;
        c.seconds = 600;
    });
    run(&mut s, 2_500);
    let st = s.audio_capture_status();
    assert_eq!(st.channels, 2);
    assert_eq!(st.limit_bytes, 1 << 20);
    assert!(st.bytes <= st.limit_bytes, "{st:?}");
    assert!(st.seconds > 1.0 && st.seconds < 1.25, "{st:?}");
    // A capture holds what was kept, not more.
    wait_until(&mut s, Session::can_capture_audio, "something to capture");
    s.dispatch(Action::CaptureAudio).unwrap();
    s.wait_for_audio_captures();
    let c = clips(&s, t);
    assert_eq!(c.len(), 1);
    let length = c[0].as_audio().unwrap().length;
    assert!(length <= (1.25 * f64::from(RATE)) as i64, "{length}");
    // Switched off: nothing is kept.
    set_capture(&mut s, |c| c.on = false);
    run(&mut s, 50);
    let st = s.audio_capture_status();
    assert_eq!((st.channels, st.bytes), (0, 0));
    assert!(!s.can_capture_audio());
}

#[test]
fn the_settings_choose_the_tracks_kept() {
    let (mut s, armed) = session(DummyBackend::with_input_spread(220.0, 7.0), SR, BUFFER);
    let monitored = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackInput {
        track: monitored,
        input: InputRouting::Hardware { first_channel: 1 },
    })
    .unwrap();
    s.edit(Command::SetTrackMonitor {
        track: monitored,
        mode: MonitorMode::Input,
    })
    .unwrap();
    let other = s.add_track(TrackKind::Audio).unwrap();
    // Off the device: never kept.
    let beyond = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackInput {
        track: beyond,
        input: InputRouting::Hardware { first_channel: 40 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm {
        track: beyond,
        on: true,
    })
    .unwrap();
    run(&mut s, 50);
    assert_eq!(s.audio_capture_status().tracks, [armed, monitored]);
    assert_eq!(s.audio_capture_status().channels, 2);
    set_capture(&mut s, |c| c.tracks = CaptureTracks::Armed);
    run(&mut s, 50);
    assert_eq!(s.audio_capture_status().tracks, [armed]);
    assert_eq!(s.audio_capture_status().channels, 1);
    set_capture(&mut s, |c| c.tracks = CaptureTracks::All);
    run(&mut s, 50);
    assert_eq!(s.audio_capture_status().tracks, [armed, monitored, other]);
    // Two tracks playing input 1 and 2 (the tones from one second in):
    // both captured, each from its own channel.
    set_capture(&mut s, |c| c.tracks = CaptureTracks::Monitored);
    wait_until(&mut s, Session::can_capture_audio, "the tones");
    run(&mut s, 300);
    s.dispatch(Action::CaptureAudio).unwrap();
    s.wait_for_audio_captures();
    assert_eq!(clips(&s, armed).len(), 1);
    assert_eq!(clips(&s, monitored).len(), 1);
    assert!(clips(&s, other).is_empty());
    // Not while recording.
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 100);
    assert!(!s.can_capture_audio());
    assert!(s.dispatch(Action::CaptureAudio).is_err());
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
}

/// Abort Recording throws the take away: what was played can still be
/// captured, in its place.
#[test]
fn an_aborted_take_can_still_be_captured() {
    let (mut s, t) = loopback();
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::ZERO,
    )))
    .unwrap();
    run(&mut s, 30);
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let target = (1.2 * f64::from(SR)) as i64;
    wait_until(
        &mut s,
        |s| s.transport().playing && s.transport().position >= target,
        "the playhead",
    );
    s.dispatch(Action::Transport(TransportAction::AbortRecording))
        .unwrap();
    s.wait_for_discarded_recordings();
    run(&mut s, 300);
    assert!(clips(&s, t).is_empty(), "nothing recorded");
    wait_until(&mut s, Session::can_capture_audio, "the take played");
    s.dispatch(Action::CaptureAudio).unwrap();
    s.wait_for_audio_captures();
    let c = clips(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0].start, MusicalTime::ZERO);
    let a = c[0].as_audio().unwrap();
    let data = source(&s, a.source);
    // The click of beat 2 where it was heard.
    let beat = i64::from(SR) / 2;
    let from = (a.source_offset + beat - 2_000) as usize;
    let first = data[from..].iter().position(|v| *v != 0.0).unwrap() as i64 + beat - 2_000;
    assert!((beat..=beat + 1).contains(&first), "{first} for {beat}");
}

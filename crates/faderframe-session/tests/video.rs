#![allow(clippy::unwrap_used)]
//! Video in the session: a movie (the sync test's flashes and beeps, muxed
//! into one file) imported with its sound, shown frame-exact, undone,
//! saved and reopened, written out with the mix, and shown while playing.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::timecode::FrameRate;
use faderframe_engine::EngineConfig;
use faderframe_project::{Project, TrackKind};
use faderframe_session::video::VideoOp;
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_video::mux::Container;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The video cache's folder is the process's: the tests take turns.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn turn() -> std::sync::MutexGuard<'static, ()> {
    TURN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A fresh folder for a test, with the video cache in it (never the
/// user's).
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-video-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    faderframe_session::video::set_cache_dir(Some(d.join("cache")));
    d
}

/// A 10 s movie at 25 fps: white at every whole second (frames 0, 25, …),
/// a beep with each flash.
fn movie(d: &std::path::Path) -> PathBuf {
    let (v, w) = (d.join("flash.mkv"), d.join("beep.wav"));
    faderframe_video::sync_test::make_sync_test(&v, &w, 10, 25).unwrap();
    let out = d.join("movie.mkv");
    let cancel = AtomicBool::new(false);
    faderframe_video::mux::mux(
        &v,
        &[w],
        &out,
        Container::Mkv,
        Default::default(),
        &cancel,
        |_| {},
    )
    .unwrap();
    out
}

fn brightness(s: &faderframe_session::video::VideoShown) -> u8 {
    let f = &s.picture.as_ref().unwrap().frame;
    f.pixel(f.width / 2, f.height / 4)[0]
}

/// The still picture at the playhead, once exact.
fn still(s: &mut Session) -> faderframe_session::video::VideoShown {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        s.tick(0.01);
        if let Some(v) = s.video_picture(0, (640, 360))
            && v.picture
                .as_ref()
                .is_some_and(|p| p.exact && p.number == v.frame)
        {
            return v;
        }
        if Instant::now() > end {
            let src = *s.project().video.sources.keys().next().unwrap();
            panic!(
                "no exact picture: {:?}, state {:?}, jobs {:?}",
                s.video_picture(0, (640, 360)).map(|v| (
                    v.frame,
                    v.picture.map(|p| (p.number, p.exact, p.frame.width))
                )),
                s.video_source_state(src),
                s.video_jobs()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_movie_is_imported_shown_saved_and_written_out() {
    let _turn = turn();
    let d = dir("import");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    let tracks = s.project().tracks.len();
    s.dispatch(Action::Video(VideoOp::Import {
        path: file.clone(),
        sound: true,
    }))
    .unwrap();
    s.wait_for_video();
    let video = &s.project().video;
    assert_eq!(video.tracks.len(), 1);
    assert_eq!(video.tracks[0].clips.len(), 1);
    let clip = video.tracks[0].clips[0].clone();
    assert_eq!(clip.start, 0);
    assert!(
        (clip.length - 10_000_000_000).abs() < 50_000_000,
        "{}",
        clip.length
    );
    assert_eq!(s.project().timecode.unwrap().rate, FrameRate::Fps25);
    // Its sound on a track of its own, 10 s long.
    assert_eq!(s.project().tracks.len(), tracks + 1);
    let sound = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio && t.name.ends_with("(sound)"))
        .expect("a sound track");
    assert_eq!(sound.clips.len(), 1);

    // Two seconds in: frame 50, a flash; a frame later, dark.
    s.dispatch(Action::Transport(TransportAction::Locate(
        s.engine().samples_to_musical(s.project(), 96_000),
    )))
    .unwrap();
    let v = still(&mut s);
    assert_eq!(v.frame, 50);
    assert!(brightness(&v) > 200, "a flash at frame 50");
    let next = 96_000 + 48_000 / 25 + 10;
    s.dispatch(Action::Transport(TransportAction::Locate(
        s.engine().samples_to_musical(s.project(), next),
    )))
    .unwrap();
    let v = still(&mut s);
    assert_eq!(v.frame, 51);
    assert!(brightness(&v) < 30, "dark after it");

    // One undo step takes it all away; redo brings it back.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().video.tracks.is_empty() || s.project().video.tracks[0].clips.is_empty());
    assert_eq!(s.project().tracks.len(), tracks);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.project().video.tracks[0].clips.len(), 1);

    // Saved relative, reopened absolute and shown again.
    let proj = d.join("Film").join("Film.ffproj");
    std::fs::create_dir_all(proj.parent().unwrap()).unwrap();
    let local = proj.parent().unwrap().join("movie.mkv");
    std::fs::copy(&file, &local).unwrap();
    let mut v2 = s.project().video.clone();
    for src in v2.sources.values_mut() {
        src.path = local.clone();
    }
    s.edit(faderframe_project::Command::SetVideo {
        video: Box::new(v2),
    })
    .unwrap();
    s.save_as(&proj).unwrap();
    let text = std::fs::read_to_string(&proj).unwrap();
    assert!(text.contains("\"movie.mkv\""), "stored relative");
    s.open(&proj).unwrap();
    s.wait_for_video();
    let src = s.project().video.sources.values().next().unwrap();
    assert_eq!(src.path, local);
    let v = still(&mut s);
    assert_eq!(v.frame, 0);
    assert!(brightness(&v) > 200);

    // Written out: the picture with the mix, as long as the movie.
    let out = d.join("out.mov");
    s.dispatch(Action::Video(VideoOp::Export {
        clip: None,
        path: out.clone(),
        container: Container::Mov,
    }))
    .unwrap();
    s.wait_for_video();
    let info = faderframe_video::probe::probe(&out).unwrap();
    assert!(info.video.is_some());
    assert_eq!(info.audio.len(), 1);
    assert!(
        (info.duration_ns - 10_000_000_000).abs() < 100_000_000,
        "{}",
        info.duration_ns
    );
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let ix = faderframe_video::index::index(&out, &cancel, |_| {}).unwrap();
    let tc = s.project().timecode.unwrap();
    assert_eq!(
        ix.timecode,
        Some((tc.start, tc.rate)),
        "the project's timecode"
    );

    // A trimmed clip writes only its span (an intra-coded movie cuts at
    // any frame), labelled with the timecode where it sits.
    let clip = s.project().video.tracks[0].clips[0].clone();
    s.dispatch(Action::Video(VideoOp::TrimClip {
        clip: clip.id,
        start: clip.start + 2 * 48_000,
        offset: clip.offset + 2_000_000_000,
        length: 3_000_000_000,
    }))
    .unwrap();
    let out = d.join("part.mov");
    s.dispatch(Action::Video(VideoOp::Export {
        clip: Some(clip.id),
        path: out.clone(),
        container: Container::Mov,
    }))
    .unwrap();
    s.wait_for_video();
    let info = faderframe_video::probe::probe(&out).unwrap();
    assert!(
        (info.duration_ns - 3_000_000_000).abs() < 100_000_000,
        "{}",
        info.duration_ns
    );
    let ix = faderframe_video::index::index(&out, &cancel, |_| {}).unwrap();
    let at = tc.at(clip.start + 2 * 48_000, 48_000);
    assert_eq!(ix.timecode, Some((at, tc.rate)));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_picture_follows_the_engine_while_playing() {
    picture_follows_the_engine(false);
}

#[test]
fn the_picture_follows_the_engine_while_recording() {
    picture_follows_the_engine(true);
}

fn picture_follows_the_engine(recording: bool) {
    let _turn = turn();
    let d = dir("play");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file, false);
    s.wait_for_video();
    let record_track = recording.then(|| {
        let track = s.add_track(TrackKind::Audio).unwrap();
        s.edit(faderframe_project::Command::SetTrackInput {
            track,
            input: faderframe_project::InputRouting::Hardware { first_channel: 0 },
        })
        .unwrap();
        s.edit(faderframe_project::Command::SetTrackRecordArm { track, on: true })
            .unwrap();
        track
    });
    let _ = still(&mut s);
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(440.0))],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::Transport(if recording {
        TransportAction::ToggleRecord
    } else {
        TransportAction::Play
    }))
    .unwrap();
    let start = Instant::now();
    let mut frames = Vec::new();
    let mut decoded = std::collections::BTreeSet::new();
    while start.elapsed() < Duration::from_millis(1500) {
        s.tick(0.01);
        if let Some(v) = s.video_picture(0, (640, 360)) {
            if let Some(p) = &v.picture {
                decoded.insert(p.number);
            }
            frames.push((start.elapsed(), v.frame, v.position));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    // About 1.5 s of polling every 10 ms (fewer on a slow runner: 30 on a
    // CI Mac): enough to see the frames move.
    assert!(
        frames.len() > 15,
        "pictures while playing: {}",
        frames.len()
    );
    // Frames move forward with the transport, about 25 a second.
    let (t0, f0, _) = frames[frames.len() / 4];
    let (t1, f1, _) = *frames.last().unwrap();
    let rate = (f1 - f0) as f64 / (t1 - t0).as_secs_f64();
    assert!((15.0..35.0).contains(&rate), "{rate} frames a second");
    assert!(frames.windows(2).all(|w| w[1].1 >= w[0].1), "never back");
    // The desired frame number advancing alone does not mean the Video
    // Dock received new pictures. Count what was actually decoded too.
    assert!(decoded.len() > 15, "only {} decoded frames", decoded.len());
    let (shown, late) = s.video_frames_late();
    assert!(shown > 10, "{shown} frames counted");
    eprintln!("{shown} frames shown, {late} not ready in time");
    if let Some(track) = record_track {
        s.wait_for_recordings();
        assert!(
            !s.project().track(track).unwrap().clips.is_empty(),
            "a recorded take"
        );
    }
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&d);
}

/// The cache: what a movie leaves there (its index), cleared on request,
/// and moved elsewhere by the settings.
#[test]
fn the_video_cache_is_kept_where_set_and_cleared() {
    let _turn = turn();
    let d = dir("cache");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file.clone(), false);
    s.wait_for_video();
    assert!(
        faderframe_session::video::video_cache_size() > 0,
        "an index"
    );
    let freed = s.clear_video_cache();
    assert!(freed > 0);
    assert_eq!(faderframe_session::video::video_cache_size(), 0);
    // Another folder: the next index goes there.
    let other = d.join("elsewhere");
    s.set_video_settings(faderframe_session::video::VideoSettings {
        proxy_height: None,
        cache_dir: Some(other.clone()),
    });
    s.import_video(file, false);
    s.wait_for_video();
    assert!(std::fs::read_dir(&other).unwrap().count() > 0);
    let _ = std::fs::remove_dir_all(&d);
}

/// Two video tracks: a clip moved onto the second shows there (A/B), and
/// the flash test's flashes come back as cut markers.
#[test]
fn video_tracks_compare_and_cuts_become_markers() {
    let _turn = turn();
    let d = dir("tracks");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file.clone(), false);
    s.wait_for_video();
    s.import_video(file, false);
    s.wait_for_video();
    s.dispatch(Action::Video(VideoOp::AddTrack)).unwrap();
    let v = &s.project().video;
    assert_eq!(v.tracks.len(), 2);
    let second = v.tracks[0].clips[1].clone();
    let to = v.tracks[1].id;
    s.dispatch(Action::Video(VideoOp::MoveClip {
        clip: second.id,
        start: 48_000,
        track: Some(to),
    }))
    .unwrap();
    let v = &s.project().video;
    assert_eq!(v.tracks[0].clips.len(), 1);
    assert_eq!(v.tracks[1].clips.len(), 1);
    assert_eq!(v.tracks[1].clips[0].start, 48_000);
    // At 1.5 s: track B shows its clip half a second in, A a second and a half.
    s.dispatch(Action::Transport(TransportAction::Locate(
        s.engine().samples_to_musical(s.project(), 72_000),
    )))
    .unwrap();
    s.tick(0.01);
    let a = s
        .video_picture_on(Some(s.project().video.tracks[0].id), 0, (64, 36))
        .unwrap();
    let b = s.video_picture_on(Some(to), 0, (64, 36)).unwrap();
    assert_eq!(a.frame, 37);
    assert_eq!(b.frame, 12);
    // A flash a second: a cut into each flash and out of it.
    let markers = s.project().markers.len();
    let first = s.project().video.tracks[0].clips[0].id;
    s.dispatch(Action::Video(VideoOp::DetectCuts(first)))
        .unwrap();
    s.wait_for_video();
    let cuts = s.project().markers.len() - markers;
    assert!((18..=20).contains(&cuts), "{cuts} cut markers");
    let _ = std::fs::remove_dir_all(&d);
}

/// J/K/L: reverse at 2× runs the playhead and the picture back about two
/// seconds a second, heard as scrub snippets (never plain playback);
/// stopping leaves the playhead where it got to; K with J/L steps the
/// movie's own frames.
#[test]
fn the_shuttle_runs_back_and_frames_step() {
    use faderframe_session::shuttle::ShuttleOp;
    let _turn = turn();
    let d = dir("shuttle");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file, false);
    s.wait_for_video();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let locate = |s: &mut Session, samples: i64| {
        let at = s.engine().samples_to_musical(s.project(), samples);
        s.dispatch(Action::Transport(TransportAction::Locate(at)))
            .unwrap();
        for _ in 0..50 {
            s.tick(0.01);
            if s.transport().position == samples {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    locate(&mut s, 6 * 48_000);
    s.dispatch(Action::Shuttle(ShuttleOp::Reverse)).unwrap();
    s.dispatch(Action::Shuttle(ShuttleOp::Reverse)).unwrap();
    assert_eq!(s.shuttle_speed(), Some(-2.0));
    let start = Instant::now();
    let mut frames = Vec::new();
    let (mut heard, mut ticks) = (0, 0);
    while start.elapsed() < Duration::from_millis(1000) {
        s.tick(0.01);
        assert!(!s.transport().playing, "snippets, not playback");
        heard += usize::from(s.transport().scrubbing);
        ticks += 1;
        if let Some(v) = s.video_picture(0, (64, 36)) {
            frames.push(v.frame);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    s.dispatch(Action::Shuttle(ShuttleOp::Stop)).unwrap();
    assert_eq!(s.shuttle_speed(), None);
    // Snippets heard (a tick sees one only if it is still playing: with
    // ticks as far apart as a snippet's length that is a race, so only
    // that some were; the frames running back show the shuttle moved).
    assert!(heard >= 2, "snippets heard in {heard} of {ticks} ticks");
    s.tick(0.01);
    let at = s.transport().position as f64 / 48_000.0;
    // Six seconds, back two a second for one: about four (a slow runner
    // ticks late, never early).
    assert!((3.5..4.3).contains(&at), "stopped at {at} s");
    assert!(frames.windows(2).all(|w| w[1] <= w[0]), "never forward");
    let (first, last) = (frames[0], *frames.last().unwrap());
    assert!(first >= 145 && last <= 110, "frames {first} → {last}");
    // Frame steps from frame 100: its start, then the next frames'.
    locate(&mut s, 4 * 48_000);
    s.dispatch(Action::Shuttle(ShuttleOp::Step(1))).unwrap();
    assert_eq!(s.video_picture(0, (64, 36)).unwrap().frame, 101);
    assert_eq!(s.transport().position, 4 * 48_000 + 1920);
    s.dispatch(Action::Shuttle(ShuttleOp::Step(-1))).unwrap();
    s.dispatch(Action::Shuttle(ShuttleOp::Step(-1))).unwrap();
    assert_eq!(s.video_picture(0, (64, 36)).unwrap().frame, 99);
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&d);
}

/// A picture output of its own (as a DeckLink card): paced by the output
/// (25 frames a second here), showing the picture the sound is at.
#[test]
fn the_picture_goes_to_an_output_of_its_own() {
    use faderframe_video::output::Sink;
    let _turn = turn();
    let d = dir("output");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file, false);
    s.wait_for_video();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.set_picture_output(Some(Sink::Test {
        size: (160, 90),
        fps: (25, 1),
    }))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let start = Instant::now();
    let mut seen = Vec::new();
    while start.elapsed() < Duration::from_millis(2000) {
        s.tick(0.01);
        if let Some((_, n, last)) = s.picture_output() {
            seen.push((start.elapsed(), n, last));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    let (t0, n0, _) = seen[seen.len() / 4];
    let (t1, n1, last) = *seen.last().unwrap();
    let rate = (n1 - n0) as f64 / (t1 - t0).as_secs_f64();
    assert!((20.0..30.0).contains(&rate), "{rate} frames a second out");
    // The picture moves with the sound: about two seconds in.
    let last = last.expect("a picture, not black");
    assert!((25..=60).contains(&last), "frame {last} after 2 s");
    s.set_picture_output(None).unwrap();
    assert!(s.picture_output().is_none());
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&d);
}

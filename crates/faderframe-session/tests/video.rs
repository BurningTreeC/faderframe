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
    faderframe_video::mux::mux(&v, &[w], &out, Container::Mkv, &cancel, |_| {}).unwrap();
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
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_picture_follows_the_engine_while_playing() {
    let d = dir("play");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file, false);
    s.wait_for_video();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let start = Instant::now();
    let mut frames = Vec::new();
    while start.elapsed() < Duration::from_millis(1500) {
        s.tick(0.01);
        if let Some(v) = s.video_picture(0, (640, 360)) {
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
    let (shown, late) = s.video_frames_late();
    assert!(shown > 10, "{shown} frames counted");
    eprintln!("{shown} frames shown, {late} not ready in time");
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&d);
}

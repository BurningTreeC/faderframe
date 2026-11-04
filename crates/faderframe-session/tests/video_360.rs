#![allow(clippy::unwrap_used)]
//! 360° video in the session: a movie marked 360° with an ambisonic sound
//! (as a 360° camera's export) imports as a spherical picture with an
//! AmbiX track; the viewport is the listener's head (dragged or turned by
//! a head tracker over OSC); a click on the picture pans a track, written
//! as automation while playing; a flat movie is marked 360° by hand; and
//! the movie written out keeps both marks with the master's first order.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::spherical::{MAX_FOV, Spherical, StereoMode};
use faderframe_core::{AmbiOrder, ChannelLayout, SurroundParam, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::headtrack::{HeadOp, HeadTrackingSettings};
use faderframe_session::sphere::SphereOp;
use faderframe_session::video::VideoOp;
use faderframe_session::{Action, AudioPreferences, AutomationTarget, Session, TransportAction};
use faderframe_video::mux::{Container, MuxOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The video cache is the process's: the tests take turns.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn turn() -> std::sync::MutexGuard<'static, ()> {
    TURN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-360-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    faderframe_session::video::set_cache_dir(Some(d.join("cache")));
    d
}

/// 2 s of a flat grey MJPEG picture, 128×64, in `mux`.
fn picture(path: &Path, mux: &str) {
    use gst::prelude::*;
    faderframe_video::init().unwrap();
    let desc = format!(
        "videotestsrc num-buffers=50 pattern=smpte ! video/x-raw,width=128,height=64,framerate=25/1 ! jpegenc ! {mux} ! filesink name=out"
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline
        .by_name("out")
        .unwrap()
        .set_property("location", path.to_string_lossy().as_ref());
    pipeline.set_state(gst::State::Playing).unwrap();
    let msg = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(60),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap();
    if let gst::MessageView::Error(e) = msg.view() {
        panic!("making {}: {}", path.display(), e.error());
    }
    pipeline.set_state(gst::State::Null).unwrap();
}

/// A 360° camera's movie: the picture marked equirectangular, a
/// first-order AmbiX sound marked so.
fn camera_movie(d: &Path) -> PathBuf {
    let clip = d.join("raw.mov");
    picture(&clip, "qtmux");
    let wav = d.join("field.wav");
    let n = 96_000;
    let channels: Vec<Vec<f32>> = [0.5f32, 0.25, 0.1, 0.4]
        .iter()
        .map(|l| (0..n).map(|i| l * ((i as f32) * 0.05).sin()).collect())
        .collect();
    faderframe_audio_files::write_wav(
        &wav,
        &channels,
        48_000,
        faderframe_audio_files::WavFormat::Pcm24,
        false,
    )
    .unwrap();
    let out = d.join("camera.mov");
    faderframe_video::mux::mux(
        &clip,
        &[wav],
        &out,
        Container::Mov,
        MuxOptions {
            spherical: Some(Spherical::EQUIRECTANGULAR),
            ambisonic: Some(AmbiOrder::First),
            ..Default::default()
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    out
}

fn session(master: ChannelLayout) -> Session {
    let mut s = Session::new(Project::new("360", 48_000), None, EngineConfig::default()).unwrap();
    let m = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: m,
        layout: master,
    }))
    .unwrap();
    s
}

fn sphere(s: &mut Session, op: SphereOp) {
    s.dispatch(Action::Video(VideoOp::Sphere(op))).unwrap();
}

fn yaw(s: &Session) -> f32 {
    s.sphere_look().look.ypr().0
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.05
}

fn mono_track(s: &mut Session) -> TrackId {
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    t
}

#[test]
fn a_360_movie_imports_and_its_view_is_the_listeners_head() {
    let _turn = turn();
    let d = dir("import");
    let movie = camera_movie(&d);
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::First));
    s.dispatch(Action::Video(VideoOp::Import {
        path: movie,
        sound: true,
    }))
    .unwrap();
    s.wait_for_video();
    let (&source, src) = s.project().video.sources.iter().next().unwrap();
    assert_eq!(src.spherical, Some(Spherical::EQUIRECTANGULAR));
    // The sound is the field it is marked as.
    let sound = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name.ends_with("(sound)"))
        .unwrap();
    assert_eq!(sound.layout, ChannelLayout::Ambisonic(AmbiOrder::First));
    // Frames come through memory, as wide as the view needs (the file's
    // 128 at most).
    let end = Instant::now() + Duration::from_secs(20);
    let frame = loop {
        s.tick(0.01);
        if let Some(p) = s.video_picture(0, (640, 360)).and_then(|v| v.picture)
            && p.exact
        {
            break p.frame;
        }
        assert!(Instant::now() < end, "no picture");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(frame.gpu.is_none() && !frame.rgba.is_empty());
    assert_eq!((frame.width, frame.height), (128, 64));

    // Linked: looking is the head.
    let steps = s.history_steps().0.len();
    assert!(s.sphere_look().linked);
    sphere(
        &mut s,
        SphereOp::Look {
            yaw: 90.0,
            pitch: 10.0,
            roll: 0.0,
        },
    );
    assert!(close(yaw(&s), 90.0));
    assert!(close(s.head_orientation().ypr().0, 90.0));
    assert!(close(s.head_orientation().ypr().1, 10.0));
    // The head turned by hand turns the picture.
    s.dispatch(Action::Head(HeadOp::Turn {
        yaw: -45.0,
        pitch: 0.0,
        roll: 0.0,
    }))
    .unwrap();
    assert!(close(yaw(&s), -45.0));
    // Apart: the picture looks on its own, the head stays.
    sphere(&mut s, SphereOp::Link(false));
    sphere(
        &mut s,
        SphereOp::Look {
            yaw: 30.0,
            pitch: 0.0,
            roll: 0.0,
        },
    );
    assert!(close(yaw(&s), 30.0));
    assert!(close(s.head_orientation().ypr().0, -45.0));
    // Together again: the head takes the picture's way.
    sphere(&mut s, SphereOp::Link(true));
    assert!(close(s.head_orientation().ypr().0, 30.0));
    sphere(&mut s, SphereOp::Ahead);
    assert!(s.head_orientation().is_identity());
    sphere(&mut s, SphereOp::Fov(400.0));
    assert_eq!(s.sphere_look().fov, MAX_FOV);
    // None of this is an edit.
    assert_eq!(s.history_steps().0.len(), steps);

    // Marked flat by hand (one undo step), and back.
    sphere(
        &mut s,
        SphereOp::SetLayout {
            source,
            spherical: None,
        },
    );
    assert_eq!(s.video_spherical(source), None);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.video_spherical(source), Some(Spherical::EQUIRECTANGULAR));
    let _ = std::fs::remove_dir_all(&d);
}

/// A head tracker's OSC turns the picture.
#[test]
fn a_head_tracker_turns_the_picture() {
    let _turn = turn();
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::First));
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    s.dispatch(Action::Head(HeadOp::Tracking(HeadTrackingSettings {
        enabled: true,
        port,
        ..HeadTrackingSettings::default()
    })))
    .unwrap();
    let send = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let ypr = faderframe_control::osc::encode(&faderframe_control::osc::Message {
        addr: "/ypr".into(),
        args: vec![
            faderframe_control::osc::Arg::Float(60.0),
            faderframe_control::osc::Arg::Float(-5.0),
            faderframe_control::osc::Arg::Float(0.0),
        ],
    });
    let shown = s.display_revision();
    let end = Instant::now() + Duration::from_secs(5);
    while !close(yaw(&s), 60.0) {
        send.send_to(&ypr, ("127.0.0.1", port)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
        assert!(Instant::now() < end, "the tracker never turned the picture");
    }
    assert!(close(s.sphere_look().look.ypr().1, -5.0));
    // The views are told to draw it again.
    assert!(s.display_revision() > shown);
    // Dragged while tracked: the picture (and head) look where dragged,
    // the tracker's reading now relative to that.
    sphere(
        &mut s,
        SphereOp::Look {
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
        },
    );
    assert!(s.head_orientation().is_identity());
}

/// A click on the picture pans a track into the field; while playing,
/// a drag across it is written into the direction lanes (Touch).
#[test]
fn clicks_on_the_picture_pan_and_write_automation() {
    let _turn = turn();
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::Second));
    let t = mono_track(&mut s);
    // A stereo master has no direction to pan to.
    let targets = s.sphere_pan_targets();
    assert_eq!(targets.len(), 1);
    assert!(targets[0].field && targets[0].has_elevation);
    sphere(&mut s, SphereOp::PanTrack(Some(t)));
    assert_eq!(s.sphere_look().pan_track, Some(t));
    sphere(
        &mut s,
        SphereOp::PanTo {
            track: t,
            azimuth: 60.0,
            elevation: 20.0,
        },
    );
    let pan = s.project().track(t).unwrap().surround;
    assert!(close(pan.azimuth, 60.0) && close(pan.elevation, 20.0));
    s.dispatch(Action::Undo).unwrap();
    assert!(close(s.project().track(t).unwrap().surround.azimuth, 0.0));
    // A field into the field is not panned: it cannot be chosen.
    let field = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: field,
        layout: ChannelLayout::Ambisonic(AmbiOrder::First),
    }))
    .unwrap();
    assert!(
        s.dispatch(Action::Video(VideoOp::Sphere(SphereOp::PanTrack(Some(
            field
        )))))
        .is_err()
    );

    // Writing while playing: a gesture across the picture.
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    sphere(&mut s, SphereOp::WriteAutomation { track: t, on: true });
    let lane = |s: &Session, p: SurroundParam| {
        s.project()
            .track(t)
            .unwrap()
            .automation
            .lane(AutomationTarget::Surround(p))
            .cloned()
            .unwrap()
    };
    assert_eq!(
        lane(&s, SurroundParam::Azimuth).mode,
        faderframe_session::AutomationMode::Touch
    );
    assert!(s.sphere_pan_targets()[0].writing);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let wait = |s: &mut Session, ms: u64| {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
            s.tick(0.01);
        }
    };
    wait(&mut s, 150);
    s.dispatch(Action::BeginGesture("Pan on the Picture".into()))
        .unwrap();
    for i in 0..=10 {
        sphere(
            &mut s,
            SphereOp::PanTo {
                track: t,
                azimuth: -90.0 + 18.0 * i as f32,
                elevation: 30.0,
            },
        );
        wait(&mut s, 30);
    }
    s.dispatch(Action::EndGesture).unwrap();
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    let az = lane(&s, SurroundParam::Azimuth);
    let points = az.curve.points();
    assert!(points.len() >= 2, "{} points", points.len());
    assert!((points.first().unwrap().value + 90.0).abs() < 1.0);
    assert!((points.last().unwrap().value - 90.0).abs() < 1.0);
    assert!(
        lane(&s, SurroundParam::Elevation)
            .curve
            .points()
            .iter()
            .any(|p| (p.value - 30.0).abs() < 0.5)
    );
    // Back to reading.
    sphere(
        &mut s,
        SphereOp::WriteAutomation {
            track: t,
            on: false,
        },
    );
    assert_eq!(
        lane(&s, SurroundParam::Azimuth).mode,
        faderframe_session::AutomationMode::Read
    );
}

/// A flat movie marked 360° by hand; the movie written out of an
/// ambisonic project keeps the mark and the master's first order.
#[test]
fn the_movie_written_out_keeps_the_marks() {
    let _turn = turn();
    let d = dir("export");
    let flat = d.join("flat.mov");
    picture(&flat, "qtmux");
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::Third));
    s.dispatch(Action::Video(VideoOp::Import {
        path: flat,
        sound: false,
    }))
    .unwrap();
    s.wait_for_video();
    let source = *s.project().video.sources.keys().next().unwrap();
    assert_eq!(s.video_spherical(source), None);
    let tb = Spherical {
        stereo: StereoMode::TopBottom,
        ..Spherical::EQUIRECTANGULAR
    };
    sphere(
        &mut s,
        SphereOp::SetLayout {
            source,
            spherical: Some(tb),
        },
    );
    // Something to hear, panned up and to the left.
    let t = mono_track(&mut s);
    sphere(
        &mut s,
        SphereOp::PanTo {
            track: t,
            azimuth: 45.0,
            elevation: 30.0,
        },
    );
    let out = d.join("out.mov");
    s.dispatch(Action::Video(VideoOp::Export {
        clip: None,
        path: out.clone(),
        container: Container::Mov,
    }))
    .unwrap();
    s.wait_for_video();
    let info = faderframe_video::probe::probe(&out).unwrap();
    assert_eq!(info.spherical, Some(tb));
    assert_eq!(info.audio.len(), 1);
    assert_eq!(info.audio[0].channels, 4, "the first order of the field");
    assert_eq!(
        info.audio[0].ambisonic.as_ref().and_then(|a| a.field()),
        Some(AmbiOrder::First)
    );
    // Matroska: written, unmarked, every channel of the field.
    let mkv = d.join("out.mkv");
    s.dispatch(Action::Video(VideoOp::Export {
        clip: None,
        path: mkv.clone(),
        container: Container::Mkv,
    }))
    .unwrap();
    s.wait_for_video();
    let info = faderframe_video::probe::probe(&mkv).unwrap();
    assert_eq!(info.spherical, None);
    assert_eq!(info.audio[0].channels, 16);
    let _ = std::fs::remove_dir_all(&d);
}

#![allow(clippy::unwrap_used)]
//! Panning that follows the picture: an object boxed on a clip's first
//! frame is followed through the clip and becomes a track's pan (or its
//! surround position, with depth) — one undo step; corrections are keys.

use faderframe_automation::AutomationTarget;
use faderframe_core::{ChannelLayout, SurroundFormat, SurroundParam};
use faderframe_engine::EngineConfig;
use faderframe_project::video::ns_to_samples;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::object_tracking::{FollowOp, FollowTarget, Seen};
use faderframe_session::video::VideoOp;
use faderframe_session::{Action, Session};
use faderframe_video::track::Area;
use std::path::{Path, PathBuf};
use std::time::Duration;

const W: usize = 320;
const H: usize = 180;
const FRAMES: u32 = 60;
const SIDE: f32 = 40.0;

/// The video cache's folder is the process's: the tests take turns.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-follow-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    faderframe_session::video::set_cache_dir(Some(d.join("cache")));
    d
}

/// The object's centre on frame `n` (pixels).
fn centre(n: u32) -> (f32, f32) {
    (60.0 + 3.3 * n as f32, 90.0 + 10.0 * (n as f32 * 0.1).sin())
}

fn texture(x: f32, y: f32, seed: u32) -> f32 {
    let h = |i: i32, j: i32| {
        let mut v = (i as u32).wrapping_mul(73_856_093)
            ^ (j as u32).wrapping_mul(19_349_663)
            ^ seed.wrapping_mul(83_492_791);
        v ^= v >> 13;
        v = v.wrapping_mul(0x5bd1_e995);
        v ^= v >> 15;
        (v & 0xff) as f32
    };
    let (u, v) = (x / 5.0, y / 5.0);
    let (i, j) = (u.floor() as i32, v.floor() as i32);
    let (fx, fy) = (u - i as f32, v - j as f32);
    let a = h(i, j) * (1.0 - fx) + h(i + 1, j) * fx;
    let b = h(i, j + 1) * (1.0 - fx) + h(i + 1, j + 1) * fx;
    a * (1.0 - fy) + b * fy
}

/// A clip of a bright textured square moving over a dim textured ground,
/// MJPEG in Matroska at 25 fps.
fn moving_object(path: &Path) {
    use gst::prelude::*;
    faderframe_video::init().unwrap();
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width={W},height={H},framerate=25/1 ! videoconvert ! jpegenc quality=95 ! matroskamux ! filesink name=out"
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline
        .by_name("out")
        .unwrap()
        .set_property("location", path.to_string_lossy().as_ref());
    let src = pipeline
        .by_name("src")
        .unwrap()
        .downcast::<gst_app::AppSrc>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    for n in 0..FRAMES {
        let (cx, cy) = centre(n);
        let mut data = vec![0u8; W * H * 4];
        for y in 0..H {
            for x in 0..W {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let (dx, dy) = (fx - cx, fy - cy);
                let v = if dx.abs() < SIDE / 2.0 && dy.abs() < SIDE / 2.0 {
                    70.0 + 0.7 * texture(dx + 100.0, dy + 100.0, 1)
                } else {
                    0.25 * texture(fx, fy, 2)
                };
                let v = v.clamp(0.0, 255.0) as u8;
                let i = (y * W + x) * 4;
                data[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let mut buf = gst::Buffer::from_mut_slice(data);
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(n as u64 * 40_000_000));
            b.set_duration(gst::ClockTime::from_nseconds(40_000_000));
        }
        src.push_buffer(buf).unwrap();
    }
    src.end_of_stream().unwrap();
    let msg = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(60),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap();
    if let gst::MessageView::Error(e) = msg.view() {
        panic!("{}", e.error());
    }
    pipeline.set_state(gst::State::Null).unwrap();
}

fn area(n: u32) -> Area {
    let (cx, cy) = centre(n);
    Area::new(
        (cx - SIDE / 2.0) / W as f32,
        (cy - SIDE / 2.0) / H as f32,
        SIDE / W as f32,
        SIDE / H as f32,
    )
}

/// A session with the clip imported; the clip.
fn session(d: &Path) -> (Session, faderframe_core::VideoClipId) {
    let file = d.join("object.mkv");
    moving_object(&file);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file, false);
    s.wait_for_video();
    let clip = s.project().video.tracks[0].clips[0].id;
    (s, clip)
}

/// The lane's value at frame `n`'s start.
fn value_at(s: &Session, track: faderframe_core::TrackId, target: AutomationTarget, n: u32) -> f64 {
    let p = s.project();
    let c = &p.video.tracks[0].clips[0];
    let index = s.video_index(c.source).unwrap();
    let at = c.start + ns_to_samples(index.times[n as usize] - c.offset, p.sample_rate);
    let musical = p.timeline.to_musical(at, f64::from(p.sample_rate));
    p.track(track)
        .unwrap()
        .automation
        .lane(target)
        .unwrap()
        .curve
        .value_at(musical)
        .unwrap()
}

#[test]
fn a_tracks_pan_follows_an_object_across_the_picture() {
    let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
    let d = dir("pan");
    let (mut s, clip) = session(&d);
    let track = s.add_track(TrackKind::Audio).unwrap();
    assert_eq!(s.follow_target(track), Some(FollowTarget::Pan));
    // The box tool, then a box on the first frame.
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Draw(true))))
        .unwrap();
    assert!(s.follow_drawing());
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Key {
        clip,
        frame: 0,
        area: area(0),
    })))
    .unwrap();
    assert!(!s.follow_drawing(), "the tool is off once a box is drawn");
    // Applied as soon as it is followed through.
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Apply {
        track,
        depth: None,
    })))
    .unwrap();
    let steps = s.history_steps().0.len();
    assert!(s.wait_for_following(Duration::from_secs(120)));
    let status = s.follow_status().unwrap();
    assert!(!status.busy);
    assert_eq!(status.followed, FRAMES as usize, "{status:?}");
    assert_eq!(status.lost, 0);
    // Where it was found, frame by frame (in pixels of the picture).
    let mut worst = 0.0f32;
    for n in 1..FRAMES {
        let (a, seen) = s.followed_box(clip, n as usize).unwrap();
        assert_eq!(seen, Seen::Followed);
        let (cx, cy) = a.centre();
        let (tx, ty) = centre(n);
        worst = worst.max((cx * W as f32 - tx).abs().max((cy * H as f32 - ty).abs()));
    }
    eprintln!("worst {worst:.2} px");
    assert!(worst < 1.5, "worst {worst} px off");
    // The pan: −1 at the picture's left edge, 1 at its right.
    for n in [0, 10, 25, 40, 59] {
        let want = 2.0 * centre(n).0 as f64 / W as f64 - 1.0;
        let got = value_at(&s, track, AutomationTarget::TrackPan, n);
        assert!(
            (got - want).abs() < 0.02,
            "frame {n}: pan {got:.3}, want {want:.3}"
        );
    }
    // One undo step.
    assert_eq!(s.history_steps().0.len(), steps + 1);
    s.dispatch(Action::Undo).unwrap();
    assert!(
        s.project()
            .track(track)
            .unwrap()
            .automation
            .lane(AutomationTarget::TrackPan)
            .is_none()
    );
    // A correction is a key: it shows as drawn, the path goes on from it.
    let corrected = Area::new(area(30).x, area(30).y, area(30).w, area(30).h);
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Key {
        clip,
        frame: 30,
        area: corrected,
    })))
    .unwrap();
    assert!(s.wait_for_following(Duration::from_secs(120)));
    let status = s.follow_status().unwrap();
    assert_eq!(status.keys, vec![0, 30]);
    assert_eq!(status.followed, FRAMES as usize);
    assert_eq!(s.followed_box(clip, 30), Some((corrected, Seen::Key)));
    let (a, _) = s.followed_box(clip, 45).unwrap();
    assert!((a.centre().0 * W as f32 - centre(45).0).abs() < 1.5);
    // Taken back: followed on from the first key.
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::RemoveKey {
        frame: 30,
    })))
    .unwrap();
    assert!(s.wait_for_following(Duration::from_secs(120)));
    assert_eq!(s.follow_status().unwrap().keys, vec![0]);
    // The first key cannot be taken back (forgotten instead).
    assert!(
        s.dispatch(Action::Video(VideoOp::Follow(FollowOp::RemoveKey {
            frame: 0
        })))
        .is_err()
    );
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Forget)))
        .unwrap();
    assert!(s.follow_status().is_none());
    // A box off the clip's frames is refused.
    assert!(
        s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Key {
            clip,
            frame: 500,
            area: area(0),
        })))
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_track_in_a_surround_bed_moves_in_the_room_with_depth() {
    let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
    let d = dir("room");
    let (mut s, clip) = session(&d);
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S51),
    }))
    .unwrap();
    let track = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    assert_eq!(s.follow_target(track), Some(FollowTarget::Room));
    assert!(
        s.follow_tracks()
            .iter()
            .any(|(t, _, f)| *t == track && *f == FollowTarget::Room)
    );
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Key {
        clip,
        frame: 0,
        area: area(0),
    })))
    .unwrap();
    assert!(s.wait_for_following(Duration::from_secs(120)));
    s.dispatch(Action::Video(VideoOp::Follow(FollowOp::Apply {
        track,
        depth: Some(0.5),
    })))
    .unwrap();
    let x = AutomationTarget::Surround(SurroundParam::X);
    let y = AutomationTarget::Surround(SurroundParam::Y);
    for n in [5, 30, 55] {
        let want = 2.0 * centre(n).0 as f64 / W as f64 - 1.0;
        let got = value_at(&s, track, x, n);
        assert!(
            (got - want).abs() < 0.02,
            "frame {n}: X {got:.3}, want {want:.3}"
        );
        // The object keeps its size: it stays at the front.
        let depth = value_at(&s, track, y, n);
        assert!((depth - 1.0).abs() < 0.03, "frame {n}: Y {depth:.3}");
    }
    // Both lanes in one step.
    s.dispatch(Action::Undo).unwrap();
    let t = s.project().track(track).unwrap();
    assert!(t.automation.lane(x).is_none() && t.automation.lane(y).is_none());
    let _ = std::fs::remove_dir_all(&d);
}

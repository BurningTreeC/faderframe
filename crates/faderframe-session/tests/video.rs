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

/// Frame `n`'s colour in [`numbered_clip`]s (neighbours far apart).
fn colour(n: u32) -> [u8; 3] {
    [
        ((n * 37) % 200 + 20) as u8,
        ((n * 91) % 200 + 20) as u8,
        128,
    ]
}

/// A clip of `frames` flat-coloured 64×36 frames (frame `n` in
/// [`colour`]`(n)`) at `num/den` frames a second, MJPEG through `mux`.
fn numbered_clip(path: &std::path::Path, mux: &str, (num, den): (u64, u64), frames: u32) {
    use gst::prelude::*;
    faderframe_video::init().unwrap();
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width=64,height=36,framerate={num}/{den} ! videoconvert ! jpegenc ! {mux} ! filesink name=out"
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
    let at = |n: u32| n as u64 * 1_000_000_000 * den / num;
    for n in 0..frames {
        let c = colour(n);
        let data: Vec<u8> = (0..64 * 36).flat_map(|_| [c[0], c[1], c[2], 255]).collect();
        let mut buf = gst::Buffer::from_mut_slice(data);
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(at(n)));
            b.set_duration(gst::ClockTime::from_nseconds(at(n + 1) - at(n)));
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
        panic!("making {}: {}", path.display(), e.error());
    }
    pipeline.set_state(gst::State::Null).unwrap();
}

/// That the picture shown is frame `n`, not one beside it.
fn assert_frame(s: &faderframe_session::video::VideoShown, n: u32, what: &str) {
    let f = &s.picture.as_ref().unwrap().frame;
    let p = f.pixel(f.width / 2, f.height / 2);
    let off = |m: u32| -> i32 {
        let c = colour(m);
        (0..3).map(|i| (p[i] as i32 - c[i] as i32).abs()).sum()
    };
    let nearest = (n.saturating_sub(1)..=n + 1)
        .min_by_key(|&m| off(m))
        .unwrap();
    assert_eq!(
        nearest, n,
        "{what}: frame {n} wanted, {p:?} is frame {nearest}'s"
    );
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

    // Onto the first movie: asked first; Cancel keeps it, Overwrite
    // replaces it once the new one is written (beside it until then).
    let first = d.join("out.mov");
    let ask = |s: &mut Session| {
        s.dispatch(Action::Video(VideoOp::Export {
            clip: Some(clip.id),
            path: first.clone(),
            container: Container::Mov,
        }))
        .unwrap();
        let q: Vec<_> = s
            .take_ui_requests()
            .into_iter()
            .filter_map(|r| match r {
                faderframe_session::UiRequest::AskReplace(q) => Some(q),
                _ => None,
            })
            .collect();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].paths, std::slice::from_ref(&first));
        q[0].id
    };
    let long = |p: &std::path::Path| faderframe_video::probe::probe(p).unwrap().duration_ns;
    let id = ask(&mut s);
    s.dispatch(Action::AnswerReplace {
        question: id,
        replace: false,
    })
    .unwrap();
    s.wait_for_video();
    assert!((long(&first) - 10_000_000_000).abs() < 100_000_000);
    let id = ask(&mut s);
    s.dispatch(Action::AnswerReplace {
        question: id,
        replace: true,
    })
    .unwrap();
    s.wait_for_video();
    assert!((long(&first) - 3_000_000_000).abs() < 100_000_000);
    for e in std::fs::read_dir(&d).unwrap() {
        let p = e.unwrap().path();
        assert!(
            !faderframe_session::render::files::is_temporary(&p),
            "{}",
            p.display()
        );
    }
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

/// Play starts on the frame the playhead stands on. The sound is heard a
/// latency after it is processed (a long device buffer here: 2048 frames,
/// 43 ms, more than a frame); until then what is heard is the stopped
/// position, so the picture waits there too — never the frame before it
/// (nor no picture at the clip's start) — and then moves on with the
/// sound.
#[test]
fn play_starts_on_the_frame_the_playhead_stands_on() {
    let _turn = turn();
    let d = dir("start");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file, false);
    s.wait_for_video();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            buffer_size: Some(2048),
            ..Default::default()
        },
    )
    .unwrap();
    // The clip's start, and frame 50's first sample (two seconds in).
    for (at, frame) in [(0, 0), (96_000, 50)] {
        s.dispatch(Action::Transport(TransportAction::Locate(
            s.engine().samples_to_musical(s.project(), at),
        )))
        .unwrap();
        let v = still(&mut s);
        assert_eq!((v.frame, v.position), (frame, at));
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        let start = Instant::now();
        let mut shown = Vec::new();
        // How far the picture is from the playhead heard (the arranger's).
        let mut apart = Vec::new();
        while start.elapsed() < Duration::from_millis(300) {
            s.tick(0.001);
            let v = s.video_picture(0, (64, 36));
            apart.push(v.as_ref().map(|v| v.position - s.transport().position));
            shown.push(v.map(|v| (v.frame, v.position)));
            std::thread::sleep(Duration::from_millis(1));
        }
        s.dispatch(Action::Transport(TransportAction::Stop))
            .unwrap();
        let stopped_by = Instant::now() + Duration::from_secs(5);
        while s.transport().playing {
            assert!(Instant::now() < stopped_by, "still playing");
            s.tick(0.01);
            std::thread::sleep(Duration::from_millis(5));
        }
        let first_wrong = shown
            .iter()
            .position(|v| v.is_none_or(|(f, p)| f < frame || p < at));
        assert!(
            first_wrong.is_none(),
            "from frame {frame}: {:?} shown at poll {} (a millisecond apart)",
            shown[first_wrong.unwrap_or(0)],
            first_wrong.unwrap_or(0),
        );
        assert_eq!(shown[0], Some((frame, at)), "the frame standing first");
        let frames: Vec<usize> = shown.iter().flatten().map(|v| v.0).collect();
        assert!(frames.windows(2).all(|w| w[1] >= w[0]), "never back");
        assert!(
            *frames.last().unwrap() >= frame + 4,
            "played on: {frames:?}"
        );
        // The picture is the one for the sound heard, all along.
        let worst = apart.iter().flatten().map(|d| d.abs()).max().unwrap();
        eprintln!("from frame {frame}: at most {worst} samples from the playhead heard");
        assert!(worst < 1920, "{worst} samples from the playhead heard");
    }
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&d);
}

/// A locate right after Stop shows its target at once and stays there —
/// the playhead and the picture never show where playback stopped in
/// between: whether the engine applies both in one callback, or the stop
/// first (a click a few milliseconds after Stop, callbacks in between).
/// The stop is a jump too, and it released the shown target before the
/// locate had landed; the picture, still playing until the next tick,
/// showed where the sound was.
#[test]
fn a_locate_right_after_stop_shows_its_target() {
    let _turn = turn();
    let d = dir("stop-locate");
    let file = movie(&d);
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    s.import_video(file, false);
    s.wait_for_video();
    // 256 frames: a callback every 5 ms.
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    for round in 0..6i64 {
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(250) {
            s.tick(0.01);
            std::thread::sleep(Duration::from_millis(10));
        }
        let target = 96_000 + round * 1920;
        s.dispatch(Action::Transport(TransportAction::Stop))
            .unwrap();
        if round % 2 == 1 {
            // The engine stops before the locate comes (the session not
            // ticked meanwhile, as between two clicks).
            std::thread::sleep(Duration::from_millis(30));
        }
        s.dispatch(Action::Transport(TransportAction::Locate(
            s.engine().samples_to_musical(s.project(), target),
        )))
        .unwrap();
        let start = Instant::now();
        let mut shown = Vec::new();
        while start.elapsed() < Duration::from_millis(300) {
            s.tick(0.002);
            let picture = s.video_picture(0, (64, 36)).map(|v| v.position);
            shown.push((s.transport().position, picture));
            std::thread::sleep(Duration::from_millis(2));
        }
        let wrong = shown
            .iter()
            .position(|&(p, v)| p != target || v != Some(target));
        assert!(
            wrong.is_none(),
            "round {round}: {:?} shown at poll {} (playhead, picture), the target {target}",
            shown[wrong.unwrap_or(0)],
            wrong.unwrap_or(0)
        );
        assert!(!s.transport().playing);
    }
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&d);
}

/// A timecode typed lands on its frame: the counter reads it back and the
/// picture is that frame — at the rates whose frames start between two
/// samples (29.97, 23.976 and 59.94, drop-frame or not, through a
/// drop-frame minute; 44.1 and 48 kHz), from files with exact frame times
/// (QuickTime) and rounded ones (Matroska's milliseconds), and with the
/// clip spotted to a label that starts between two samples. (The nearest
/// sample to a label lay before its frame for up to half of them: the
/// counter and the picture showed the frame before.)
#[test]
fn typed_timecodes_land_on_their_frames() {
    use faderframe_core::timecode::{FrameRate::*, Timecode};
    use faderframe_project::video::ProjectTimecode;
    use faderframe_session::{CounterUnit, format_position, parse_position};
    let _turn = turn();
    let d = dir("timecode");
    const N: u32 = 60;
    let rates: [((u64, u64), &[FrameRate]); 3] = [
        ((30_000, 1001), &[Fps2997, Fps2997Drop]),
        ((24_000, 1001), &[Fps23976]),
        ((60_000, 1001), &[Fps5994, Fps5994Drop]),
    ];
    for (fps, labels) in rates {
        for (mux, ext) in [("qtmux", "mov"), ("matroskamux", "mkv")] {
            let file = d.join(format!("{}.{ext}", fps.0));
            numbered_clip(&file, mux, fps, N);
            for sr in [44_100, 48_000] {
                for &rate in labels {
                    let what = |label: Timecode| {
                        format!("{} {ext} at {sr} Hz: {}", rate.id(), label.display(rate))
                    };
                    // Labels through a drop-frame minute (00:01:00;00 and
                    // ;01 are skipped at 29.97 drop-frame).
                    let start = Timecode::parse("00:00:59:00", rate).unwrap();
                    let tc = ProjectTimecode { rate, start };
                    let first = start.total_frames(rate);
                    let config = EngineConfig {
                        sample_rate: sr,
                        ..EngineConfig::default()
                    };
                    let mut s = Session::new(Project::new("TC", sr), None, config).unwrap();
                    s.edit(faderframe_project::Command::SetTimecode { timecode: Some(tc) })
                        .unwrap();
                    s.import_video(file.clone(), false);
                    s.wait_for_video();
                    // Spotted at the clip's start, then at the label of
                    // frame 7 (between two samples at these rates).
                    for spot in [0, 7] {
                        if spot > 0 {
                            let mut video = s.project().video.clone();
                            video.tracks[0].clips[0].start = rate.first_sample(spot, sr);
                            s.edit(faderframe_project::Command::SetVideo {
                                video: Box::new(video),
                            })
                            .unwrap();
                        }
                        for k in 0..N as i64 - 1 {
                            let label = Timecode::from_frames(first + spot + k, rate);
                            let text = label.display(rate);
                            let timeline = s.project().timeline.clone();
                            let at =
                                parse_position(&text, CounterUnit::Timecode, &timeline, sr, tc)
                                    .unwrap();
                            s.dispatch(Action::Transport(TransportAction::Locate(at)))
                                .unwrap();
                            let pos = s.transport().position;
                            assert_eq!(pos, rate.first_sample(spot + k, sr), "{}", what(label));
                            assert_eq!(
                                format_position(s.project(), pos, CounterUnit::Timecode),
                                text,
                                "{}: the counter",
                                what(label)
                            );
                            let v = s.video_picture(0, (64, 36)).unwrap();
                            assert_eq!(v.frame, k as usize, "{}: the picture", what(label));
                        }
                    }
                    // The pictures themselves (the clip at frame 7's label).
                    for k in [1u32, 2, 3, 5, 8, 13] {
                        let label = Timecode::from_frames(first + 7 + k as i64, rate);
                        let at = s
                            .engine()
                            .samples_to_musical(s.project(), tc.position_of(label, sr));
                        s.dispatch(Action::Transport(TransportAction::Locate(at)))
                            .unwrap();
                        let v = still(&mut s);
                        assert_eq!(v.frame, k as usize);
                        assert_frame(&v, k, &what(label));
                    }
                }
            }
        }
    }
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
    // Stopped on frame 50's first sample: frame 50, not the one the
    // sound's latency back.
    let stopped_by = Instant::now() + Duration::from_secs(5);
    while s.transport().playing {
        assert!(Instant::now() < stopped_by, "still playing");
        s.tick(0.01);
        std::thread::sleep(Duration::from_millis(5));
    }
    s.dispatch(Action::Transport(TransportAction::Locate(
        s.engine().samples_to_musical(s.project(), 96_000),
    )))
    .unwrap();
    let shown_by = Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while Instant::now() < shown_by && last != Some(50) {
        s.tick(0.01);
        last = s.picture_output().and_then(|(_, _, l)| l);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(last, Some(50), "the frame stopped on");
    s.set_picture_output(None).unwrap();
    assert!(s.picture_output().is_none());
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&d);
}

//! Conforming to a new picture cut: from two EDLs (the old cut and the new
//! one), and from two pictures matched frame by frame.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::timecode::{FrameRate, Timecode};
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::video::ProjectTimecode;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Marker, Project, SourceSpec,
    StretchSettings, TrackKind,
};
use faderframe_session::conform::ConformOp;
use faderframe_session::{Action, Session};
use std::path::PathBuf;

const SR: u32 = 48_000;
const S: i64 = SR as i64;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-conform-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A session at timecode 01:00:00:00 (25 fps) with one dialogue clip over
/// the first 15 seconds and a marker at 12 s.
fn setup() -> (Session, TrackId, ClipId) {
    let mut s = Session::new(Project::new("Reel 1", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    let source = AudioSourceId(9000);
    let at = |s: &Session, x: i64| s.project().timeline.to_musical(x, SR as f64);
    let mut cmds = vec![
        Command::SetTimecode {
            timecode: Some(ProjectTimecode {
                rate: FrameRate::Fps25,
                start: Timecode::parse("01:00:00:00", FrameRate::Fps25).unwrap(),
            }),
        },
        Command::AddSource {
            source: Box::new(AudioSource {
                id: source,
                name: "dialogue".into(),
                spec: SourceSpec::Generated {
                    generator: GeneratorSpec::DrumLoop {
                        bpm: 120.0,
                        bars: 16,
                        seed: 1,
                    },
                },
            }),
        },
    ];
    let clip = ClipId(10_000);
    cmds.push(Command::AddClip {
        clip: Box::new(Clip {
            id: clip,
            track: t,
            name: "dialogue".into(),
            color: None,
            start: at(&s, 0),
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: 15 * S,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
            }),
        }),
    });
    cmds.push(Command::AddMarker {
        marker: Marker {
            id: faderframe_core::MarkerId(77_000),
            position: at(&s, 12 * S),
            name: "line 4".into(),
        },
    });
    for c in cmds {
        s.dispatch(Action::Edit(c)).unwrap();
    }
    (s, t, clip)
}

/// The clips of `t`: (start, source offset, length) in samples.
fn parts(s: &Session, t: TrackId) -> Vec<(i64, i64, i64)> {
    let p = s.project();
    let mut out: Vec<_> = p
        .track(t)
        .unwrap()
        .clips
        .iter()
        .map(|c| {
            let c = &p.clips[c];
            let a = c.as_audio().unwrap();
            (
                p.timeline.to_samples(c.start, SR as f64),
                a.source_offset,
                a.length,
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_new_cut_from_two_edls() {
    let d = dir("edl");
    let (mut s, t, _) = setup();
    // The old cut: shot A for 10 s, shot B for 5 s.
    let old = d.join("old.edl");
    std::fs::write(
        &old,
        "TITLE: REEL 1 V1\nFCM: NON-DROP FRAME\n\n\
         001  A001  V  C  10:00:00:00 10:00:10:00 01:00:00:00 01:00:10:00\n\
         002  B002  V  C  20:00:00:00 20:00:05:00 01:00:10:00 01:00:15:00\n",
    )
    .unwrap();
    // The new cut: B first, a new 2 s shot, then A without its first 3 s.
    let new = d.join("new.edl");
    std::fs::write(
        &new,
        "TITLE: REEL 1 V2\nFCM: NON-DROP FRAME\n\n\
         001  B002  V  C  20:00:00:00 20:00:05:00 01:00:00:00 01:00:05:00\n\
         002  N003  V  C  13:00:00:00 13:00:02:00 01:00:05:00 01:00:07:00\n\
         003  A001  V  C  10:00:03:00 10:00:10:00 01:00:07:00 01:00:14:00\n",
    )
    .unwrap();
    // An ADR cue on the line at 12 s.
    let (c0, c1) = {
        let tl = &s.project().timeline;
        (
            tl.to_musical(12 * S, SR as f64),
            tl.to_musical(13 * S, SR as f64),
        )
    };
    s.dispatch(Action::Adr(faderframe_session::adr::AdrOp::Add {
        start: c0,
        end: c1,
        text: "line 4".into(),
        track: Some(t),
    }))
    .unwrap();
    let before = parts(&s, t);
    s.dispatch(Action::Conform(ConformOp::Lists {
        old: old.clone(),
        new: new.clone(),
    }))
    .unwrap();
    // B's sound (source 10–15 s) at 0, A's from its 3rd second at 7 s.
    assert_eq!(
        parts(&s, t),
        vec![(0, 10 * S, 5 * S), (7 * S, 3 * S, 7 * S)]
    );
    let p = s.project();
    let names: Vec<(&str, i64)> = p
        .markers
        .iter()
        .map(|m| {
            (
                m.name.as_str(),
                p.timeline.to_samples(m.position, SR as f64),
            )
        })
        .collect();
    assert!(names.contains(&("line 4", 2 * S)), "{names:?}");
    assert!(names.contains(&("New shot 1", 5 * S)), "{names:?}");
    // The cue went with its line.
    let cue = &p.adr.cues[0];
    assert_eq!(p.timeline.to_samples(cue.start, SR as f64), 2 * S);
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(parts(&s, t), before);
    let _ = std::fs::remove_dir_all(&d);
}

/// Makes a movie of `shots` (each: its colour seed, first frame, frames),
/// every frame different (a moving bar) so that frames can be matched.
fn movie(path: &std::path::Path, shots: &[(u8, u32, u32)]) {
    use gst::prelude::*;
    faderframe_video::init().unwrap();
    let (w, h) = (160u32, 90u32);
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width={w},height={h},framerate=25/1 ! videoconvert ! jpegenc quality=95 ! matroskamux ! filesink name=out"
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
    let mut n = 0u64;
    for &(seed, first, frames) in shots {
        for k in first..first + frames {
            let mut data = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    // A shot's own pattern, and a bar that moves a pixel a
                    // frame.
                    let base =
                        ((x / 20 + y / 15) as u8).wrapping_mul(seed.wrapping_mul(37)) / 2 + 40;
                    let bar = if (x + 160 - (k % 160)) % 160 < 8 {
                        120
                    } else {
                        0
                    };
                    let v = base.saturating_add(bar);
                    data.extend_from_slice(&[v, v / 2 + seed, 255 - v, 255]);
                }
            }
            let mut buf = gst::Buffer::from_mut_slice(data);
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_mseconds(n * 40));
            b.set_duration(gst::ClockTime::from_mseconds(40));
            src.push_buffer(buf).unwrap();
            n += 1;
        }
    }
    src.end_of_stream().unwrap();
    pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(60),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap();
    pipeline.set_state(gst::State::Null).unwrap();
}

#[test]
fn a_new_picture_matched_shot_by_shot() {
    use faderframe_session::video::VideoOp;
    let d = dir("picture");
    let cache = d.join("cache");
    faderframe_session::video::set_cache_dir(Some(cache));
    // Old: shot 1 (4 s), shot 2 (4 s); new: shot 2, then shot 1 less its
    // first second.
    let (old, new) = (d.join("old.mkv"), d.join("new.mkv"));
    movie(&old, &[(1, 0, 100), (2, 0, 100)]);
    movie(&new, &[(2, 0, 100), (1, 25, 75)]);
    let (mut s, t, _) = setup();
    s.import_video(old, false);
    s.wait_for_video();
    s.import_video(new, false);
    s.wait_for_video();
    let clips: Vec<_> = s.project().video.tracks[0]
        .clips
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(clips.len(), 2);
    // Both pictures from the timeline's start, the new one on a track of
    // its own.
    s.dispatch(Action::Video(VideoOp::AddTrack)).unwrap();
    let second = s.project().video.tracks[1].id;
    for (c, track) in [(clips[0], None), (clips[1], Some(second))] {
        s.dispatch(Action::Video(VideoOp::MoveClip {
            clip: c,
            start: 0,
            track,
        }))
        .unwrap();
    }
    s.dispatch(Action::Video(VideoOp::ConformPicture {
        old: clips[0],
        new: clips[1],
    }))
    .unwrap();
    s.wait_for_video();
    // The dialogue (0–15 s) followed: shot 2's part (4–8 s) at 0, shot 1's
    // from 1 s (1–4 s) at 4 s; what followed the old picture (8–15 s)
    // after the new one's end (7 s).
    assert_eq!(
        parts(&s, t),
        vec![(0, 4 * S, 4 * S), (4 * S, S, 3 * S), (7 * S, 8 * S, 7 * S)]
    );
    faderframe_session::video::set_cache_dir(None);
    let _ = std::fs::remove_dir_all(&d);
}

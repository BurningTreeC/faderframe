//! IAMF masters from a session: a 7.1.4 mix (and a stereo one) rendered
//! with each codec, read back, its loudness measured as IAMF wants it;
//! when FFmpeg is installed, decoded by it.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan};
use faderframe_engine::EngineConfig;
use faderframe_iamf::{Codec, read};
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::render::{RenderChannels, RenderRange, RenderSettings};
use faderframe_session::{Action, Session};

fn session(dir: &std::path::Path, master: ChannelLayout) -> Session {
    let mut s = Session::new(
        Project::new("Immersive", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let m = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: m,
        layout: master,
    }))
    .unwrap();
    let file = dir.join("tone.wav");
    let tone: Vec<f32> = (0..96_000)
        .map(|i| 0.3 * (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin())
        .collect();
    write_wav(&file, &[tone], 48_000, WavFormat::Float32, false).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    s.import_audio(
        vec![file],
        faderframe_session::ImportTarget {
            track: Some(t),
            at: faderframe_timeline::MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    // At the front left, up high.
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: t,
        pan: SurroundPan {
            x: -1.0,
            y: 1.0,
            z: 1.0,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    s
}

#[test]
fn a_714_mix_becomes_an_iamf_master() {
    // `FADERFRAME_IAMF_SESSION_OUT=<dir>` keeps the files.
    let keep = std::env::var("FADERFRAME_IAMF_SESSION_OUT").ok();
    let dir = keep.as_ref().map_or_else(
        || std::env::temp_dir().join(format!("ff-iamf-session-{}", std::process::id())),
        std::path::PathBuf::from,
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = session(&dir, ChannelLayout::Surround(SurroundFormat::S714));
    let plan = faderframe_session::iamf::plan(s.project());
    assert_eq!(plan.describe(), "7.1.4");
    for (codec, name) in [
        (Codec::Lpcm { bits: 24 }, "lpcm.iamf"),
        (Codec::Flac { bits: 24 }, "flac.mp4"),
        (
            Codec::Opus {
                stereo_bitrate: 192_000,
            },
            "opus.iamf",
        ),
    ] {
        let out = dir.join(name);
        let job = s
            .render(RenderSettings {
                range: RenderRange::Bars { start: 0, end: 2 },
                channels: RenderChannels::Iamf(codec),
                tail_seconds: 0.0,
                ..RenderSettings::defaults_for(s.project(), out.clone())
            })
            .unwrap();
        let rendered = job.join().unwrap();
        assert_eq!(rendered[0].path, out);
        if name.ends_with(".iamf") {
            let seq = read::parse(&std::fs::read(&out).unwrap()).unwrap();
            assert_eq!(seq.layout, 7, "7.1.4");
            assert_eq!((seq.substreams, seq.coupled), (7, 5));
            assert_eq!(seq.label, "Immersive");
            assert_eq!(seq.headphones_mode, 1, "binaural on headphones");
            // Stereo and 7.1.4 (sound system J) loudness: the tone's.
            assert_eq!(seq.loudness.len(), 2);
            assert_eq!((seq.loudness[0].0, seq.loudness[1].0), (0, 9));
            let lufs = f64::from(seq.loudness[0].1) / 256.0;
            assert!((-30.0..-10.0).contains(&lufs), "{lufs}");
            // The top front left carries the tone: its substream (Ltf/Rtf,
            // the fourth) is the loud one.
            if let Codec::Lpcm { .. } = codec {
                let frame = |sub: u8| {
                    seq.frames
                        .iter()
                        .filter(|f| f.0 == sub)
                        .nth(10)
                        .map(|f| f.3.clone())
                        .unwrap()
                };
                let level = |b: &[u8]| {
                    b.as_chunks::<3>()
                        .0
                        .iter()
                        .map(|c| i32::from_le_bytes([0, c[0], c[1], c[2]]).unsigned_abs() >> 8)
                        .max()
                        .unwrap()
                };
                assert!(level(&frame(3)) > 1_000_000, "Ltf");
                assert!(level(&frame(0)) < 10_000, "L");
            }
        }
        if std::process::Command::new("ffprobe")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            let probe = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-show_stream_groups", "-of", "json"])
                .arg(&out)
                .output()
                .unwrap();
            assert!(
                probe.status.success(),
                "{}",
                String::from_utf8_lossy(&probe.stderr)
            );
            let json = String::from_utf8_lossy(&probe.stdout);
            assert!(json.contains("IAMF Mix Presentation"), "{json}");
        }
    }
    // A stereo master: one coupled substream, no binaural rendering.
    let mut st = session(&dir, ChannelLayout::Stereo);
    let out = dir.join("stereo.iamf");
    st.render(RenderSettings {
        range: RenderRange::Bars { start: 0, end: 1 },
        channels: RenderChannels::Iamf(Codec::Lpcm { bits: 16 }),
        tail_seconds: 0.0,
        ..RenderSettings::defaults_for(st.project(), out.clone())
    })
    .unwrap()
    .join()
    .unwrap();
    let seq = read::parse(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!((seq.layout, seq.substreams, seq.coupled), (1, 1, 1));
    assert_eq!(seq.headphones_mode, 0);
    assert_eq!(seq.loudness.len(), 1);
    if keep.is_none() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

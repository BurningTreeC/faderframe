#![allow(clippy::unwrap_used)]
//! 360° movies made here: an equirectangular "compass" (red ahead, green
//! to the left, blue to the right, yellow behind) and a first-order AmbiX
//! sound whose four channels have levels of their own. Written by `mux`
//! with the marks, probed back, decoded (the media still where the index
//! says), its sound taken out channel for channel, looked into — and, when
//! FFmpeg's `ffprobe` is installed, read by a second implementation.

use faderframe_audio_files::import::ImportProgress;
use faderframe_core::AmbiOrder;
use faderframe_core::ambisonics::{Orientation, angles};
use faderframe_core::spherical::{Mapping, Spherical, StereoMode, Viewport};
use faderframe_video::mux::{Container, MuxOptions, mux};
use faderframe_video::reproject::{Source, reproject};
use faderframe_video::{Decoder, audio, has_element, probe};
use gst::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const W: u32 = 256;
const H: u32 = 128;
const FPS: u64 = 25;
const FRAMES: u32 = 25;
/// The sound's channels' levels: W, Y, Z, X.
const LEVELS: [f32; 4] = [0.5, 0.3, 0.2, 0.1];

fn dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("ff-video-360-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn compass(d: [f32; 3]) -> [u8; 3] {
    let (az, _) = angles(d);
    if az.abs() <= 45.0 {
        [230, 20, 20]
    } else if az > 45.0 && az <= 135.0 {
        [20, 230, 20]
    } else if (-135.0..-45.0).contains(&az) {
        [20, 20, 230]
    } else {
        [230, 230, 20]
    }
}

/// `FRAMES` frames of the compass through `encoder` into `mux`.
fn compass_clip(path: &Path, encoder: &str, mux: &str) {
    faderframe_video::init().unwrap();
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width={W},height={H},framerate={FPS}/1 ! videoconvert ! {encoder} ! queue ! {mux} ! filesink name=out"
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
    let m = Mapping::new(Spherical::EQUIRECTANGULAR, W, H);
    let picture: Vec<u8> = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let c = compass(m.direction_at(x as f32 + 0.5, y as f32 + 0.5).unwrap());
            [c[0], c[1], c[2], 255]
        })
        .collect();
    pipeline.set_state(gst::State::Playing).unwrap();
    for n in 0..FRAMES as u64 {
        let mut buf = gst::Buffer::from_mut_slice(picture.clone());
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(n * 1_000_000_000 / FPS));
            b.set_duration(gst::ClockTime::from_nseconds(1_000_000_000 / FPS));
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

/// 1 s of a 300 Hz tone at 48 kHz, channel `c` at `levels[c]`.
fn sound(path: &Path, levels: &[f32]) {
    let n = 48_000;
    let channels: Vec<Vec<f32>> = levels
        .iter()
        .map(|l| {
            (0..n)
                .map(|i| l * (i as f32 * 300.0 * std::f32::consts::TAU / 48_000.0).sin())
                .collect()
        })
        .collect();
    faderframe_audio_files::write_wav(
        path,
        &channels,
        48_000,
        faderframe_audio_files::WavFormat::Pcm24,
        false,
    )
    .unwrap();
}

fn rms(c: &[f32]) -> f32 {
    // Away from the ends (codec delay, fades).
    let mid = &c[c.len() / 4..c.len() * 3 / 4];
    (mid.iter().map(|v| v * v).sum::<f32>() / mid.len() as f32).sqrt()
}

/// The movie's sound, taken out as a project would (every channel).
fn levels_of(movie: &Path, d: &Path) -> Vec<f32> {
    let cancel = AtomicBool::new(false);
    let got = audio::extract_audio(
        movie,
        0,
        &d.join("media"),
        48_000,
        &ImportProgress::default(),
        &cancel,
    )
    .unwrap();
    let data = faderframe_audio_files::read_wav(&got.path).unwrap();
    data.channels.iter().map(|c| rms(c)).collect()
}

/// The decoded frame shows the compass where it was drawn.
fn assert_compass(movie: &Path) {
    let mut dec = Decoder::open(movie, W, H).unwrap();
    let f = dec.frame_at(400_000_000, true).unwrap().unwrap();
    let near = |x: u32, y: u32, want: [u8; 3]| {
        let p = f.pixel(x, y);
        let off: i32 = (0..3).map(|i| (p[i] as i32 - want[i] as i32).abs()).sum();
        assert!(off < 60, "({x},{y}) {p:?} not {want:?}");
    };
    near(W / 2, H / 2, [230, 20, 20]);
    near(W / 4, H / 2, [20, 230, 20]);
    near(3 * W / 4, H / 2, [20, 20, 230]);
    // Looked into to the left: green in the middle of the view.
    let m = Mapping::new(Spherical::EQUIRECTANGULAR, f.width, f.height);
    let view = Viewport::new(Orientation::from_ypr(90.0, 0.0, 0.0), 60.0, 64.0, 36.0);
    let mut out = Vec::new();
    reproject(
        Source {
            rgba: &f.rgba,
            width: f.width,
            height: f.height,
        },
        &m,
        &view,
        &mut out,
    );
    let i = (18 * 64 + 32) * 4;
    assert!(out[i + 1] > 180 && out[i] < 80, "{:?}", &out[i..i + 4]);
}

/// `ffprobe`'s reading of a stream (`None`: no ffprobe here).
fn ffprobe(movie: &Path, select: &str, entries: &str) -> Option<String> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            select,
            "-show_entries",
            entries,
        ])
        .arg(movie)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn a_quicktime_movie_carries_a_360_picture_and_an_ambisonic_sound() {
    let d = dir();
    let clip = d.join("compass.mov");
    compass_clip(&clip, "jpegenc", "qtmux");
    assert_eq!(probe::probe(&clip).unwrap().spherical, None);
    let wav = d.join("field.wav");
    sound(&wav, &LEVELS);
    let out = d.join("movie.mov");
    let cancel = AtomicBool::new(false);
    mux(
        &clip,
        std::slice::from_ref(&wav),
        &out,
        Container::Mov,
        MuxOptions {
            spherical: Some(Spherical::EQUIRECTANGULAR),
            ambisonic: Some(AmbiOrder::First),
            ..Default::default()
        },
        &cancel,
        |_| {},
    )
    .unwrap();
    let info = probe::probe(&out).unwrap();
    assert_eq!(info.spherical, Some(Spherical::EQUIRECTANGULAR));
    assert_eq!(info.audio.len(), 1);
    assert_eq!(info.audio[0].channels, 4, "every channel kept");
    assert_eq!(
        info.audio[0].ambisonic.as_ref().and_then(|a| a.field()),
        Some(AmbiOrder::First)
    );
    // Still a movie: the picture decodes, the sound comes out channel for
    // channel at its levels (24-bit).
    assert_compass(&out);
    let levels = levels_of(&out, &d);
    assert_eq!(levels.len(), 4);
    for (got, want) in levels.iter().zip(LEVELS) {
        let want = want / std::f32::consts::SQRT_2;
        assert!((got - want).abs() < want * 0.02, "{levels:?}");
    }
    // A second reader agrees.
    if let Some(v) = ffprobe(&out, "v:0", "stream_side_data") {
        assert!(v.contains("Spherical Mapping"), "{v}");
        assert!(v.contains("equirectangular"), "{v}");
    }
    if let Some(a) = ffprobe(&out, "a:0", "stream=channels,channel_layout") {
        assert!(a.contains("channels=4"), "{a}");
        assert!(a.contains("ambisonic 1"), "{a}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_mp4_movie_carries_them_with_opus() {
    if !(has_element("vp9enc") && has_element("opusenc")) {
        eprintln!("no vp9enc or opusenc: skipped");
        return;
    }
    let d = dir();
    let clip = d.join("compass.mkv");
    compass_clip(&clip, "vp9enc deadline=1 cpu-used=8", "matroskamux");
    let wav = d.join("field.wav");
    sound(&wav, &LEVELS);
    let out = d.join("movie.mp4");
    let cancel = AtomicBool::new(false);
    let tb = Spherical {
        stereo: StereoMode::TopBottom,
        ..Spherical::EQUIRECTANGULAR
    };
    mux(
        &clip,
        std::slice::from_ref(&wav),
        &out,
        Container::Mp4,
        MuxOptions {
            spherical: Some(tb),
            ambisonic: Some(AmbiOrder::First),
            ..Default::default()
        },
        &cancel,
        |_| {},
    )
    .unwrap();
    let info = probe::probe(&out).unwrap();
    assert_eq!(info.spherical, Some(tb));
    assert_eq!(info.audio[0].channels, 4);
    assert_eq!(
        info.audio[0].ambisonic.as_ref().and_then(|a| a.field()),
        Some(AmbiOrder::First)
    );
    // Opus says the field too: channel mapping family 2.
    let bytes = std::fs::read(&out).unwrap();
    let i = bytes.windows(4).position(|w| w == b"dOps").unwrap();
    assert_eq!(bytes[i + 4 + 10], 2, "mapping family");
    // Lossy, every channel its own level still.
    let levels = levels_of(&out, &d);
    assert_eq!(levels.len(), 4);
    for (got, want) in levels.iter().zip(LEVELS) {
        let want = want / std::f32::consts::SQRT_2;
        assert!((got - want).abs() < want * 0.1, "{levels:?}");
    }
    if let Some(v) = ffprobe(&out, "v:0", "stream_side_data") {
        assert!(v.contains("Spherical Mapping"), "{v}");
    }
    if let Some(a) = ffprobe(&out, "a:0", "stream=channels,channel_layout") {
        assert!(a.contains("ambisonic 1"), "{a}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Matroska keeps every channel (with the WAV's speakers) and carries no
/// marks; QuickTime keeps more than two plain channels too (they were
/// folded down to two before).
#[test]
fn more_than_two_channels_are_kept_in_every_container() {
    let d = dir();
    let clip = d.join("compass.mov");
    compass_clip(&clip, "jpegenc", "qtmux");
    let wav = d.join("six.wav");
    sound(&wav, &[0.5, 0.4, 0.3, 0.2, 0.1, 0.05]);
    let cancel = AtomicBool::new(false);
    for container in [Container::Mov, Container::Mkv] {
        let out = d.join(format!("six.{}", container.extension()));
        mux(
            &clip,
            std::slice::from_ref(&wav),
            &out,
            container,
            MuxOptions {
                spherical: Some(Spherical::EQUIRECTANGULAR),
                ..Default::default()
            },
            &cancel,
            |_| {},
        )
        .unwrap();
        let info = probe::probe(&out).unwrap();
        assert_eq!(info.audio[0].channels, 6, "{container:?}");
        assert_eq!(info.audio[0].ambisonic, None);
        assert_eq!(
            info.spherical.is_some(),
            container.carries_spherical(),
            "{container:?}"
        );
        let levels = levels_of(&out, &d);
        assert_eq!(levels.len(), 6, "{container:?}");
        assert!(levels[0] > levels[1] && levels[4] > levels[5], "{levels:?}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

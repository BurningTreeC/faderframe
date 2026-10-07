#![allow(clippy::unwrap_used)]
//! The video crate on clips made here: every frame a flat colour of its
//! own number, so a decoded frame says which one it is.

use faderframe_video::mux::{Container, mux};
use faderframe_video::proxy::{ProxySpec, make_proxy};
use faderframe_video::{Decoder, audio, has_element, index, probe};
use gst::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const W: u32 = 320;
const H: u32 = 240;
const FPS: i32 = 25;
const FRAMES: u32 = 75;

fn colour(n: u32) -> [u8; 3] {
    [
        ((n * 37) % 200 + 20) as u8,
        ((n * 91) % 200 + 20) as u8,
        128,
    ]
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-video-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A clip of `FRAMES` frames through `encoder` (after videoconvert) into
/// `mux`, with 3 s of a 440 Hz tone at 44.1 kHz when `sound`.
fn make_clip(path: &Path, encoder: &str, mux: &str, sound: bool) {
    faderframe_video::init().unwrap();
    let sound = if sound {
        format!(
            " audiotestsrc num-buffers={} samplesperbuffer=441 freq=440 ! audio/x-raw,format=F32LE,rate=44100,channels=2 ! audioconvert ! queue ! m.",
            FRAMES as u64 * 100 / FPS as u64
        )
    } else {
        String::new()
    };
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width={W},height={H},framerate={FPS}/1 ! videoconvert ! {encoder} ! queue ! {mux} name=m ! filesink location=\"{}\"{sound}",
        path.display()
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    let src = pipeline
        .by_name("src")
        .unwrap()
        .downcast::<gst_app::AppSrc>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    for n in 0..FRAMES {
        let c = colour(n);
        let mut data = Vec::with_capacity((W * H * 4) as usize);
        for _ in 0..W * H {
            data.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
        let mut buf = gst::Buffer::from_mut_slice(data);
        {
            let b = buf.get_mut().unwrap();
            let len = 1_000_000_000 / FPS as u64;
            b.set_pts(gst::ClockTime::from_nseconds(n as u64 * len));
            b.set_duration(gst::ClockTime::from_nseconds(len));
        }
        src.push_buffer(buf).unwrap();
    }
    src.end_of_stream().unwrap();
    let bus = pipeline.bus().unwrap();
    let msg = bus
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

/// Frame `n`'s colour, within two JPEG passes' error (neighbours differ by
/// 37 steps or more).
fn assert_frame(f: &faderframe_video::Frame, n: u32) {
    let p = f.pixel(f.width / 2, f.height / 2);
    let c = colour(n);
    for i in 0..3 {
        assert!(
            (p[i] as i32 - c[i] as i32).abs() <= 12,
            "expected frame {n} {c:?}, got {p:?} (time {} ns)",
            f.time
        );
    }
}

fn frame_ns(n: u32) -> i64 {
    n as i64 * 1_000_000_000 / FPS as i64
}

#[test]
fn an_intra_clip_is_probed_indexed_decoded_proxied_and_remuxed() {
    let d = dir();
    let clip = d.join("intra.mkv");
    make_clip(&clip, "jpegenc", "matroskamux", true);

    let info = probe::probe(&clip).unwrap();
    let v = info.video.clone().unwrap();
    assert_eq!((v.width, v.height), (W, H));
    assert_eq!(v.fps, (25, 1));
    assert_eq!(info.audio.len(), 1);
    assert_eq!(info.audio[0].channels, 2);

    let cancel = AtomicBool::new(false);
    let ix = index::index(&clip, &cancel, |_| {}).unwrap();
    assert_eq!(ix.len(), FRAMES as usize);
    assert_eq!(ix.keys.len(), FRAMES as usize, "every frame a keyframe");
    for n in [0, 1, 37, 74] {
        assert_eq!(ix.times[n as usize], frame_ns(n));
        assert_eq!(ix.frame_at(frame_ns(n) + 5_000_000), Some(n as usize));
    }
    assert_eq!(ix.frame_at(frame_ns(FRAMES)), None, "past the end");
    assert!(ix.constant_rate());

    // Exact frames, anywhere, in any order.
    let mut dec = Decoder::open(&clip, W, H).unwrap();
    for n in [37, 3, 74, 0, 50, 51] {
        let f = dec.frame_at(frame_ns(n), true).unwrap().unwrap();
        assert_eq!(f.time, frame_ns(n));
        assert_frame(&f, n);
        // Mid-frame, the same frame.
        let f = dec
            .frame_at(frame_ns(n) + 15_000_000, true)
            .unwrap()
            .unwrap();
        assert_frame(&f, n);
    }
    // Playback: in order from a start.
    dec.play_from(frame_ns(10)).unwrap();
    for n in 10..20 {
        let f = dec.next_frame().unwrap().unwrap();
        assert_eq!(f.time, frame_ns(n), "frame {n} in order");
        assert_frame(&f, n);
    }
    // A smaller size on request.
    let mut small = Decoder::open(&clip, W / 2, H / 2).unwrap();
    let f = small.frame_at(frame_ns(5), true).unwrap().unwrap();
    assert_eq!((f.width, f.height), (W / 2, H / 2));
    assert_frame(&f, 5);

    // The proxy keeps every frame at its time.
    let proxy = d.join("proxies").join("intra.proxy.mkv");
    let mut shares = Vec::new();
    make_proxy(
        &clip,
        &proxy,
        (W, H, (1, 1)),
        ProxySpec {
            height: 120,
            quality: 80,
        },
        &cancel,
        |s| shares.push(s),
    )
    .unwrap();
    assert!(proxy.is_file() && !proxy.with_extension("partial").exists());
    let pix = index::index(&proxy, &cancel, |_| {}).unwrap();
    assert_eq!(pix.times, ix.times);
    let mut pdec = Decoder::open(&proxy, 160, 120).unwrap();
    let f = pdec.frame_at(pix.times[42], true).unwrap().unwrap();
    assert_frame(&f, 42);

    // The sound, as a project's media at 48 kHz.
    let progress = Default::default();
    let a = audio::extract_audio(&clip, 0, &d.join("Audio"), 48_000, &progress, &cancel).unwrap();
    assert_eq!(a.channels, 2);
    let seconds = a.frames as f64 / 48_000.0;
    assert!((seconds - 3.0).abs() < 0.03, "{seconds} s of sound");
    assert!(a.path.is_file() && a.peaks_path.is_file());

    // MJPEG has no place in MPEG-4: refused at once, nothing written.
    let refused = d.join("refused.mp4");
    let e = mux(
        &clip,
        std::slice::from_ref(&a.path),
        &refused,
        Container::Mp4,
        &cancel,
        |_| {},
    );
    assert!(e.is_err() && !refused.exists(), "{e:?}");
    // The picture copied with new sound.
    for container in [Container::Mov, Container::Mkv] {
        let out = d.join(format!("muxed.{}", container.extension()));
        mux(
            &clip,
            std::slice::from_ref(&a.path),
            &out,
            container,
            &cancel,
            |_| {},
        )
        .unwrap();
        let info = probe::probe(&out).unwrap();
        assert!(info.video.is_some(), "{container:?} has the picture");
        assert_eq!(info.audio.len(), 1, "{container:?} has the sound");
        assert_eq!(info.audio[0].channels, 2);
        let mix = index::index(&out, &cancel, |_| {}).unwrap();
        assert_eq!(mix.len(), ix.len(), "{container:?}: every frame");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Long-GOP H.264 with B-frames (where a hardware encoder makes one):
/// frames come out in presentation order and exact.
#[test]
fn a_long_gop_clip_is_exact_frame_for_frame() {
    if !(has_element("vah264enc") && has_element("h264parse")) {
        eprintln!("skipped: no H.264 encoder (vah264enc) here");
        return;
    }
    let d = dir();
    let clip = d.join("gop.mp4");
    make_clip(
        &clip,
        "vah264enc key-int-max=25 b-frames=2 ! h264parse",
        "mp4mux",
        false,
    );
    let cancel = AtomicBool::new(false);
    let ix = index::index(&clip, &cancel, |_| {}).unwrap();
    assert_eq!(ix.len(), FRAMES as usize);
    assert!(
        ix.times.windows(2).all(|w| w[0] < w[1]),
        "presentation order"
    );
    assert!(ix.longest_gop() >= 20, "long GOP: {}", ix.longest_gop());
    // H.264 goes in every container, MPEG-4 with Opus.
    let wav = d.join("tone.wav");
    let tone: Vec<f32> = (0..144_000)
        .map(|n| (n as f32 * 0.0576).sin() * 0.5)
        .collect();
    faderframe_audio_files::write_wav(
        &wav,
        &[tone.clone(), tone],
        48_000,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    for container in Container::ALL {
        let out = d.join(format!("gop-muxed.{}", container.extension()));
        mux(
            &clip,
            std::slice::from_ref(&wav),
            &out,
            container,
            &cancel,
            |_| {},
        )
        .unwrap();
        let info = probe::probe(&out).unwrap();
        assert!(
            info.video.is_some() && info.audio.len() == 1,
            "{container:?}"
        );
        let mix = index::index(&out, &cancel, |_| {}).unwrap();
        assert_eq!(
            mix.times, ix.times,
            "{container:?}: every frame where it was"
        );
    }
    let mut dec = Decoder::open(&clip, W, H).unwrap();
    for n in [37, 12, 74, 1, 26] {
        // At the frame's own start (the index's): decoders may stamp a
        // frame cut by a seek with the seek's time.
        let at = ix.times[n as usize];
        let f = dec.frame_at(at, true).unwrap().unwrap();
        assert_eq!(f.time, ix.times[n as usize]);
        assert_frame(&f, n);
        // The keyframe at once, for scrubbing.
        let k = dec.frame_at(at, false).unwrap().unwrap();
        assert_eq!(k.time, ix.times[ix.key_before(n as usize)]);
    }
    let _ = std::fs::remove_dir_all(&d);
}

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

/// A folder of its own for each test (tests run side by side in one
/// process under `cargo test` and remove their folders when done).
fn dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("ff-video-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A clip of `FRAMES` frames through `encoder` (after videoconvert) into
/// `mux`, with 3 s of a 440 Hz tone at 44.1 kHz when `sound`.
fn make_clip(path: &Path, encoder: &str, mux: &str, sound: bool) {
    make_clip_with(path, encoder, mux, sound, colour);
}

/// [`make_clip`] with frame `n` in `paint(n)`.
fn make_clip_with(
    path: &Path,
    encoder: &str,
    mux: &str,
    sound: bool,
    paint: impl Fn(u32) -> [u8; 3],
) {
    faderframe_video::init().unwrap();
    let sound = if sound {
        format!(
            " audiotestsrc num-buffers={} samplesperbuffer=480 freq=440 ! audio/x-raw,format=F32LE,rate=48000,channels=2 ! audioconvert ! queue ! m.",
            FRAMES as u64 * 100 / FPS as u64
        )
    } else {
        String::new()
    };
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width={W},height={H},framerate={FPS}/1 ! videoconvert ! {encoder} ! queue ! {mux} name=m ! filesink name=out{sound}"
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    // The path as a property: in a pipeline description Windows'
    // backslashes would be escapes.
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
        let c = paint(n);
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

/// AVI can omit presentation timestamps on B-frames. Every frame still
/// needs an index slot, including when playback uses a proxy. An MP4
/// with the same codec checks that its presentation timing stays intact.
#[test]
fn avi_and_mp4_playback_keep_up_with_the_transport() {
    use faderframe_video::{FrameService, Media, Want};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let d = dir();
    for (name, needs, encoder, mux) in [
        ("mjpeg.avi", "jpegenc", "jpegenc", "avimux"),
        (
            "mpeg4.avi",
            "avenc_mpeg4",
            "avenc_mpeg4 bitrate=4000000",
            "avimux",
        ),
        (
            "mpeg4-bframes.avi",
            "avenc_mpeg4",
            "avenc_mpeg4 bitrate=4000000 max-bframes=2 gop-size=50",
            "avimux",
        ),
        (
            "mpeg4-bframes.mp4",
            "avenc_mpeg4",
            "avenc_mpeg4 bitrate=4000000 max-bframes=2 gop-size=50 ! mpeg4videoparse",
            "qtmux",
        ),
    ] {
        if !has_element(needs) {
            eprintln!("skipped {name}: no {needs}");
            continue;
        }
        let clip = d.join(name);
        make_clip(&clip, encoder, mux, true);
        let ix = Arc::new(index::index(&clip, &AtomicBool::new(false), |_| {}).unwrap());
        assert_eq!(ix.len(), FRAMES as usize, "{name}: every frame indexed");
        if name.ends_with(".avi") {
            for n in 0..FRAMES {
                assert_eq!(ix.times[n as usize], frame_ns(n), "{name}: frame {n}");
            }
        }
        let proxy = d.join(format!("{name}.proxy.mkv"));
        make_proxy(
            &clip,
            &proxy,
            (W, H, (1, 1)),
            ProxySpec {
                height: 120,
                quality: 95,
            },
            Default::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        for proxy in [None, Some((proxy, (160, 120)))] {
            let proxied = proxy.is_some();
            let service = FrameService::new(64 << 20);
            service.set_media(
                1,
                Media {
                    original: clip.clone(),
                    index: ix.clone(),
                    size: (W, H),
                    par: (1, 1),
                    proxy,
                },
            );
            // Preroll at the stopped playhead before the transport starts.
            let ready_by = Instant::now() + Duration::from_secs(5);
            while !service
                .picture(1, 0, (W, H), Want::Play)
                .is_some_and(|p| p.exact)
            {
                assert!(Instant::now() < ready_by, "{name}: no first frame");
                std::thread::sleep(Duration::from_millis(5));
            }
            let start = Instant::now();
            let mut decoded = std::collections::BTreeSet::new();
            let mut worst_lag = 0;
            while start.elapsed() < Duration::from_secs(2) {
                let at = start.elapsed();
                let time = at.as_nanos() as i64;
                let n = ix.frame_at(time).unwrap();
                if let Some(p) = service.picture(1, time, (W, H), Want::Play) {
                    assert_frame(&p.frame, p.number as u32);
                    if at > Duration::from_millis(500) {
                        decoded.insert(p.number);
                        worst_lag = worst_lag.max(n.saturating_sub(p.number));
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            eprintln!(
                "{name}, proxy={proxied}: {} distinct frames, worst lag {worst_lag}",
                decoded.len()
            );
            assert!(
                decoded.len() >= 20,
                "{name}: only {} frames in 1.5 s",
                decoded.len()
            );
            assert!(worst_lag <= 5, "{name}: {worst_lag} frames late");
        }
    }
    let _ = std::fs::remove_dir_all(d);
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
        Default::default(),
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
        Default::default(),
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
            Default::default(),
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
            Default::default(),
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
    // A span copied: the GOP from frame 25 up to the keyframe at 50, into
    // a QuickTime movie that starts at 0 with a timecode track.
    {
        use faderframe_core::timecode::{FrameRate, Timecode};
        let out = d.join("span.mov");
        let tc = Timecode::parse("01:00:00:00", FrameRate::Fps25).unwrap();
        mux(
            &clip,
            std::slice::from_ref(&wav),
            &out,
            Container::Mov,
            faderframe_video::mux::MuxOptions {
                from: ix.times[25],
                to: Some(ix.times[50]),
                timecode: Some((tc, FrameRate::Fps25)),
            },
            &cancel,
            |_| {},
        )
        .unwrap();
        let six = index::index(&out, &cancel, |_| {}).unwrap();
        assert_eq!(six.len(), 25, "the frames of one GOP");
        assert_eq!(six.times[0], 0, "from the movie's start");
        assert_eq!(
            six.timecode,
            Some((tc, FrameRate::Fps25)),
            "its timecode track"
        );
        let mut dec = Decoder::open(&out, W, H).unwrap();
        let f = dec.frame_at(six.times[0], true).unwrap().unwrap();
        assert_frame(&f, 25);
        let f = dec.frame_at(six.times[24], true).unwrap().unwrap();
        assert_frame(&f, 49);
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

/// The service: a still frame arrives exact (a keyframe or an earlier
/// frame meanwhile), playing reads ahead in order, the proxy stands in.
#[test]
fn the_service_answers_at_once_and_catches_up() {
    use faderframe_video::{FrameService, Media, Want};
    use std::time::{Duration, Instant};
    let d = dir().join("service");
    std::fs::create_dir_all(&d).unwrap();
    let clip = d.join("svc.mkv");
    make_clip(&clip, "jpegenc", "matroskamux", false);
    let cancel = AtomicBool::new(false);
    let ix = std::sync::Arc::new(index::index(&clip, &cancel, |_| {}).unwrap());
    let svc = FrameService::new(64 << 20);
    let media = Media {
        original: clip.clone(),
        index: ix.clone(),
        size: (W, H),
        par: (1, 1),
        proxy: None,
    };
    svc.set_media(7, media.clone());
    let until = |f: &mut dyn FnMut() -> bool| {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    };
    // Nothing yet, then the exact frame.
    let t = ix.times[40] + 10_000_000;
    let mut got = None;
    assert!(until(&mut || {
        got = svc.picture(7, t, (W, H), Want::Still).filter(|p| p.exact);
        got.is_some()
    }));
    let p = got.unwrap();
    assert_eq!(p.number, 40);
    assert_frame(&p.frame, 40);
    // Outside the video: nothing.
    assert!(svc.picture(7, ix.end + 1, (W, H), Want::Still).is_none());
    // Playing: frame after frame, each exact once there.
    for n in 10..40u32 {
        let t = ix.times[n as usize];
        let mut got = None;
        assert!(until(&mut || {
            got = svc.picture(7, t, (W, H), Want::Play).filter(|p| p.exact);
            got.is_some()
        }));
        let p = got.unwrap();
        assert_eq!(p.number, n as usize);
        assert_frame(&p.frame, n);
    }
    // With a proxy: playing reads it (its size), still frames come sharp.
    let proxy = d.join("svc.proxy.mkv");
    make_proxy(
        &clip,
        &proxy,
        (W, H, (1, 1)),
        ProxySpec {
            height: 120,
            quality: 80,
        },
        Default::default(),
        &cancel,
        |_| {},
    )
    .unwrap();
    svc.set_media(
        7,
        Media {
            proxy: Some((proxy, (160, 120))),
            ..media
        },
    );
    let t = ix.times[60];
    let mut got = None;
    assert!(until(&mut || {
        got = svc
            .picture(7, t, (W, H), Want::Play)
            .filter(|p| p.number == 60);
        got.is_some()
    }));
    assert_eq!(
        got.take().unwrap().frame.width,
        160,
        "the proxy while playing"
    );
    assert!(until(&mut || {
        got = svc
            .picture(7, t, (W, H), Want::Still)
            .filter(|p| p.exact && p.number == 60);
        got.is_some()
    }));
    let p = got.unwrap();
    assert_eq!(p.frame.width, W, "sharp when stopped");
    assert_frame(&p.frame, 60);
    // Thumbnails.
    let mut thumb = None;
    assert!(until(&mut || {
        thumb = svc.thumbnail(7, 5, 60);
        thumb.is_some()
    }));
    assert_eq!(thumb.unwrap().height, 60);
    svc.remove(7);
    assert!(svc.picture(7, t, (W, H), Want::Still).is_none());
}

/// Shots that change at frames 30 and 60, each moving a little (its
/// brightness wobbles): the cuts are found, the wobble is not one.
#[test]
fn cuts_are_found_where_shots_change() {
    let d = dir().join("cuts");
    std::fs::create_dir_all(&d).unwrap();
    let clip = d.join("shots.mkv");
    let shot = |n: u32| -> [u8; 3] {
        let wobble = (n % 4) as u8 * 3;
        match n {
            0..30 => [180 + wobble, 60, 40],
            30..60 => [40, 70 + wobble, 190],
            _ => [120 + wobble, 120, 120],
        }
    };
    make_clip_with(&clip, "jpegenc", "matroskamux", false, shot);
    let cancel = AtomicBool::new(false);
    let cuts =
        faderframe_video::cuts::detect_cuts(&clip, 0, frame_ns(FRAMES), &cancel, |_| {}).unwrap();
    assert_eq!(cuts, vec![frame_ns(30), frame_ns(60)]);
    // Within a span: only the cut inside it.
    let some =
        faderframe_video::cuts::detect_cuts(&clip, frame_ns(40), frame_ns(FRAMES), &cancel, |_| {})
            .unwrap();
    assert_eq!(some, vec![frame_ns(60)]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Zero-copy playback: VA-API decodes and scales into RGB dmabufs (here
/// with VA-API: a display taking what its post-processor makes).
#[cfg(target_os = "linux")]
#[test]
fn frames_decode_into_dmabufs() {
    use faderframe_video::zero_copy;
    if !(has_element("vah264enc") && has_element("h264parse") && has_element("vapostproc")) {
        eprintln!("skipped: no VA-API here");
        return;
    }
    faderframe_video::init().unwrap();
    let formats = zero_copy::postproc_formats();
    assert!(!formats.is_empty(), "RGB formats out of vapostproc");
    zero_copy::set_display_formats(formats.clone());
    let d = dir();
    let clip = d.join("dmabuf.mp4");
    make_clip(
        &clip,
        "vah264enc key-int-max=25 ! h264parse",
        "mp4mux",
        false,
    );
    let mut dec = Decoder::open_dmabuf(&clip, 160, 120).unwrap();
    dec.play_from(0).unwrap();
    // Frames held (the cache keeps some) never stall the decoder.
    let mut held = Vec::new();
    for n in 0..FRAMES {
        let f = dec.next_frame().unwrap().unwrap();
        assert_eq!((f.width, f.height), (160, 120));
        assert!(f.rgba.is_empty());
        let g = f.gpu.as_ref().expect("a dmabuf frame");
        assert!(formats.contains(&(g.fourcc, g.modifier)), "{g:?}");
        assert!(!g.planes.is_empty() && g.planes.iter().all(|p| p.fd >= 0 && p.stride >= 160 * 4));
        assert_eq!(f.time, frame_ns(n));
        held.push(f);
    }
    assert!(dec.next_frame().unwrap().is_none(), "the end");
    let _ = std::fs::remove_dir_all(&d);
}

/// A proxy keeps the picture's colours: video-range white, black and a
/// saturated colour come back as they were (not washed out).
#[test]
fn proxies_keep_the_colours() {
    if !(has_element("vah264enc") && has_element("h264parse")) {
        eprintln!("skipped: no H.264 encoder (vah264enc) here");
        return;
    }
    let d = dir();
    let clip = d.join("colours.mp4");
    let paint = |n: u32| match n % 3 {
        0 => [250, 250, 250],
        1 => [6, 6, 6],
        _ => [230, 30, 30],
    };
    make_clip_with(&clip, "vah264enc ! h264parse", "mp4mux", false, paint);
    let proxy = d.join("colours.proxy.mkv");
    let cancel = AtomicBool::new(false);
    make_proxy(
        &clip,
        &proxy,
        (W, H, (1, 1)),
        ProxySpec {
            height: 120,
            quality: 90,
        },
        Default::default(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let ix = index::index(&proxy, &cancel, |_| {}).unwrap();
    let mut dec = Decoder::open(&proxy, 160, 120).unwrap();
    for n in [30, 31, 32] {
        let f = dec.frame_at(ix.times[n as usize], true).unwrap().unwrap();
        let p = f.pixel(80, 60);
        let c = paint(n);
        for i in 0..3 {
            assert!(
                (p[i] as i32 - c[i] as i32).abs() <= 8,
                "frame {n}: {c:?} came back as {p:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// MXF (broadcast and Avid's container): an uncompressed picture (no
/// codec needed) with sound; indexed and decoded frame-exact, its sound
/// read. (GStreamer's own muxer writes long-GOP H.264 its demuxer cannot
/// seek in — and asserts on —, so that is left out.)
#[test]
fn an_mxf_clip_is_read() {
    if !(has_element("mxfmux") && has_element("mxfdemux")) {
        eprintln!("skipped: no MXF elements here");
        return;
    }
    let d = dir();
    let kinds = [("raw", "video/x-raw,format=UYVY")];
    for (name, encoder) in kinds {
        let clip = d.join(format!("{name}.mxf"));
        make_clip(&clip, encoder, "mxfmux", true);
        let info = probe::probe(&clip).unwrap();
        let v = info.video.as_ref().expect("a picture");
        assert_eq!(info.audio.len(), 1, "{name}: its sound");
        let cancel = AtomicBool::new(false);
        let ix = index::index(&clip, &cancel, |_| {}).unwrap();
        assert_eq!(ix.len(), FRAMES as usize, "{name}: every frame");
        let (w, h) = faderframe_video::fit(v.width, v.height, v.par, 160, 120);
        let mut dec = Decoder::open(&clip, w, h).unwrap();
        for n in [37, 12, 74, 1] {
            let f = dec.frame_at(ix.times[n as usize], true).unwrap().unwrap();
            assert_frame(&f, n);
        }
        let progress = Default::default();
        let a = audio::extract_audio(&clip, 0, &d.join(name), 48_000, &progress, &cancel).unwrap();
        let seconds = a.frames as f64 / 48_000.0;
        assert!((seconds - 3.0).abs() < 0.05, "{name}: {seconds} s of sound");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Post-production intermediates: ProRes in QuickTime and DNxHR in MXF
/// (where FFmpeg's encoders are here — the packages carry the decoders
/// only), decoded frame-exact.
#[test]
fn prores_and_dnxhr_decode() {
    let d = dir();
    let kinds = [
        ("prores.mov", "avenc_prores_ks", "avenc_prores_ks", "qtmux"),
        (
            "dnxhr.mxf",
            "avenc_dnxhd",
            "videoscale ! video/x-raw,width=1280,height=720 ! videoconvert ! video/x-raw,format=Y42B ! avenc_dnxhd profile=dnxhr_lb",
            "mxfmux",
        ),
    ];
    for (name, needs, encoder, mux) in kinds {
        if !(has_element(needs) && has_element(mux)) {
            eprintln!("skipped {name}: no {needs} here");
            continue;
        }
        let clip = d.join(name);
        make_clip(&clip, encoder, mux, false);
        let cancel = AtomicBool::new(false);
        let ix = index::index(&clip, &cancel, |_| {}).unwrap();
        assert_eq!(ix.len(), FRAMES as usize, "{name}: every frame");
        assert_eq!(ix.longest_gop(), 1, "{name}: intra-coded");
        let mut dec = Decoder::open(&clip, 160, 120).unwrap();
        for n in [37, 12, 74, 1] {
            let f = dec.frame_at(ix.times[n as usize], true).unwrap().unwrap();
            assert_frame(&f, n);
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// HDR: a PQ BT.2020 picture (raw, exact code values) is recognised from
/// its stream and mapped for an SDR screen — reference white (203 nits)
/// just under white (room for the highlights), a 1000-nit highlight
/// rolled off to white, 20 nits a dark grey — where shown as it is it
/// would be dull.
#[test]
fn hdr_pictures_are_mapped_for_sdr_screens() {
    use faderframe_video::colour::{Gamut, Transfer, nits_to_pq};
    faderframe_video::init().unwrap();
    let d = dir();
    let clip = d.join("pq.mkv");
    let colorimetry = gst_video::VideoColorimetry::new(
        gst_video::VideoColorRange::Range0_255,
        gst_video::VideoColorMatrix::Rgb,
        gst_video::VideoTransferFunction::Smpte2084,
        gst_video::VideoColorPrimaries::Bt2020,
    );
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA64_LE,width={W},height={H},framerate={FPS}/1,colorimetry={colorimetry} ! matroskamux ! filesink name=out"
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline
        .by_name("out")
        .unwrap()
        .set_property("location", clip.to_string_lossy().as_ref());
    let src = pipeline
        .by_name("src")
        .unwrap()
        .downcast::<gst_app::AppSrc>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    // Thirds: 20 nits, 203 nits, 1000 nits (grey).
    let code = |nits: f64| ((nits_to_pq(nits) * 65_535.0).round() as u16).to_le_bytes();
    for n in 0..10u64 {
        let mut data = Vec::with_capacity((W * H * 8) as usize);
        for _ in 0..H {
            for x in 0..W {
                let c = code([20.0, 203.0, 1000.0][(x * 3 / W) as usize]);
                for _ in 0..3 {
                    data.extend_from_slice(&c);
                }
                data.extend_from_slice(&u16::MAX.to_le_bytes());
            }
        }
        let mut buf = gst::Buffer::from_mut_slice(data);
        let b = buf.get_mut().unwrap();
        b.set_pts(gst::ClockTime::from_mseconds(n * 40));
        b.set_duration(gst::ClockTime::from_mseconds(40));
        src.push_buffer(buf).unwrap();
    }
    src.end_of_stream().unwrap();
    let bus = pipeline.bus().unwrap();
    bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(30),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    )
    .unwrap();
    pipeline.set_state(gst::State::Null).unwrap();

    let cancel = AtomicBool::new(false);
    let ix = index::index(&clip, &cancel, |_| {}).unwrap();
    assert_eq!(ix.colour.transfer, Transfer::Pq);
    assert_eq!(ix.colour.gamut, Gamut::Bt2020);
    assert!(ix.colour.label().unwrap().contains("PQ"));
    let mut dec = Decoder::open_colour(&clip, W, H, ix.colour).unwrap();
    let f = dec.frame_at(ix.times[3], true).unwrap().unwrap();
    let at = |third: u32| f.pixel(W * third / 3 + W / 6, H / 2);
    let (dark, white, bright) = (at(0), at(1), at(2));
    assert!((90..=105).contains(&dark[0]), "20 nits: {dark:?}");
    assert!(
        (220..=240).contains(&white[0]),
        "reference white: {white:?}"
    );
    assert!(
        bright[0] >= 252 && bright[1] >= 252,
        "1000 nits: {bright:?}"
    );
    // Shown as it is (no mapping), reference white is dull grey.
    let mut plain = Decoder::open(&clip, W, H).unwrap();
    let f = plain.frame_at(ix.times[3], true).unwrap().unwrap();
    assert!(f.pixel(W / 2, H / 2)[0] < 170);
    // Its proxy is mapped the same way (and SDR itself).
    let proxy = d.join("pq.proxy.mkv");
    make_proxy(
        &clip,
        &proxy,
        (W, H, (1, 1)),
        ProxySpec {
            height: 120,
            quality: 95,
        },
        ix.colour,
        &cancel,
        |_| {},
    )
    .unwrap();
    let pix = index::index(&proxy, &cancel, |_| {}).unwrap();
    assert_eq!(pix.len(), 10);
    assert!(!pix.colour.needs_mapping(), "{:?}", pix.colour);
    let mut dec = Decoder::open(&proxy, 160, 120).unwrap();
    let f = dec.frame_at(pix.times[3], true).unwrap().unwrap();
    let mid = f.pixel(80, 60);
    assert!(
        (220..=240).contains(&mid[0]),
        "proxy's reference white: {mid:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A real HDR10 stream (HEVC Main 10, PQ, BT.2020) is recognised from what
/// its parser says, and decodes mapped.
#[test]
fn hevc_hdr10_is_recognised() {
    use faderframe_video::colour::{Gamut, Transfer};
    if !(has_element("vah265enc") && has_element("h265parse")) {
        eprintln!("skipped: no HEVC encoder (vah265enc) here");
        return;
    }
    faderframe_video::init().unwrap();
    let d = dir();
    let clip = d.join("hdr10.mp4");
    let pipeline = gst::parse::launch(
        "videotestsrc num-buffers=25 ! video/x-raw,width=320,height=240,framerate=25/1,format=P010_10LE,colorimetry=bt2100-pq ! vah265enc ! h265parse ! mp4mux ! filesink name=out",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    pipeline
        .by_name("out")
        .unwrap()
        .set_property("location", clip.to_string_lossy().as_ref());
    pipeline.set_state(gst::State::Playing).unwrap();
    pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap();
    pipeline.set_state(gst::State::Null).unwrap();
    let cancel = AtomicBool::new(false);
    let ix = index::index(&clip, &cancel, |_| {}).unwrap();
    assert_eq!(
        (ix.colour.transfer, ix.colour.gamut),
        (Transfer::Pq, Gamut::Bt2020)
    );
    let mut dec = Decoder::open_colour(&clip, 160, 120, ix.colour).unwrap();
    let f = dec.frame_at(ix.times[5], true).unwrap().unwrap();
    assert_eq!((f.width, f.height), (160, 120));
    let _ = std::fs::remove_dir_all(&d);
}

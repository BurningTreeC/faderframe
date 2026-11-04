//! The codec monitor: switched from the session, its platform gain from
//! the song's measured loudness (kept by the mix's fingerprint), never in
//! a render; and "Preview on platforms" on a finished render.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::{WavFormat, read_wav};
use faderframe_codec::platform;
use faderframe_engine::EngineConfig;
use faderframe_project::Command;
use faderframe_session::codec::{CodecHear, CodecMonitorSettings, CodecOp, GainSource};
use faderframe_session::delivery::Finish;
use faderframe_session::render::{self, RenderRange, RenderSettings};
use faderframe_session::{Action, AudioPreferences, Session};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(300);

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-codec-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn running_demo() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(48_000),
            buffer_size: Some(256),
            ..Default::default()
        },
    )
    .unwrap();
    s
}

fn wait(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {what}: {:?}", s.codec_status());
}

/// The master rendered over the export's range (loop off, 2 s of tail),
/// as the song's measurement renders it: integrated loudness, true peak.
fn rendered_loudness(s: &mut Session, dir: &std::path::Path) -> (f64, f64) {
    let out = dir.join("song.wav");
    let mut settings = RenderSettings::defaults_for(s.project(), out.clone());
    settings.format = WavFormat::Float32;
    settings.dither = faderframe_audio_files::Dither::Off;
    settings.range = RenderRange::Project;
    settings.tail_seconds = 2.0;
    settings.overwrite = render::Overwrite::Always;
    let job = s.render(settings).unwrap();
    job.join().unwrap();
    let w = read_wav(&out).unwrap();
    let views: Vec<&[f32]> = w.channels.iter().map(Vec::as_slice).collect();
    faderframe_analysis::integrated_weighted(&views, &[1.0, 1.0], w.sample_rate)
}

#[test]
fn the_monitor_switches_and_takes_the_platforms_gain_from_the_song() {
    let dir = temp("switch");
    let mut s = running_demo();
    // The loop is off for the reference render (the measurement unloops).
    let range = s.project().loop_range;
    s.dispatch(Action::Edit(Command::SetLoop {
        range,
        enabled: false,
    }))
    .unwrap();
    assert!(!s.codec_monitor_on());
    s.dispatch(Action::Codec(CodecOp::Opus {
        kbps: 96,
        vbr: false,
    }))
    .unwrap();
    let st = s.codec_status();
    assert!(st.on);
    assert_eq!(st.title, "Opus 96 kb/s CBR");
    assert_eq!(st.gain, GainSource::None);
    assert_eq!(st.gain_text, "No loudness normalisation");
    // The listening node takes a chain, warms it up and is heard.
    wait(
        &mut s,
        |s| s.codec_status().heard == 1.0,
        "the monitor heard",
    );
    let st = s.codec_status();
    assert!(st.ready);
    let expected_ms = (faderframe_codec::FRAME + 312) as f64 / 48.0;
    if faderframe_codec::opus_available() {
        assert!((st.delay_ms - expected_ms).abs() < 0.01, "{}", st.delay_ms);
    }
    // What is heard reaches the node.
    s.dispatch(Action::Codec(CodecOp::Hear(CodecHear::Difference)))
        .unwrap();
    assert_eq!(s.codec_link().shared().hear(), CodecHear::Difference);

    // A platform: its codec, and its gain from the song as rendered.
    s.dispatch(Action::Codec(CodecOp::Platform("spotify".into())))
        .unwrap();
    assert!(s.wait_for_codec(WAIT));
    let spotify = platform("spotify").unwrap();
    let st = s.codec_status();
    assert_eq!(st.kbps, 320);
    assert!(st.title.starts_with("Spotify · Ogg Vorbis 320 kb/s"));
    let GainSource::Song(song) = st.gain else {
        panic!("{:?}", st.gain);
    };
    let (lufs, tp) = rendered_loudness(&mut s, &dir);
    assert!(
        (song.integrated - lufs).abs() < 0.05,
        "{} vs {lufs}",
        song.integrated
    );
    assert!(
        (song.true_peak - tp).abs() < 0.05,
        "{} vs {tp}",
        song.true_peak
    );
    let g = spotify.gain(song.integrated, song.true_peak).db;
    assert_eq!(st.gain_db, g);
    s.tick(0.005);
    let heard = s.codec_link().shared().gain();
    let want = faderframe_core::db_to_gain(((g * 10.0).round() / 10.0) as f32);
    assert!((heard - want).abs() < 1e-4, "{heard} vs {want}");

    // An edit changes the song: measured again; undone, the measurement
    // made before is taken again (by the fingerprint), nothing rendered.
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: master,
        db: -6.0,
    }))
    .unwrap();
    assert!(s.wait_for_codec(WAIT));
    let GainSource::Song(quieter) = s.codec_status().gain else {
        panic!("not measured again");
    };
    assert!(
        (quieter.integrated - (song.integrated - 6.0)).abs() < 0.1,
        "{} vs {}",
        quieter.integrated,
        song.integrated
    );
    s.dispatch(Action::Undo).unwrap();
    s.tick(0.005);
    assert!(s.wait_for_codec(Duration::from_secs(2)));
    assert_eq!(s.codec_status().gain, GainSource::Song(song));

    // Off: the mix as it is.
    s.dispatch(Action::Codec(CodecOp::Off)).unwrap();
    wait(&mut s, |s| !s.codec_status().ready, "the chain handed back");
    assert!(!s.codec_link().shared().on());
    assert_eq!(s.codec_status().delay_ms, 0.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn renders_never_hear_the_codec_monitor() {
    let dir = temp("render");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let render = |s: &mut Session, name: &str| {
        let out = dir.join(name);
        let mut settings = RenderSettings::defaults_for(s.project(), out.clone());
        settings.range = RenderRange::Bars { start: 0, end: 2 };
        settings.format = WavFormat::Float32;
        settings.dither = faderframe_audio_files::Dither::Off;
        settings.overwrite = render::Overwrite::Always;
        s.render(settings).unwrap().join().unwrap();
        read_wav(&out).unwrap().channels
    };
    let plain = render(&mut s, "plain.wav");
    s.dispatch(Action::Codec(CodecOp::Platform("youtube".into())))
        .unwrap();
    s.dispatch(Action::Codec(CodecOp::Hear(CodecHear::Difference)))
        .unwrap();
    let monitored = render(&mut s, "monitored.wav");
    assert_eq!(plain, monitored);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_preview_reports_what_each_platform_does_to_the_master() {
    let dir = temp("preview");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    // A loud master: finished to −9 LUFS under −0.3 dBTP.
    let mut settings = RenderSettings::defaults_for(s.project(), dir.join("unused.wav"));
    settings.range = RenderRange::Bars { start: 0, end: 4 };
    settings.finish = Finish {
        loudness: Some(-9.0),
        ceiling: Some(-0.3),
        ..Finish::default()
    };
    s.dispatch(Action::Codec(CodecOp::Preview {
        settings: Box::new(settings),
        platforms: vec!["spotify".into(), "apple-music".into(), "deezer".into()],
    }))
    .unwrap();
    let running = s.platform_preview().unwrap();
    assert!(running.progress.is_some() && running.result.is_none());
    assert!(s.wait_for_codec(WAIT));
    let report = s.platform_preview().unwrap().result.unwrap().unwrap();
    assert!(report.rendered.contains("bars 1–4"), "{}", report.rendered);
    assert!(
        (report.master.integrated - (-9.0)).abs() < 0.6,
        "{}",
        report.master.integrated
    );
    assert!(
        report.master.true_peak <= -0.2,
        "{}",
        report.master.true_peak
    );
    assert_eq!(report.platforms.len(), 3);
    let ids: Vec<&str> = report.platforms.iter().map(|p| p.platform.id).collect();
    assert_eq!(ids, ["spotify", "apple-music", "deezer"]);
    for p in &report.platforms {
        // Every one turns this master down (to its target).
        let want = p.platform.target_lufs - report.master.integrated;
        assert!(
            (p.gain.db - want).abs() < 1e-9,
            "{}: {:?}",
            p.platform.id,
            p.gain
        );
        assert!(p.heard_true_peak < p.decoded_true_peak);
        assert!(
            p.advice
                .iter()
                .any(|a| a.contains("turns this master down")),
            "{}: {:?}",
            p.platform.id,
            p.advice
        );
        if faderframe_codec::opus_available() {
            let a = p.artefacts_db.unwrap();
            assert!(a < -10.0 && a > -90.0, "{}: artefacts {a}", p.platform.id);
        }
    }
    // The chosen platforms are remembered for the next preview.
    assert_eq!(
        s.codec_settings().preview,
        ["spotify", "apple-music", "deezer"]
    );
    // The temporary render is gone.
    let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("faderframe-platforms-{}-", std::process::id()))
        })
        .collect();
    assert!(left.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_settings_are_remembered_by_id() {
    let s = CodecMonitorSettings {
        hear: CodecHear::Original,
        kbps: 192,
        vbr: false,
        platform: Some("tidal".into()),
        preview: vec!["youtube".into()],
    };
    let text = serde_json::to_string(&s).unwrap();
    assert!(text.contains("\"original\""), "{text}");
    let back: CodecMonitorSettings = serde_json::from_str(&text).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.codec(), (320, true), "Tidal's own codec");
    // Unknown ids fall back.
    let odd: CodecMonitorSettings =
        serde_json::from_str(r#"{"hear":"loud","platform":"napster"}"#).unwrap();
    assert_eq!(odd.hear, CodecHear::Codec);
    assert!(odd.platform().is_none());
    assert_eq!(
        odd.preview_platforms().len(),
        faderframe_codec::PLATFORMS.len()
    );
}

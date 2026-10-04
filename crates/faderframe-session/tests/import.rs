#![allow(clippy::unwrap_used)]
//! Importing audio through the session: undo/redo, media consolidation on
//! first save, reopening, offline files and bouncing streamed audio.

use faderframe_audio_files::wavstream::WavFile;
use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::{Project, SourceSpec, TrackKind};
use faderframe_session::render::{RenderRange, RenderSettings, RenderSource};
use faderframe_session::{Action, ImportTarget, MEDIA_FOLDER, Session};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-import-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn empty_session() -> Session {
    let project = Project::new("Import Test", 48_000);
    Session::new(project, None, EngineConfig::default()).unwrap()
}

fn tone(path: &Path, rate: u32, seconds: f32, channels: usize) {
    let n = (rate as f32 * seconds) as usize;
    let ch: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    write_wav(path, &vec![ch; channels], rate, WavFormat::Pcm24, false).unwrap();
}

fn file_sources(s: &Session) -> Vec<PathBuf> {
    s.project()
        .sources
        .values()
        .filter_map(|src| match &src.spec {
            SourceSpec::File { path, .. } => Some(path.clone()),
            SourceSpec::Generated { .. } => None,
        })
        .collect()
}

#[test]
fn import_undo_redo_save_and_reopen() {
    let dir = tmp("cycle");
    let a = dir.join("Kick.wav");
    let b = dir.join("Pad.wav");
    tone(&a, 44_100, 1.0, 1);
    tone(&b, 48_000, 2.0, 2);

    let mut s = empty_session();
    let tracks_before = s.project().tracks.len();
    s.dispatch(Action::ImportFiles {
        files: vec![a.clone(), b.clone()],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let p = s.project();
    assert_eq!(p.tracks.len(), tracks_before + 2, "one new track per file");
    assert_eq!(p.sources.len(), 2);
    assert_eq!(p.clips.len(), 2);
    let kick = p.tracks.iter().find(|t| t.name == "Kick").unwrap();
    assert_eq!(kick.kind, TrackKind::Audio);
    assert_eq!(kick.layout, faderframe_core::ChannelLayout::Mono);
    let kick_clip = p.clip(kick.clips[0]).unwrap().as_audio().unwrap().clone();
    assert_eq!(
        kick_clip.length, 48_000,
        "44.1 kHz second converted to the project rate"
    );
    assert!(s.peaks(kick_clip.source).is_some());
    assert_eq!(s.selection.clips.len(), 2);
    let scratch = s.media_dir().to_path_buf();
    for f in file_sources(&s) {
        assert!(
            f.starts_with(&scratch),
            "unsaved imports go to the scratch folder"
        );
        assert!(f.exists());
    }

    // One undo step removes tracks, clips and sources; redo restores them.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), tracks_before);
    assert!(s.project().sources.is_empty());
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.project().sources.len(), 2);
    assert_eq!(s.missing_sources(), 0);

    // First save moves scratch media next to the project and stores
    // relative paths.
    let proj = dir.join("Song.ffproj");
    s.save_as(&proj).unwrap();
    let media = dir.join(MEDIA_FOLDER);
    for f in file_sources(&s) {
        assert!(
            f.starts_with(&media),
            "{} moved into the project",
            f.display()
        );
        assert!(f.exists());
    }
    assert!(!scratch.exists(), "scratch folder removed");
    let text = std::fs::read_to_string(&proj).unwrap();
    assert!(
        text.contains("\"Audio/Kick.wav\""),
        "stored relative: {text}"
    );

    // Undo + redo after the move still find the media.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.missing_sources(), 0);
    assert!(file_sources(&s).iter().all(|f| f.starts_with(&media)));
    drop(s);
    assert!(
        media.join("Kick.wav").exists(),
        "saved media survives the session"
    );

    let mut s2 = empty_session();
    s2.open(&proj).unwrap();
    assert_eq!(s2.project().clips.len(), 2);
    assert_eq!(s2.missing_sources(), 0);
    for src in s2.project().sources.keys() {
        assert!(s2.peaks(*src).is_some(), "peak caches loaded from disk");
    }

    // A missing file makes the source offline, not the project unopenable.
    drop(s2);
    std::fs::remove_file(media.join("Kick.wav")).unwrap();
    let mut s3 = empty_session();
    s3.open(&proj).unwrap();
    assert_eq!(s3.missing_sources(), 1);
    assert!(s3.notices().any(|n| n.text.contains("offline")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn import_onto_selected_track_at_position_and_bounce() {
    let dir = tmp("bounce");
    let a = dir.join("loop.wav");
    tone(&a, 48_000, 0.5, 2);
    let mut s = empty_session();
    let track = s.add_track(TrackKind::Audio).unwrap();
    let at = MusicalTime::QUARTER;
    s.import_audio(
        vec![a],
        ImportTarget {
            track: Some(track),
            at,
        },
    );
    s.wait_for_imports();
    let t = s.project().track(track).unwrap();
    assert_eq!(t.clips.len(), 1, "imported onto the chosen track");
    let clip = s.project().clip(t.clips[0]).unwrap();
    assert_eq!(clip.start, at);

    // Bounce the streamed clip offline: audible from beat 2.
    let out = dir.join("bounce.wav");
    let job = s
        .render(RenderSettings {
            range: RenderRange::Bars { start: 0, end: 1 },
            source: RenderSource::Master,
            format: WavFormat::Float32,
            tail_seconds: 0.0,
            dither: faderframe_audio_files::Dither::Off,
            ..RenderSettings::defaults_for(s.project(), out.clone())
        })
        .unwrap();
    let written = job.join().unwrap();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].path, out);
    let f = WavFile::open(&out).unwrap();
    let mut l = vec![0.0f32; f.frames() as usize];
    f.read(0, &mut [&mut l], &mut Vec::new()).unwrap();
    let beat = s.engine().musical_to_samples(s.project(), at) as usize;
    assert!(
        l[..beat - 100].iter().all(|v| v.abs() < 1e-6),
        "silent before the clip"
    );
    let peak = l[beat..beat + 20_000]
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.1, "streamed clip audible in the bounce ({peak})");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failed_imports_are_reported_and_change_nothing() {
    let dir = tmp("fail");
    let junk = dir.join("junk.wav");
    std::fs::write(&junk, b"definitely not audio").unwrap();
    let mut s = empty_session();
    let before = s.history().revision();
    s.import_audio(
        vec![junk],
        ImportTarget {
            track: None,
            at: MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    assert_eq!(s.history().revision(), before);
    assert!(s.notices().any(|n| n.text.contains("junk.wav")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn live_playback_streams_without_misses_and_bounded_memory() {
    use faderframe_audio::dummy::DummyBackend;
    use faderframe_session::{AudioPreferences, TransportAction};
    use std::time::{Duration, Instant};

    let dir = tmp("live");
    let long = dir.join("long.wav");
    tone(&long, 48_000, 60.0, 1);
    let mut s = empty_session();
    s.import_audio(
        vec![long],
        ImportTarget {
            track: None,
            at: MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(48_000),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    let start = 20 * 48_000;
    let at = s.engine().samples_to_musical(s.project(), start);
    s.dispatch(Action::Transport(TransportAction::Locate(at)))
        .unwrap();
    // Give the loader a moment to fill the window at the new position.
    std::thread::sleep(Duration::from_millis(150));
    let (_, misses_before) = s.streaming_stats();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.transport().position < start + 72_000 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
    }
    assert!(s.transport().position >= start + 72_000, "played 1.5 s");
    let (resident, misses) = s.streaming_stats();
    assert_eq!(misses, misses_before, "no page was missing during playback");
    assert!(resident > 0);
    assert!(
        resident < 3 << 20,
        "resident window bounded ({resident} bytes for a 11 MB file)"
    );
    let track = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "long")
        .unwrap()
        .id;
    assert!(
        s.meter(track).left.hold_db > -20.0,
        "streamed audio reached the track meter"
    );
    s.stop_audio();
    std::fs::remove_dir_all(&dir).unwrap();
}

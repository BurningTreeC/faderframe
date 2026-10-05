//! The album: songs from sections, this project and audio files, analysed
//! and exported with album levelling, true-peak limiting and a CUE sheet.
#![allow(clippy::unwrap_used)]

use faderframe_analysis::delivery::measure;
use faderframe_audio_files::{Dither, WavFormat, read_wav, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::album::{AlbumLevel, SongSource};
use faderframe_session::album::AlbumAction;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-album-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn album(s: &mut Session, a: AlbumAction) {
    s.dispatch(Action::Album(a)).unwrap();
}

#[test]
fn songs_are_analysed_and_exported_as_an_album() {
    let dir = tmp("export");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    // Two sections of the demo (bars 1–4 and 5–8) and a quiet tone file.
    for (a, b) in [(0.0, 16.0), (16.0, 32.0)] {
        s.dispatch(Action::AddSection {
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
        })
        .unwrap();
    }
    let tone: Vec<f32> = (0..48_000 * 3)
        .map(|i| 0.05 * (i as f32 * 2.0 * std::f32::consts::PI * 330.0 / 48_000.0).sin())
        .collect();
    let file = dir.join("tone.wav");
    write_wav(&file, &[tone], 48_000, WavFormat::Pcm24, false).unwrap();

    album(&mut s, AlbumAction::AddSections);
    album(&mut s, AlbumAction::AddFiles(vec![file.clone()]));
    let songs = s.project().album.songs.clone();
    assert_eq!(songs.len(), 3);
    assert!(matches!(songs[0].source, SongSource::Section(_)));
    assert_eq!(songs[2].source, SongSource::AudioFile(file));
    assert_eq!(songs[2].title, "tone");
    // Adding sections again finds none; undo removes the file song.
    album(&mut s, AlbumAction::AddSections);
    assert_eq!(s.project().album.songs.len(), 3);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().album.songs.len(), 2);
    s.dispatch(Action::Redo).unwrap();

    // Reorder: the tone first, with a fade-in.
    album(
        &mut s,
        AlbumAction::Move {
            song: songs[2].id,
            to: 0,
        },
    );
    let mut first = s.project().album.songs[0].clone();
    assert_eq!(first.id, songs[2].id);
    first.fade_in = 0.5;
    album(&mut s, AlbumAction::Update(first.clone()));

    album(&mut s, AlbumAction::Analyse);
    assert!(s.album_progress().is_some());
    s.wait_album();
    for song in &s.project().album.songs {
        let a = s
            .album_analysis(song)
            .unwrap_or_else(|| panic!("{}", song.title));
        assert!(a.report.integrated.is_finite(), "{}", song.title);
        assert!(a.seconds > 2.9, "{} {}", song.title, a.seconds);
    }
    let tone_lufs = s.album_analysis(&first).unwrap().report.integrated;
    assert!(tone_lufs < -25.0, "{tone_lufs}");
    let whole = s.album_loudness().unwrap();
    assert!(whole.integrated > tone_lufs);
    // A changed song needs a new analysis.
    let mut louder = first.clone();
    louder.gain_db = 3.0;
    assert!(s.album_analysis(&louder).is_none());

    // Album levelling at −14 LUFS, −1 dBTP, 16-bit with shaped dither.
    let mut settings = s.project().album.settings.clone();
    settings.level = AlbumLevel::Album;
    settings.format = WavFormat::Pcm16;
    settings.dither = Dither::Shaped;
    settings.output = Some(dir.join("out"));
    settings.tail = 0.5;
    album(&mut s, AlbumAction::Settings(settings));
    let predicted: Vec<f64> = s
        .project()
        .album
        .songs
        .iter()
        .map(|song| s.album_delivered(song.id).unwrap().gain_db)
        .collect();
    assert!(predicted.windows(2).all(|w| w[0] == w[1]), "{predicted:?}");
    album(&mut s, AlbumAction::Export);
    s.wait_album();
    let done = s.album_export().unwrap().clone();
    assert_eq!(done.files.len(), 3);
    let names: Vec<String> = done
        .files
        .iter()
        .map(|(_, p, _)| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names[0], "01 tone.wav");
    let mut measured = Vec::new();
    for (_, path, finished) in &done.files {
        let wav = read_wav(path).unwrap();
        assert_eq!(wav.format, WavFormat::Pcm16);
        let r = measure(&wav.channels, wav.sample_rate);
        assert!(r.true_peak <= -0.95, "{}: {r:?}", path.display());
        assert!((r.integrated - finished.report.integrated).abs() < 0.1);
        measured.push(r.integrated);
    }
    // One gain for all: the quiet tone stays far below the sections.
    assert!(measured[0] < measured[1] - 8.0, "{measured:?}");
    // The album file holds the songs with pauses, the CUE sheet indexes them.
    let whole = read_wav(done.album_file.as_ref().unwrap()).unwrap();
    let lengths: usize = done
        .files
        .iter()
        .map(|(_, p, _)| read_wav(p).unwrap().channels[0].len())
        .sum();
    // Each pause runs on to the next CD frame (1/75 s), so the track marks
    // lie on CD frames.
    let paused = whole.channels[0].len() - lengths;
    assert!(
        (2 * 2 * 48_000..2 * 2 * 48_000 + 2 * 640).contains(&paused),
        "{paused}"
    );
    let cue = std::fs::read_to_string(done.cue.as_ref().unwrap()).unwrap();
    assert!(cue.contains("TRACK 03 AUDIO"), "{cue}");
    assert!(cue.contains("INDEX 00"), "pauses are pregaps: {cue}");
    assert!(cue.contains("TITLE \"tone\""));
    // No temporary files are left.
    let left: Vec<_> = std::fs::read_dir(dir.join("out"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(left.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_section_is_reported_not_rendered() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::AddSection {
        start: MusicalTime::ZERO,
        end: MusicalTime::from_quarters(8.0),
    })
    .unwrap();
    album(&mut s, AlbumAction::AddSections);
    let section = s.project().sections[0].id;
    s.edit(faderframe_project::Command::RemoveSection { section })
        .unwrap();
    album(&mut s, AlbumAction::Analyse);
    s.wait_album();
    let song = s.project().album.songs[0].clone();
    assert!(s.album_analysis(&song).is_none());
    assert!(s.album_error(song.id).unwrap().contains("section"));
}

#[test]
fn the_album_plays_as_it_will_be_delivered() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    for (a, b) in [(0.0, 16.0), (16.0, 32.0)] {
        s.dispatch(Action::AddSection {
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
        })
        .unwrap();
    }
    album(&mut s, AlbumAction::AddSections);
    let songs = s.project().album.songs.clone();
    assert!(s.album_playback().is_none());
    // Play: prepared first, then playing from the first song.
    album(&mut s, AlbumAction::Play(None));
    assert!(s.album_playback().unwrap().preparing.is_some());
    s.wait_album();
    let pb = s.album_playback().unwrap();
    assert!(pb.playing && pb.preparing.is_none(), "{pb:?}");
    assert_eq!(pb.song, Some(0));
    let (marks, length) = s.album_marks().unwrap();
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0], (songs[0].id, 0.0));
    // The second song starts after the first (its section and the tail
    // of its effects) and its pause (2 s).
    let first = 16.0 * 60.0 / 112.0;
    assert!(marks[1].1 >= first + 2.0 - 0.01, "{marks:?}");
    let before = marks[1].1;
    assert!(length > marks[1].1);
    // Next song, seek, pause.
    album(&mut s, AlbumAction::Skip(1));
    assert_eq!(s.album_playback().unwrap().song, Some(1));
    album(&mut s, AlbumAction::Seek(1.0));
    let pb = s.album_playback().unwrap();
    assert_eq!(pb.song, Some(0));
    assert!((pb.position - 1.0).abs() < 0.01);
    album(&mut s, AlbumAction::Pause);
    assert!(!s.album_playback().unwrap().playing);
    // Playing again with nothing changed starts at once.
    album(&mut s, AlbumAction::Play(Some(songs[1].id)));
    let pb = s.album_playback().unwrap();
    assert!(pb.playing && pb.preparing.is_none() && pb.song == Some(1));
    // A change to the album: prepared again.
    album(&mut s, AlbumAction::StopPlaying);
    assert!(s.album_playback().is_none());
    let mut song = songs[1].clone();
    song.pause = 0.5;
    album(&mut s, AlbumAction::Update(song));
    album(&mut s, AlbumAction::Play(None));
    assert!(s.album_playback().unwrap().preparing.is_some());
    s.wait_album();
    let (marks, _) = s.album_marks().unwrap();
    assert!(
        (before - marks[1].1 - 1.5).abs() < 0.03,
        "{before} → {marks:?}"
    );
    album(&mut s, AlbumAction::StopPlaying);
    assert!(s.album_playback().is_none());
}

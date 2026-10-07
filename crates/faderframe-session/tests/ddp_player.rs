//! The DDP player: a CD master FaderFrame wrote, opened, checked, played
//! by track and imported back as songs whose samples are the disc's.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_session::album::AlbumAction;
use faderframe_session::ddp::DdpAction;
use faderframe_session::{Action, Session};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-ddp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A CD master of three tones (5 s each, 2 s pauses) with titles and ISRC.
fn master(dir: &std::path::Path) -> (Session, PathBuf) {
    let mut s = Session::new(
        faderframe_project::Project::new("Disc", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let files: Vec<PathBuf> = [220.0f32, 330.0, 440.0]
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let tone: Vec<f32> = (0..48_000 * 5)
                .map(|n| 0.25 * (n as f32 * std::f32::consts::TAU * f / 48_000.0).sin())
                .collect();
            let p = dir.join(format!("tone {}.wav", i + 1));
            write_wav(&p, &[tone.clone(), tone], 48_000, WavFormat::Pcm24, false).unwrap();
            p
        })
        .collect();
    s.dispatch(Action::Album(AlbumAction::AddFiles(files)))
        .unwrap();
    let mut info = s.project().album.info.clone();
    info.title = "Tones".into();
    info.credits.performer = "Oscillator".into();
    s.dispatch(Action::Album(AlbumAction::Info(info))).unwrap();
    for (i, mut song) in s.project().album.songs.clone().into_iter().enumerate() {
        song.isrc = format!("DE-A12-26-0000{}", i + 1);
        s.dispatch(Action::Album(AlbumAction::Update(song)))
            .unwrap();
    }
    let mut settings = s.project().album.settings.clone();
    settings.loudness = None;
    settings.output = Some(dir.join("out"));
    settings.tail = 0.0;
    settings.ddp = true;
    s.dispatch(Action::Album(AlbumAction::Settings(settings)))
        .unwrap();
    s.dispatch(Action::Album(AlbumAction::Export)).unwrap();
    s.wait_album();
    let done = s
        .album_export()
        .unwrap_or_else(|| panic!("{:?}", s.notices().collect::<Vec<_>>()))
        .clone();
    (s, done.ddp.unwrap().0)
}

#[test]
fn a_ddp_is_checked_played_by_track_and_imported() {
    let dir = tmp("player");
    let (_, ddp) = master(&dir);
    let mut s = Session::new(
        faderframe_project::Project::new("Player", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    s.dispatch(Action::Ddp(DdpAction::Open(ddp.clone())))
        .unwrap();
    s.wait_ddp();
    let disc = s.ddp().unwrap().clone();
    assert_eq!(disc.checksums, Some(true));
    assert!(disc.problems.is_empty(), "{:?}", disc.problems);
    assert!(!disc.big_endian);
    assert_eq!(disc.disc.tracks.len(), 3);
    assert_eq!(disc.disc.text.title, "Tones");
    assert_eq!(disc.disc.tracks[1].isrc.as_deref(), Some("DEA122600002"));
    assert!(disc.overview.iter().filter(|p| **p > 0.2).count() > disc.overview.len() / 2);
    // Tracks: 5 s each.
    let (start2, len2) = disc.track_span(1).unwrap();
    assert!((len2 - 5.0).abs() < 0.03, "{len2}");
    // In track 2's pregap the time counts down to its start.
    let pregap = disc.disc.tracks[1].pregap.unwrap();
    let (t, index, at) = disc.locate(pregap + 75).unwrap();
    assert_eq!((t, index), (1, 0));
    assert!(at < 0.0, "{at}");

    // Played from track 2 (on the dummy-less session: the preview locates).
    s.dispatch(Action::Ddp(DdpAction::Play(Some(1)))).unwrap();
    let p = s.ddp_playback().unwrap();
    assert!(
        (p.position - start2).abs() < 0.05,
        "{} vs {start2}",
        p.position
    );
    assert_eq!(p.track, Some(1));
    s.dispatch(Action::Ddp(DdpAction::Stop)).unwrap();
    assert!(s.ddp_playback().is_none());

    // Imported: three songs with the disc's text, codes and pauses, their
    // samples exactly the image's.
    s.dispatch(Action::Ddp(DdpAction::Import)).unwrap();
    s.wait_ddp();
    let album = s.project().album.clone();
    assert_eq!(album.songs.len(), 3);
    assert_eq!(album.info.title, "Tones");
    assert_eq!(album.info.credits.performer, "Oscillator");
    assert_eq!(album.songs[0].title, "tone 1");
    assert_eq!(album.songs[2].isrc, "DEA122600003");
    assert!(
        (album.songs[1].pause - 2.0).abs() < 0.03,
        "{}",
        album.songs[1].pause
    );
    let image = std::fs::read(&disc.image).unwrap();
    let faderframe_project::album::SongSource::AudioFile(path) = &album.songs[1].source else {
        panic!("a file");
    };
    let wav = read_wav(path).unwrap();
    assert_eq!(wav.sample_rate, 44_100);
    let start = disc.disc.tracks[1].start().unwrap() as usize * 2352;
    for (i, (l, r)) in wav.channels[0]
        .iter()
        .zip(&wav.channels[1])
        .enumerate()
        .step_by(997)
    {
        let at = start + i * 4;
        let want_l = f32::from(i16::from_le_bytes([image[at], image[at + 1]])) / 32768.0;
        let want_r = f32::from(i16::from_le_bytes([image[at + 2], image[at + 3]])) / 32768.0;
        assert_eq!((*l, *r), (want_l, want_r), "frame {i}");
    }
    // The first save takes the tracks into the project's folder, and the
    // songs follow.
    let saved = dir.join("project").join("Player.ffproj");
    std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
    s.save_as(&saved).unwrap();
    for song in &s.project().album.songs {
        let faderframe_project::album::SongSource::AudioFile(p) = &song.source else {
            panic!("a file");
        };
        assert!(p.starts_with(saved.parent().unwrap()), "{}", p.display());
        assert!(p.exists(), "{}", p.display());
    }
    // One step to undo.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().album.songs.is_empty());

    // A big-endian copy is recognised.
    let swapped: Vec<u8> = image
        .as_chunks::<2>()
        .0
        .iter()
        .flat_map(|b| [b[1], b[0]])
        .collect();
    let be = dir.join("be.dat");
    std::fs::write(&be, swapped).unwrap();
    assert!(faderframe_disc::ddp::image_big_endian(&be).unwrap());
    assert!(!faderframe_disc::ddp::image_big_endian(&disc.image).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Writes the three-tone master (with a German translation) to `$DDP_OUT`
/// for looking at it in the player.
#[test]
#[ignore]
fn write_a_ddp_to_look_at() {
    let Ok(out) = std::env::var("DDP_OUT") else {
        return;
    };
    let dir = PathBuf::from(out);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, _) = master(&dir);
    let mut info = s.project().album.info.clone();
    let mut german = faderframe_project::album::Translation {
        language: 0x08,
        title: "Töne".into(),
        ..Default::default()
    };
    for (i, song) in s.project().album.songs.iter().enumerate() {
        german.song_mut(song.id).title = format!("Ton {}", i + 1);
    }
    info.translations = vec![german];
    s.dispatch(Action::Album(AlbumAction::Texts { info, song: None }))
        .unwrap();
    s.dispatch(Action::Album(AlbumAction::Export)).unwrap();
    s.wait_album();
    eprintln!(
        "DDP in {}",
        s.album_export().unwrap().ddp.clone().unwrap().0.display()
    );
}

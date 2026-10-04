//! The album as a release: codes and credits, a crossfade, a song's own
//! inserts, the cue sheet and a CD master (DDP 2.00) read back.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ParameterId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::album::AlbumLevel;
use faderframe_project::{Command, PluginRef};
use faderframe_session::album::AlbumAction;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-album-cd-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn album(s: &mut Session, a: AlbumAction) {
    s.dispatch(Action::Album(a)).unwrap();
}

#[test]
fn a_release_with_a_crossfade_inserts_and_a_cd_master() {
    let dir = tmp("release");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    for (a, b) in [(0.0, 16.0), (16.0, 32.0)] {
        s.dispatch(Action::AddSection {
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
        })
        .unwrap();
    }
    let tone: Vec<f32> = (0..48_000 * 5)
        .map(|i| 0.25 * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin())
        .collect();
    let file = dir.join("tone.wav");
    write_wav(&file, &[tone], 48_000, WavFormat::Pcm24, false).unwrap();
    album(&mut s, AlbumAction::AddSections);
    album(&mut s, AlbumAction::AddFiles(vec![file]));

    // Release information: codes are checked and normalised.
    let mut info = s.project().album.info.clone();
    info.title = "Test Album".into();
    info.credits.performer = "The Testers".into();
    info.upc = "0360-0029-1453".into();
    assert!(
        s.dispatch(Action::Album(AlbumAction::Info(info.clone())))
            .is_err(),
        "a wrong check digit"
    );
    info.upc = "036000291452".into();
    album(&mut s, AlbumAction::Info(info));
    assert_eq!(s.project().album.info.upc, "0036000291452");
    let songs = s.project().album.songs.clone();
    let mut first = songs[0].clone();
    first.isrc = "us-ab1-26-00001".into();
    first.credits.songwriter = "A. Writer".into();
    album(&mut s, AlbumAction::Update(first));
    assert_eq!(s.project().album.songs[0].isrc, "USAB12600001");
    let mut bad = s.project().album.songs[1].clone();
    bad.isrc = "nonsense".into();
    assert!(s.dispatch(Action::Album(AlbumAction::Update(bad))).is_err());
    // The second song crossfades out of the first.
    let mut second = s.project().album.songs[1].clone();
    second.crossfade = 1.0;
    album(&mut s, AlbumAction::Update(second));

    // The tone's own insert: a gain at −12 dB (set through the usual
    // plugin parameter command, naming the master).
    let tone_id = songs[2].id;
    album(&mut s, AlbumAction::Analyse);
    s.wait_album();
    let plain = s
        .album_analysis(&s.project().album.songs[2])
        .unwrap()
        .report
        .integrated;
    album(
        &mut s,
        AlbumAction::AddInsert {
            song: tone_id,
            plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
        },
    );
    let slot = s.project().album.songs[2].inserts[0].id;
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: master,
        plugin: slot,
        parameter: ParameterId(0),
        value: Some(-12.0),
    }))
    .unwrap();
    assert_eq!(s.song_insert(slot).unwrap().1.parameters[0].value, -12.0);
    // The analysis is stale now; monitoring hosts the chain on the master.
    assert!(s.album_analysis(&s.project().album.songs[2]).is_none());
    album(&mut s, AlbumAction::Monitor(Some(tone_id)));
    assert_eq!(s.album_monitor(), Some(tone_id));
    assert!(!s.plugin_parameter_views(slot).is_empty(), "hosted");

    let mut settings = s.project().album.settings.clone();
    settings.level = AlbumLevel::Album;
    settings.loudness = None;
    settings.format = WavFormat::Pcm16;
    settings.output = Some(dir.join("out"));
    settings.tail = 0.5;
    settings.ddp = true;
    settings.copy_permitted = true;
    album(&mut s, AlbumAction::Settings(settings));
    album(&mut s, AlbumAction::Export);
    s.wait_album();
    let done = s
        .album_export()
        .unwrap_or_else(|| panic!("{:?}", s.notices().collect::<Vec<_>>()))
        .clone();
    let inserted = s
        .album_analysis(&s.project().album.songs[2])
        .unwrap()
        .report
        .integrated;
    assert!(
        (plain - 12.0 - inserted).abs() < 0.2,
        "{plain} → {inserted}"
    );

    // Gapless: with a crossfade the song files are cut from the album at
    // the track marks, so together they are the album.
    let whole = read_wav(done.album_file.as_ref().unwrap()).unwrap();
    let parts: Vec<usize> = done
        .files
        .iter()
        .map(|(_, p, _)| read_wav(p).unwrap().channels[0].len())
        .collect();
    assert_eq!(parts.iter().sum::<usize>(), whole.channels[0].len());
    for p in &parts[..2] {
        assert_eq!(p % 640, 0, "marks on CD frames");
    }
    let cue = std::fs::read_to_string(done.cue.as_ref().unwrap()).unwrap();
    for line in [
        "CATALOG 0036000291452",
        "TITLE \"Test Album\"",
        "PERFORMER \"The Testers\"",
        "    ISRC USAB12600001",
        "    SONGWRITER \"A. Writer\"",
        "    FLAGS DCP",
        "  TRACK 03 AUDIO",
    ] {
        assert!(cue.contains(line), "{line}: {cue}");
    }
    // The crossfaded song has no pregap.
    let track2 = &cue[cue.find("TRACK 02").unwrap()..cue.find("TRACK 03").unwrap()];
    assert!(!track2.contains("INDEX 00"), "{track2}");
    assert!(cue[cue.find("TRACK 03").unwrap()..].contains("INDEX 00"));

    // The CD master: 44.1 kHz, the 2 s pregap, codes and CD-Text.
    let (ddp, sectors) = done.ddp.clone().unwrap();
    let back = faderframe_disc::ddp::read(&ddp).unwrap();
    assert_eq!(back.checksums, Some(true));
    let d = back.disc;
    assert_eq!(d.sectors, sectors);
    assert_eq!(d.upc.as_deref(), Some("0036000291452"));
    assert_eq!(d.tracks.len(), 3);
    assert_eq!(d.tracks[0].pregap, Some(0));
    assert_eq!(d.tracks[0].indexes, vec![150]);
    assert_eq!(d.tracks[0].isrc.as_deref(), Some("USAB12600001"));
    assert!(d.tracks.iter().all(|t| t.flags.copy_permitted));
    assert_eq!(d.tracks[1].pregap, None);
    assert!(d.tracks[2].pregap.is_some());
    assert_eq!(d.text.title, "Test Album");
    assert_eq!(d.text.performer, "The Testers");
    assert_eq!(d.tracks[0].text.songwriter, "A. Writer");
    assert_eq!(d.tracks[2].text.title, "tone");
    // The marks match the album file's (48 kHz) at 44.1 kHz.
    assert_eq!(
        d.tracks[1].indexes[0] - 150,
        (parts[0] / 640) as u32,
        "track 2 starts where the first song file ends"
    );
    let image = std::fs::read(&back.image).unwrap();
    assert_eq!(image.len() as u64, u64::from(sectors) * 2352);
    // The album's audio after 2 s of digital silence, as loud as in the
    // album file (resampled, dithered to 16 bits).
    let samples: Vec<f32> = image
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
        .collect();
    let pregap = 150 * 588 * 2;
    assert!(samples[..pregap].iter().all(|v| *v == 0.0));
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    let left: Vec<f32> = samples[pregap..].iter().step_by(2).copied().collect();
    let (cd, file) = (rms(&left), rms(&whole.channels[0]));
    assert!((20.0 * (cd / file).log10()).abs() < 0.1, "{cd} vs {file}");
    let seconds = f64::from(sectors) / 75.0;
    let album_seconds = whole.channels[0].len() as f64 / 48_000.0;
    assert!((seconds - 2.0 - album_seconds).abs() < 0.02, "{seconds}");

    // Removing the monitored song stops monitoring.
    album(&mut s, AlbumAction::Remove(tone_id));
    assert_eq!(s.album_monitor(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cd_track_lasts_four_seconds() {
    let dir = tmp("short");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let short: Vec<f32> = vec![0.1; 48_000 * 2];
    let file = dir.join("short.wav");
    write_wav(&file, &[short], 48_000, WavFormat::Pcm24, false).unwrap();
    album(&mut s, AlbumAction::AddFiles(vec![file.clone(), file]));
    let mut last = s.project().album.songs[1].clone();
    last.pause = 0.5;
    album(&mut s, AlbumAction::Update(last));
    let mut settings = s.project().album.settings.clone();
    settings.output = Some(dir.join("out"));
    settings.ddp = true;
    album(&mut s, AlbumAction::Settings(settings));
    album(&mut s, AlbumAction::Export);
    s.wait_album();
    assert!(s.album_export().is_none());
    let notices = format!("{:?}", s.notices().collect::<Vec<_>>());
    assert!(notices.contains("at least 4 s"), "{notices}");
    std::fs::remove_dir_all(&dir).unwrap();
}

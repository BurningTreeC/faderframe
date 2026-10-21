//! Exports never replace a file without asking: an export to a place the
//! user chose asks about the files there (all in one question) before it
//! starts — unless a save dialog asked already —, writes beside them and
//! moves everything into place at the end; a file that appears while it
//! is written is asked about while the export waits; Cancel keeps what
//! was there. The album (song files, album file and cue sheet, the DDP
//! fileset as a folder replaced whole), a sample, MIDI and MIDI 2.0 files,
//! lyrics, a lead sheet and a track preset; the album's report measures
//! each written file.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_session::album::AlbumAction;
use faderframe_session::exports::ReplaceQuestion;
use faderframe_session::render::files;
use faderframe_session::sampling::SampleTarget;
use faderframe_session::{Action, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-exports-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The questions asked since the last look.
fn questions(s: &mut Session) -> Vec<ReplaceQuestion> {
    s.take_ui_requests()
        .into_iter()
        .filter_map(|r| match r {
            UiRequest::AskReplace(q) => Some(q),
            _ => None,
        })
        .collect()
}

fn answer(s: &mut Session, q: &ReplaceQuestion, replace: bool) {
    s.dispatch(Action::AnswerReplace {
        question: q.id,
        replace,
    })
    .unwrap();
}

/// No temporary file (or folder) stays in `dir`.
fn no_temporaries(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        assert!(!files::is_temporary(&p), "left behind: {}", p.display());
    }
}

fn read(p: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(p).unwrap()).into_owned()
}

#[test]
fn a_midi_export_asks_before_replacing_a_file() {
    let d = tmp("midi");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let path = d.join("Demo.mid");
    std::fs::write(&path, "old").unwrap();
    s.export_midi_file(&path, None).unwrap();
    let q = questions(&mut s);
    assert_eq!(q.len(), 1, "{q:?}");
    assert_eq!(q[0].paths, std::slice::from_ref(&path));
    assert!(!q[0].appeared);
    assert_eq!(q[0].what, "MIDI file");
    assert_eq!(read(&path), "old", "nothing written before the answer");
    // Cancel: kept as it was.
    answer(&mut s, &q[0], false);
    assert_eq!(read(&path), "old");
    assert!(s.replace_questions().is_empty());
    // Overwrite: replaced.
    s.export_midi_file(&path, None).unwrap();
    let q = questions(&mut s);
    answer(&mut s, &q[0], true);
    assert!(read(&path).starts_with("MThd"));
    no_temporaries(&d);
    // A save dialog that asked already: no second question.
    std::fs::write(&path, "old").unwrap();
    s.agree_to_replace([path.clone()]);
    s.export_midi_file(&path, None).unwrap();
    s.forget_agreement();
    assert!(questions(&mut s).is_empty());
    assert!(read(&path).starts_with("MThd"));
    // A new file: written at once.
    let fresh = d.join("Fresh.mid");
    s.export_midi_file(&fresh, None).unwrap();
    assert!(questions(&mut s).is_empty());
    assert!(read(&fresh).starts_with("MThd"));
    no_temporaries(&d);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn lyrics_ask_about_both_files_in_one_question() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let line = |text: &str| faderframe_project::lyrics::LyricLine {
        start: MusicalTime::ZERO,
        end: MusicalTime::from_quarters(4.0),
        text: text.into(),
    };
    s.dispatch(Action::Edit(faderframe_project::Command::SetLyrics {
        lyrics: vec![line("first words")],
    }))
    .unwrap();
    // Not there yet: written at once.
    let lrc = s.export_lyrics().unwrap();
    let srt = lrc.with_extension("srt");
    assert!(questions(&mut s).is_empty());
    assert!(read(&lrc).contains("first words"));
    s.dispatch(Action::Edit(faderframe_project::Command::SetLyrics {
        lyrics: vec![line("second words")],
    }))
    .unwrap();
    s.dispatch(Action::ExportLyrics).unwrap();
    let q = questions(&mut s);
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].paths, [lrc.clone(), srt.clone()]);
    answer(&mut s, &q[0], false);
    assert!(read(&lrc).contains("first words") && read(&srt).contains("first words"));
    s.dispatch(Action::ExportLyrics).unwrap();
    let q = questions(&mut s);
    answer(&mut s, &q[0], true);
    assert!(read(&lrc).contains("second words") && read(&srt).contains("second words"));
    no_temporaries(lrc.parent().unwrap());
}

#[test]
fn a_track_preset_export_asks_and_a_dialog_answer_counts() {
    let d = tmp("preset");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let path = d.join("Bass.fftrack");
    std::fs::write(&path, "old").unwrap();
    s.export_track_preset(bass, &path).unwrap();
    let q = questions(&mut s);
    assert_eq!(q[0].paths, std::slice::from_ref(&path));
    answer(&mut s, &q[0], false);
    assert_eq!(read(&path), "old");
    s.dispatch_agreeing(Action::Undo, vec![path.clone()]).ok();
    // Agreed for one dispatch only: forgotten after it.
    s.export_track_preset(bass, &path).unwrap();
    assert_eq!(questions(&mut s).len(), 1, "the agreement was forgotten");
    std::fs::remove_dir_all(&d).unwrap();
}

/// Two five-second tones as album songs, exported with the album file
/// and a CD master into `out`.
fn album_session(d: &Path, out: &Path) -> Session {
    let config = EngineConfig::default();
    let mut p = faderframe_project::demo::demo_project(config.sample_rate);
    p.sections.clear();
    let mut s = Session::new(p, None, config).unwrap();
    let mut songs = Vec::new();
    for (i, hz) in [440.0f32, 660.0].into_iter().enumerate() {
        let tone: Vec<f32> = (0..48_000 * 5)
            .map(|k| 0.25 * (k as f32 * std::f32::consts::TAU * hz / 48_000.0).sin())
            .collect();
        let file = d.join(format!("tone{i}.wav"));
        write_wav(
            &file,
            &[tone.clone(), tone],
            48_000,
            WavFormat::Pcm24,
            false,
        )
        .unwrap();
        songs.push(file);
    }
    s.dispatch(Action::Album(AlbumAction::AddFiles(songs)))
        .unwrap();
    let mut settings = s.project().album.settings.clone();
    settings.output = Some(out.to_path_buf());
    settings.album_file = true;
    settings.ddp = true;
    settings.sample_rate = Some(44_100);
    s.dispatch(Action::Album(AlbumAction::Settings(settings)))
        .unwrap();
    s
}

#[test]
fn an_album_export_asks_first_replaces_the_ddp_folder_whole_and_reports_its_files() {
    let d = tmp("album");
    let out = d.join("out");
    let mut s = album_session(&d, &out);
    let dests = s.album_destinations();
    let ddp = dests
        .iter()
        .find(|p| p.to_string_lossy().ends_with("(DDP)"))
        .unwrap()
        .clone();
    assert_eq!(ddp.parent(), Some(out.as_path()));
    assert_eq!(
        dests.len(),
        5,
        "two songs, album file, cue sheet, DDP: {dests:?}"
    );
    // Something there already: the first song's file and an old DDP
    // fileset with a file the new one does not have; and a temporary a
    // killed export left two hours ago.
    std::fs::create_dir_all(&ddp).unwrap();
    std::fs::write(ddp.join("OLD.TXT"), "an older fileset").unwrap();
    std::fs::write(&dests[0], "old song").unwrap();
    let stale = out.join(".01 tone0.ffrender-4000000000-1.wav");
    std::fs::write(&stale, "x").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        .unwrap();
    s.dispatch(Action::Album(AlbumAction::Export)).unwrap();
    let q = questions(&mut s);
    assert_eq!(q.len(), 1, "one question");
    assert_eq!(q[0].what, "Album");
    assert_eq!(q[0].paths, [dests[0].clone(), ddp.clone()]);
    assert!(
        s.album_progress().is_none(),
        "nothing runs before the answer"
    );
    // Cancel: nothing written.
    answer(&mut s, &q[0], false);
    assert!(s.album_progress().is_none());
    assert_eq!(read(&dests[0]), "old song");
    // Overwrite: the export runs and replaces them.
    s.dispatch(Action::Album(AlbumAction::Export)).unwrap();
    let q = questions(&mut s);
    answer(&mut s, &q[0], true);
    s.wait_album();
    let done = s
        .album_export()
        .unwrap_or_else(|| panic!("{:?}", s.notices().collect::<Vec<_>>()))
        .clone();
    assert!(read_wav(&dests[0]).is_ok(), "the song replaced");
    assert!(!ddp.join("OLD.TXT").exists(), "the fileset replaced whole");
    assert!(faderframe_disc::ddp::read(&ddp).unwrap().checksums == Some(true));
    assert_eq!(done.ddp.as_ref().unwrap().0, ddp);
    if cfg!(target_os = "linux") {
        assert!(!stale.exists(), "the killed export's temporary swept");
    }
    let _ = std::fs::remove_file(&stale);
    no_temporaries(&out);
    // The report: each song file and the album file, measured as written.
    let reqs = s.take_ui_requests();
    let Some(UiRequest::RenderReport { what, files }) = reqs
        .iter()
        .find(|r| matches!(r, UiRequest::RenderReport { .. }))
    else {
        panic!("no report: {reqs:?}");
    };
    assert!(what.starts_with("Album"), "{what}");
    let paths: Vec<&PathBuf> = files.iter().map(|f| &f.path).collect();
    assert_eq!(paths, [&dests[0], &dests[1], &dests[2]]);
    for f in files {
        let st = f.stats.clone().unwrap().unwrap();
        assert_eq!(st.path, f.path);
        assert_eq!(st.sample_rate, 44_100);
        assert_eq!(st.bytes, std::fs::metadata(&f.path).unwrap().len());
        let wav = read_wav(&f.path).unwrap();
        assert_eq!(st.frames, wav.channels[0].len() as u64);
        let peak = wav
            .channels
            .iter()
            .flatten()
            .fold(0f32, |m, x| m.max(x.abs()));
        let levels = st.levels.unwrap();
        assert!(
            (levels.sample_peak - 20.0 * f64::from(peak).log10()).abs() < 0.01,
            "{} vs {peak}",
            levels.sample_peak
        );
        assert!(st.render_seconds > 0.0);
    }
    assert!(files[0].finished.is_some(), "a song's levelling");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_file_appearing_while_the_album_is_written_is_asked_about() {
    let d = tmp("album-appears");
    let out = d.join("out");
    let mut s = album_session(&d, &out);
    let dests = s.album_destinations();
    s.dispatch(Action::Album(AlbumAction::Export)).unwrap();
    assert!(questions(&mut s).is_empty(), "nothing there yet");
    // The album file turns up while the songs are rendered.
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(&dests[2], "someone else's").unwrap();
    s.wait_album();
    let q = questions(&mut s);
    assert_eq!(q.len(), 1, "{:?}", s.notices().collect::<Vec<_>>());
    assert!(q[0].appeared);
    assert_eq!(q[0].paths, std::slice::from_ref(&dests[2]));
    assert!(s.album_export().is_none(), "waiting");
    assert!(!dests[0].exists(), "nothing moved while one is in the way");
    // Cancel: the export is thrown away, the file stays.
    answer(&mut s, &q[0], false);
    assert_eq!(read(&dests[2]), "someone else's");
    assert!(!dests[0].exists());
    no_temporaries(&out);
    // Again, and Overwrite this time.
    std::fs::remove_file(&dests[2]).unwrap();
    s.dispatch(Action::Album(AlbumAction::Export)).unwrap();
    std::fs::write(&dests[2], "someone else's").unwrap();
    s.wait_album();
    let q = questions(&mut s);
    answer(&mut s, &q[0], true);
    assert!(s.album_export().is_some());
    assert!(read_wav(&dests[2]).is_ok());
    for p in &dests {
        assert!(p.exists(), "{}", p.display());
    }
    no_temporaries(&out);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_sample_file_asks_before_replacing_and_when_one_appears() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    let d = tmp("sample");
    let path = d.join("Bass Sample.wav");
    std::fs::write(&path, "old").unwrap();
    let make = |s: &mut Session| {
        s.dispatch(Action::MakeSample {
            track: bass,
            start: MusicalTime::ZERO,
            end: MusicalTime::from_quarters(2.0),
            target: SampleTarget::File(path.clone()),
        })
        .unwrap();
    };
    let wait = |s: &mut Session| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !s.sampling().is_empty() && std::time::Instant::now() < end {
            s.tick(0.01);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    make(&mut s);
    let q = questions(&mut s);
    assert_eq!(q[0].paths, std::slice::from_ref(&path));
    assert!(
        s.sampling().is_empty(),
        "nothing rendered before the answer"
    );
    answer(&mut s, &q[0], true);
    wait(&mut s);
    assert_eq!(read_wav(&path).unwrap().format, WavFormat::Pcm24);
    // A file that appears while it is made.
    std::fs::remove_file(&path).unwrap();
    make(&mut s);
    assert!(questions(&mut s).is_empty());
    std::fs::write(&path, "appeared").unwrap();
    wait(&mut s);
    let q = questions(&mut s);
    assert!(q[0].appeared, "{q:?}");
    answer(&mut s, &q[0], false);
    assert_eq!(read(&path), "appeared");
    no_temporaries(&d);
    std::fs::remove_dir_all(&d).unwrap();
}

/// A folder where a file is to go cannot be replaced by it: the export or
/// render is refused at once — nothing asked, nothing rendered, the
/// folder untouched.
#[test]
fn a_folder_where_a_file_goes_is_refused_before_anything_runs() {
    let d = tmp("in-the-way");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let folder = d.join("Demo.mid");
    std::fs::create_dir_all(folder.join("inside")).unwrap();
    let e = s.export_midi_file(&folder, None).unwrap_err().to_string();
    assert!(e.contains("is a folder"), "{e}");
    assert!(questions(&mut s).is_empty());
    assert!(folder.join("inside").is_dir());
    // A render the same way (the Render window says so before asking).
    let wav = d.join("Mix.wav");
    std::fs::create_dir_all(&wav).unwrap();
    let settings = faderframe_session::render::RenderSettings::defaults_for(s.project(), wav);
    assert!(matches!(
        s.render(settings),
        Err(faderframe_session::render::RenderError::NotAFile(_))
    ));
    // The album: a song file's name taken by a folder.
    let out = d.join("album");
    let mut s = album_session(&d, &out);
    let first = s.album_destinations()[0].clone();
    std::fs::create_dir_all(&first).unwrap();
    assert!(s.dispatch(Action::Album(AlbumAction::Export)).is_err());
    assert!(questions(&mut s).is_empty());
    assert!(s.album_progress().is_none(), "nothing rendered");
    assert!(first.is_dir());
    no_temporaries(&d);
    std::fs::remove_dir_all(&d).unwrap();
}

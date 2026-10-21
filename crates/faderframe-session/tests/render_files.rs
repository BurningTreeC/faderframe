//! Render destinations and statistics: a track renders alone and its file
//! is measured as written (a tone at a known level reads that level, a
//! clipping one shows its overs); an existing destination is replaced
//! only with consent — after the render succeeded, by a rename from a
//! temporary file beside it — and stays as it was when the render is
//! refused, cancelled or fails; a file that appears while rendering is
//! caught at the end, the render held aside until the user decides.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::render::{
    self, Overwrite, RenderError, RenderRange, RenderSettings, RenderSource, files,
};
use faderframe_session::{Action, ImportTarget, Session};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

const SR: u32 = 48_000;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-render-files-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stereo 997 Hz tone of `amp` (peak), `seconds` long, as a float file.
fn tone_file(dir: &Path, name: &str, amp: f32, seconds: f32) -> PathBuf {
    let x: Vec<f32> = (0..(SR as f32 * seconds) as usize)
        .map(|i| amp * (i as f32 * 997.0 * std::f32::consts::TAU / SR as f32).sin())
        .collect();
    let path = dir.join(format!("{name}.wav"));
    write_wav(&path, &[x.clone(), x], SR, WavFormat::Float32, false).unwrap();
    path
}

/// A session with one audio track per `(name, file)`.
fn session(tracks: &[(&str, PathBuf)]) -> (Session, Vec<TrackId>) {
    let mut s = Session::new(Project::new("Renders", SR), None, EngineConfig::default()).unwrap();
    let mut ids = Vec::new();
    for (name, file) in tracks {
        let t = s.add_track(TrackKind::Audio).unwrap();
        s.dispatch(Action::Edit(Command::RenameTrack {
            track: t,
            name: (*name).into(),
        }))
        .unwrap();
        // Stereo all the way: no pan law between the file and the master.
        s.dispatch(Action::Edit(Command::SetTrackLayout {
            track: t,
            layout: faderframe_core::ChannelLayout::Stereo,
        }))
        .unwrap();
        s.import_audio(
            vec![file.clone()],
            ImportTarget {
                track: Some(t),
                at: MusicalTime::ZERO,
            },
        );
        s.wait_for_imports();
        ids.push(t);
    }
    (s, ids)
}

/// Settings rendering `track` alone over its clip to `output`.
fn track_settings(s: &Session, track: TrackId, output: PathBuf) -> RenderSettings {
    let p = s.project();
    let clip = p.clips_of(track)[0];
    RenderSettings {
        range: RenderRange::Span {
            start: clip.start,
            end: clip.end(&p.timeline, p.sample_rate),
        },
        source: RenderSource::Track(track),
        tail_seconds: 0.0,
        ..RenderSettings::defaults_for(p, output)
    }
}

/// What is in `dir` (no temporary files may stay).
fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_track_renders_alone_and_its_file_is_measured_as_written() {
    let d = dir("track");
    let quiet = tone_file(&d, "quiet", 10f32.powf(-6.0 / 20.0), 6.0);
    let loud = tone_file(&d, "loud", 0.9, 6.0);
    let (mut s, ids) = session(&[("Tone", quiet), ("Other", loud)]);
    let out = d.join("Tone render.wav");
    let settings = RenderSettings {
        format: WavFormat::Pcm24,
        ..track_settings(&s, ids[0], out.clone())
    };
    assert_eq!(
        render::destinations(s.project(), &settings).unwrap(),
        std::slice::from_ref(&out)
    );
    let files = s.render(settings).unwrap().join().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, out);
    assert!(files[0].finished.is_none(), "the level was left alone");
    let st = files[0].stats.clone().unwrap().unwrap();
    // The header as written.
    assert_eq!(st.path, out);
    assert_eq!(st.bytes, std::fs::metadata(&out).unwrap().len());
    assert_eq!(
        (st.container.as_str(), st.encoding.as_str()),
        ("WAV", "24-bit PCM")
    );
    assert_eq!(
        (st.sample_rate, st.channels, st.layout.as_str()),
        (SR, 2, "Stereo")
    );
    assert!(st.from_file && st.notes.is_empty(), "{:?}", st.notes);
    let wav = read_wav(&out).unwrap();
    assert_eq!(st.frames, wav.channels[0].len() as u64);
    assert!((st.seconds() - 6.0).abs() < 0.01, "{}", st.seconds());
    assert!(st.render_seconds > 0.0 && st.realtime_factor().unwrap() > 0.0);
    // The levels: the tone alone (the other track, at −0.9 dBFS, is not
    // in it), as the file holds it.
    let l = st.levels.as_ref().unwrap();
    assert!((l.sample_peak + 6.0).abs() < 0.05, "{l:?}");
    assert!(
        l.true_peak >= l.sample_peak - 1e-6 && l.true_peak < -5.9,
        "{l:?}"
    );
    assert!((l.rms + 9.01).abs() < 0.05, "{l:?}");
    let loudness = l.loudness.unwrap();
    assert!((loudness.integrated + 6.0).abs() < 0.15, "{loudness:?}");
    assert!(loudness.range < 0.5, "{loudness:?}");
    assert_eq!((l.full_scale_samples, l.overs), (0, 0));
    assert!(st.warnings().is_empty(), "{:?}", st.warnings());
    let again = faderframe_analysis::delivery::measure(&wav.channels, wav.sample_rate);
    assert!((again.integrated - loudness.integrated).abs() < 0.01);
    assert!((again.sample_peak - l.sample_peak).abs() < 1e-6);
    let summary = render::RenderSummary::of(&files);
    assert_eq!(
        (summary.files, summary.bytes, summary.warned),
        (1, st.bytes, 0)
    );
    assert_eq!(
        entries(&d),
        ["Tone render.wav", "loud.wav", "quiet.wav"],
        "no temporary file stays"
    );
    // A track that makes no audio renders nothing.
    let folder = s.add_track(TrackKind::Folder).unwrap();
    let bad = RenderSettings {
        source: RenderSource::Track(folder),
        ..track_settings(&s, ids[0], d.join("folder.wav"))
    };
    assert!(matches!(s.render(bad), Err(RenderError::NoTrack(_))));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_clipping_render_shows_its_overs() {
    let d = dir("clipping");
    let hot = tone_file(&d, "hot", 1.5, 2.0);
    let (mut s, ids) = session(&[("Hot", hot)]);
    // 16-bit: the file clips at full scale.
    let fixed = RenderSettings {
        format: WavFormat::Pcm16,
        ..track_settings(&s, ids[0], d.join("fixed.wav"))
    };
    let files = s.render(fixed).unwrap().join().unwrap();
    let st = files[0].stats.clone().unwrap().unwrap();
    let l = st.levels.as_ref().unwrap();
    assert!(l.full_scale_samples > 10_000, "{l:?}");
    // Both channels, each half cycle of 997 Hz for 2 s on a rail.
    assert!(l.overs > 3_900, "{l:?}");
    assert!(l.sample_peak > -0.01, "{l:?}");
    let warnings = st.warnings();
    assert!(
        warnings.iter().any(|w| w.contains("clipped")),
        "{warnings:?}"
    );
    // A float file keeps the peaks over 0 dBFS: they are named too.
    let float = RenderSettings {
        format: WavFormat::Float32,
        ..track_settings(&s, ids[0], d.join("float.wav"))
    };
    let files = s.render(float).unwrap().join().unwrap();
    let st = files[0].stats.clone().unwrap().unwrap();
    let l = st.levels.as_ref().unwrap();
    assert!(l.above_full_scale > 10_000, "{l:?}");
    assert!((l.sample_peak - 3.52).abs() < 0.05, "{l:?}");
    let warnings = st.warnings();
    assert!(
        warnings.iter().any(|w| w.contains("above 0 dBFS")),
        "{warnings:?}"
    );
    assert!(warnings.iter().any(|w| w.contains("dBTP")), "{warnings:?}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_existing_file_is_replaced_only_after_consent_and_a_finished_render() {
    let d = dir("existing");
    let tone = tone_file(&d, "tone", 0.25, 2.0);
    let (mut s, ids) = session(&[("Tone", tone)]);
    let out = d.join("Mix.wav");
    std::fs::write(&out, b"the old mix").unwrap();
    let settings = track_settings(&s, ids[0], out.clone());
    let wanted = render::destinations(s.project(), &settings).unwrap();
    assert_eq!(
        files::existing(&wanted),
        std::slice::from_ref(&out),
        "asked before rendering"
    );

    // Not agreed to: rendered, held aside, the old file untouched.
    let Err(RenderError::Exists(staged)) = s.render(settings.clone()).unwrap().join() else {
        panic!("replaced without consent");
    };
    assert_eq!(staged.conflicts(), std::slice::from_ref(&out));
    assert_eq!(std::fs::read(&out).unwrap(), b"the old mix");
    assert_eq!(entries(&d).len(), 3, "the render waits beside it");
    staged.discard();
    assert_eq!(entries(&d), ["Mix.wav", "tone.wav"]);
    assert_eq!(std::fs::read(&out).unwrap(), b"the old mix");

    // Cancelled: nothing replaced, nothing left behind.
    let long = RenderSettings {
        range: RenderRange::Bars { start: 0, end: 400 },
        overwrite: Overwrite::Confirmed(vec![out.clone()]),
        ..settings.clone()
    };
    let job = s.render(long).unwrap();
    job.cancel();
    assert!(matches!(job.join(), Err(RenderError::Cancelled)));
    assert_eq!(std::fs::read(&out).unwrap(), b"the old mix");
    assert_eq!(entries(&d), ["Mix.wav", "tone.wav"]);

    // Failed (a folder that does not exist): reported, nothing touched.
    let lost = RenderSettings {
        output: d.join("tone.wav").join("Mix.wav"),
        ..settings.clone()
    };
    assert!(matches!(
        s.render(lost).unwrap().join(),
        Err(RenderError::Folder { .. })
    ));
    assert_eq!(entries(&d), ["Mix.wav", "tone.wav"]);

    // Agreed: replaced by the finished render, measured.
    let agreed = RenderSettings {
        overwrite: Overwrite::Confirmed(vec![out.clone()]),
        ..settings
    };
    let files = s.render(agreed).unwrap().join().unwrap();
    assert!(files[0].stats.clone().unwrap().is_ok());
    let wav = read_wav(&out).unwrap();
    assert_eq!(wav.channels.len(), 2);
    assert_eq!(entries(&d), ["Mix.wav", "tone.wav"]);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_file_that_appears_while_rendering_is_not_replaced_unasked() {
    let d = dir("appears");
    let tone = tone_file(&d, "tone", 0.25, 2.0);
    let (mut s, ids) = session(&[("Tone", tone)]);
    let out = d.join("Mix.wav");
    // Nothing there when the render starts (no question asked) …
    let settings = RenderSettings {
        range: RenderRange::Bars { start: 0, end: 60 },
        ..track_settings(&s, ids[0], out.clone())
    };
    assert!(files::existing(&render::destinations(s.project(), &settings).unwrap()).is_empty());
    let job = s.render(settings).unwrap();
    // … then a file appears there while it renders.
    while job.progress.done.load(Ordering::Relaxed) == 0 && !job.is_finished() {
        std::thread::yield_now();
    }
    assert!(!job.is_finished(), "the render was over too soon to test");
    std::fs::write(&out, b"written meanwhile").unwrap();
    let Err(RenderError::Exists(staged)) = job.join() else {
        panic!("the new file was replaced");
    };
    assert_eq!(std::fs::read(&out).unwrap(), b"written meanwhile");
    assert_eq!(staged.conflicts(), std::slice::from_ref(&out));
    // Asked now, the user agrees: the render moves into place.
    let files = staged
        .commit(&Overwrite::Confirmed(vec![out.clone()]))
        .unwrap();
    assert_eq!(files[0].path, out);
    assert!(files[0].stats.clone().unwrap().unwrap().levels.is_some());
    assert_eq!(read_wav(&out).unwrap().channels.len(), 2);
    assert_eq!(entries(&d), ["Mix.wav", "tone.wav"]);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn stems_list_every_file_and_move_together() {
    let d = dir("stems");
    let a = tone_file(&d, "a", 0.25, 1.0);
    let b = tone_file(&d, "b", 0.5, 1.0);
    // Two tracks of one name: two files all the same.
    let (mut s, _) = session(&[("Gtr", a), ("gtr", b)]);
    let out = d.join("stems");
    let settings = RenderSettings {
        range: RenderRange::Bars { start: 0, end: 1 },
        source: RenderSource::Stems,
        tail_seconds: 0.0,
        ..RenderSettings::defaults_for(s.project(), out.clone())
    };
    let wanted = render::destinations(s.project(), &settings).unwrap();
    assert_eq!(
        wanted,
        [
            out.join("Renders - Gtr.wav"),
            out.join("Renders - gtr (2).wav")
        ]
    );
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(&wanted[1], b"old stem").unwrap();
    assert_eq!(files::existing(&wanted), std::slice::from_ref(&wanted[1]));
    let Err(RenderError::Exists(staged)) = s.render(settings.clone()).unwrap().join() else {
        panic!("a stem was replaced without consent");
    };
    assert!(!wanted[0].exists(), "no stem moves while one is in the way");
    assert_eq!(std::fs::read(&wanted[1]).unwrap(), b"old stem");
    drop(staged);
    assert_eq!(entries(&out), ["Renders - gtr (2).wav"]);
    let files = s
        .render(RenderSettings {
            overwrite: Overwrite::Confirmed(vec![wanted[1].clone()]),
            ..settings
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(files.len(), 2);
    let peaks: Vec<f64> = files
        .iter()
        .map(|f| {
            f.stats
                .clone()
                .unwrap()
                .unwrap()
                .levels
                .unwrap()
                .sample_peak
        })
        .collect();
    // Each stem is its own track.
    assert!((peaks[0] - 20.0 * 0.25f64.log10()).abs() < 0.1, "{peaks:?}");
    assert!((peaks[1] - 20.0 * 0.5f64.log10()).abs() < 0.1, "{peaks:?}");
    assert_eq!(
        entries(&out),
        ["Renders - Gtr.wav", "Renders - gtr (2).wav"]
    );
    std::fs::remove_dir_all(&d).unwrap();
}

//! Delivery QC: stems null-tested against the mix (tracks straight to the
//! master null to the format; a bus's stem is left out of the sum; a
//! limiter on the master keeps them from nulling and is named as the
//! cause, before the render too; stems made to null — the mix's gain on
//! each, a Shared Processing stem — sum to a master render of the same
//! settings), a later null test of an untested stems render, and the QC
//! report: its JSON values (receipts that are the files' SHA-256, levels
//! as measured, checks against the spec) and a PDF that `pdftotext`
//! reads.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{Dither, WavFormat, read_wav, write_wav};
use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, OutputRouting, PluginRef, Project, TrackKind};
use faderframe_qc::{CauseKind, NullOutcome, Verdict};
use faderframe_session::render::{
    self, RenderJob, RenderRange, RenderSettings, RenderSource, Rendered, StemOptions,
};
use faderframe_session::{Action, ImportTarget, Session};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};

const SR: u32 = 48_000;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-qc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stereo tone of `hz` at `amp` (peak), `seconds` long, as a float
/// file (half a second of silence at its end).
fn tone_file(dir: &Path, name: &str, hz: f32, amp: f32, seconds: f32) -> PathBuf {
    let n = (SR as f32 * seconds) as usize;
    let x: Vec<f32> = (0..n)
        .map(|i| {
            if i + SR as usize / 2 > n {
                0.0
            } else {
                amp * (i as f32 * hz * std::f32::consts::TAU / SR as f32).sin()
            }
        })
        .collect();
    let path = dir.join(format!("{name}.wav"));
    write_wav(&path, &[x.clone(), x], SR, WavFormat::Float32, false).unwrap();
    path
}

/// A session with one stereo audio track per `(name, file)`.
fn session(name: &str, tracks: &[(&str, PathBuf)]) -> (Session, Vec<TrackId>) {
    let mut s = Session::new(Project::new(name, SR), None, EngineConfig::default()).unwrap();
    let mut ids = Vec::new();
    for (name, file) in tracks {
        let t = s.add_track(TrackKind::Audio).unwrap();
        s.dispatch(Action::Edit(Command::RenameTrack {
            track: t,
            name: (*name).into(),
        }))
        .unwrap();
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

fn three_tones(d: &Path, name: &str, amp: f32) -> (Session, Vec<TrackId>) {
    let a = tone_file(d, "a", 220.0, amp, 4.0);
    let b = tone_file(d, "b", 331.0, amp, 4.0);
    let c = tone_file(d, "c", 447.0, amp, 4.0);
    session(name, &[("Bass", a), ("Keys", b), ("Lead", c)])
}

/// Stems of the first clip's span to `out`.
fn stems(s: &Session, out: PathBuf, stems: StemOptions) -> RenderSettings {
    let p = s.project();
    let clip = p.clips_of(
        p.tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .unwrap()
            .id,
    )[0];
    RenderSettings {
        range: RenderRange::Span {
            start: clip.start,
            end: clip.end(&p.timeline, p.sample_rate),
        },
        source: RenderSource::Stems,
        tail_seconds: 0.25,
        stems,
        ..RenderSettings::defaults_for(p, out)
    }
}

fn run(
    s: &mut Session,
    settings: RenderSettings,
) -> (Vec<Rendered>, Option<faderframe_qc::NullTest>) {
    let job: RenderJob = s.render(settings).unwrap();
    while !job.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let test = job.null_test();
    (job.join().unwrap(), test)
}

fn limiter_on_master(s: &mut Session) {
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::InsertPlugin {
        track: master,
        index: 0,
        plugin: PluginRef::builtin(builtin::LIMITER, "Limiter"),
    })
    .unwrap();
}

#[test]
fn stems_straight_to_the_master_null_and_the_report_says_so() {
    let d = dir("null");
    let (mut s, _) = three_tones(&d, "Nulls", 0.2);
    let tests = StemOptions {
        null_test: true,
        shared_stem: false,
    };
    // Floating point: exact.
    let settings = RenderSettings {
        format: WavFormat::Float32,
        ..stems(&s, d.join("float"), tests)
    };
    assert!(s.null_causes(&settings).is_empty(), "nothing in the way");
    let (files, test) = run(&mut s, settings);
    assert_eq!(files.len(), 3);
    let test = test.expect("a null test");
    assert_eq!(test.outcome, NullOutcome::Exact, "{test:?}");
    assert_eq!(test.verdict, Verdict::Pass);
    assert_eq!(test.summed.len(), 3);
    assert!(test.left_out.is_empty());
    assert!(test.problems().next().is_none(), "{:?}", test.causes);
    // Only the stems are left beside each other: the test's mix is gone.
    let mut names: Vec<String> = std::fs::read_dir(d.join("float"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["Nulls - Bass.wav", "Nulls - Keys.wav", "Nulls - Lead.wav"]
    );
    // 16-bit with TPDF dither: to the dither (or better).
    let settings = RenderSettings {
        format: WavFormat::Pcm16,
        dither: Dither::Tpdf,
        ..stems(&s, d.join("pcm16"), tests)
    };
    let (_, test) = run(&mut s, settings);
    let test = test.unwrap();
    assert!(
        matches!(test.outcome, NullOutcome::Exact | NullOutcome::DitherFloor),
        "{test:?}"
    );
    assert!(test.causes.iter().any(|c| c.kind == CauseKind::Dither));
    let mix = test.mix.as_ref().unwrap();
    assert!(mix.facts.as_ref().is_some_and(|f| f.bits == Some(16)));
}

#[test]
fn a_bus_stem_is_left_out_of_the_sum() {
    let d = dir("bus");
    let (mut s, ids) = three_tones(&d, "Bussed", 0.2);
    let bus = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: bus,
        name: "Drums".into(),
    }))
    .unwrap();
    for t in &ids[..2] {
        s.dispatch(Action::Edit(Command::SetTrackOutput {
            track: *t,
            output: OutputRouting::Track { track: bus },
        }))
        .unwrap();
    }
    let (summed, left) = render::null::stems_summed(s.project());
    assert_eq!(summed, ids, "the tracks no track feeds");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].0, bus);
    let settings = RenderSettings {
        format: WavFormat::Float32,
        ..stems(
            &s,
            d.join("stems"),
            StemOptions {
                null_test: true,
                shared_stem: false,
            },
        )
    };
    let (files, test) = run(&mut s, settings);
    assert_eq!(files.len(), 4, "every stem is written");
    let test = test.unwrap();
    assert_eq!(test.outcome, NullOutcome::Exact, "{test:?}");
    assert_eq!(test.left_out.len(), 1);
    assert!(test.left_out[0].name.contains("Drums"));
    assert!(
        test.left_out[0].why.contains("a bus"),
        "{}",
        test.left_out[0].why
    );
}

#[test]
fn a_master_limiter_keeps_stems_from_nulling_and_is_named() {
    let d = dir("limiter");
    let (mut s, _) = three_tones(&d, "Limited", 0.4);
    limiter_on_master(&mut s);
    let settings = RenderSettings {
        format: WavFormat::Float32,
        ..stems(
            &s,
            d.join("stems"),
            StemOptions {
                null_test: true,
                shared_stem: false,
            },
        )
    };
    // Warned of before the render.
    let before = s.null_causes(&settings);
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].kind, CauseKind::NonLinear);
    assert!(before[0].text.contains("“Limiter”"), "{}", before[0].text);
    assert!(before[0].text.contains("the master"), "{}", before[0].text);
    assert!(before[0].text.contains("3 stems"), "{}", before[0].text);
    let (_, test) = run(&mut s, settings);
    let test = test.unwrap();
    assert_eq!(test.outcome, NullOutcome::Fails, "{test:?}");
    assert_eq!(test.verdict, Verdict::Fail);
    assert!(test.summary.contains("do not sum"), "{}", test.summary);
    assert!(
        test.causes
            .iter()
            .any(|c| c.kind == CauseKind::NonLinear && c.text.contains("Limiter")),
        "{:?}",
        test.causes
    );
    assert!(
        !test.causes.iter().any(|c| c.kind == CauseKind::Latency),
        "no offset: {:?}",
        test.causes
    );
    let f = test.residual;
    assert!(
        f.peak_dbfs.unwrap() > -40.0 && f.worst_at.is_some(),
        "{f:?}"
    );
    // Per-stem level processing is another cause.
    let finished = RenderSettings {
        finish: faderframe_session::delivery::DELIVERY_PRESETS[0].finish,
        ..stems(&s, d.join("x"), StemOptions::default())
    };
    assert!(
        s.null_causes(&finished)
            .iter()
            .any(|c| c.kind == CauseKind::LevelPerStem)
    );
}

/// A compressor on `track` keyed by `key`, pressing hard.
fn keyed_compressor(s: &mut Session, track: TrackId, key: TrackId) {
    s.dispatch(Action::InsertPlugin {
        track,
        index: 0,
        plugin: PluginRef::builtin(builtin::COMPRESSOR, "Compressor"),
    })
    .unwrap();
    let plugin = s.project().track(track).unwrap().inserts[0].id;
    // Threshold, ratio, External (the key).
    for (parameter, value) in [(0, -40.0), (1, 10.0), (13, 1.0)] {
        s.dispatch(Action::Edit(Command::SetPluginParameter {
            track,
            plugin,
            parameter: faderframe_core::ParameterId(parameter),
            value: Some(value),
        }))
        .unwrap();
    }
    s.dispatch(Action::Edit(Command::SetPluginSidechain {
        track,
        plugin,
        source: Some(key),
    }))
    .unwrap();
}

#[test]
fn a_key_from_a_track_of_its_own_nulls_a_key_from_a_bus_does_not() {
    let d = dir("keys");
    let (mut s, ids) = three_tones(&d, "Keyed", 0.3);
    // The bass ducks under the keys: the key is taken before the keys'
    // fader, mute and solo, so the bass's stem ducks as the mix does.
    keyed_compressor(&mut s, ids[0], ids[1]);
    let settings = RenderSettings {
        format: WavFormat::Float32,
        ..stems(
            &s,
            d.join("own"),
            StemOptions {
                null_test: true,
                shared_stem: false,
            },
        )
    };
    assert!(
        s.null_causes(&settings).is_empty(),
        "{:?}",
        s.null_causes(&settings)
    );
    let (_, test) = run(&mut s, settings);
    let test = test.unwrap();
    assert_eq!(test.outcome, NullOutcome::Exact, "{test:?}");
    // Keyed by a bus the lead feeds: in the bass's stem the solo silences
    // the lead, so the key is silent.
    let master = s.project().master_id().unwrap();
    let plugin = s.project().track(ids[0]).unwrap().inserts[0].id;
    let bus = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackOutput {
        track: ids[2],
        output: OutputRouting::Track { track: bus },
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackOutput {
        track: bus,
        output: OutputRouting::Track { track: master },
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetPluginSidechain {
        track: ids[0],
        plugin,
        source: Some(bus),
    }))
    .unwrap();
    let settings = RenderSettings {
        format: WavFormat::Float32,
        ..stems(
            &s,
            d.join("bus"),
            StemOptions {
                null_test: true,
                shared_stem: false,
            },
        )
    };
    let foreseen = s.null_causes(&settings);
    assert!(
        foreseen.iter().any(|c| c.kind == CauseKind::Sidechain),
        "{foreseen:?}"
    );
    let (_, test) = run(&mut s, settings);
    let test = test.unwrap();
    assert_ne!(test.outcome, NullOutcome::Exact, "{test:?}");
    assert!(test.causes.iter().any(|c| c.kind == CauseKind::Sidechain));
}

/// Every channel of every file in `paths` summed, per channel.
fn sum_of(paths: &[PathBuf]) -> Vec<Vec<f64>> {
    let mut sum: Vec<Vec<f64>> = Vec::new();
    for p in paths {
        let w = read_wav(p).unwrap();
        sum.resize(w.channels.len(), Vec::new());
        for (s, c) in sum.iter_mut().zip(&w.channels) {
            s.resize(s.len().max(c.len()), 0.0);
            for (a, b) in s.iter_mut().zip(c) {
                *a += f64::from(*b);
            }
        }
    }
    sum
}

#[test]
fn stems_made_to_null_sum_to_a_master_render_of_the_same_settings() {
    let d = dir("shared");
    let (mut s, _) = three_tones(&d, "Shared", 0.4);
    limiter_on_master(&mut s);
    let finish = faderframe_session::delivery::DELIVERY_PRESETS[0].finish;
    let settings = RenderSettings {
        format: WavFormat::Pcm24,
        dither: Dither::Tpdf,
        finish,
        ..stems(
            &s,
            d.join("stems"),
            StemOptions {
                null_test: false,
                shared_stem: true,
            },
        )
    };
    assert!(
        s.null_causes(&settings)
            .iter()
            .all(|c| c.kind != CauseKind::LevelPerStem),
        "the gain is linked"
    );
    let (files, test) = run(&mut s, settings.clone());
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "Shared - Bass.wav",
            "Shared - Keys.wav",
            "Shared - Lead.wav",
            "Shared - Shared Processing.wav"
        ]
    );
    let test = test.unwrap();
    assert_eq!(test.outcome, NullOutcome::Exact, "{test:?}");
    assert_eq!(
        test.shared_stem.as_deref(),
        Some("Shared - Shared Processing.wav")
    );
    // Each stem got the mix's gain, not a level of its own.
    let gains: Vec<f64> = files[..3]
        .iter()
        .map(|f| f.finished.unwrap().gain_db)
        .collect();
    assert!(
        gains.iter().all(|g| (g - gains[0]).abs() < 1e-9),
        "{gains:?}"
    );
    // The master rendered on its own with the same settings: the files
    // sum to it to the last bit (24-bit codes).
    let master = d.join("master.wav");
    let (m, _) = run(
        &mut s,
        RenderSettings {
            source: RenderSource::Master,
            output: master.clone(),
            stems: StemOptions::default(),
            ..settings
        },
    );
    assert_eq!(m.len(), 1);
    let mix = read_wav(&master).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    let sum = sum_of(&paths);
    let lsb = 1.0 / 8_388_608.0;
    let mut worst = 0.0f64;
    for (s, m) in sum.iter().zip(&mix.channels) {
        assert_eq!(s.len(), m.len());
        for (a, b) in s.iter().zip(m) {
            worst = worst.max((a - f64::from(*b)).abs());
        }
    }
    assert!(worst <= 0.5 * lsb, "the sum is off by {} LSB", worst / lsb);
}

#[test]
fn an_untested_stems_render_is_null_tested_later_and_reported() {
    let d = dir("report");
    let (mut s, _) = three_tones(&d, "Report", 0.2);
    let settings = RenderSettings {
        format: WavFormat::Pcm24,
        ..stems(&s, d.join("stems"), StemOptions::default())
    };
    let (files, test) = run(&mut s, settings);
    assert!(test.is_none());
    s.delivered("Stems", files.clone(), None);
    assert!(s.delivery().unwrap().can_null_test());
    s.dispatch(Action::NullTestStems).unwrap();
    s.wait_for_qc();
    let d_ = s.delivery().unwrap();
    let test = d_.null_test.clone().expect("tested");
    assert!(
        matches!(test.outcome, NullOutcome::Exact | NullOutcome::DitherFloor),
        "{test:?}"
    );
    assert!(!d_.can_null_test());

    // The report of it, against a streaming spec.
    let out = d.join("Report QC.pdf");
    s.dispatch(Action::SaveQcReport {
        path: out.clone(),
        spec: "streaming".into(),
    })
    .unwrap();
    s.wait_for_qc();
    let json_path = d.join("Report QC.json");
    assert!(out.exists() && json_path.exists());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    assert_eq!(v["schema"], "faderframe.qc/1");
    assert_eq!(v["title"], "Stems");
    assert_eq!(v["project"]["name"], "Report");
    assert_eq!(v["spec"]["id"], "streaming");
    let jfiles = v["files"].as_array().unwrap();
    assert_eq!(jfiles.len(), 3);
    for (j, f) in jfiles.iter().zip(&files) {
        // The receipt is the file's SHA-256.
        let hash = faderframe_qc::sha256::file(&f.path, &|| false).unwrap();
        assert_eq!(j["sha256"], hash.as_str());
        assert_eq!(j["role"], "stem");
        // Its levels as the render measured them.
        let st = f.stats.clone().unwrap().unwrap();
        let lv = st.levels.unwrap();
        let tp = j["levels"]["true_peak_dbtp"].as_f64().unwrap();
        assert!((tp - lv.true_peak).abs() < 1e-9);
        let lufs = j["levels"]["loudness"]["integrated_lufs"].as_f64().unwrap();
        assert!((lufs - lv.loudness.unwrap().integrated).abs() < 1e-9);
        assert_eq!(j["facts"]["bits"], 24);
        // A stem is not judged on the spec's loudness.
        let check = j["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "loudness.integrated")
            .unwrap();
        assert_eq!(check["verdict"], "info");
    }
    // The mix is the programme: three tones at −14 dBFS each make about
    // −10 LUFS, louder than the target — a warning for streaming (it is
    // turned down), not a failure.
    let mix_checks = v["null_test"]["mix"]["checks"].as_array().unwrap();
    let loud = mix_checks
        .iter()
        .find(|c| c["id"] == "loudness.integrated")
        .unwrap();
    assert_eq!(loud["verdict"], "warn");
    assert!(v["null_test"]["outcome"] == "exact" || v["null_test"]["outcome"] == "dither-floor");
    assert!(v["created"].as_str().unwrap().ends_with('Z'));
    let stems_row = v["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Stems")
        .unwrap();
    assert!(
        stems_row["value"]
            .as_str()
            .unwrap()
            .contains("after the render")
    );

    // The PDF reads as text (when pdftotext is there).
    let text = std::process::Command::new("pdftotext")
        .arg(&out)
        .arg("-")
        .output();
    match text {
        Ok(o) if o.status.success() => {
            let t = String::from_utf8_lossy(&o.stdout);
            assert!(t.contains("Delivery QC Report"), "{t}");
            assert!(t.contains("Stems — Report") || t.contains("Stems"), "{t}");
            assert!(t.contains("Report - Bass.wav"), "{t}");
            assert!(t.contains("Null test"), "{t}");
            assert!(t.contains("SHA-256"), "{t}");
            let hash = faderframe_qc::sha256::file(&files[0].path, &|| false).unwrap();
            // Hashes may wrap: the first half is on one line.
            assert!(t.contains(&hash[..32]), "{t}");
        }
        _ => eprintln!("pdftotext not available: the PDF text is not checked"),
    }
    if let Ok(o) = std::env::var("FADERFRAME_QC_SESSION_OUT") {
        let o = Path::new(&o);
        let _ = std::fs::copy(&out, o.join("session-qc.pdf"));
        let _ = std::fs::copy(&json_path, o.join("session-qc.json"));
    }
    // Written again: the existing report is asked about first.
    s.dispatch(Action::SaveQcReport {
        path: out.clone(),
        spec: "ebu-r128".into(),
    })
    .unwrap();
    s.wait_for_qc();
    assert!(
        s.take_ui_requests()
            .iter()
            .any(|r| matches!(r, faderframe_session::UiRequest::AskReplace(_))),
        "asked before replacing"
    );
}

#[test]
fn an_album_with_a_spec_writes_its_report_with_every_export() {
    use faderframe_session::album::AlbumAction;
    let d = dir("album");
    let config = EngineConfig::default();
    let mut p = faderframe_project::demo::demo_project(config.sample_rate);
    p.sections.clear();
    let mut s = Session::new(p, None, config).unwrap();
    for (a, b) in [(0.0, 16.0), (16.0, 32.0)] {
        s.dispatch(Action::AddSection {
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
        })
        .unwrap();
    }
    s.dispatch(Action::Album(AlbumAction::AddSections)).unwrap();
    let mut settings = s.project().album.settings.clone();
    settings.format = WavFormat::Pcm16;
    settings.sample_rate = Some(44_100);
    settings.dither = Dither::Shaped;
    settings.output = Some(d.join("out"));
    settings.tail = 0.5;
    settings.album_file = true;
    settings.qc_spec = Some("cd".into());
    s.dispatch(Action::Album(AlbumAction::Settings(settings)))
        .unwrap();
    s.dispatch(Action::Album(AlbumAction::Export)).unwrap();
    s.wait_album();
    s.wait_for_qc();
    let name = render::sanitize(&s.project().name);
    let json = d.join("out").join(format!("{name} QC Report.json"));
    assert!(json.exists(), "{}", json.display());
    assert!(d.join("out").join(format!("{name} QC Report.pdf")).exists());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert!(v["title"].as_str().unwrap().starts_with("Album"));
    assert_eq!(v["spec"]["id"], "cd");
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "two songs and the album file");
    for f in files {
        assert_eq!(f["role"], "programme");
        let rate = f["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "format.sample_rate")
            .unwrap();
        assert_eq!(rate["verdict"], "pass", "{f}");
        assert_eq!(f["sha256"].as_str().unwrap().len(), 64);
    }
    // The cue sheet is a receipt.
    let receipts = v["receipts"].as_array().unwrap();
    assert!(
        receipts
            .iter()
            .any(|r| r["name"].as_str().unwrap().ends_with(".cue")),
        "{receipts:?}"
    );
    // The last export's report again.
    s.take_ui_requests();
    s.dispatch(Action::Album(AlbumAction::ShowReport)).unwrap();
    assert!(
        s.take_ui_requests()
            .iter()
            .any(|r| matches!(r, faderframe_session::UiRequest::RenderReport { .. }))
    );
    assert!(s.delivery().unwrap().title.starts_with("Album"));
}

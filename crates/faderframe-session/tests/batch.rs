//! Batch processing on generated files: normalising reaches its peak,
//! loudness its target, conversion its rate, depth and channels (WAV and
//! FLAC), trimming and fades do what they say, a built-in plugin changes
//! the level by its gain, names follow the pattern without ever taking an
//! input's or another file's, several files run side by side, a cancel
//! leaves nothing half written, and presets keep their chains.
#![allow(clippy::unwrap_used)]

use faderframe_analysis::delivery;
use faderframe_audio_files::{Dither, WavFormat, read_wav, write_wav};
use faderframe_core::ParameterId;
use faderframe_project::{FadeShape, PluginRef};
use faderframe_session::batch::{
    self, BatchChain, BatchOutput, BatchRequest, BatchStatus, BatchStep, Channels, ConvertStep,
    FadeStep, FileFormat, FileState, IfExists, LoudnessStep, NormalizeStep, PeakKind, PluginStep,
    Stamp, TrimStep,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const SR: u32 = 48_000;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-batch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(seconds: f64, amp: f32, hz: f32) -> Vec<f32> {
    (0..(seconds * f64::from(SR)) as usize)
        .map(|i| amp * (i as f32 * hz * std::f32::consts::TAU / SR as f32).sin())
        .collect()
}

/// Noise-like programme (a few detuned tones and a little hash).
fn programme(seconds: f64, amp: f32) -> Vec<f32> {
    let mut seed = 0x1234_5678u32;
    (0..(seconds * f64::from(SR)) as usize)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let t = i as f32 / SR as f32;
            let tones = (t * 220.0 * std::f32::consts::TAU).sin()
                + 0.5 * (t * 331.0 * std::f32::consts::TAU).sin()
                + 0.25 * (t * 1_710.0 * std::f32::consts::TAU).sin();
            amp * (0.5 * tones + 0.1 * (seed as f32 / u32::MAX as f32 - 0.5))
        })
        .collect()
}

fn write(path: &Path, channels: &[Vec<f32>]) -> PathBuf {
    write_wav(path, channels, SR, WavFormat::Float32, false).unwrap();
    path.to_path_buf()
}

fn chain(steps: Vec<BatchStep>) -> BatchChain {
    BatchChain {
        steps,
        output: BatchOutput {
            depth: WavFormat::Float32,
            ..BatchOutput::default()
        },
    }
}

fn request(files: Vec<PathBuf>, chain: BatchChain, folder: &Path) -> BatchRequest {
    BatchRequest {
        files,
        chain,
        folder: folder.to_path_buf(),
        existing: IfExists::Number,
        workers: 2,
        preset: "Test".into(),
        stamp: Stamp {
            date: "2026-11-03".into(),
            time: "12-00-00".into(),
        },
    }
}

fn run(r: BatchRequest) -> BatchStatus {
    let status = batch::start(r).unwrap().wait();
    assert!(status.finished);
    status
}

/// The one output of a status (panics with the state otherwise).
fn output(status: &BatchStatus, i: usize) -> PathBuf {
    match &status.files[i].state {
        FileState::Done(r) => r.output.clone(),
        other => panic!("file {i}: {other:?}"),
    }
}

fn peak_db(channels: &[Vec<f32>]) -> f64 {
    let p = channels
        .iter()
        .flatten()
        .fold(0.0f32, |m, s| m.max(s.abs()));
    20.0 * f64::from(p).log10()
}

#[test]
fn normalising_brings_the_peak_to_the_ceiling() {
    let d = dir("normalise");
    let input = write(
        &d.join("quiet.wav"),
        &[tone(1.0, 0.1, 997.0), tone(1.0, 0.05, 997.0)],
    );
    let out = d.join("out");
    let status = run(request(
        vec![input.clone()],
        chain(vec![BatchStep::Normalize(NormalizeStep {
            peak: PeakKind::Sample,
            ceiling_db: -3.0,
        })]),
        &out,
    ));
    let back = read_wav(&output(&status, 0)).unwrap();
    assert!((peak_db(&back.channels) + 3.0).abs() < 0.01);
    // The balance between the channels stays.
    let right = peak_db(&back.channels[1..]);
    assert!((right - (-3.0 - 6.02)).abs() < 0.05, "{right}");

    // True peak: a tone a quarter of the rate, sampled between its peaks.
    let x: Vec<f32> = (0..SR as usize)
        .map(|i| 0.3 * (std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4).sin())
        .collect();
    let input = write(&d.join("between.wav"), &[x]);
    let status = run(request(
        vec![input],
        chain(vec![BatchStep::Normalize(NormalizeStep {
            peak: PeakKind::True,
            ceiling_db: -1.0,
        })]),
        &out,
    ));
    let back = read_wav(&output(&status, 0)).unwrap();
    let report = delivery::measure(&back.channels, SR);
    assert!((report.true_peak + 1.0).abs() < 0.1, "{report:?}");
    // The samples sit 3 dB under it.
    assert!((report.sample_peak + 4.0).abs() < 0.2, "{report:?}");
}

#[test]
fn loudness_reaches_its_target_under_its_ceiling() {
    let d = dir("loudness");
    let a = write(
        &d.join("a.wav"),
        &[programme(6.0, 0.05), programme(6.0, 0.04)],
    );
    let b = write(&d.join("b.wav"), &[programme(6.0, 0.9)]);
    let out = d.join("out");
    let mut c = chain(vec![BatchStep::Loudness(LoudnessStep {
        target_lufs: -16.0,
        true_peak_db: -1.0,
        lower_gain: false,
    })]);
    c.output.depth = WavFormat::Pcm24;
    let status = run(request(vec![a, b], c, &out));
    for i in 0..2 {
        let back = read_wav(&output(&status, i)).unwrap();
        let report = delivery::measure(&back.channels, SR);
        assert!(
            (report.integrated + 16.0).abs() < 0.5,
            "file {i}: {report:?}"
        );
        assert!(report.true_peak <= -0.9, "file {i}: {report:?}");
        let FileState::Done(r) = &status.files[i].state else {
            unreachable!()
        };
        assert!((r.loudness + 16.0).abs() < 0.5);
        assert!(!r.notes.is_empty());
    }
}

#[test]
fn conversion_writes_the_rate_depth_and_channels_asked_for() {
    let d = dir("convert");
    let input = write(
        &d.join("wide.wav"),
        &[tone(1.0, 0.5, 440.0), tone(1.0, 0.3, 440.0)],
    );
    let out = d.join("out");
    let mut c = chain(vec![BatchStep::Convert(ConvertStep {
        sample_rate: Some(44_100),
        channels: Channels::Mono,
    })]);
    c.output.depth = WavFormat::Pcm16;
    c.output.dither = Dither::Off;
    let status = run(request(vec![input.clone()], c.clone(), &out));
    let back = read_wav(&output(&status, 0)).unwrap();
    assert_eq!(back.sample_rate, 44_100);
    assert_eq!(back.format, WavFormat::Pcm16);
    assert_eq!(back.channels.len(), 1);
    assert_eq!(back.channels[0].len(), 44_100);
    // The average of the two sides.
    let middle = &back.channels[..];
    let p = peak_db(middle);
    assert!((p - 20.0 * 0.4f64.log10()).abs() < 0.1, "{p}");

    // FLAC, 24 bits, stereo from mono.
    let mono = write(&d.join("mono.wav"), &[tone(0.5, 0.5, 440.0)]);
    c.steps = vec![BatchStep::Convert(ConvertStep {
        sample_rate: None,
        channels: Channels::Stereo,
    })];
    c.output.format = FileFormat::Flac;
    c.output.depth = WavFormat::Pcm24;
    let status = run(request(vec![mono], c, &out));
    let flac = output(&status, 0);
    assert_eq!(flac.extension().unwrap(), "flac");
    let decoded =
        faderframe_audio_files::decode::decode_at_rate(&flac, SR, &AtomicBool::new(false)).unwrap();
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0].len(), SR as usize / 2);
    let x = tone(0.5, 0.5, 440.0);
    for side in &decoded {
        for (a, b) in side.iter().zip(&x) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}

#[test]
fn trimming_keeps_the_sound_and_its_pads() {
    let d = dir("trim");
    let mut x = vec![0.0f32; SR as usize];
    x.extend(tone(1.0, 0.5, 440.0));
    x.extend(vec![0.0f32; 2 * SR as usize]);
    let input = write(&d.join("gaps.wav"), &[x]);
    let out = d.join("out");
    let step = TrimStep {
        threshold_db: -50.0,
        min_length: 0.01,
        pre_pad: 0.01,
        post_pad: 0.1,
        start: true,
        end: true,
    };
    let status = run(request(
        vec![input.clone()],
        chain(vec![BatchStep::TrimSilence(step)]),
        &out,
    ));
    let back = read_wav(&output(&status, 0)).unwrap();
    let seconds = back.channels[0].len() as f64 / f64::from(SR);
    assert!((seconds - 1.11).abs() < 0.01, "{seconds}");
    // Only the end.
    let status = run(request(
        vec![input],
        chain(vec![BatchStep::TrimSilence(TrimStep {
            start: false,
            ..step
        })]),
        &out,
    ));
    let back = read_wav(&output(&status, 0)).unwrap();
    let seconds = back.channels[0].len() as f64 / f64::from(SR);
    assert!((seconds - 2.1).abs() < 0.01, "{seconds}");
}

#[test]
fn fades_shape_the_ends() {
    let d = dir("fades");
    let input = write(&d.join("dc.wav"), &[vec![0.5f32; SR as usize]]);
    let out = d.join("out");
    let status = run(request(
        vec![input],
        chain(vec![BatchStep::Fade(FadeStep {
            fade_in: 0.1,
            fade_out: 0.2,
            shape_in: FadeShape::Linear,
            shape_out: FadeShape::EqualPower,
        })]),
        &out,
    ));
    let back = read_wav(&output(&status, 0)).unwrap();
    let c = &back.channels[0];
    assert_eq!(c[0], 0.0);
    assert!((c[2_400] - 0.25).abs() < 1e-3, "{}", c[2_400]);
    assert_eq!(c[24_000], 0.5);
    assert_eq!(*c.last().unwrap(), 0.0);
    // Equal power: half way out at sin(π/4).
    let mid = c[SR as usize - 4_800];
    assert!(
        (mid - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3,
        "{mid}"
    );
}

#[test]
fn a_built_in_plugin_changes_the_level_by_its_gain() {
    let d = dir("plugin");
    let stereo = write(
        &d.join("stereo.wav"),
        &[tone(1.0, 0.5, 997.0), tone(1.0, 0.5, 997.0)],
    );
    let mono = write(&d.join("mono.wav"), &[tone(1.0, 0.5, 997.0)]);
    let out = d.join("out");
    let mut gain = PluginStep::new(PluginRef::builtin(
        faderframe_core::builtin::GAIN,
        "Utility",
    ));
    gain.set_parameter(ParameterId(0), -6.0);
    let status = run(request(
        vec![stereo, mono],
        chain(vec![BatchStep::Plugin(gain)]),
        &out,
    ));
    for (i, channels) in [(0, 2), (1, 1)] {
        let back = read_wav(&output(&status, i)).unwrap();
        assert_eq!(back.channels.len(), channels, "file {i}");
        assert_eq!(back.channels[0].len(), SR as usize, "file {i}");
        // After the first block (a value set ramps in).
        let rest: Vec<Vec<f32>> = back.channels.iter().map(|c| c[4_800..].to_vec()).collect();
        let p = peak_db(&rest);
        assert!(
            (p - (20.0 * 0.5f64.log10() - 6.0)).abs() < 0.05,
            "file {i}: {p}"
        );
    }
}

#[test]
fn a_plugin_refuses_more_than_two_channels_and_the_others_go_on() {
    let d = dir("six");
    let six = write(&d.join("six.wav"), &vec![tone(0.2, 0.5, 440.0); 6]);
    let fine = write(&d.join("fine.wav"), &[tone(0.2, 0.5, 440.0)]);
    let missing = d.join("missing.wav");
    let junk = d.join("junk.wav");
    std::fs::write(&junk, b"not audio at all").unwrap();
    let gain = PluginStep::new(PluginRef::builtin(
        faderframe_core::builtin::GAIN,
        "Utility",
    ));
    let status = run(request(
        vec![six, fine, missing, junk],
        chain(vec![BatchStep::Plugin(gain)]),
        &d.join("out"),
    ));
    assert!(
        matches!(&status.files[0].state, FileState::Failed(why) if why.contains("6 channels")),
        "{:?}",
        status.files[0].state
    );
    output(&status, 1);
    assert!(matches!(status.files[2].state, FileState::Failed(_)));
    assert!(matches!(status.files[3].state, FileState::Failed(_)));
    assert_eq!(status.done(), 1);
    assert_eq!(status.failed(), 3);
}

#[test]
fn names_follow_the_pattern_and_never_take_an_input() {
    let d = dir("names");
    let a = d.join("one");
    let b = d.join("two");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let x = write(&a.join("take.wav"), &[tone(0.1, 0.5, 440.0)]);
    let y = write(&b.join("take.wav"), &[tone(0.1, 0.5, 440.0)]);
    let out = d.join("out");
    let mut c = chain(Vec::new());
    c.output.pattern = "{n:2} {name} {rate} {preset}".into();
    let mut r = request(vec![x.clone(), y.clone()], c.clone(), &out);
    r.workers = 1;
    let status = run(r);
    let names: Vec<String> = (0..2)
        .map(|i| {
            output(&status, i)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert_eq!(names, ["01 take 48k Test.wav", "02 take 48k Test.wav"]);

    // The same name twice: numbered, in the list's order.
    c.output.pattern = "{name}".into();
    let mut r = request(vec![x.clone(), y], c.clone(), &out);
    r.workers = 1;
    let status = run(r);
    assert_eq!(output(&status, 0).file_name().unwrap(), "take.wav");
    assert_eq!(output(&status, 1).file_name().unwrap(), "take 2.wav");

    // Into the input's own folder, even replacing: the input stays.
    let before = std::fs::read(&x).unwrap();
    let mut r = request(vec![x.clone()], c.clone(), &a);
    r.existing = IfExists::Replace;
    let status = run(r);
    assert_eq!(output(&status, 0).file_name().unwrap(), "take 2.wav");
    assert_eq!(std::fs::read(&x).unwrap(), before);

    // Replacing an earlier output; skipping one.
    let mut r = request(vec![x.clone()], c.clone(), &out);
    r.existing = IfExists::Replace;
    let status = run(r);
    assert_eq!(output(&status, 0).file_name().unwrap(), "take.wav");
    let mut r = request(vec![x], c, &out);
    r.existing = IfExists::Skip;
    let status = run(r);
    assert!(matches!(status.files[0].state, FileState::Skipped(_)));
    // Nothing temporary left behind.
    for e in std::fs::read_dir(&out).unwrap() {
        let p = e.unwrap().path();
        assert!(
            !faderframe_session::render::files::is_temporary(&p),
            "{}",
            p.display()
        );
    }
}

#[test]
fn several_files_run_side_by_side() {
    let d = dir("parallel");
    let files: Vec<PathBuf> = (0..8)
        .map(|i| {
            write(
                &d.join(format!("f{i}.wav")),
                &[programme(1.0, 0.1 + 0.05 * i as f32)],
            )
        })
        .collect();
    let out = d.join("out");
    let mut r = request(
        files,
        chain(vec![
            BatchStep::Normalize(NormalizeStep {
                peak: PeakKind::Sample,
                ceiling_db: -6.0,
            }),
            BatchStep::Fade(FadeStep::default()),
        ]),
        &out,
    );
    r.workers = 4;
    let status = run(r);
    assert_eq!(status.done(), 8, "{}", status.summary());
    assert!((status.fraction() - 1.0).abs() < 1e-9);
    let mut names: Vec<PathBuf> = (0..8).map(|i| output(&status, i)).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 8);
    for n in &names {
        let back = read_wav(n).unwrap();
        assert!((peak_db(&back.channels) + 6.0).abs() < 0.01);
    }
}

#[test]
fn a_cancelled_batch_leaves_nothing_half_written() {
    let d = dir("cancel");
    let files: Vec<PathBuf> = (0..6)
        .map(|i| {
            write(
                &d.join(format!("long{i}.wav")),
                &[programme(20.0, 0.2), programme(20.0, 0.2)],
            )
        })
        .collect();
    let out = d.join("out");
    let mut r = request(
        files,
        chain(vec![
            BatchStep::Convert(ConvertStep {
                sample_rate: Some(44_100),
                channels: Channels::Keep,
            }),
            BatchStep::Loudness(LoudnessStep::default()),
        ]),
        &out,
    );
    r.workers = 1;
    let run = batch::start(r).unwrap();
    run.cancel();
    let status = run.wait();
    assert!(status.cancelled && status.finished);
    assert!(status.done() < 6);
    assert!(matches!(status.files[5].state, FileState::Cancelled));
    for e in std::fs::read_dir(&out).unwrap() {
        let p = e.unwrap().path();
        assert!(
            !faderframe_session::render::files::is_temporary(&p),
            "{}",
            p.display()
        );
    }
}

#[test]
fn nothing_starts_without_files_or_with_settings_that_cannot_work() {
    let d = dir("refused");
    assert!(matches!(
        batch::start(request(Vec::new(), chain(Vec::new()), &d)),
        Err(batch::BatchError::NoFiles)
    ));
    let mut c = chain(Vec::new());
    c.output.pattern = "{nope}".into();
    assert!(matches!(
        batch::start(request(vec![d.join("x.wav")], c, &d)),
        Err(batch::BatchError::Pattern(_))
    ));
}

#[test]
fn presets_keep_their_chains() {
    let d = dir("presets");
    let mut c = chain(vec![
        BatchStep::Loudness(LoudnessStep::default()),
        BatchStep::Fade(FadeStep::default()),
    ]);
    c.output.pattern = "{name} (mastered)".into();
    let path = batch::save_preset_in(&d, "My Master", &c).unwrap();
    assert!(path.starts_with(&d));
    let found = batch::preset_named_in(&d, "my master").unwrap();
    assert_eq!(found.chain, c);
    assert!(!found.factory);
    // Saving again under the name (any case) replaces it.
    c.steps.pop();
    let again = batch::save_preset_in(&d, "MY MASTER", &c).unwrap();
    assert_eq!(again, path);
    assert_eq!(batch::preset_named_in(&d, "My Master").unwrap().chain, c);
    let all = batch::presets_in(&d);
    assert_eq!(all.iter().filter(|p| !p.factory).count(), 1);
    assert!(all.iter().any(|p| p.factory && p.name.contains("Podcast")));
    // Factory names are not taken; factory presets stay.
    let factory = batch::factory_presets()[0].name.clone();
    assert!(batch::save_preset_in(&d, &factory, &c).is_err());
    assert!(batch::delete_preset_in(&d, &factory).is_err());
    batch::delete_preset_in(&d, "my master").unwrap();
    assert!(batch::preset_named_in(&d, "My Master").is_none());
    assert!(batch::delete_preset_in(&d, "My Master").is_err());
}

#[test]
fn a_factory_preset_runs_as_named() {
    let d = dir("factory");
    let input = write(&d.join("voice.wav"), &[programme(3.0, 0.3)]);
    let preset = batch::presets_in(&d)
        .into_iter()
        .find(|p| p.name == "CD 44.1/16")
        .unwrap();
    let status = run(request(vec![input], preset.chain, &d.join("out")));
    let back = read_wav(&output(&status, 0)).unwrap();
    assert_eq!(back.sample_rate, 44_100);
    assert_eq!(back.format, WavFormat::Pcm16);
    assert_eq!(back.channels.len(), 2);
}

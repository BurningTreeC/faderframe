//! Console summing: one step puts the mix through a console — the
//! channels through its line amplifiers, the buses and the master through
//! its bus amplifier circuit (latency compensated, the level kept at the
//! nominal level) — and takes it off again; families switch with the bus
//! settings kept, buses added later get theirs, and a bus's microphone
//! preamp stays.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::builtin::{CONSOLE_BUSES, console_bus_index};
use faderframe_core::{ParameterId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::render::{self, RenderChannels, RenderRange, RenderSettings};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn master(project: &Project, name: &str) -> Vec<f32> {
    let path = std::env::temp_dir().join(format!("ff-console-{}-{name}.wav", std::process::id()));
    let settings = RenderSettings {
        range: RenderRange::Bars { start: 0, end: 2 },
        channels: RenderChannels::Mono,
        tail_seconds: 0.0,
        normalize_db: None,
        format: WavFormat::Float32,
        ..RenderSettings::defaults_for(project, path.clone())
    };
    render::start(project.clone(), settings)
        .unwrap()
        .join()
        .unwrap();
    let wav = read_wav(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    wav.channels.into_iter().next().unwrap()
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// The 1 kHz tone in the second bar: its phase (degrees) and what is not
/// the tone (distortion and noise) against it.
fn tone(x: &[f32]) -> (f64, f64) {
    let x = &x[96_000..180_000];
    let w = std::f64::consts::TAU * 1000.0 / f64::from(SR);
    let (mut s, mut c) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let t = w * (96_000 + i) as f64;
        s += f64::from(*v) * t.sin();
        c += f64::from(*v) * t.cos();
    }
    let n = x.len() as f64;
    let (a, b) = (2.0 * s / n, 2.0 * c / n);
    let rest: f64 = x
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let t = w * (96_000 + i) as f64;
            (f64::from(*v) - a * t.sin() - b * t.cos()).powi(2)
        })
        .sum::<f64>()
        / n;
    (
        b.atan2(a).to_degrees(),
        rest.sqrt() / ((a * a + b * b) / 2.0).sqrt(),
    )
}

fn bus_amp(s: &Session, track: TrackId) -> Option<usize> {
    s.project()
        .track(track)
        .unwrap()
        .preamp
        .as_ref()
        .and_then(|p| console_bus_index(&p.plugin.id))
}

#[test]
fn a_console_runs_the_mix_and_comes_off_in_one_step() {
    let dir = std::env::temp_dir().join(format!("ff-console-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(Project::new("Console", SR), None, EngineConfig::default()).unwrap();
    // A 1 kHz tone at −18 dBFS RMS (the nominal level) through a bus.
    let sine: Vec<f32> = (0..200_000)
        .map(|i| 0.177 * (i as f32 * std::f32::consts::TAU * 1000.0 / SR as f32).sin())
        .collect();
    let file = dir.join("tone.wav");
    write_wav(&file, &[sine], SR, WavFormat::Float32, false).unwrap();
    let voice = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: voice,
        layout: faderframe_core::ChannelLayout::Mono,
    }))
    .unwrap();
    s.import_audio(
        vec![file],
        faderframe_session::ImportTarget {
            track: Some(voice),
            at: MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    let bus = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackOutput {
        track: voice,
        output: faderframe_project::OutputRouting::Track { track: bus },
    }))
    .unwrap();
    // A bus with a microphone preamp of its own.
    let miked = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::SetPreamp {
        track: miked,
        model: Some(1),
    })
    .unwrap();
    let master_id = s.project().master_id().unwrap();
    let dry = master(s.project(), "dry");
    let before = s.project().clone();
    let steps = s.history_steps().0.len();

    s.dispatch(Action::SetConsole { family: Some(2) }).unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one undo step");
    assert_eq!(s.console().unwrap().name(), "British 73");
    assert_eq!(bus_amp(&s, bus), Some(2));
    assert_eq!(bus_amp(&s, master_id), Some(2));
    assert_eq!(bus_amp(&s, miked), None, "the microphone preamp stays");
    assert!(s.project().track(miked).unwrap().preamp.is_some());
    assert!(s.project().track(voice).unwrap().preamp.is_none());
    // The bus amplifiers' latency is compensated and the level kept: at
    // the nominal level the console is nearly clean and lines up with the
    // mix in the box.
    assert!(s.engine().graph_stats().output_latency > 0);
    let console = master(s.project(), "console");
    let level = 20.0 * (rms(&console[96_000..180_000]) / rms(&dry[96_000..180_000])).log10();
    assert!(level.abs() < 0.3, "level {level:+.2} dB");
    // Lined up to the sample (one is 7.5° at 1 kHz) and clean.
    let ((dry_phase, _), (phase, clean)) = (tone(&dry), tone(&console));
    assert!(
        (phase - dry_phase).abs() < 4.0,
        "phase {phase:.1}° vs {dry_phase:.1}°"
    );
    assert!(clean < 0.002, "at the nominal level: {clean}");
    // Driven, it colours (the drive is taken off after: the level stays).
    s.dispatch(Action::Edit(Command::SetConsoleDrive { drive_db: 12.0 }))
        .unwrap();
    let driven = master(s.project(), "driven");
    let level = 20.0 * (rms(&driven[96_000..180_000]) / rms(&dry[96_000..180_000])).log10();
    assert!(level.abs() < 1.0, "driven level {level:+.2} dB");
    let (_, coloured) = tone(&driven);
    assert!(coloured > 3.0 * clean, "driven {coloured} vs {clean}");

    // Another family keeps the bus amplifiers' settings.
    let amp = s.project().master().unwrap().preamp.clone().unwrap();
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: master_id,
        plugin: amp.id,
        parameter: ParameterId(0),
        value: Some(3.0),
    }))
    .unwrap();
    s.dispatch(Action::SetConsole { family: Some(0) }).unwrap();
    let amp = s.project().master().unwrap().preamp.clone().unwrap();
    assert_eq!(amp.plugin.id, CONSOLE_BUSES[0].0);
    assert!(
        amp.parameters
            .iter()
            .any(|p| p.id == ParameterId(0) && p.value == 3.0),
        "{:?}",
        amp.parameters
    );
    assert_eq!(s.console().unwrap().drive_db, 12.0, "the drive stays");
    // A bus added now gets its amplifier.
    let later = s.add_track(TrackKind::Aux).unwrap();
    assert_eq!(bus_amp(&s, later), Some(0));
    // Off: every bus amplifier goes, the preamp stays.
    s.dispatch(Action::SetConsole { family: None }).unwrap();
    assert!(s.console().is_none());
    for t in [bus, master_id, later] {
        assert!(s.project().track(t).unwrap().preamp.is_none());
    }
    assert!(s.project().track(miked).unwrap().preamp.is_some());
    // Back to where it started, step by step.
    for _ in 0..6 {
        s.dispatch(Action::Undo).unwrap();
    }
    assert_eq!(s.project().console, None);
    assert_eq!(s.project().tracks, before.tracks);
    let _ = std::fs::remove_dir_all(&dir);
}

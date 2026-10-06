//! Clip effects: a chain on one clip renders into a new file the clip
//! plays, in one undo step; trims carry over to the next render; removing
//! the effects gives the clip its audio back.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ClipId, ParameterId};
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, PluginFormat, PluginRef, Project, SourceSpec};
use faderframe_session::clip_fx::ClipFxOp;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn session() -> (Session, ClipId, Vec<f32>) {
    let dir = std::env::temp_dir().join(format!("ff-session-clip-fx-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Tone.wav");
    let tone: Vec<f32> = (0..SR as usize)
        .map(|i| ((i as f64 * 330.0 * std::f64::consts::TAU / f64::from(SR)).sin() * 0.5) as f32)
        .collect();
    write_wav(&file, std::slice::from_ref(&tone), SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Clip FX", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    (s, clip, tone)
}

fn settle(s: &mut Session, clip: ClipId) {
    let start = Instant::now();
    while s.clip_fx_busy(clip) {
        assert!(start.elapsed() < Duration::from_secs(60), "render hangs");
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
    }
}

/// The audio the clip plays (its source's file, its region).
fn played(s: &Session, clip: ClipId) -> Vec<f32> {
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    let SourceSpec::File { path, .. } = &s.project().sources[&a.source].spec else {
        panic!("a file");
    };
    let wav = read_wav(path).unwrap();
    wav.channels[0][a.source_offset as usize..(a.source_offset + a.length) as usize].to_vec()
}

fn utility() -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: faderframe_core::builtin::GAIN.into(),
        name: "Utility".into(),
    }
}

#[test]
fn a_chain_renders_onto_the_clip_and_goes_again() {
    let (mut s, clip, tone) = session();
    let original = s.project().clip(clip).unwrap().as_audio().unwrap().source;
    let steps = s.history_steps().0.len();
    // A utility at −6 dB, its gain set while the render waits.
    s.dispatch(Action::ClipEffects {
        clip,
        op: ClipFxOp::Add(utility()),
    })
    .unwrap();
    assert!(!s.clip_fx_parameters(&utility()).is_empty());
    s.dispatch(Action::ClipEffects {
        clip,
        op: ClipFxOp::SetParameter {
            index: 0,
            parameter: ParameterId(0),
            value: -6.0206,
        },
    })
    .unwrap();
    assert_eq!(s.clip_fx_chain(clip).len(), 1, "shown at once");
    settle(&mut s, clip);
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_ne!(a.source, original, "plays the render");
    assert_eq!(a.effects.as_ref().unwrap().original.source, original);
    let out = played(&s, clip);
    assert_eq!(out.len(), tone.len());
    for (i, (x, y)) in out.iter().zip(&tone).enumerate().skip(4_800) {
        assert!((x - y * 0.5).abs() < 1e-3, "{i}: {x} vs {}", y * 0.5);
    }
    // Trimmed, then changed: the trim carries over.
    let mut trimmed = a.clone();
    trimmed.source_offset += 4_800;
    trimmed.length -= 4_800;
    let start = s.project().clip(clip).unwrap().start;
    s.dispatch(Action::Edit(Command::SetClipContent {
        clip,
        start,
        content: Box::new(ClipContent::Audio(trimmed)),
    }))
    .unwrap();
    s.dispatch(Action::ClipEffects {
        clip,
        op: ClipFxOp::SetParameter {
            index: 0,
            parameter: ParameterId(0),
            value: 0.0,
        },
    })
    .unwrap();
    settle(&mut s, clip);
    let out = played(&s, clip);
    assert_eq!(out.len(), tone.len() - 4_800);
    for (i, (x, y)) in out.iter().zip(&tone[4_800..]).enumerate().skip(100) {
        assert!((x - y).abs() < 1e-3, "{i}: {x} vs {y}");
    }
    // All gone: the clip's own audio, trimmed as it is.
    s.dispatch(Action::ClipEffects {
        clip,
        op: ClipFxOp::Clear,
    })
    .unwrap();
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_eq!((a.source, a.source_offset), (original, 4_800));
    assert!(a.effects.is_none());
    // And back by undo.
    s.dispatch(Action::Undo).unwrap();
    assert!(
        s.project()
            .clip(clip)
            .unwrap()
            .as_audio()
            .unwrap()
            .effects
            .is_some()
    );
}

#[test]
fn an_overtaken_render_is_dropped() {
    let (mut s, clip, tone) = session();
    s.dispatch(Action::ClipEffects {
        clip,
        op: ClipFxOp::Add(utility()),
    })
    .unwrap();
    // Changes in quick succession: only the last renders and lands.
    for db in [-20.0, -12.0, -6.0206] {
        s.dispatch(Action::ClipEffects {
            clip,
            op: ClipFxOp::SetParameter {
                index: 0,
                parameter: ParameterId(0),
                value: db,
            },
        })
        .unwrap();
        s.tick(0.01);
    }
    settle(&mut s, clip);
    let out = played(&s, clip);
    let peak = out[4_800..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let want = tone.iter().fold(0.0f32, |m, v| m.max(v.abs())) * 0.5;
    assert!((peak - want).abs() < 1e-3, "{peak} vs {want}");
    assert_eq!(s.history_steps().0.len(), 2, "import and one render");
}

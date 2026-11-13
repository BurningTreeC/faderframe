//! Clip effects: a chain on one clip renders into a new file the clip
//! plays, in one undo step; trims carry over to the next render; removing
//! the effects gives the clip its audio back; one take of a take folder
//! has a chain of its own.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ClipId, ParameterId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioRef, ClipContent, Command, PluginFormat, PluginRef, Project, SourceSpec, Take, TakeFolder,
    TakeRef,
};
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
    write_wav(
        &file,
        std::slice::from_ref(&tone),
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
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

fn settle(s: &mut Session, audio: impl Into<AudioRef>) {
    let audio = audio.into();
    let start = Instant::now();
    while s.clip_fx_busy(audio) {
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
        audio: clip.into(),
        op: ClipFxOp::Add(utility()),
    })
    .unwrap();
    assert!(!s.clip_fx_parameters(&utility()).is_empty());
    s.dispatch(Action::ClipEffects {
        audio: clip.into(),
        op: ClipFxOp::SetParameter {
            index: 0,
            parameter: ParameterId(0),
            value: -6.0206,
        },
    })
    .unwrap();
    assert_eq!(s.clip_fx_chain(clip.into()).len(), 1, "shown at once");
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
        audio: clip.into(),
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
        audio: clip.into(),
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
        audio: clip.into(),
        op: ClipFxOp::Add(utility()),
    })
    .unwrap();
    // Changes in quick succession: only the last renders and lands.
    for db in [-20.0, -12.0, -6.0206] {
        s.dispatch(Action::ClipEffects {
            audio: clip.into(),
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

/// A clip effect opens in its own editor window: the session hosts it,
/// answers for it as a plugin's owner, and turns the commands an editor
/// window sends about it (values, bypass) into edits of the clip's chain,
/// rendered as any other; a removed effect leaves no window behind.
#[test]
fn a_clip_effect_opens_in_its_own_window_and_its_commands_edit_the_chain() {
    let (mut s, clip, tone) = session();
    let track = s.project().clip(clip).unwrap().track;
    s.dispatch(Action::ClipEffects {
        audio: clip.into(),
        op: ClipFxOp::Add(utility()),
    })
    .unwrap();
    settle(&mut s, clip);
    let id = s.clip_fx_chain(clip.into())[0].id;
    s.take_ui_requests();
    s.dispatch(Action::OpenClipEffectEditor {
        audio: clip.into(),
        index: 0,
        generic: false,
    })
    .unwrap();
    let asked = s.take_ui_requests().into_iter().any(|r| {
        matches!(r, faderframe_session::UiRequest::PluginEditor { track: t, plugin, generic: false }
            if t == track && plugin == id)
    });
    assert!(asked, "the shell is asked for the window");
    assert_eq!(
        s.plugin_owner(id).map(|(t, slot)| (t, slot.id)),
        Some((track, id))
    );
    assert_eq!(s.device_name(id).as_deref(), Some("Utility"));
    // The window moves its gain: the chain takes it, not a track.
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track,
        plugin: id,
        parameter: ParameterId(0),
        value: Some(-6.0206),
    }))
    .unwrap();
    let chain = s.clip_fx_chain(clip.into());
    assert_eq!(chain[0].parameters[0].value, -6.0206);
    assert!(s.project().tracks.iter().all(|t| t.plugin(id).is_none()));
    assert!(s.clip_fx_busy(clip.into()), "it renders");
    settle(&mut s, clip);
    let out = played(&s, clip);
    for (i, (x, y)) in out.iter().zip(&tone).enumerate().skip(4_800) {
        assert!((x - y * 0.5).abs() < 1e-3, "{i}: {x} vs {}", y * 0.5);
    }
    // The hosted instance follows the chain.
    let v = s.plugin_parameter_value(id, ParameterId(0)).unwrap();
    assert!((v + 6.0206).abs() < 1e-6, "{v}");
    // A device editor batches its values.
    s.dispatch(Action::Edit(Command::Batch {
        label: "Two values".into(),
        commands: vec![
            Command::SetPluginParameter {
                track,
                plugin: id,
                parameter: ParameterId(0),
                value: Some(-3.0),
            },
            Command::SetPluginParameter {
                track,
                plugin: id,
                parameter: ParameterId(0),
                value: Some(-6.0206),
            },
        ],
    }))
    .unwrap();
    assert_eq!(s.clip_fx_chain(clip.into())[0].parameters[0].value, -6.0206);
    // Its header's Bypass, and a value back to the device's own.
    s.dispatch(Action::Edit(Command::SetPluginBypass {
        track,
        plugin: id,
        bypass: true,
    }))
    .unwrap();
    assert!(s.clip_fx_chain(clip.into())[0].bypass);
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track,
        plugin: id,
        parameter: ParameterId(0),
        value: None,
    }))
    .unwrap();
    assert!(s.clip_fx_chain(clip.into())[0].parameters.is_empty());
    settle(&mut s, clip);
    // Removed: nothing answers for it any more (its window closes).
    s.dispatch(Action::ClipEffects {
        audio: clip.into(),
        op: ClipFxOp::Remove(0),
    })
    .unwrap();
    s.tick(0.01);
    assert!(s.plugin_owner(id).is_none());
    assert!(
        s.plugin_parameter_value(id, ParameterId(0)).is_none(),
        "released"
    );
}

/// A take is a clip of its own: effects go on one take of a folder,
/// render onto it in one step, and the comp plays the render where it
/// plays that take, in step with the recording; the other take keeps the
/// recording; undo takes them off.
#[test]
fn one_take_of_a_folder_has_effects_of_its_own() {
    let (mut s, clip, tone) = session();
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
    let original = a.source;
    let mut f = TakeFolder::new(a.length);
    f.add_take(Take::of_clip("Take 1", 0, a.clone()));
    f.add_take(Take::of_clip("Take 2", 0, a.clone()));
    f.use_take(0);
    f.set_comp(a.length / 2, a.length, Some(1));
    s.edit(Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    let (one, two) = {
        let p = s.project();
        (p.take_ref(clip, 0).unwrap(), p.take_ref(clip, 1).unwrap())
    };
    let take = |s: &Session, r: TakeRef| s.project().take(r).unwrap().audio.clone();
    s.dispatch(Action::OpenClipEffects(two.into())).unwrap();
    assert_eq!(s.clip_fx_target(), Some(two.into()));
    let steps = s.history_steps().0.len();
    s.dispatch(Action::ClipEffects {
        audio: two.into(),
        op: ClipFxOp::Add(utility()),
    })
    .unwrap();
    s.dispatch(Action::ClipEffects {
        audio: two.into(),
        op: ClipFxOp::SetParameter {
            index: 0,
            parameter: ParameterId(0),
            value: -6.0206,
        },
    })
    .unwrap();
    assert_eq!(s.clip_fx_chain(two.into()).len(), 1, "shown at once");
    assert!(s.clip_fx_chain(one.into()).is_empty());
    settle(&mut s, two);
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    let rendered = take(&s, two);
    assert_ne!(rendered.source, original, "take 2 plays the render");
    assert_eq!(rendered.effects.as_ref().unwrap().original.source, original);
    assert_eq!(take(&s, one).source, original, "take 1 the recording");
    // Where the comp plays take 2: the render, half as loud, in step.
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    let pieces = f.played();
    assert_eq!(pieces[0].audio.source, original);
    let piece = &pieces[1];
    assert_eq!(piece.audio.source, rendered.source);
    let SourceSpec::File { path, .. } = &s.project().sources[&rendered.source].spec else {
        panic!("a file");
    };
    let wav = read_wav(path).unwrap();
    let so = piece.audio.source_offset as usize;
    for i in (1_000..piece.audio.length as usize).step_by(97) {
        let (got, want) = (
            wav.channels[0][so + i],
            tone[piece.start as usize + i] * 0.5,
        );
        assert!((got - want).abs() < 1e-3, "{i}: {got} vs {want}");
    }
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(take(&s, two).source, original);
    assert!(s.clip_fx_chain(two.into()).is_empty());
}

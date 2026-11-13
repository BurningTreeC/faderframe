//! Spectral editing: edits render into a processed copy of the clip's
//! source that the clip plays, in one undo step; the unedited source is
//! kept; clearing the edits gives it back; with clip effects the effects
//! render again from the edited audio; the editor's picture shows the
//! source; one take of a take folder is edited on its own.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ClipId, ParameterId};
use faderframe_engine::EngineConfig;
use faderframe_project::spectral::{SpectralEdit, SpectralOp, SpectralShape};
use faderframe_project::{
    ClipContent, Command, PluginFormat, PluginRef, Project, SourceSpec, Take, TakeFolder, TakeRef,
};
use faderframe_session::clip_fx::ClipFxOp;
use faderframe_session::spectral::{PictureKey, SpectralChange};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

/// Two seconds of 1 kHz and 5 kHz.
fn session() -> (Session, ClipId) {
    let dir = std::env::temp_dir().join(format!("ff-session-spectral-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Two Tones.wav");
    let tone: Vec<f32> = (0..2 * SR as usize)
        .map(|i| {
            let t = i as f64 / f64::from(SR);
            (0.25 * (std::f64::consts::TAU * 1000.0 * t).sin()
                + 0.25 * (std::f64::consts::TAU * 5000.0 * t).sin()) as f32
        })
        .collect();
    write_wav(&file, &[tone], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Spectral", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    (s, clip)
}

/// The amplitude of `hz` in `x[a..b]`.
fn amplitude(x: &[f32], a: usize, b: usize, hz: f64) -> f64 {
    let n = b - a;
    let (mut re, mut im, mut sum) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
        let ph = std::f64::consts::TAU * hz * (a + i) as f64 / f64::from(SR);
        re += f64::from(x[a + i]) * w * ph.cos();
        im -= f64::from(x[a + i]) * w * ph.sin();
        sum += w;
    }
    2.0 * (re * re + im * im).sqrt() / sum
}

/// The audio of a source's file.
fn audio(s: &Session, source: faderframe_core::AudioSourceId) -> Vec<f32> {
    let SourceSpec::File { path, .. } = &s.project().sources[&source].spec else {
        panic!("a file");
    };
    read_wav(path).unwrap().channels.remove(0)
}

fn remove_5k() -> SpectralEdit {
    SpectralEdit::new(
        SpectralShape::Rect {
            start: 24_000,
            end: 72_000,
            low: 4_000.0,
            high: 6_000.0,
        },
        SpectralOp::Remove,
    )
}

#[test]
fn an_edit_renders_onto_the_clip_in_one_step_and_comes_off() {
    let (mut s, clip) = session();
    let original = s.project().clip(clip).unwrap().as_audio().unwrap().source;
    s.dispatch(Action::OpenSpectralEditor(clip.into())).unwrap();
    assert_eq!(s.spectral_target(), Some(clip.into()));
    let steps = s.history_steps().0.len();
    s.dispatch(Action::EditSpectral {
        audio: clip.into(),
        change: SpectralChange::Add(remove_5k()),
    })
    .unwrap();
    assert_eq!(s.spectral_edits(clip.into()).len(), 1, "shown at once");
    assert!(s.spectral_busy(clip.into()).is_some());
    s.wait_for_spectral();
    assert!(s.spectral_busy(clip.into()).is_none());
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_ne!(a.source, original, "plays the processed copy");
    assert_eq!(a.spectral.as_ref().unwrap().original, original);
    assert_eq!(s.spectral_sources(clip.into()), Some((a.source, original)));
    let out = audio(&s, a.source);
    assert_eq!(out.len(), 2 * SR as usize, "same frames");
    assert!(20.0 * (amplitude(&out, 30_000, 66_000, 5000.0) / 0.25).log10() < -40.0);
    assert!((20.0 * (amplitude(&out, 30_000, 66_000, 1000.0) / 0.25).log10()).abs() < 0.1);
    let before = audio(&s, original);
    assert_eq!(&out[..10_000], &before[..10_000], "untouched before it");

    // Undo: the original back; redo: the copy again.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().clip(clip).unwrap().as_audio().unwrap().source,
        original
    );
    assert!(s.spectral_edits(clip.into()).is_empty());
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.spectral_edits(clip.into()).len(), 1);

    // A second edit renders from the unedited source, both applied.
    s.dispatch(Action::EditSpectral {
        audio: clip.into(),
        change: SpectralChange::Add(SpectralEdit::new(
            SpectralShape::Rect {
                start: 24_000,
                end: 72_000,
                low: 800.0,
                high: 1_250.0,
            },
            SpectralOp::Gain { db: -6.0 },
        )),
    })
    .unwrap();
    s.wait_for_spectral();
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_eq!(a.spectral.as_ref().unwrap().original, original);
    let out = audio(&s, a.source);
    let g = 20.0 * (amplitude(&out, 30_000, 66_000, 1000.0) / 0.25).log10();
    assert!((g + 6.0).abs() < 0.2, "{g:+.2} dB");
    assert!(20.0 * (amplitude(&out, 30_000, 66_000, 5000.0) / 0.25).log10() < -40.0);

    // Clear: the unedited audio, one step.
    let steps = s.history_steps().0.len();
    s.dispatch(Action::EditSpectral {
        audio: clip.into(),
        change: SpectralChange::Clear,
    })
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_eq!(a.source, original);
    assert!(a.spectral.is_none());
}

#[test]
fn with_clip_effects_the_edits_go_before_them() {
    let (mut s, clip) = session();
    let utility = PluginRef {
        format: PluginFormat::Builtin,
        id: faderframe_core::builtin::GAIN.into(),
        name: "Utility".into(),
    };
    s.dispatch(Action::ClipEffects {
        audio: clip.into(),
        op: ClipFxOp::Add(utility),
    })
    .unwrap();
    s.dispatch(Action::ClipEffects {
        audio: clip.into(),
        op: ClipFxOp::SetParameter {
            index: 0,
            parameter: ParameterId(0),
            value: -6.0206,
        },
    })
    .unwrap();
    let settle = |s: &mut Session| {
        let start = Instant::now();
        while s.clip_fx_busy(clip.into()) || s.spectral_busy(clip.into()).is_some() {
            assert!(start.elapsed() < Duration::from_secs(60), "render hangs");
            std::thread::sleep(Duration::from_millis(10));
            s.tick(0.01);
            s.wait_for_spectral();
        }
    };
    settle(&mut s);
    let fx_original = s
        .project()
        .clip(clip)
        .unwrap()
        .as_audio()
        .unwrap()
        .effects
        .as_ref()
        .unwrap()
        .original
        .source;
    s.dispatch(Action::EditSpectral {
        audio: clip.into(),
        change: SpectralChange::Add(remove_5k()),
    })
    .unwrap();
    settle(&mut s);
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    let fx = a.effects.as_ref().unwrap();
    assert_ne!(
        fx.original.source, fx_original,
        "the effects take the edited audio"
    );
    assert_eq!(a.spectral.as_ref().unwrap().original, fx_original);
    // What plays: the edits, then the −6 dB.
    let out = audio(&s, a.source);
    let tone = 20.0 * (amplitude(&out, 30_000, 66_000, 1000.0) / 0.25).log10();
    assert!((tone + 6.0).abs() < 0.2, "{tone:+.2} dB");
    assert!(20.0 * (amplitude(&out, 30_000, 66_000, 5000.0) / 0.25).log10() < -40.0);
}

#[test]
fn the_picture_shows_the_tones() {
    let (mut s, clip) = session();
    s.dispatch(Action::OpenSpectralEditor(clip.into())).unwrap();
    let (now, _) = s.spectral_sources(clip.into()).unwrap();
    assert_eq!(s.source_format(now), Some((SR, 1, 2 * i64::from(SR))));
    let key = PictureKey {
        source: now,
        from: 0,
        to: 2 * i64::from(SR),
        columns: 100,
        rows: 300,
    };
    let picture = s.wait_for_spectrogram(key).unwrap();
    let at = |hz: f32| picture.db(picture.row_at(hz) as usize, 50);
    assert!(at(1000.0) > -15.0 && at(5000.0) > -15.0);
    assert!(at(300.0) < -60.0);
}

/// The clip folded into a take folder of two takes of its audio: take 1
/// comped on the first second, take 2 on the second.
fn two_takes(s: &mut Session, clip: ClipId) -> (TakeRef, TakeRef) {
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
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
    let p = s.project();
    (p.take_ref(clip, 0).unwrap(), p.take_ref(clip, 1).unwrap())
}

/// A take is a clip of its own: the spectral editor opens on one take of a
/// folder; its edit renders onto that take in one step, the other take
/// keeps the recording, and the comp plays the edited take where it plays
/// it; undo gives the take its audio back.
#[test]
fn one_take_of_a_folder_has_its_spectrum_edited_on_its_own() {
    let (mut s, clip) = session();
    let original = s.project().clip(clip).unwrap().as_audio().unwrap().source;
    let (one, two) = two_takes(&mut s, clip);
    let take = |s: &Session, r: TakeRef| s.project().take(r).unwrap().audio.clone();
    s.dispatch(Action::OpenSpectralEditor(two.into())).unwrap();
    assert_eq!(s.spectral_target(), Some(two.into()));
    let steps = s.history_steps().0.len();
    // The 5 kHz tone out over the second second (where take 2 plays).
    let mut edit = remove_5k();
    edit.shape = SpectralShape::Rect {
        start: 54_000,
        end: 90_000,
        low: 4_000.0,
        high: 6_000.0,
    };
    s.dispatch(Action::EditSpectral {
        audio: two.into(),
        change: SpectralChange::Add(edit),
    })
    .unwrap();
    assert_eq!(s.spectral_edits(two.into()).len(), 1, "shown at once");
    assert!(s.spectral_edits(one.into()).is_empty());
    s.wait_for_spectral();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    let edited = take(&s, two);
    assert_ne!(edited.source, original, "take 2 plays the processed copy");
    assert_eq!(edited.spectral.as_ref().unwrap().original, original);
    assert_eq!(take(&s, one).source, original, "take 1 the recording");
    assert_eq!(
        s.spectral_sources(two.into()),
        Some((edited.source, original))
    );
    let out = audio(&s, edited.source);
    assert!(20.0 * (amplitude(&out, 60_000, 84_000, 5000.0) / 0.25).log10() < -40.0);
    // The comp: take 1's recording, then take 2's copy.
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    let sources: Vec<_> = f.played().iter().map(|p| p.audio.source).collect();
    assert_eq!(sources, vec![original, edited.source]);
    assert!(s.project().used_sources().contains(&original));
    // The folder itself is no clip of audio to edit.
    assert!(s.dispatch(Action::OpenSpectralEditor(clip.into())).is_err());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(take(&s, two).source, original);
    assert!(s.spectral_edits(two.into()).is_empty());
}

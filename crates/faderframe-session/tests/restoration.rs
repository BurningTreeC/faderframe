//! Restoration in the session: the spectral editor's "Learn Noise and
//! Remove" (a De-noise edit learns from the region drawn and takes the
//! noise out of the whole clip, in one undo step), and the restoration
//! devices as inserts (their parameters and latency reach the engine).
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ClipId, ParameterId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::spectral::{SpectralEdit, SpectralOp, SpectralShape};
use faderframe_project::{Command, PluginRef, Project, SourceSpec};
use faderframe_session::spectral::SpectralChange;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

/// White noise of unit variance (a hash per frame).
fn white(n: usize, seed: u64) -> f64 {
    let mut z = (n as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(seed.wrapping_mul(0xbf58_476d_1ce4_e5b9));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    ((z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * 3f64.sqrt()
}

/// The tune in the clip: from 0.6 s, two notes in turn.
fn tune(i: usize) -> f64 {
    let t = i as f64 / f64::from(SR);
    if t < 0.6 {
        return 0.0;
    }
    let f = if ((t * 3.0) as usize).is_multiple_of(2) {
        392.0
    } else {
        523.25
    };
    0.3 * (std::f64::consts::TAU * f * t).sin()
}

/// Three seconds: hiss, a tune over it from 0.6 s.
fn session() -> (Session, ClipId, Vec<f32>) {
    let dir = std::env::temp_dir().join(format!("ff-session-restoration-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Hissy Tune.wav");
    let x: Vec<f32> = (0..3 * SR as usize)
        .map(|i| (tune(i) + 0.02 * white(i, 9)) as f32)
        .collect();
    write_wav(
        &file,
        std::slice::from_ref(&x),
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut s = Session::new(
        Project::new("Restoration", SR),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    (s, clip, x)
}

fn audio(s: &Session, source: faderframe_core::AudioSourceId) -> Vec<f32> {
    let SourceSpec::File { path, .. } = &s.project().sources[&source].spec else {
        panic!("a file");
    };
    read_wav(path).unwrap().channels.remove(0)
}

fn snr(x: &[f32], from: usize) -> f64 {
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (i, v) in x.iter().enumerate().skip(from) {
        let c = tune(i);
        sig += c * c;
        err += (f64::from(*v) - c).powi(2);
    }
    10.0 * (sig / err).log10()
}

#[test]
fn learn_noise_and_remove_cleans_the_whole_clip_in_one_step() {
    let (mut s, clip, input) = session();
    let original = s.project().clip(clip).unwrap().as_audio().unwrap().source;
    s.dispatch(Action::OpenSpectralEditor(clip)).unwrap();
    let steps = s.history_steps().0.len();
    // The noise alone: 0.1–0.5 s, every frequency.
    let edit = SpectralEdit::new(
        SpectralShape::Rect {
            start: 4_800,
            end: 24_000,
            low: 20.0,
            high: 24_000.0,
        },
        SpectralOp::Denoise { reduction_db: 20.0 },
    );
    s.dispatch(Action::EditSpectral {
        clip,
        change: SpectralChange::Add(edit),
    })
    .unwrap();
    s.wait_for_spectral();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    assert_eq!(
        s.history().undo_label().map(str::to_string),
        Some("Spectral Edit".to_string())
    );
    let a = s.project().clip(clip).unwrap().as_audio().unwrap().clone();
    assert_ne!(a.source, original);
    let out = audio(&s, a.source);
    assert_eq!(out.len(), input.len());
    let from = SR as usize;
    let (before, after) = (snr(&input, from), snr(&out, from));
    assert!(after > before + 10.0, "SNR {before:.1} → {after:.1} dB");
    // Undo gives the recording back.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().clip(clip).unwrap().as_audio().unwrap().source,
        original
    );
}

#[test]
fn restoration_devices_are_inserts_with_their_latency() {
    let (mut s, _, _) = session();
    let track = s.project().tracks[0].id;
    for (i, (id, name)) in [
        (builtin::DENOISE, "De-noise"),
        (builtin::DECLICK, "De-click"),
        (builtin::DEHUM, "De-hum"),
        (builtin::DECLIP, "De-clip"),
        (builtin::DEREVERB, "De-reverb"),
    ]
    .into_iter()
    .enumerate()
    {
        s.dispatch(Action::InsertPlugin {
            track,
            index: i,
            plugin: PluginRef::builtin(id, name),
        })
        .unwrap();
        let plugin = s.project().tracks[0].inserts[i].id;
        assert!(s.plugin_tap(plugin).is_some(), "{id} is hosted");
        assert!(faderframe_core::builtin::has_editor(id));
    }
    // The chain's latency is the devices' (De-hum has none).
    let rate = f64::from(SR);
    let params = |id: &str| {
        faderframe_plugin_host::ParamValues::new(faderframe_plugin_host::presets::parameters(id))
    };
    use faderframe_plugin_host::devices as d;
    let chain = d::denoise::latency(&params(builtin::DENOISE), rate)
        + d::declick::latency(&params(builtin::DECLICK), rate)
        + d::declip::latency(&params(builtin::DECLIP), rate)
        + d::dereverb::latency(&params(builtin::DEREVERB), rate);
    wait_until(
        &mut s,
        |s| s.engine().graph_latency() == chain,
        "the chain's latency",
    );
    // De-noise's Quality mode looks ahead: the graph is rebuilt longer.
    let denoise = s.project().tracks[0].inserts[0].id;
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track,
        plugin: denoise,
        parameter: ParameterId(d::denoise::id::MODE),
        value: Some(2.0),
    }))
    .unwrap();
    let quality = params(builtin::DENOISE);
    quality
        .set_by_id(ParameterId(d::denoise::id::MODE), 2.0)
        .unwrap();
    let longer = chain - d::denoise::latency(&params(builtin::DENOISE), rate)
        + d::denoise::latency(&quality, rate);
    assert!(longer > chain);
    wait_until(
        &mut s,
        |s| s.engine().graph_latency() == longer,
        "Quality's latency",
    );
}

fn wait_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!(
        "timed out waiting for {what}: {}",
        s.engine().graph_latency()
    );
}

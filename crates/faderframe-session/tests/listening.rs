//! Listening on headphones: heads (built in or from a SOFA file), the
//! headphone correction, and renders for headphones through the head.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::render::{RenderChannels, RenderRange, RenderSettings, RenderSource};
use faderframe_session::{Action, Session};

fn session(dir: &std::path::Path) -> Session {
    let mut s = Session::new(Project::new("Ears", 48_000), None, EngineConfig::default()).unwrap();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S51),
    }))
    .unwrap();
    let file = dir.join("noise.wav");
    let mut x = 12345u32;
    let noise: Vec<f32> = (0..96_000)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        })
        .collect();
    write_wav(&file, &[noise], 48_000, WavFormat::Float32, false).unwrap();
    let track = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    s.import_audio(
        vec![file],
        faderframe_session::ImportTarget {
            track: Some(track),
            at: faderframe_timeline::MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track,
        pan: SurroundPan {
            x: -1.0,
            y: -1.0,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    s
}

fn headphones(s: &mut Session, dir: &std::path::Path, name: &str) -> Vec<Vec<f32>> {
    let out = dir.join(name);
    let job = s
        .render(RenderSettings {
            range: RenderRange::Bars { start: 0, end: 1 },
            source: RenderSource::Master,
            format: WavFormat::Float32,
            tail_seconds: 0.0,
            channels: RenderChannels::Binaural(faderframe_binaural::Room::Near),
            dither: faderframe_audio_files::Dither::Off,
            ..RenderSettings::defaults_for(s.project(), out.clone())
        })
        .unwrap();
    job.join().unwrap();
    read_wav(&out).unwrap().channels
}

#[test]
fn heads_and_corrections_are_chosen_and_renders_follow_the_head() {
    let dir = std::env::temp_dir().join(format!("ff-listening-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = session(&dir);
    assert_eq!(s.head().id(), "ku100");
    let ku100 = headphones(&mut s, &dir, "ku100.wav");
    // Another head: another render.
    s.dispatch(Action::SetHead("kemar".into())).unwrap();
    assert_eq!(s.head().id(), "kemar");
    assert!(
        s.head_choices()
            .iter()
            .any(|c| c.checked && c.label.contains("KEMAR"))
    );
    let kemar = headphones(&mut s, &dir, "kemar.wav");
    let diff = ku100[0]
        .iter()
        .zip(&kemar[0])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(diff > 1e-3, "the heads differ ({diff})");
    // On headphones the graph is rebuilt for a new head.
    s.dispatch(Action::SetHeadphones(Some(faderframe_binaural::Room::Mid)))
        .unwrap();
    s.dispatch(Action::SetHead("sadie-h12".into())).unwrap();
    // A SOFA file, and its entry in the choices.
    let tiny = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../faderframe-binaural/tests/data/tiny.sofa");
    let id = faderframe_session::listening::sofa_id(&tiny);
    s.dispatch(Action::SetHead(id.clone())).unwrap();
    assert_eq!(s.head().id(), id);
    assert!(s.head_choices().last().unwrap().checked);
    assert!(s.dispatch(Action::SetHead("nobody".into())).is_err());
    assert_eq!(s.head().id(), id, "a failed load keeps the head");
    // Corrections: AutoEq's text, an impulse response, none.
    let eq = dir.join("ParametricEQ.txt");
    std::fs::write(
        &eq,
        "Preamp: -6.4 dB\nFilter 1: ON LSC Fc 105 Hz Gain 6.9 dB Q 0.70\nFilter 2: ON PK Fc 2000 Hz Gain -3 dB Q 1.4\n",
    )
    .unwrap();
    s.dispatch(Action::SetHeadphoneCorrection(Some(eq.clone())))
        .unwrap();
    let (path, c) = s.headphone_correction().unwrap();
    assert_eq!(path, eq.as_path());
    assert_eq!(c.describe(), "2 filters, preamp -6.4 dB");
    let ir = dir.join("headphones.wav");
    let mut taps = vec![0.0f32; 512];
    taps[0] = 0.8;
    write_wav(
        &ir,
        &[taps.clone(), taps],
        48_000,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    s.dispatch(Action::SetHeadphoneCorrection(Some(ir)))
        .unwrap();
    assert!(
        s.headphone_correction()
            .unwrap()
            .1
            .describe()
            .contains("impulse response")
    );
    assert!(
        s.dispatch(Action::SetHeadphoneCorrection(Some(
            dir.join("missing.txt")
        )))
        .is_err()
    );
    s.dispatch(Action::SetHeadphoneCorrection(None)).unwrap();
    assert!(s.headphone_correction().is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

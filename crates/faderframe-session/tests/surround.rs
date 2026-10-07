//! A surround mix delivered: rendered as the master is (every channel, the
//! file naming its speakers) or folded down to stereo.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::render::{RenderChannels, RenderRange, RenderSettings, RenderSource};
use faderframe_session::{Action, Session};

#[test]
fn a_51_mix_renders_with_its_speakers_or_folded_down() {
    let dir = std::env::temp_dir().join(format!("ff-surround-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(Project::new("Bed", 48_000), None, EngineConfig::default()).unwrap();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S51),
    }))
    .unwrap();
    let file = dir.join("tone.wav");
    write_wav(
        &file,
        &[vec![0.5; 96_000]],
        48_000,
        WavFormat::Float32,
        false,
    )
    .unwrap();
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
    let render = |s: &mut Session, channels: RenderChannels, name: &str| {
        let out = dir.join(name);
        let job = s
            .render(RenderSettings {
                range: RenderRange::Bars { start: 0, end: 1 },
                source: RenderSource::Master,
                format: WavFormat::Float32,
                tail_seconds: 0.0,
                channels,
                dither: faderframe_audio_files::Dither::Off,
                ..RenderSettings::defaults_for(s.project(), out.clone())
            })
            .unwrap();
        job.join().unwrap();
        read_wav(&out).unwrap()
    };
    assert_eq!(
        RenderSettings::defaults_for(s.project(), dir.join("x")).channels,
        RenderChannels::Master
    );
    let bed = render(&mut s, RenderChannels::Master, "bed.wav");
    assert_eq!(bed.channels.len(), 6);
    assert_eq!(bed.channel_mask, Some(0x60F));
    let at = 20_000;
    assert!((bed.channels[4][at] - 0.5).abs() < 1e-4, "Ls");
    assert!(bed.channels[0][at].abs() < 1e-6);
    let folded = render(&mut s, RenderChannels::Stereo, "stereo.wav");
    assert_eq!(folded.channels.len(), 2);
    assert_eq!(folded.channel_mask, None);
    assert!(
        (folded.channels[0][at] - 0.5).abs() < 1e-4,
        "Ls folds to the left"
    );
    assert!(folded.channels[1][at].abs() < 1e-6);
    // Listening on headphones in mono changes no render…
    s.dispatch(Action::SetHeadphones(Some(faderframe_binaural::Room::Mid)))
        .unwrap();
    s.dispatch(Action::SetMonoCheck(true)).unwrap();
    assert!(s.mono_check());
    let again = render(&mut s, RenderChannels::Stereo, "again.wav");
    assert_eq!(again.channels, folded.channels);
    // …but a render for headphones is binaural: the rear left speaker
    // louder in the left ear, both ears hearing it (a DC source: at low
    // frequencies the head shadows little).
    let ears = render(
        &mut s,
        RenderChannels::Binaural(faderframe_binaural::Room::Near),
        "headphones.wav",
    );
    assert_eq!(ears.channels.len(), 2);
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    let (l, r) = (
        rms(&ears.channels[0][4_000..40_000]),
        rms(&ears.channels[1][4_000..40_000]),
    );
    assert!(l > 1.25 * r && r > 1e-3, "left {l} right {r}");
    std::fs::remove_dir_all(&dir).unwrap();
}

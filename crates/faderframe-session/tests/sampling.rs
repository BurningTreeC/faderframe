//! Samples made of a selection of an audio track.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav};
use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::samples::unpack;
use faderframe_project::TrackKind;
use faderframe_session::sampling::SampleTarget;
use faderframe_session::{Action, EditRange, SelectMode, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn wait(s: &mut Session) {
    let end = Instant::now() + Duration::from_secs(60);
    while !s.sampling().is_empty() {
        assert!(Instant::now() < end, "the sample timed out");
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// The files a sampler slot's state names.
fn files(s: &Session, track: TrackId) -> Vec<Option<String>> {
    let t = s.project().track(track).unwrap();
    let slot = s.instrument_slot(t).unwrap();
    let state = faderframe_engine::decode_state(slot.state.as_deref().unwrap()).unwrap();
    unpack(&state).unwrap().1.files
}

#[test]
fn a_selection_becomes_a_sampler_a_pad_or_a_file() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let clip = s.project().clips_of(bass)[0].id;
    let rate = f64::from(s.project().sample_rate);
    // One bar of the bassline, as an edit selection.
    let start = s.project().clip(clip).unwrap().start;
    let end = start + MusicalTime::from_quarters(4.0);
    s.selection.select_tracks(&[bass], SelectMode::Replace);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(start, end))))
        .unwrap();
    let choices = s.sample_choices(bass, Some(clip));
    let labels: Vec<&str> = choices.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Selection to New Sampler",
            "Selection to New Drum Sampler",
            "Save Selection as Sample…"
        ]
    );
    // MIDI and instrument tracks have nothing to sample.
    assert!(s.sample_choices(track(&s, "Lead Synth"), None).is_empty());

    let tracks = s.project().tracks.len();
    s.dispatch(choices[0].1.clone()).unwrap();
    wait(&mut s);
    assert_eq!(s.project().tracks.len(), tracks + 1);
    let sampler = track(&s, "Bass Sample");
    let t = s.project().track(sampler).unwrap();
    assert_eq!(t.kind, TrackKind::Instrument);
    assert_eq!(s.instrument_slot(t).unwrap().plugin.id, builtin::SAMPLER);
    let file = files(&s, sampler)[0].clone().unwrap();
    let wav = read_wav(file.as_ref()).unwrap();
    let frames = (s.project().timeline.to_samples(end, rate)
        - s.project().timeline.to_samples(start, rate)) as usize;
    assert_eq!(wav.channels.len(), 1, "a mono track makes a mono sample");
    assert_eq!(wav.channels[0].len(), frames, "exactly the selection");
    let peak = wav.channels[0].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.05, "the bass is in it (peak {peak})");
    assert!(
        s.take_ui_requests()
            .iter()
            .any(|r| matches!(r, UiRequest::PluginEditor { track, .. } if *track == sampler)),
        "the sampler opens"
    );
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), tracks);
    s.dispatch(Action::Redo).unwrap();

    // A Drum Sampler with it on pad 1, then the next one on pad 2.
    s.selection.select_tracks(&[bass], SelectMode::Replace);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(start, end))))
        .unwrap();
    s.dispatch(Action::MakeSample {
        track: bass,
        start,
        end,
        target: SampleTarget::Drums,
    })
    .unwrap();
    wait(&mut s);
    let kit = track(&s, "Bass Drums");
    let kit_plugin = {
        let t = s.project().track(kit).unwrap();
        let slot = s.instrument_slot(t).unwrap();
        assert_eq!(slot.plugin.id, builtin::DRUMS);
        slot.id
    };
    assert!(files(&s, kit)[0].is_some());
    s.selection.select_tracks(&[bass], SelectMode::Replace);
    s.dispatch(Action::SetEditRange(Some(EditRange::new(start, end))))
        .unwrap();
    let pad = s
        .sample_choices(bass, None)
        .into_iter()
        .find(|c| c.0 == "Selection to the Next Free Pad of ‘Bass Drums’")
        .unwrap();
    assert_eq!(
        pad.1,
        Action::MakeSample {
            track: bass,
            start,
            end,
            target: SampleTarget::Pad(kit_plugin)
        }
    );
    s.dispatch(pad.1).unwrap();
    wait(&mut s);
    let pads = files(&s, kit);
    assert!(pads[0].is_some() && pads[1].is_some());
    assert_ne!(pads[0], pads[1], "each sample is its own file");

    // A file: asked for, then written as 24-bit.
    s.dispatch(Action::PromptSaveSample {
        track: bass,
        start,
        end,
    })
    .unwrap();
    assert!(
        s.take_ui_requests()
            .iter()
            .any(|r| matches!(r, UiRequest::SaveSample { name, .. } if name == "Bass Sample.wav"))
    );
    let path = std::env::temp_dir().join(format!("ff-sample-{}.wav", std::process::id()));
    s.dispatch(Action::MakeSample {
        track: bass,
        start,
        end,
        target: SampleTarget::File(path.clone()),
    })
    .unwrap();
    wait(&mut s);
    let wav = read_wav(&path).unwrap();
    assert_eq!(wav.format, WavFormat::Pcm24);
    assert_eq!(wav.channels[0].len(), frames);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn without_a_selection_the_clip_is_sampled() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    let clip = s.project().clips_of(drums)[0].clone();
    s.dispatch(Action::SelectClips {
        clips: vec![clip.id],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let choices = s.sample_choices(drums, Some(clip.id));
    assert_eq!(choices[0].0, "Clip to New Sampler");
    let Action::MakeSample { start, end, .. } = choices[0].1 else {
        panic!()
    };
    let p = s.project();
    assert_eq!(start, clip.start);
    assert_eq!(end, clip.end(&p.timeline, p.sample_rate));
}

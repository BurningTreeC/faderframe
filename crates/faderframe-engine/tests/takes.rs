#![allow(clippy::unwrap_used)]
//! Take folders: the comp decides which take plays, with crossfades at
//! comp boundaries and silence where a take has no material.

mod common;

use common::TestProject;
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::render_project;
use faderframe_project::{Clip, ClipContent, Take, TakeFolder, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn render(tp: &TestProject, frames: usize) -> Vec<f32> {
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    render_project(&tp.project, &tp.sources, config, 256, 0, frames).unwrap()[0].clone()
}

#[test]
fn comp_selects_takes_with_crossfades() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Vox", ChannelLayout::Mono);
    let a = tp.dc(1, 0.25, 20_000);
    let b = tp.dc(1, 0.5, 20_000);
    // Reference gain of the channel path for a plain clip.
    tp.clip(t, a, MusicalTime::ZERO, 100);
    let reference = render(&tp, 50)[10] / 0.25;
    let plain = *tp.project.track(t).unwrap().clips.last().unwrap();
    tp.project.clips.remove(&plain);
    tp.project.track_mut(t).unwrap().clips.clear();

    let mut f = TakeFolder::new(16_000);
    // Material from folder frame `start` to `end`, the source frame the
    // folder frame (the sources start with the folder).
    let take = |name: &str, source, start: i64, end: i64| {
        Take::plain(name, source, start, start, end - start)
    };
    let ta = f.add_take(take("A", a, 0, 16_000));
    let tb = f.add_take(take("B", b, 4_000, 16_000));
    f.use_take(ta);
    f.set_comp(8_000, 12_000, Some(tb));
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track: t,
            name: "takes".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Takes(f.clone()),
        },
    );
    tp.project.track_mut(t).unwrap().clips.push(id);
    let out = render(&tp, 18_000);
    let at = |i: usize| out[i] / reference;
    assert!(
        (at(4_000) - 0.25).abs() < 1e-4,
        "take A at 4000: {}",
        at(4_000)
    );
    assert!(
        (at(10_000) - 0.5).abs() < 1e-4,
        "take B inside its comp segment"
    );
    assert!((at(14_000) - 0.25).abs() < 1e-4, "back to A");
    assert!(at(17_000).abs() < 1e-6, "silence after the folder");
    // Crossfade at 8000 (480 frames, equal power): smooth, no step.
    let max_step = out[7_500..8_500]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs() / reference)
        .fold(0.0f32, f32::max);
    assert!(max_step < 0.01, "crossfade step {max_step}");
    let mid = at(8_000);
    let expected = (0.25 + 0.5) * std::f32::consts::FRAC_1_SQRT_2;
    assert!(
        (mid - expected).abs() < 0.01,
        "equal-power midpoint {mid} vs {expected}"
    );

    // A take without material where the comp points at it plays silence.
    let mut f2 = f.clone();
    f2.use_take(tb);
    tp.project.clips.get_mut(&id).unwrap().content = ClipContent::Takes(f2);
    let out = render(&tp, 18_000);
    assert!(out[2_000].abs() < 1e-6, "take B starts at 4000");
    assert!((out[6_000] / reference - 0.5).abs() < 1e-4);
}

/// An auditioned take plays whole where the folder plays its comp (the
/// arranger's ▶ on a take lane), until the audition ends.
#[test]
fn an_auditioned_take_plays_whole_instead_of_the_comp() {
    use faderframe_engine::offline::OfflineRenderer;
    use faderframe_project::Impact;
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Vox", ChannelLayout::Mono);
    let a = tp.dc(1, 0.25, 20_000);
    let b = tp.dc(1, 0.5, 20_000);
    let mut f = TakeFolder::new(16_000);
    let take = |name: &str, source| Take::plain(name, source, 0, 0, 16_000);
    let ta = f.add_take(take("A", a));
    let tb = f.add_take(take("B", b));
    f.use_take(ta);
    f.set_comp(8_000, 12_000, Some(tb));
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track: t,
            name: "takes".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Takes(f),
        },
    );
    tp.project.track_mut(t).unwrap().clips.push(id);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let play = |audition: Option<usize>| {
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 1).unwrap();
        r.controller.set_take_audition(audition.map(|k| (id, k)));
        r.controller
            .sync(&tp.project, &tp.sources, Impact::Timeline)
            .unwrap();
        r.play_from(0).unwrap();
        let mut out = Vec::new();
        while out.len() < 16_000 {
            out.extend_from_slice(r.step().output_ref(0));
        }
        out
    };
    let comp = play(None);
    let reference = comp[2_000] / 0.25;
    let level = |out: &[f32], i: usize| out[i] / reference;
    assert!((level(&comp, 10_000) - 0.5).abs() < 1e-4, "B in the comp");
    let heard = play(Some(ta));
    for i in [2_000, 10_000, 14_000] {
        assert!((level(&heard, i) - 0.25).abs() < 1e-4, "A whole at {i}");
    }
    let heard = play(Some(tb));
    for i in [2_000, 10_000, 14_000] {
        assert!((level(&heard, i) - 0.5).abs() < 1e-4, "B whole at {i}");
    }
}

/// A take is a clip of its own: the comp plays each piece through its
/// take's edits (here reversed, an envelope, a gain; transposed), and the
/// pieces the comp plays, placed as clips (what Flatten Comp does), play
/// the same samples.
#[test]
fn takes_play_their_edits_and_their_flattened_pieces_play_the_same() {
    use faderframe_audio_files::AudioData;
    use faderframe_project::clip_envelope::{ClipEnvelope, EnvelopePoint};
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Vox", ChannelLayout::Mono);
    let ramp: Vec<f32> = (0..20_000).map(|i| i as f32 / 20_000.0).collect();
    let r = tp.source(AudioData::from_channels(SR, vec![ramp]));
    // The track's reference gain.
    let one = tp.dc(1, 1.0, 20_000);
    tp.clip(t, one, MusicalTime::ZERO, 100);
    let reference = render(&tp, 50)[10];
    tp.project.clips.clear();
    tp.project.track_mut(t).unwrap().clips.clear();

    let mut f = TakeFolder::new(16_000);
    let a = f.add_take(Take::plain("A", r, 0, 0, 16_000));
    let mut b = Take::plain("B", r, 0, 0, 16_000);
    b.audio.reversed = true;
    b.audio.gain_db = -6.0;
    b.audio.envelope = Some(Box::new(ClipEnvelope {
        volume: vec![
            EnvelopePoint::new(0, -6.0),
            EnvelopePoint::new(16_000, -6.0),
        ],
        pan: Vec::new(),
    }));
    let b = f.add_take(b);
    let mut c = Take::plain("C", r, 0, 0, 16_000);
    c.audio.stretch = faderframe_project::StretchSettings::pitch(-12.0);
    let c = f.add_take(c);
    f.use_take(a);
    f.set_comp(6_000, 10_000, Some(b));
    f.set_comp(12_000, 16_000, Some(c));
    let place = |tp: &mut TestProject, name: &str, start: i64, content: ClipContent| {
        let id = tp.project.ids.allocate();
        let start = tp.project.timeline.to_musical(start, f64::from(SR));
        tp.project.clips.insert(
            id,
            Clip {
                id,
                track: t,
                name: name.into(),
                color: None,
                start,
                muted: false,
                content,
            },
        );
        tp.project.track_mut(t).unwrap().clips.push(id);
    };
    place(&mut tp, "takes", 0, ClipContent::Takes(f.clone()));
    let comp = render(&tp, 18_000);
    // Inside B: the ramp backwards, 12 dB down (gain and envelope).
    let quarter = 10f32.powf(-12.0 / 20.0);
    for x in [7_000usize, 8_000, 9_000] {
        let want = (15_999 - x) as f32 / 20_000.0 * quarter;
        let got = comp[x] / reference;
        assert!((got - want).abs() < 1e-4, "B at {x}: {got} vs {want}");
    }
    // Inside A: the ramp forwards.
    assert!((comp[3_000] / reference - 0.15).abs() < 1e-4);

    // Its pieces as clips.
    tp.project.clips.clear();
    tp.project.track_mut(t).unwrap().clips.clear();
    for piece in f.played() {
        let name = f.takes[piece.take].name.clone();
        place(&mut tp, &name, piece.start, ClipContent::Audio(piece.audio));
    }
    let flat = render(&tp, 18_000);
    // (Its pieces need the stretcher voices clips like them do: a take
    // transposed got none and played through varispeed.)
    let first = comp.iter().zip(&flat).position(|(a, b)| a != b);
    assert_eq!(first, None, "the flattened comp plays what the comp played");
}

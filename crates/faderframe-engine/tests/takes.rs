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
    let take = |name: &str, source, start, end| Take {
        name: name.into(),
        source,
        source_offset: 0,
        start,
        end,
        gain_db: 0.0,
        rating: 0,
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

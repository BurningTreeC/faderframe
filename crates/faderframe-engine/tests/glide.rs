//! Mix scene glides: faders, pans and send levels moved on the audio
//! thread from where they are to a project's values over a time — one
//! unbroken line through every block (no steps where the ticks fall),
//! fader levels in fader travel, the exact targets at the end; the slots
//! stay the glide's until it is released, and a glide stopped half-way
//! stays where it was.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_core::{ChannelLayout, FaderLaw, TrackId, db_to_gain, gain_to_db};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{Impact, TrackKind};
use faderframe_timeline::MusicalTime;

fn dc_project() -> (TestProject, TrackId) {
    let mut tp = TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "DC", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, 480_000);
    tp.clip(t, src, MusicalTime::ZERO, 480_000);
    (tp, t)
}

#[test]
fn a_glide_moves_a_fader_in_one_line_and_ends_on_its_target() {
    let (tp, t) = dc_project();
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let before = r.render(2048);
    let level = before[0][2000];
    assert!(level > 0.1, "{level}");
    let mut target = tp.project.clone();
    target.track_mut(t).unwrap().volume_db = -24.0;
    // 0.25 s at 48 kHz.
    let id = r.controller.glide_mix(&target, 0.25).unwrap().unwrap();
    assert!(r.controller.gliding());
    let out = r.render(12_000 + 4096);
    let left = &out[0];
    // Down all the way, never up, without steps: the largest move from
    // one sample to the next is a sample's share of the line.
    let law = FaderLaw::console();
    for w in left[..12_000].windows(2) {
        assert!(w[1] <= w[0] + 1e-7, "{} then {}", w[0], w[1]);
    }
    let steepest = left[..12_000]
        .windows(2)
        .map(|w| w[0] - w[1])
        .fold(0.0f32, f32::max);
    assert!(
        steepest < 2.0 * level / 12_000.0 * 8.0,
        "a step: {steepest}"
    );
    // Even in travel: the quarter points are a quarter of the way.
    let travel = |v: f32| law.db_to_position(gain_to_db(v / level));
    let (a, b) = (travel(level), law.db_to_position(-24.0));
    for (i, share) in [(3000usize, 0.25f32), (6000, 0.5), (9000, 0.75)] {
        let at = travel(left[i]);
        let want = a + (b - a) * share;
        assert!((at - want).abs() < 0.01, "at {i}: {at} vs {want}");
    }
    let end = left[12_000 + 2000];
    assert!(
        (end - level * db_to_gain(-24.0)).abs() < 1e-6,
        "{end} vs {}",
        level * db_to_gain(-24.0)
    );
    assert!(r.controller.glide_done(id));
    assert_eq!(r.controller.glide_progress(), Some(1.0));
}

#[test]
fn the_slots_are_the_glides_until_released() {
    let (mut tp, t) = dc_project();
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let level = r.render(2048)[0][2000];
    let mut target = tp.project.clone();
    target.track_mut(t).unwrap().volume_db = -12.0;
    r.controller.glide_mix(&target, 1.0).unwrap().unwrap();
    let half = r.render(24_000);
    let midway = half[0][23_999];
    assert!(midway < level && midway > level * db_to_gain(-12.0));
    // A parameter sync of the project as it was (0 dB) leaves the
    // gliding fader alone.
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    let on = r.render(256);
    assert!(on[0][255] < midway, "still gliding down");
    // Stopped where it is: the project's value again from the next sync.
    r.controller.release_glide(true).unwrap();
    let stopped = r.render(2048);
    let held = stopped[0][2047];
    assert!((held - stopped[0][1024]).abs() < 1e-6, "stopped: {held}");
    tp.project.track_mut(t).unwrap().volume_db = -6.0;
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    let synced = r.render(2048);
    assert!((synced[0][2047] - level * db_to_gain(-6.0)).abs() < 1e-6);
    // Released once finished: held until then, the project's after.
    let mut target = tp.project.clone();
    target.track_mut(t).unwrap().pan = -1.0;
    let id = r.controller.glide_mix(&target, 0.05).unwrap().unwrap();
    r.controller.release_glide(false).unwrap();
    assert!(r.controller.gliding());
    r.render(4800);
    assert!(r.controller.glide_done(id));
    r.controller.collect_garbage();
    assert!(!r.controller.gliding());
    // Nothing to move: no glide.
    assert_eq!(r.controller.glide_mix(&target, 0.05).unwrap(), None);
}

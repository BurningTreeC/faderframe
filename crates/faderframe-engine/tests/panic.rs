//! The panic button: tails and voices go, without one callback clearing
//! every delay line at once.

#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use common::TestProject;
use faderframe_core::{ChannelLayout, ParameterId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_plugin_host::devices::delay::id;
use faderframe_project::{Impact, PluginRef, PluginSlot, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
const BLOCK: usize = 256;

/// A click at 0.5 s into a 400 ms echo (repeats at 0.9 s, 1.3 s, …), and
/// a plain click at 1.5 s on a track of its own.
fn echoes() -> TestProject {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Echo", ChannelLayout::Stereo);
    let imp = tp.impulse(2, 64);
    tp.clip(t, imp, MusicalTime::from_quarters_i(1), 64);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::ECHO, "Delay"),
        bypass: false,
        parameters: vec![
            SavedParameter {
                id: ParameterId(id::TIME),
                value: 400.0,
            },
            SavedParameter {
                id: ParameterId(id::FEEDBACK),
                value: 0.6,
            },
        ],
        state: None,
        sidechain: None,
    };
    tp.project.track_mut(t).unwrap().inserts.push(slot);
    let later = tp.track(TrackKind::Audio, "Later", ChannelLayout::Stereo);
    tp.clip(later, imp, MusicalTime::from_quarters_i(3), 64);
    tp
}

/// Play 2 s, the panic at `panic_at` (`None`: no panic); the left output
/// by frame and the transport's position after each callback. `ahead`:
/// the tracks render ahead, at the device's pace — tried again (up to
/// three times) when the anticipator was late (a busy machine), since a
/// late block is silence.
fn play(panic_at: Option<usize>, ahead: bool) -> (Vec<f32>, Vec<i64>) {
    for tries in 1..=3 {
        let (out, positions, misses) = play_once(panic_at, ahead);
        if misses == 0 {
            return (out, positions);
        }
        eprintln!("try {tries}: {misses} blocks late (a busy machine): again");
    }
    panic!("late in every try: the anticipator does not keep up here");
}

fn play_once(panic_at: Option<usize>, ahead: bool) -> (Vec<f32>, Vec<i64>, u64) {
    let tp = echoes();
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    if ahead {
        r.controller
            .set_render_ahead(Some(Duration::from_millis(150)), 2);
        r.controller
            .sync(&tp.project, &tp.sources, Impact::Graph)
            .unwrap();
    }
    r.play_from(0).unwrap();
    let (mut out, mut positions) = (Vec::new(), Vec::new());
    while out.len() < 2 * SR as usize {
        if panic_at.is_some_and(|at| out.len() >= at && out.len() < at + BLOCK) {
            r.controller.reset_processors().unwrap();
        }
        out.extend_from_slice(r.step().output_ref(0));
        positions.push(r.controller.transport_snapshot().position);
        if ahead {
            std::thread::sleep(Duration::from_secs_f64(BLOCK as f64 / f64::from(SR)));
        }
    }
    (out, positions, r.controller.ahead_misses())
}

fn loudest(s: &[f32]) -> f32 {
    s.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// Where in the output the transport played position `x` (rendering
/// ahead, playback starts a little after the first callback).
fn index_of(positions: &[i64], x: i64) -> usize {
    let i = positions
        .iter()
        .position(|p| *p > x)
        .expect("played that far");
    i * BLOCK + (x - (positions[i] - BLOCK as i64)) as usize
}

#[test]
fn a_panic_silences_the_echoes_to_come() {
    for ahead in [false, true] {
        let sr = SR as usize;
        let at = sr * 7 / 10;
        let (plain, plain_positions) = play(None, ahead);
        let later = index_of(&plain_positions, (sr * 3 / 2) as i64);
        // The echoes are there without the panic.
        assert!(loudest(&plain[at..later]) > 0.05, "ahead {ahead}");
        let (panicked, positions) = play(Some(at), ahead);
        // Until the panic it plays the same.
        assert_eq!(&plain[..at], &panicked[..at], "ahead {ahead}");
        // After it, nothing but the later click, on its sample: the delay
        // lines were reset, what was rendered ahead with the tails in it
        // is not heard, and the transport went on (no jump back).
        assert_eq!(
            common::hits(&panicked[at..], 1e-6),
            vec![later - at],
            "ahead {ahead}: what plays after the panic"
        );
        assert!((panicked[later] - 1.0).abs() < 1e-5, "ahead {ahead}");
        let from = at / BLOCK;
        for w in positions[from..].windows(2) {
            assert_eq!(
                w[1] - w[0],
                BLOCK as i64,
                "ahead {ahead}: the transport jumped"
            );
        }
    }
}

/// Audio waiting in a delay that compensates another path's latency goes
/// too: a click on a dry track beside a track 20000 frames late is held
/// back at the master until then — a panic in between drops it.
#[test]
fn a_panic_empties_the_compensating_delays() {
    let play = |panic: bool| {
        let mut tp = TestProject::new(SR);
        let dry = tp.track(TrackKind::Audio, "Dry", ChannelLayout::Stereo);
        let late = tp.track(TrackKind::Audio, "Late", ChannelLayout::Stereo);
        let imp = tp.impulse(2, 64);
        tp.clip(dry, imp, MusicalTime::from_quarters_i(1), 64);
        let slot = PluginSlot {
            id: tp.project.ids.allocate(),
            plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
            bypass: false,
            parameters: vec![SavedParameter {
                id: ParameterId(0),
                value: 20_000.0,
            }],
            state: None,
            sidechain: None,
        };
        tp.project.track_mut(late).unwrap().inserts.push(slot);
        let config = EngineConfig {
            sample_rate: SR,
            max_block_size: BLOCK,
            ..EngineConfig::default()
        };
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
        r.play_from(0).unwrap();
        let at = SR as usize * 7 / 10;
        let mut out = Vec::new();
        while out.len() < SR as usize * 3 / 2 {
            if panic && out.len() >= at && out.len() < at + BLOCK {
                r.controller.reset_processors().unwrap();
            }
            out.extend_from_slice(r.step().output_ref(0));
        }
        out
    };
    // The click comes out at 0.5 s + 20000 frames.
    let heard = 24_000 + 20_000;
    assert!(play(false)[heard].abs() > 0.5);
    assert_eq!(loudest(&play(true)), 0.0, "the click after the panic");
}

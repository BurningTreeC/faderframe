#![allow(clippy::unwrap_used)]
//! A track's own delay (`Track::delay_ms`): later by whole samples or a
//! fraction of one, earlier through the delay compensation (the rest
//! waits), changed while playing without a jump, in the render-ahead
//! graph as in the audio thread's.

mod common;

use common::{TestProject, hits};
use faderframe_audio::OwnedBuffers;
use faderframe_core::{ChannelLayout, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{
    Command, History, Impact, OutputRouting, PluginRef, PluginSlot, Project, TrackKind,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;
use std::time::Duration;

const SR: u32 = 48_000;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

/// A mono track playing an impulse at quarter 1, on device output `out`.
fn impulse_track(tp: &mut TestProject, name: &str, out: u16) -> TrackId {
    let t = tp.track(TrackKind::Audio, name, ChannelLayout::Mono);
    let imp = tp.impulse(1, 64);
    tp.clip(t, imp, MusicalTime::from_quarters_i(1), 64);
    tp.project.track_mut(t).unwrap().output = OutputRouting::Hardware { first_channel: out };
    t
}

/// Samples of frames per sample at 48 kHz.
fn ms(samples: f64) -> f64 {
    samples * 1000.0 / f64::from(SR)
}

fn render(project: &Project, tp: &TestProject, outputs: usize, frames: usize) -> Vec<Vec<f32>> {
    let mut r = OfflineRenderer::new(project, &tp.sources, config(), 256, outputs).unwrap();
    r.play_from(0).unwrap();
    r.render(frames)
}

/// Where a filtered impulse is centred (its phase delay at `hz`) and its
/// level there (dB).
fn phase_delay(y: &[f32], hz: f64) -> (f64, f64) {
    let w = 2.0 * std::f64::consts::PI * hz / f64::from(SR);
    let (mut re, mut im) = (0.0, 0.0);
    for (n, v) in y.iter().enumerate() {
        re += f64::from(*v) * (-w * n as f64).cos();
        im += f64::from(*v) * (-w * n as f64).sin();
    }
    let mut d = -im.atan2(re) / w;
    // The phase wraps every period.
    let period = f64::from(SR) / hz;
    let rough = y
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map_or(0.0, |(i, _)| i as f64);
    d += ((rough - d) / period).round() * period;
    (d, 20.0 * (re * re + im * im).sqrt().log10())
}

#[test]
fn whole_samples_later_and_earlier() {
    let mut tp = TestProject::new(SR);
    let dry = impulse_track(&mut tp, "Dry", 0);
    let moved = impulse_track(&mut tp, "Moved", 1);
    let _ = dry;
    for (samples, latency) in [(48.0, 0u32), (480.0, 0), (-480.0, 512), (-48.0, 32 + 48)] {
        let mut p = tp.project.clone();
        p.track_mut(moved).unwrap().delay_ms = ms(samples);
        let r = OfflineRenderer::new(&p, &tp.sources, config(), 256, 2).unwrap();
        assert_eq!(
            r.controller.graph_stats().output_latency,
            latency,
            "{samples}"
        );
        let out = render(&p, &tp, 2, Q + 2048);
        let a = hits(&out[0], 1e-4);
        let b = hits(&out[1], 1e-4);
        assert_eq!(a, vec![Q + latency as usize], "{samples}: dry");
        assert_eq!(
            b,
            vec![(Q as i64 + i64::from(latency) + samples as i64) as usize],
            "{samples}: moved"
        );
        assert!((out[1][b[0]] - out[0][a[0]]).abs() < 1e-6);
    }
}

#[test]
fn fractions_of_a_sample_either_way() {
    let mut tp = TestProject::new(SR);
    let _dry = impulse_track(&mut tp, "Dry", 0);
    let moved = impulse_track(&mut tp, "Moved", 1);
    for samples in [48.5, 100.25, 0.5, 13.37, -0.3, -37.75, -1_000.1] {
        let mut p = tp.project.clone();
        p.track_mut(moved).unwrap().delay_ms = ms(samples);
        let out = render(&p, &tp, 2, Q + 4096);
        let at = hits(&out[0], 1e-4)[0];
        for hz in [100.0, 1_000.0, 5_000.0, 10_000.0, 18_000.0] {
            let (d, db) = phase_delay(&out[1][at - 2048..at + 2048], hz);
            let d = d - 2048.0;
            assert!(
                (d - samples).abs() < 0.002,
                "{samples} at {hz} Hz: {d} samples late"
            );
            assert!(db.abs() < 0.01, "{samples} at {hz} Hz: {db} dB");
        }
    }
}

#[test]
fn an_earlier_track_inside_a_bus_still_meets_a_dry_one() {
    // The bus sums the early track with a dry one: the dry one waits
    // there; the master's other dry track waits for both.
    let mut tp = TestProject::new(SR);
    let bus = tp.track(TrackKind::Bus, "Kit", ChannelLayout::Stereo);
    let close = impulse_track(&mut tp, "Close", 0);
    let room = impulse_track(&mut tp, "Room", 0);
    for t in [close, room] {
        tp.project.track_mut(t).unwrap().output = OutputRouting::Track { track: bus };
    }
    let other = impulse_track(&mut tp, "Other", 2);
    // The room's impulse 240 samples late: aligned to the close one.
    let mut late = vec![0.0f32; 512];
    late[240] = 1.0;
    let source = tp.source(faderframe_audio_files::AudioData::from_channels(
        SR,
        vec![late],
    ));
    let room_clip = tp.project.track(room).unwrap().clips[0];
    tp.project.clips.remove(&room_clip);
    tp.project.track_mut(room).unwrap().clips.clear();
    tp.clip(room, source, MusicalTime::from_quarters_i(1), 512);
    tp.project.track_mut(room).unwrap().delay_ms = ms(-240.0);
    let r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 3).unwrap();
    let latency = r.controller.graph_stats().output_latency as usize;
    assert_eq!(latency, 32 + 240);
    let out = render(&tp.project, &tp, 3, Q + 4096);
    // Both kit microphones on one sample, with the other track.
    let kit = hits(&out[0], 1e-4);
    assert_eq!(kit, vec![Q + latency], "{kit:?}");
    // Each into the stereo bus by the pan law: both together.
    assert!((out[0][Q + latency] - 2.0 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
    assert_eq!(hits(&out[2], 1e-4), vec![Q + latency]);
    let _ = other;
}

#[test]
fn a_new_delay_while_playing_glides_over_without_a_jump() {
    // A slow sine (a jump would show as a step), its delay changed while
    // playing within its shape (no rebuild), then into another (a
    // rebuild, the latency changes: the engine dips).
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Mono);
    let frames = 4 * SR as usize;
    let tone: Vec<f32> = (0..frames)
        .map(|i| {
            (0.5 * (2.0 * std::f64::consts::PI * 100.0 * i as f64 / f64::from(SR)).sin()) as f32
        })
        .collect();
    let src = tp.source(faderframe_audio_files::AudioData::from_channels(
        SR,
        vec![tone],
    ));
    tp.clip(t, src, MusicalTime::ZERO, frames as i64);
    tp.project.track_mut(t).unwrap().delay_ms = 2.0;
    tp.project.track_mut(t).unwrap().output = OutputRouting::Hardware { first_channel: 0 };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 1).unwrap();
    r.play_from(0).unwrap();
    // Whole callbacks (a render keeps none of a callback's rest).
    let half = 256 * 94;
    let mut out = r.render(half).remove(0);
    let mut h = History::default();
    h.apply(
        &mut tp.project,
        Command::SetTrackDelay {
            track: t,
            ms: 5.333,
        },
    )
    .unwrap();
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    assert_eq!(r.controller.graph_stats().output_latency, 0, "no rebuild");
    out.extend(r.render(half).remove(0));
    // The largest step of a 100 Hz sine at 0.5 is 0.0066 a sample.
    let steps = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(steps < 0.01, "a jump of {steps}");
    // Now 5.333 ms late: compare with the source.
    let want = |i: usize| {
        let s = i as f64 - 5.333 * 48.0;
        0.5 * (2.0 * std::f64::consts::PI * 100.0 * s / f64::from(SR)).sin()
    };
    let end = out.len() - 1;
    assert!((f64::from(out[end]) - want(end)).abs() < 1e-4);
    // Earlier: another shape, a rebuild and the latency.
    h.apply(
        &mut tp.project,
        Command::SetTrackDelay { track: t, ms: -1.5 },
    )
    .unwrap();
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    assert_eq!(r.controller.graph_stats().output_latency, 32 + 96);
}

#[test]
fn rendered_ahead_a_delayed_track_sounds_the_same() {
    // A track with a device (rendered ahead) delayed earlier and later,
    // beside a dry one: the anticipator's graph counts the delay's latency
    // as the audio thread's does.
    let mut tp = TestProject::new(SR);
    let noise: Vec<f32> = {
        let mut x = 1u32;
        (0..4 * SR as usize)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    };
    let src = tp.source(faderframe_audio_files::AudioData::from_channels(
        SR,
        vec![noise],
    ));
    for (name, ms) in [("Early", -2.345), ("Late", 3.21), ("Dry", 0.0)] {
        let t = tp.track(TrackKind::Audio, name, ChannelLayout::Mono);
        tp.clip(t, src, MusicalTime::ZERO, 4 * i64::from(SR));
        tp.project.track_mut(t).unwrap().delay_ms = ms;
        if name != "Dry" {
            let id = tp.project.ids.allocate();
            tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
                id,
                plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
                bypass: false,
                parameters: Vec::new(),
                state: None,
                sidechain: None,
            });
        }
    }
    const BLOCK: usize = 256;
    let run = |ahead: bool| -> (Vec<f32>, u64, usize) {
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
        let ahead_tracks = r.controller.ahead_tracks().len();
        let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
        // Let the anticipator start.
        for _ in 0..20 {
            r.processor.process_device(&mut bufs);
            std::thread::sleep(Duration::from_secs_f64(BLOCK as f64 / f64::from(SR)));
            r.controller.collect_garbage();
        }
        r.controller.transport(TransportCommand::Play).unwrap();
        let mut heard = Vec::new();
        let mut started = None;
        for _ in 0..(SR as usize / BLOCK) {
            r.processor.process_device(&mut bufs);
            let t = r.controller.transport_snapshot();
            if t.playing {
                started.get_or_insert(t.position - BLOCK as i64);
                heard.extend_from_slice(bufs.output_ref(0));
            }
            if ahead {
                std::thread::sleep(Duration::from_secs_f64(BLOCK as f64 / f64::from(SR)));
            }
            r.controller.collect_garbage();
        }
        assert_eq!(started, Some(0));
        (heard, r.controller.ahead_misses(), ahead_tracks)
    };
    for tries in 1..=3 {
        let (plain, _, none) = run(false);
        let (ahead, misses, rendered) = run(true);
        assert_eq!(none, 0);
        assert_eq!(rendered, 2, "both delayed tracks are rendered ahead");
        if misses > 0 {
            eprintln!("try {tries}: {misses} blocks late (a busy machine): again");
            continue;
        }
        let n = plain.len().min(ahead.len());
        assert!(n > SR as usize / 2);
        assert!(plain.iter().any(|s| s.abs() > 0.1));
        let differ = (0..n).filter(|i| plain[*i] != ahead[*i]).count();
        assert_eq!(differ, 0, "{differ} of {n} samples differ");
        return;
    }
    panic!("late in every try: the anticipator does not keep up here");
}

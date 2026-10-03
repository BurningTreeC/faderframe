mod common;

use common::{TestProject, approx, hits};
use faderframe_core::ParameterId;
use faderframe_core::{ChannelLayout, SendId, builtin};
use faderframe_engine::offline::{OfflineRenderer, render_project};
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::demo::demo_project;
use faderframe_project::{
    AuxSend, Command, History, Impact, PluginRef, PluginSlot, SavedParameter, SendTap, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

#[test]
fn mono_tracks_sum_into_master_with_constant_power_pan() {
    let mut tp = TestProject::new(SR);
    let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let b = tp.track(TrackKind::Audio, "B", ChannelLayout::Mono);
    let sa = tp.dc(1, 0.25, 4800);
    let sb = tp.dc(1, 0.5, 4800);
    tp.clip(a, sa, MusicalTime::ZERO, 4800);
    tp.clip(b, sb, MusicalTime::ZERO, 4800);
    let out = render_project(&tp.project, &tp.sources, config(), 256, 0, 2048).unwrap();
    // Centre pan, constant power: each side gets 1/√2 of the mono signal.
    let expected = (0.25 + 0.5) * std::f32::consts::FRAC_1_SQRT_2;
    assert!(approx(out[0][1000], expected, 1e-5), "{}", out[0][1000]);
    assert!(approx(out[1][1000], expected, 1e-5));
}

#[test]
fn routing_through_bus_with_fader_and_hard_pan() {
    let mut tp = TestProject::new(SR);
    let bus = tp.track(TrackKind::Bus, "Bus", ChannelLayout::Stereo);
    let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, 9600);
    tp.clip(a, src, MusicalTime::ZERO, 9600);
    tp.route(a, bus);
    let p = &mut tp.project;
    p.track_mut(bus).unwrap().volume_db = -6.020_6; // ×0.5
    p.track_mut(a).unwrap().pan = 1.0; // balance hard right: L silent
    let out = render_project(&tp.project, &tp.sources, config(), 128, 0, 4096).unwrap();
    assert!(approx(out[0][3000], 0.0, 1e-6));
    assert!(approx(out[1][3000], 0.25, 1e-4), "{}", out[1][3000]);
}

#[test]
fn mute_and_solo_are_applied_with_ramps() {
    let mut tp = TestProject::new(SR);
    let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let b = tp.track(TrackKind::Audio, "B", ChannelLayout::Stereo);
    let sa = tp.dc(2, 0.1, SR as usize);
    let sb = tp.dc(2, 0.2, SR as usize);
    tp.clip(a, sa, MusicalTime::ZERO, SR as i64);
    tp.clip(b, sb, MusicalTime::ZERO, SR as i64);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    let before = r.render(1024);
    assert!(approx(before[0][1000], 0.3, 1e-5));

    // Mute B.
    let mut h = History::default();
    let impact = h
        .apply(
            &mut tp.project,
            Command::SetTrackMute { track: b, on: true },
        )
        .unwrap();
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let after = r.render(1024);
    // First block ramps, then steady state.
    assert!(approx(after[0][1000], 0.1, 1e-5), "{}", after[0][1000]);
    assert!(
        after[0][..256].windows(2).all(|w| w[1] <= w[0] + 1e-7),
        "mute ramps down monotonically"
    );

    // Unmute B, solo A → only A audible.
    h.apply(
        &mut tp.project,
        Command::SetTrackMute {
            track: b,
            on: false,
        },
    )
    .unwrap();
    let impact = h
        .apply(
            &mut tp.project,
            Command::SetTrackSolo { track: a, on: true },
        )
        .unwrap();
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let solo = r.render(1024);
    assert!(approx(solo[1][1000], 0.1, 1e-5), "{}", solo[1][1000]);
}

#[test]
fn post_fader_send_feeds_aux_return() {
    let mut tp = TestProject::new(SR);
    let aux = tp.track(TrackKind::Aux, "Aux", ChannelLayout::Stereo);
    let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.4, 9600);
    tp.clip(a, src, MusicalTime::ZERO, 9600);
    let send = AuxSend {
        id: SendId(tp.project.ids.allocate::<SendId>().raw()),
        target: aux,
        level_db: -6.020_6,
        tap: SendTap::PostFader,
        enabled: true,
    };
    tp.project.track_mut(a).unwrap().sends.push(send.clone());
    let out = render_project(&tp.project, &tp.sources, config(), 64, 0, 2048).unwrap();
    // Direct 0.4 + send 0.2 through the aux.
    assert!(approx(out[0][1500], 0.6, 1e-4), "{}", out[0][1500]);

    // Pre-fader sends ignore the source fader.
    tp.project.track_mut(a).unwrap().volume_db = faderframe_core::gain::SILENCE_DB;
    tp.project.track_mut(a).unwrap().sends[0].tap = SendTap::PreFader;
    let out = render_project(&tp.project, &tp.sources, config(), 64, 0, 2048).unwrap();
    assert!(approx(out[0][1500], 0.2, 1e-4), "{}", out[0][1500]);
}

#[test]
fn plugin_latency_is_compensated_at_the_summing_point() {
    for latency in [0u32, 64, 1000, 4096] {
        let mut tp = TestProject::new(SR);
        let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
        let b = tp.track(TrackKind::Audio, "B", ChannelLayout::Stereo);
        let imp = tp.impulse(2, 64);
        let start = MusicalTime::from_quarters_i(1); // 0.5 s at 120 BPM
        tp.clip(a, imp, start, 64);
        tp.clip(b, imp, start, 64);
        let slot = PluginSlot {
            id: tp.project.ids.allocate(),
            plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
            bypass: false,
            parameters: vec![SavedParameter {
                id: ParameterId(0),
                value: latency as f64,
            }],
            state: None,
        };
        tp.project.track_mut(b).unwrap().inserts.push(slot);
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), 256, 2).unwrap();
        assert_eq!(r.controller.graph_stats().output_latency, latency);
        r.play_from(0).unwrap();
        let out = r.render(SR as usize);
        // Both impulses arrive at the same sample: the uncompensated one
        // was delayed to match the plugin path.
        assert_eq!(
            hits(&out[0], 1e-3),
            vec![24_000 + latency as usize],
            "latency {latency}"
        );
        assert!(approx(out[0][24_000 + latency as usize], 2.0, 1e-5));
    }
}

#[test]
fn clips_start_on_the_exact_sample() {
    let mut tp = TestProject::new(SR);
    tp.project.timeline.tempo.set_initial_bpm(97.0);
    let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let imp = tp.impulse(2, 16);
    let start = tp.project.timeline.meter.bar_start(1) + MusicalTime::QUARTER / 3;
    tp.clip(a, imp, start, 16);
    let expected = tp.project.timeline.to_samples(start, SR as f64) as usize;
    let out = render_project(&tp.project, &tp.sources, config(), 333, 0, expected + 1000).unwrap();
    assert_eq!(hits(&out[1], 1e-6), vec![expected]);
}

#[test]
fn loop_playback_wraps_sample_accurately() {
    let mut tp = TestProject::new(SR);
    let a = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let imp = tp.impulse(2, 16);
    tp.clip(a, imp, MusicalTime::ZERO, 16);
    // Loop the first 1000 samples (≈ 0.0417 quarters at 120 BPM).
    let end = tp.project.timeline.to_musical(1000, SR as f64);
    tp.project.loop_range = faderframe_project::MusicalRange::new(MusicalTime::ZERO, end);
    tp.project.loop_enabled = true;
    let loop_len = tp.project.timeline.to_samples(end, SR as f64) as usize;
    let out = render_project(
        &tp.project,
        &tp.sources,
        config(),
        256,
        0,
        loop_len * 4 + 10,
    )
    .unwrap();
    assert_eq!(
        hits(&out[0], 1e-6),
        vec![0, loop_len, loop_len * 2, loop_len * 3, loop_len * 4]
    );
}

#[test]
fn demo_project_renders_audio_and_meters() {
    let project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let mut r = OfflineRenderer::new(&project, &sources, config(), 512, 2).unwrap();
    assert!(
        r.controller.warnings().is_empty(),
        "{:?}",
        r.controller.warnings()
    );
    // Start inside bar 5 where every track including the synth plays.
    let start = project
        .timeline
        .to_samples(project.timeline.meter.bar_start(4), SR as f64);
    r.play_from(start).unwrap();
    let out = r.render(SR as usize);
    let peak = out[0].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05 && peak < 4.0, "peak {peak}");
    assert!(out.iter().flatten().all(|s| s.is_finite()));
    for t in &project.tracks {
        let m = r.controller.take_meter(t.id).unwrap();
        assert!(m.left.peak > 0.0, "track '{}' meter is silent", t.name);
    }
    let metrics = r.controller.metrics();
    assert!(metrics.callbacks > 0);
    assert_eq!(r.controller.leaked_objects(), 0);
}

#[test]
fn instrument_track_plays_midi_through_synth_only_while_rolling() {
    let project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let synth = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap();
    // Solo the synth track so only it is audible.
    let mut p = project.clone();
    p.track_mut(synth.id).unwrap().solo = true;
    let start = p
        .timeline
        .to_samples(p.timeline.meter.bar_start(4), SR as f64);
    let out = render_project(&p, &sources, config(), 256, start, SR as usize / 2).unwrap();
    assert!(
        out[0].iter().any(|s| s.abs() > 1e-3),
        "synth produced sound"
    );
    // Before its clip (bar 1) the soloed synth track is silent.
    let early = render_project(&p, &sources, config(), 256, 0, SR as usize / 4).unwrap();
    assert!(early[0].iter().all(|s| s.abs() < 1e-6));
}

#[test]
fn graph_rebuilds_keep_playing_and_never_leak() {
    let mut project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let mut r = OfflineRenderer::new(&project, &sources, config(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    r.render(256); // install the initial graph
    let mut h = History::default();
    let bass = project.tracks.iter().find(|t| t.name == "Bass").unwrap().id;
    for i in 0..20 {
        let cmd = if i % 2 == 0 {
            Command::SetTrackMonitor {
                track: bass,
                mode: faderframe_project::MonitorMode::Input,
            }
        } else {
            Command::SetTrackMonitor {
                track: bass,
                mode: faderframe_project::MonitorMode::Off,
            }
        };
        let impact = h.apply(&mut project, cmd).unwrap();
        assert_eq!(impact, Impact::Graph);
        r.controller.sync(&project, &sources, impact).unwrap();
        r.render(512);
    }
    assert_eq!(r.controller.graphs_installed(), 21);
    assert_eq!(r.controller.leaked_objects(), 0);
    let snap = r.controller.transport_snapshot();
    assert!(snap.playing && snap.position > 0);
}

#[test]
fn feedback_routing_is_refused_by_the_project_layer() {
    let mut project = demo_project(SR);
    let mut h = History::default();
    let bus = project
        .tracks
        .iter()
        .find(|t| t.name == "Drum Bus")
        .unwrap()
        .id;
    let echo = project.tracks.iter().find(|t| t.name == "Echo").unwrap().id;
    h.apply(
        &mut project,
        Command::SetTrackOutput {
            track: echo,
            output: faderframe_project::OutputRouting::Track { track: bus },
        },
    )
    .unwrap();
    let send = AuxSend {
        id: project.ids.allocate(),
        target: echo,
        level_db: 0.0,
        tap: SendTap::PostFader,
        enabled: true,
    };
    assert!(
        h.apply(
            &mut project,
            Command::AddSend {
                track: bus,
                send,
                index: None
            }
        )
        .is_err()
    );
    // The engine graph still compiles.
    let sources = render_generated_sources(&project, SR);
    OfflineRenderer::new(&project, &sources, config(), 256, 2).unwrap();
}

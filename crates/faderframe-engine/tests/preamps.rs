#![allow(clippy::unwrap_used)]
mod common;
use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::{ChannelLayout, builtin};
use faderframe_engine::{EngineConfig, offline::render_project};
use faderframe_project::{PluginRef, PluginSlot, Project, TrackKind};
use faderframe_timeline::MusicalTime;

fn preamp(p: &mut Project, model: usize) -> PluginSlot {
    PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(builtin::PREAMPS[model].0, builtin::PREAMPS[model].1),
        bypass: false,
        parameters: vec![],
        state: None,
        sidechain: None,
    }
}
fn render(tp: &TestProject, block: usize) -> Vec<Vec<f32>> {
    render_project(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        block,
        0,
        4096,
    )
    .unwrap()
}
#[test]
fn audio_preamp_is_before_inserts_and_all_channels_are_processed_independently() {
    let mut tp = TestProject::new(48_000);
    let layout = ChannelLayout::Discrete(4);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = layout;
    let track = tp.track(TrackKind::Audio, "Input", layout);
    let tone = (0..8192).map(|n| (n as f32 * 0.13).sin() * 0.125).collect();
    let src = tp.source(AudioData::from_channels(
        48_000,
        vec![tone, vec![0.0; 8192], vec![0.0; 8192], vec![0.0; 8192]],
    ));
    tp.clip(track, src, MusicalTime::ZERO, 8192);
    let slot = preamp(&mut tp.project, 1);
    tp.project.track_mut(track).unwrap().preamp = Some(slot.clone());
    let out = render(&tp, 128);
    assert!(out[0].iter().any(|x| x.abs() > 0.01));
    assert!(out[1..].iter().flatten().all(|x| x.abs() < 1e-6));
    assert_eq!(out, render(&tp, 511));
    // The dedicated slot has exactly the same audio placement as the first effect.
    let t = tp.project.track_mut(track).unwrap();
    t.preamp = None;
    t.inserts.insert(0, slot.clone());
    assert_eq!(out, render(&tp, 128));
    let bus = tp.track(TrackKind::Bus, "Pre-FX send", layout);
    let send = tp.project.ids.allocate();
    let t = tp.project.track_mut(track).unwrap();
    t.inserts.clear();
    t.preamp = Some(slot);
    t.output = faderframe_project::OutputRouting::None;
    t.sends.push(faderframe_project::AuxSend {
        id: send,
        target: bus,
        level_db: 0.0,
        tap: faderframe_project::SendTap::PreFx,
        enabled: true,
    });
    assert_eq!(
        out,
        render(&tp, 128),
        "pre-FX sends must include the input stage"
    );
}

#[test]
fn instrument_preamp_follows_audio_generation_for_legacy_and_insert_instruments() {
    let project = faderframe_project::demo::demo_project(48_000);
    let sources = faderframe_engine::render_generated_sources(&project, 48_000);
    let mut tp = TestProject { project, sources };
    let track = tp
        .project
        .tracks
        .iter()
        .find(|t| t.instrument.is_some())
        .unwrap()
        .id;
    for t in &mut tp.project.tracks {
        t.solo = t.id == track;
    }
    for clip in tp.project.clips.values_mut().filter(|c| c.track == track) {
        clip.start = MusicalTime::ZERO;
    }
    let slot = preamp(&mut tp.project, 2);
    tp.project.track_mut(track).unwrap().preamp = Some(slot.clone());
    let legacy = render(&tp, 128);
    assert!(legacy.iter().flatten().any(|x| x.abs() > 0.001));
    let t = tp.project.track_mut(track).unwrap();
    let instrument = t.instrument.take().unwrap();
    t.inserts.insert(0, instrument);
    assert_eq!(legacy, render(&tp, 128));
    let t = tp.project.track_mut(track).unwrap();
    t.preamp = None;
    t.inserts.insert(1, slot);
    assert_eq!(legacy, render(&tp, 128));
}

#[test]
fn preamp_attenuates_the_last_synth_even_with_a_legacy_instrument() {
    use faderframe_core::ParameterId;
    use faderframe_project::SavedParameter;
    for legacy in [false, true] {
        for count in [1, 2] {
            let mut project = faderframe_project::demo::demo_project(48_000);
            let track = project
                .tracks
                .iter()
                .find(|t| t.instrument.is_some())
                .unwrap()
                .id;
            let instrument = project.track(track).unwrap().instrument.clone().unwrap();
            if !legacy {
                project.track_mut(track).unwrap().instrument = None;
            }
            for _ in 0..count {
                let mut slot = instrument.clone();
                slot.id = project.ids.allocate();
                project.track_mut(track).unwrap().inserts.push(slot);
            }
            for t in &mut project.tracks {
                t.solo = t.id == track;
                t.sends.clear();
            }
            for clip in project.clips.values_mut().filter(|c| c.track == track) {
                clip.start = MusicalTime::ZERO;
            }
            let slot = preamp(&mut project, 1);
            project.track_mut(track).unwrap().preamp = Some(slot);
            let sources = faderframe_engine::render_generated_sources(&project, 48_000);
            let mut tp = TestProject { project, sources };
            let loud = render(&tp, 128);
            tp.project
                .track_mut(track)
                .unwrap()
                .preamp
                .as_mut()
                .unwrap()
                .parameters
                .push(SavedParameter {
                    id: ParameterId(1),
                    value: -60.0,
                });
            let quiet = render(&tp, 128);
            assert!(loud.iter().flatten().any(|x| x.abs() > 0.001));
            for (a, b) in loud.iter().flatten().zip(quiet.iter().flatten()) {
                assert!(
                    (a * 0.001 - b).abs() < 1e-7,
                    "legacy={legacy}, inserts={count}: Master must trim generated audio ({a} -> {b})"
                );
            }
        }
    }
}

#[test]
fn preamp_reservoir_latency_is_compensated_on_parallel_dry_tracks() {
    let mut tp = TestProject::new(48_000);
    let dry = tp.track(TrackKind::Audio, "Dry", ChannelLayout::Stereo);
    let wet = tp.track(TrackKind::Audio, "Preamp", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.125, 8192);
    for track in [dry, wet] {
        tp.clip(track, src, MusicalTime::ZERO, 8192);
    }
    let slot = preamp(&mut tp.project, 1);
    tp.project.track_mut(wet).unwrap().preamp = Some(slot);
    let r = faderframe_engine::offline::OfflineRenderer::new(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        128,
        2,
    )
    .unwrap();
    let latency = r.controller.graph_stats().output_latency as usize;
    let instance = faderframe_plugin_host::PluginRegistry::with_builtins()
        .instantiate(
            faderframe_plugin_host::PluginFormat::Builtin,
            builtin::PREAMPS[1].0,
        )
        .unwrap();
    assert_eq!(latency, instance.latency_samples() as usize);
    assert!(latency > 128, "reservoir plus oversampling latency");
    let both = render(&tp, 128);
    tp.project.track_mut(dry).unwrap().mute = true;
    let only_preamp = render(&tp, 128);
    for c in 0..2 {
        for (i, (&sum, &processed)) in both[c].iter().zip(&only_preamp[c]).enumerate() {
            let expected = if i < latency { 0.0 } else { 0.125 };
            assert!(
                (sum - processed - expected).abs() < 1e-6,
                "dry alignment at {i}"
            );
        }
    }
}

#[test]
fn live_hardware_input_can_be_played_through_a_preamp_worker() {
    use faderframe_audio::OwnedBuffers;
    use faderframe_project::{InputRouting, MonitorMode};
    let mut tp = TestProject::new(48_000);
    let track = tp.track(TrackKind::Audio, "Live microphone", ChannelLayout::Mono);
    let slot = preamp(&mut tp.project, 1);
    let t = tp.project.track_mut(track).unwrap();
    t.preamp = Some(slot);
    t.input = InputRouting::Hardware { first_channel: 0 };
    t.monitor = MonitorMode::Input;
    t.record_arm = true;
    for block_size in [64, 128, 512] {
        let mut r = faderframe_engine::offline::OfflineRenderer::new(
            &tp.project,
            &tp.sources,
            EngineConfig::default(),
            block_size,
            2,
        )
        .unwrap();
        r.controller.plugins().set_realtime(true);
        r.controller.rebuild_graph(&tp.project).unwrap();
        assert!(r.controller.graph_stats().output_latency > 128);
        r.play_from(0).unwrap();
        let mut buffers = OwnedBuffers::new(2, 2, block_size);
        let mut peak = 0.0f32;
        for block in 0..40 {
            for (i, x) in buffers.input_mut(0).iter_mut().enumerate() {
                *x = ((block * block_size + i) as f32 * 0.13).sin() * 0.125;
            }
            r.processor.process_device(&mut buffers);
            if block == 0 {
                assert!(
                    buffers.output_ref(0)[..block_size.min(128)]
                        .iter()
                        .all(|&x| x == 0.0)
                );
            }
            peak = buffers
                .output_ref(0)
                .iter()
                .fold(peak, |p, x| p.max(x.abs()));
            std::thread::sleep(std::time::Duration::from_secs_f64(
                block_size as f64 / 48_000.0,
            ));
        }
        assert!(
            peak > 0.01,
            "live input reaches the master through the worker"
        );
        assert!(r.controller.failed_plugins().is_empty());
    }
}

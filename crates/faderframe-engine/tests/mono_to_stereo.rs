#![allow(clippy::unwrap_used)]
//! A hosted plugin with a stereo output on a mono track (an amp sim's
//! stereo preset): the graph folds its pair into the mono track, `(L + R)
//! / 2`, unless its slot is set to Mono → Stereo — then the track keeps
//! the pair and is stereo from the insert on.

mod common;

use common::TestProject;
use faderframe_audio::OwnedBuffers;
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, PluginHost};
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{Command, Impact, PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;

const SR: u32 = 48_000;
const BLOCK: usize = 128;
const LEVEL: f32 = 0.4;

// --- a stereo effect: the left as it comes, the right at half ---------------------------

struct Factory;

struct Instance {
    descriptor: PluginDescriptor,
}

struct Spread;

impl PluginFactory for Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        let stereo = AudioPortInfo {
            channels: 2,
            is_main: true,
        };
        Ok(Box::new(Instance {
            descriptor: PluginDescriptor {
                format: PluginFormat::Clap,
                id: id.into(),
                name: "Spread".into(),
                vendor: "FaderFrame".into(),
                version: "1".into(),
                category: PluginCategory::Effect,
                audio_inputs: vec![stereo],
                audio_outputs: vec![stereo],
                note_inputs: 0,
                note_outputs: 0,
            },
        }))
    }
}

impl PluginInstance for Instance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &[]
    }
    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        None
    }
    fn set_parameter(&mut self, id: ParameterId, _v: f64) -> Result<(), PluginError> {
        Err(PluginError::UnknownParameter(id))
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(Vec::new())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Spread))
    }
}

impl PluginProcessor for Spread {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let n = io.frames;
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return ProcessStatus::Continue;
        };
        assert_eq!(input.num_channels(), 2, "a stereo plugin gets a pair");
        assert_eq!(out.num_channels(), 2, "and gives one");
        for c in 0..2 {
            let gain = if c == 0 { 1.0 } else { 0.5 };
            for f in 0..n {
                out.channel_mut(c)[f] = input.channel(c)[f] * gain;
            }
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

// --- the rig ---------------------------------------------------------------------------

/// A steady level on a mono track through the stereo effect into the
/// master; the master's left and right after a while.
fn played(mono_to_stereo: bool) -> (ChannelLayout, f32, f32) {
    let mut tp = TestProject::new(SR);
    let guitar = tp.track(TrackKind::Audio, "Guitar", ChannelLayout::Mono);
    let level = tp.dc(1, LEVEL, SR as usize);
    tp.clip(guitar, level, MusicalTime::ZERO, i64::from(SR));
    let plugin = PluginInstanceId(9_900);
    tp.project
        .track_mut(guitar)
        .unwrap()
        .inserts
        .push(PluginSlot {
            id: plugin,
            plugin: PluginRef {
                format: faderframe_project::PluginFormat::Clap,
                id: "org.faderframe.test.spread".into(),
                name: "Spread".into(),
            },
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        });
    // Through the command, as the mixer's menu sets it.
    let inverse = Command::SetPluginMonoToStereo {
        track: guitar,
        plugin,
        on: mono_to_stereo,
    }
    .apply(&mut tp.project)
    .unwrap();
    assert_eq!(
        inverse,
        Command::SetPluginMonoToStereo {
            track: guitar,
            plugin,
            on: false
        }
    );
    let layout = tp.project.track(guitar).unwrap().chain_layout();
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    *r.controller.plugins() = PluginHost::new(registry);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    r.controller.transport(TransportCommand::Locate(0)).unwrap();
    r.controller.transport(TransportCommand::Play).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    for _ in 0..(SR as usize / 4 / BLOCK) {
        r.processor.process_device(&mut bufs);
        r.controller.collect_garbage();
    }
    let at = BLOCK / 2;
    (layout, bufs.output_ref(0)[at], bufs.output_ref(1)[at])
}

#[test]
fn a_stereo_plugin_on_a_mono_track_is_folded_unless_mono_to_stereo() {
    // Off: the pair (0.4, 0.2) folded to one channel, their average 0.3,
    // panned to the middle (the project's -3 dB centre).
    let (layout, l, r) = played(false);
    assert_eq!(layout, ChannelLayout::Mono);
    let folded = (LEVEL + LEVEL / 2.0) / 2.0 * std::f32::consts::FRAC_1_SQRT_2;
    assert!((l - folded).abs() < 1e-5, "folded to {l}, not {folded}");
    assert_eq!(l, r, "mono in the middle");

    // Mono → Stereo: the track stereo from the insert, the pair kept
    // (balance in the middle: as it is).
    let (layout, l, r) = played(true);
    assert_eq!(layout, ChannelLayout::Stereo);
    assert!((l - LEVEL).abs() < 1e-5, "left {l}");
    assert!((r - LEVEL / 2.0).abs() < 1e-5, "right {r}");
}

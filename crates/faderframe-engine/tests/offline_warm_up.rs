#![allow(clippy::unwrap_used)]
//! A third-party plugin that prepares its state on a thread of its own
//! after `load_state` returns, playing silence until then (TONE3000 queues
//! its models): an offline render — a self-frozen track's cache, a bounce —
//! starting at once, faster than real time, rendered the first seconds of
//! the music silent. Renders give such plugins a moment of real time first
//! (`OfflineRenderer::warm_up`).

mod common;

use common::TestProject;
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const ID: &str = "org.faderframe.test.slow-model";
/// What the plugin takes to have its "model" ready after instantiation.
const LOADS_FOR: Duration = Duration::from_millis(300);

struct Factory;

struct Instance {
    descriptor: PluginDescriptor,
    ready_at: Instant,
}

/// Plays its input once its model is ready, silence before.
struct Amp {
    ready_at: Instant,
}

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
                name: "Slow Model".into(),
                vendor: "FaderFrame".into(),
                version: "1".into(),
                category: PluginCategory::Effect,
                audio_inputs: vec![stereo],
                audio_outputs: vec![stereo],
                note_inputs: 0,
                note_outputs: 0,
            },
            ready_at: Instant::now() + LOADS_FOR,
        }))
    }
}

impl PluginInstance for Instance {
    fn third_party(&self) -> bool {
        true
    }
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
        Ok(Box::new(Amp {
            ready_at: self.ready_at,
        }))
    }
}

impl PluginProcessor for Amp {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let ready = Instant::now() >= self.ready_at;
        let n = io.frames;
        let input = io.audio_in.first();
        for out in io.audio_out.iter_mut() {
            for c in 0..out.num_channels() {
                for f in 0..n {
                    out.channel_mut(c)[f] = match input {
                        Some(i) if ready => i.channel(c.min(i.num_channels() - 1))[f],
                        _ => 0.0,
                    };
                }
            }
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

fn registry() -> PluginRegistry {
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    registry
}

#[test]
fn a_render_starts_once_a_plugin_preparing_in_the_background_is_ready() {
    faderframe_plugin_host::set_default_registry(registry);
    let mut tp = TestProject::new(SR);
    let track = tp.track(TrackKind::Audio, "Guitar", ChannelLayout::Stereo);
    // Sound from the clip's first frame: a constant over a second.
    let dc = tp.dc(2, 0.5, SR as usize);
    tp.clip(track, dc, MusicalTime::ZERO, i64::from(SR));
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: ID.into(),
            name: "Slow Model".into(),
        },
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
    };
    tp.project.track_mut(track).unwrap().inserts.push(slot);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 1024, 2).unwrap();
    r.play_from(0).unwrap();
    // A quarter of a second renders in far less than the plugin's load.
    let out = r.render(SR as usize / 4);
    let first = out[0].iter().position(|s| s.abs() > 1e-3);
    assert_eq!(
        first,
        Some(0),
        "the render's first frames played before the plugin was ready"
    );
}

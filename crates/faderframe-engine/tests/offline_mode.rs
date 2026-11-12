#![allow(clippy::unwrap_used)]
//! Plugins are told when a render is offline (issue #50): VST3's process
//! mode, CLAP's render mode, LV2's freewheeling and the AU's offline
//! render all come from `ProcessConfig::offline`. An offline renderer's
//! plugins get it; a live engine's never do — not even in the graphs
//! rendered ahead, which have no deadline of their own (FaderFrame's
//! `configure_realtime(false)`) but play live.

mod common;

use common::TestProject;
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, PluginHost};
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{Impact, PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;
use std::sync::Mutex;
use std::time::Duration;

const SR: u32 = 48_000;
const BLOCK: usize = 256;

/// What each test plugin (by id) was activated with: `offline`, and
/// FaderFrame's own "realtime" as configured last before it.
static SEEN: Mutex<Vec<(String, bool, bool)>> = Mutex::new(Vec::new());

fn seen(id: &str) -> Vec<(bool, bool)> {
    SEEN.lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p == id)
        .map(|&(_, offline, realtime)| (offline, realtime))
        .collect()
}

struct Factory;

struct Instance {
    descriptor: PluginDescriptor,
    realtime: bool,
}

struct Through;

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
                name: "Mode".into(),
                vendor: "FaderFrame".into(),
                version: "1".into(),
                category: PluginCategory::Effect,
                audio_inputs: vec![stereo],
                audio_outputs: vec![stereo],
                note_inputs: 0,
                note_outputs: 0,
            },
            realtime: true,
        }))
    }
}

impl PluginInstance for Instance {
    fn configure_realtime(&mut self, realtime: bool) {
        self.realtime = realtime;
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
        c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        SEEN.lock()
            .unwrap()
            .push((self.descriptor.id.clone(), c.offline, self.realtime));
        Ok(Box::new(Through))
    }
}

impl PluginProcessor for Through {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let n = io.frames;
        for c in 0..io.audio_out.first().map_or(0, |o| o.num_channels()) {
            let s: Vec<f32> = io.audio_in.first().map_or_else(
                || vec![0.0; n],
                |i| i.channel(c.min(i.num_channels() - 1))[..n].to_vec(),
            );
            if let Some(out) = io.audio_out.first_mut() {
                out.channel_mut(c)[..n].copy_from_slice(&s);
            }
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

fn registry() -> PluginRegistry {
    let mut r = PluginRegistry::with_builtins();
    r.add_factory(Box::new(Factory));
    r
}

/// A track with a tone through the test plugin `id`.
fn project(id: &str) -> (TestProject, faderframe_core::TrackId) {
    faderframe_plugin_host::set_default_registry(registry);
    let mut tp = TestProject::new(SR);
    let track = tp.track(TrackKind::Audio, "Source", ChannelLayout::Stereo);
    let tone = tp.dc(2, 0.25, SR as usize);
    tp.clip(track, tone, MusicalTime::ZERO, i64::from(SR));
    tp.project
        .track_mut(track)
        .unwrap()
        .inserts
        .push(PluginSlot {
            id: PluginInstanceId(9_900),
            plugin: PluginRef {
                format: faderframe_project::PluginFormat::Clap,
                id: id.into(),
                name: "Mode".into(),
            },
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        });
    (tp, track)
}

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    }
}

#[test]
fn an_offline_render_tells_its_plugins() {
    let id = "org.faderframe.test.mode.offline";
    let (tp, _) = project(id);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
    assert!(!r.controller.plugins().realtime(), "an offline renderer");
    let seen = seen(id);
    assert!(!seen.is_empty(), "the plugin was activated");
    assert!(
        seen.iter().all(|&(offline, _)| offline),
        "every activation offline: {seen:?}"
    );
}

#[test]
fn a_live_engine_never_says_offline_not_even_rendering_ahead() {
    let id = "org.faderframe.test.mode.live";
    let (tp, track) = project(id);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
    // A live session's plugin host (the renderer's own, offline, made
    // an instance of its own already: forgotten).
    *r.controller.plugins() = PluginHost::new(registry());
    SEEN.lock().unwrap().retain(|(p, _, _)| p != id);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    r.controller
        .set_render_ahead(Some(Duration::from_millis(200)), 1);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    assert!(
        r.controller.ahead_tracks().contains(&track),
        "the track is rendered ahead"
    );
    let seen = seen(id);
    assert!(
        seen.iter().any(|&(_, realtime)| !realtime),
        "built for the graph rendered ahead (no deadline of its own): {seen:?}"
    );
    assert!(
        seen.iter().all(|&(offline, _)| !offline),
        "never offline in a live engine: {seen:?}"
    );
}

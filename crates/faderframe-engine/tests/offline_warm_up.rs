#![allow(clippy::unwrap_used)]
//! Offline renders give third-party plug-ins real time before their first
//! frame (`faderframe_engine::offline::set_plugin_warm_up`): a plug-in
//! that prepares what its state names on a thread of its own plays from
//! the first frame, a latency it knows only once loaded is compensated,
//! nothing of the warm-up is captured, renders stay alike, and built-ins
//! alone never wait. The plug-ins here are CLAP-format stand-ins.

mod common;

use common::TestProject;
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_engine::capture::CapturePlan;
use faderframe_engine::offline::{OfflineRenderer, set_plugin_warm_up};
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginPoll, PluginProcessContext, PluginProcessor,
    PluginRegistry, ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const BLOCK: usize = 1024;
/// Plays silence until its "model" is ready, its input after.
const SLOW_MODEL: &str = "org.faderframe.test.slow-model";
/// Knows its latency once loaded and asks for a restart then.
const LATE_LATENCY: &str = "org.faderframe.test.late-latency";
/// A tone whose phase runs on with every frame it processes.
const FREE_RUNNING: &str = "org.faderframe.test.free-running";
/// What the slow plug-ins take to be ready after they are made.
const LOADS_FOR: Duration = Duration::from_millis(300);
const LATENCY: u32 = 256;

/// The warm-up is process-wide: one test at a time.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> std::sync::MutexGuard<'static, ()> {
    TURN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Factory;

struct Instance {
    descriptor: PluginDescriptor,
    ready_at: Instant,
    latency: u32,
    activation: u64,
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
                name: id.into(),
                vendor: "FaderFrame".into(),
                version: "1".into(),
                category: PluginCategory::Effect,
                audio_inputs: vec![stereo],
                audio_outputs: vec![stereo],
                note_inputs: 0,
                note_outputs: 0,
            },
            ready_at: Instant::now() + LOADS_FOR,
            latency: 0,
            activation: 0,
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
        self.latency
    }
    fn activation(&self) -> u64 {
        self.activation
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
    fn poll(&mut self) -> PluginPoll {
        // Loaded: the latency is known now, and asks for a restart.
        if self.descriptor.id == LATE_LATENCY
            && self.latency == 0
            && Instant::now() >= self.ready_at
        {
            self.latency = LATENCY;
            self.activation += 1;
            return PluginPoll {
                restart: true,
                ..PluginPoll::default()
            };
        }
        PluginPoll::default()
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(match self.descriptor.id.as_str() {
            SLOW_MODEL => Box::new(SlowModel {
                ready_at: self.ready_at,
            }),
            LATE_LATENCY => Box::new(Delay {
                line: vec![[0.0; 2]; self.latency as usize],
                at: 0,
            }),
            _ => Box::new(FreeRunning { frames: 0 }),
        })
    }
}

fn each_frame(io: &mut NodeIo<'_>, mut f: impl FnMut(usize, [f32; 2]) -> [f32; 2]) {
    let n = io.frames;
    for i in 0..n {
        let input = io.audio_in.first().map_or([0.0; 2], |b| {
            [b.channel(0)[i], b.channel(1.min(b.num_channels() - 1))[i]]
        });
        let out = f(i, input);
        if let Some(o) = io.audio_out.first_mut() {
            for (c, v) in out.iter().enumerate().take(o.num_channels()) {
                o.channel_mut(c)[i] = *v;
            }
        }
    }
}

struct SlowModel {
    ready_at: Instant,
}

impl PluginProcessor for SlowModel {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let ready = Instant::now() >= self.ready_at;
        each_frame(io, |_, x| if ready { x } else { [0.0; 2] });
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

struct Delay {
    line: Vec<[f32; 2]>,
    at: usize,
}

impl PluginProcessor for Delay {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let line = &mut self.line;
        let at = &mut self.at;
        each_frame(io, |_, x| {
            if line.is_empty() {
                return x;
            }
            let out = std::mem::replace(&mut line[*at], x);
            *at = (*at + 1) % line.len();
            out
        });
        ProcessStatus::Continue
    }
    fn reset(&mut self) {
        self.line.iter_mut().for_each(|f| *f = [0.0; 2]);
    }
}

struct FreeRunning {
    frames: u64,
}

impl PluginProcessor for FreeRunning {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let start = self.frames;
        each_frame(io, |i, _| {
            let phase = (start + i as u64) as f64 * 440.0 / f64::from(SR);
            let v = (0.5 * (phase * std::f64::consts::TAU).sin()) as f32;
            [v, v]
        });
        self.frames += io.frames as u64;
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

fn registry() -> PluginRegistry {
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    registry
}

/// A stereo track playing a constant from its first frame through
/// `plugin` (a stand-in above, or a built-in).
fn project(plugin: PluginRef) -> (TestProject, TrackId) {
    faderframe_plugin_host::set_default_registry(registry);
    let mut tp = TestProject::new(SR);
    let track = tp.track(TrackKind::Audio, "Guitar", ChannelLayout::Stereo);
    let dc = tp.dc(2, 0.5, SR as usize);
    tp.clip(track, dc, MusicalTime::ZERO, i64::from(SR));
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin,
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    };
    tp.project.track_mut(track).unwrap().inserts.push(slot);
    (tp, track)
}

fn clap(id: &str) -> PluginRef {
    PluginRef {
        format: faderframe_project::PluginFormat::Clap,
        id: id.into(),
        name: id.into(),
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    }
}

fn first_sound(out: &[Vec<f32>]) -> Option<usize> {
    out[0].iter().position(|s| s.abs() > 1e-3)
}

#[test]
fn a_plugin_preparing_in_the_background_plays_from_the_first_frame() {
    let _turn = turn();
    let (tp, _) = project(clap(SLOW_MODEL));
    // Without a warm-up the render is a quarter of a second in long
    // before the plug-in is ready.
    set_plugin_warm_up(0);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
    r.play_from(0).unwrap();
    assert_eq!(first_sound(&r.render(SR as usize / 4)), None);
    set_plugin_warm_up(1_000);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
    r.play_from(0).unwrap();
    assert_eq!(
        first_sound(&r.render(SR as usize / 4)),
        Some(0),
        "the render's first frames played before the plug-in was ready"
    );
}

#[test]
fn a_latency_known_once_loaded_is_compensated() {
    let _turn = turn();
    set_plugin_warm_up(1_000);
    let (tp, _) = project(clap(LATE_LATENCY));
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
    assert_eq!(
        r.controller.graph_stats().output_latency,
        LATENCY,
        "the plug-in's restart rebuilt the graph"
    );
    r.play_from(0).unwrap();
    assert_eq!(
        first_sound(&r.render(4 * BLOCK)),
        Some(LATENCY as usize),
        "the plug-in delays by what it reports"
    );
}

#[test]
fn nothing_of_the_warm_up_is_captured() {
    let _turn = turn();
    set_plugin_warm_up(300);
    let (tp, track) = project(clap(FREE_RUNNING));
    let plan = CapturePlan {
        tracks: vec![track],
        devices: true,
        block: BLOCK,
    };
    let mut r =
        OfflineRenderer::with_capture(&tp.project, &tp.sources, config(), BLOCK, 2, Some(plan))
            .unwrap();
    let capture = r.controller.capture().cloned().unwrap();
    let tap = capture.tap(track).unwrap();
    let probes = capture.probes();
    assert_eq!(probes.len(), 1);
    assert_eq!(tap.missed(), 0);
    assert!(tap.take(|c| assert!(c.iter().all(|x| x.is_empty()))));
    assert_eq!(probes[0].stats().frames, 0);
    r.play_from(0).unwrap();
    let mut taken = 0;
    for _ in 0..8 {
        r.step();
        assert!(tap.take(|c| taken += c[0].len()));
    }
    assert_eq!(taken, 8 * BLOCK);
    assert_eq!(tap.missed(), 0);
    assert_eq!(probes[0].stats().frames, 8 * BLOCK as u64);
}

#[test]
fn renders_with_a_warm_up_stay_alike() {
    let _turn = turn();
    set_plugin_warm_up(300);
    let (tp, _) = project(clap(FREE_RUNNING));
    let render = || {
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
        r.play_from(0).unwrap();
        r.render(4 * BLOCK)
    };
    let a = render();
    // Another render's set-up takes another while: the warm-up still
    // runs as many blocks.
    std::thread::sleep(Duration::from_millis(50));
    let b = render();
    assert!(first_sound(&a).is_some());
    assert_eq!(a, b, "the plug-in's tone ran on as far in both");
}

#[test]
fn built_in_devices_alone_never_wait() {
    let _turn = turn();
    set_plugin_warm_up(10_000);
    let (tp, _) = project(PluginRef::builtin(
        faderframe_core::builtin::GAIN,
        "Utility",
    ));
    let start = Instant::now();
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config(), BLOCK, 2).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a render of built-ins waited for {:?}",
        start.elapsed()
    );
    r.play_from(0).unwrap();
    assert_eq!(first_sound(&r.render(BLOCK)), Some(0));
}

#![allow(clippy::unwrap_used)]
//! A plugin's output buses taken by other tracks (a drum instrument's
//! close mics and overheads on tracks of their own, #52): what the plugin
//! gives on all of them in one block arrives at the master on the same
//! sample — whatever latency the plugin, the plugin's track or the tracks
//! taking its outputs have, offline and on a device with buses rendered
//! ahead.

mod common;

use common::{TestProject, hits};
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, PluginHost};
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{
    Impact, InputRouting, OutputRouting, PluginRef, PluginSlot, SavedParameter, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;
/// The splitter's output buses (stereo each).
const BUSES: usize = 3;

// --- a splitter: its input on every output bus, `latency` late ---------------------------

struct Factory;

struct Instance {
    descriptor: PluginDescriptor,
    latency: u32,
}

struct Split {
    latency: usize,
    /// A ring per input channel.
    rings: Vec<Vec<f32>>,
    at: usize,
}

const ID: &str = "org.faderframe.test.split";

impl PluginFactory for Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        // "<ID>@<latency>"
        let latency = id
            .split_once('@')
            .map_or(0, |(_, l)| l.parse().unwrap_or(0));
        let stereo = AudioPortInfo {
            channels: 2,
            is_main: true,
        };
        Ok(Box::new(Instance {
            descriptor: PluginDescriptor {
                format: PluginFormat::Clap,
                id: id.into(),
                name: "Split".into(),
                vendor: "FaderFrame".into(),
                version: "1".into(),
                category: PluginCategory::Effect,
                audio_inputs: vec![stereo],
                audio_outputs: vec![stereo; BUSES],
                note_inputs: 0,
                note_outputs: 0,
            },
            latency,
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
        let latency = self.latency as usize;
        Ok(Box::new(Split {
            latency,
            rings: vec![vec![0.0; latency.max(1)]; 2],
            at: 0,
        }))
    }
}

impl PluginProcessor for Split {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let n = io.frames;
        let input = io.audio_in.first();
        let start = self.at;
        for c in 0..2 {
            let mut at = start;
            for f in 0..n {
                let x = input.map_or(0.0, |i| i.channel(c.min(i.num_channels() - 1))[f]);
                let y = if self.latency == 0 {
                    x
                } else {
                    let y = self.rings[c][at];
                    self.rings[c][at] = x;
                    at = (at + 1) % self.latency;
                    y
                };
                for out in io.audio_out.iter_mut() {
                    if c < out.num_channels() {
                        out.channel_mut(c)[f] = y;
                    }
                }
            }
        }
        if self.latency > 0 {
            self.at = (start + n) % self.latency;
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {
        for r in &mut self.rings {
            r.fill(0.0);
        }
        self.at = 0;
    }
}

// --- the rig ---------------------------------------------------------------------------

fn slot(tp: &mut TestProject, plugin: PluginRef, latency: Option<u32>) -> PluginSlot {
    PluginSlot {
        id: tp.project.ids.allocate(),
        plugin,
        bypass: false,
        parameters: latency
            .map(|l| SavedParameter {
                id: ParameterId(0),
                value: f64::from(l),
            })
            .into_iter()
            .collect(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    }
}

/// A latency probe of `latency` frames on `track`'s inserts.
fn delay(tp: &mut TestProject, track: TrackId, latency: u32) {
    let s = slot(
        tp,
        PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
        Some(latency),
    );
    tp.project.track_mut(track).unwrap().inserts.push(s);
}

struct Rig {
    tp: TestProject,
    host: TrackId,
    plugin: PluginInstanceId,
}

/// A stereo track playing an impulse at quarter 1 into the splitter
/// (`latency`), balanced hard left.
fn rig(latency: u32) -> Rig {
    let mut tp = TestProject::new(SR);
    let host = tp.track(TrackKind::Audio, "Drums", ChannelLayout::Stereo);
    let imp = tp.impulse(2, 64);
    tp.clip(host, imp, MusicalTime::from_quarters_i(1), 64);
    let s = slot(
        &mut tp,
        PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: format!("{ID}@{latency}"),
            name: "Split".into(),
        },
        None,
    );
    let plugin = s.id;
    let t = tp.project.track_mut(host).unwrap();
    t.inserts.push(s);
    t.pan = -1.0;
    Rig { tp, host, plugin }
}

/// A track taking the splitter's output `bus`, balanced at `pan`.
fn taker(r: &mut Rig, bus: u16, pan: f32) -> TrackId {
    let t =
        r.tp.track(TrackKind::Aux, &format!("Out {bus}"), ChannelLayout::Stereo);
    let tr = r.tp.project.track_mut(t).unwrap();
    tr.input = InputRouting::Plugin {
        plugin: r.plugin,
        bus,
        channel: None,
    };
    tr.pan = pan;
    t
}

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    }
}

fn registry() -> PluginRegistry {
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    registry
}

/// The master's left and right, offline.
fn render(r: &Rig, block: usize, frames: usize) -> Vec<Vec<f32>> {
    render_on(r, block, frames, None)
}

/// [`render`] on a worker pool (`Some`: the graph runs in parallel).
fn render_on(
    r: &Rig,
    block: usize,
    frames: usize,
    pool: Option<std::sync::Arc<faderframe_realtime::WorkerPool>>,
) -> Vec<Vec<f32>> {
    let config = EngineConfig {
        parallel_min_ns: 0,
        ..config()
    };
    let mut o = OfflineRenderer::new(&r.tp.project, &r.tp.sources, config, block, 2).unwrap();
    o.processor.set_worker_pool(pool);
    *o.controller.plugins() = PluginHost::new(registry());
    o.controller.plugins().set_realtime(false);
    o.controller
        .sync(&r.tp.project, &r.tp.sources, Impact::Graph)
        .unwrap();
    o.play_from(0).unwrap();
    o.render(frames)
}

fn one_hit(out: &[f32], what: &str) -> usize {
    let h = hits(out, 1e-4);
    assert_eq!(h.len(), 1, "{what}: one sample, not several: {h:?}");
    h[0]
}

/// Where the left (the plugin's track) and the right (its outputs'
/// tracks) heard the impulse.
fn landed(r: &Rig, block: usize) -> (usize, usize) {
    let out = render(r, block, Q + 4096);
    (
        one_hit(&out[0], &format!("left, block {block}")),
        one_hit(&out[1], &format!("right, block {block}")),
    )
}

#[test]
fn a_plugins_outputs_meet_its_main_on_the_same_sample() {
    for latency in [0u32, 64, 333] {
        let mut r = rig(latency);
        taker(&mut r, 1, 1.0);
        for block in [1usize, 37, 128, 1024] {
            let (l, rr) = landed(&r, block);
            assert_eq!(l, Q + latency as usize, "latency {latency}, block {block}");
            assert_eq!(l, rr, "latency {latency}, block {block}");
        }
    }
}

#[test]
fn latency_after_the_plugin_on_either_side_is_compensated() {
    // On the plugin's track after it.
    let mut r = rig(100);
    taker(&mut r, 1, 1.0);
    delay(&mut r.tp, r.host, 37);
    let (l, rr) = landed(&r, 128);
    assert_eq!(l, Q + 137);
    assert_eq!(l, rr);
    // On the track taking the output.
    let mut r = rig(100);
    let t = taker(&mut r, 1, 1.0);
    delay(&mut r.tp, t, 53);
    let (l, rr) = landed(&r, 128);
    assert_eq!(l, Q + 153);
    assert_eq!(l, rr);
    // Both, and a second output taken.
    let mut r = rig(100);
    let t = taker(&mut r, 1, 1.0);
    delay(&mut r.tp, t, 53);
    let u = taker(&mut r, 2, 1.0);
    delay(&mut r.tp, u, 11);
    delay(&mut r.tp, r.host, 7);
    let out = render(&r, 128, Q + 4096);
    let l = one_hit(&out[0], "left");
    let rr = one_hit(&out[1], "right");
    assert_eq!(l, Q + 153);
    assert_eq!(l, rr);
}

#[test]
fn the_main_taken_by_a_track_meets_the_other_outputs() {
    // As "Create Output Tracks" leaves it: a track for every output, the
    // main too, the plugin's own track routed nowhere.
    let mut r = rig(64);
    r.tp.project.track_mut(r.host).unwrap().output = OutputRouting::None;
    taker(&mut r, 0, -1.0);
    taker(&mut r, 1, 1.0);
    taker(&mut r, 2, 1.0);
    let (l, rr) = landed(&r, 256);
    assert_eq!(l, Q + 64);
    assert_eq!(l, rr);
}

/// On a worker pool: every output's track waits for the plugin's block.
#[test]
fn in_parallel_the_outputs_meet_too() {
    use faderframe_realtime::{PoolConfig, WorkerPool};
    let mut r = rig(64);
    for bus in 1..BUSES as u16 {
        let t = taker(&mut r, bus, 1.0);
        delay(&mut r.tp, t, 3 * u32::from(bus));
    }
    // Other work beside it.
    for k in 0..6 {
        let t = r.tp.track(
            TrackKind::Audio,
            &format!("Beside {k}"),
            ChannelLayout::Stereo,
        );
        delay(&mut r.tp, t, 1 + k);
    }
    let pool = std::sync::Arc::new(WorkerPool::new(PoolConfig::new(4)));
    for block in [37usize, 256] {
        let out = render_on(&r, block, Q + 4096, Some(std::sync::Arc::clone(&pool)));
        let l = one_hit(&out[0], "left");
        let rr = one_hit(&out[1], "right");
        assert_eq!(l, Q + 70, "block {block}");
        assert_eq!(l, rr, "block {block}");
    }
}

/// Rendering ahead (tracks and buses, as a live session does): the
/// same — the plugin, its track and the tracks taking its outputs stay on
/// the audio thread, a bus after them too; another track beside them
/// renders ahead.
#[test]
fn with_render_ahead_the_outputs_meet_too() {
    use std::time::Duration;
    let mut r = rig(64);
    let t = taker(&mut r, 1, 1.0);
    delay(&mut r.tp, t, 53);
    // A bus after both, and a track with a device rendered ahead beside.
    let bus =
        r.tp.track(TrackKind::Bus, "Drum Bus", ChannelLayout::Stereo);
    r.tp.route(r.host, bus);
    r.tp.route(t, bus);
    let other = r.tp.track(TrackKind::Audio, "Other", ChannelLayout::Stereo);
    delay(&mut r.tp, other, 5);
    let block = 256;
    let mut o = OfflineRenderer::new(&r.tp.project, &r.tp.sources, config(), block, 2).unwrap();
    *o.controller.plugins() = PluginHost::new(registry());
    o.controller.set_render_ahead_buses(true);
    o.controller
        .set_render_ahead(Some(Duration::from_millis(150)), 2);
    o.controller
        .sync(&r.tp.project, &r.tp.sources, Impact::Graph)
        .unwrap();
    o.play_from(0).unwrap();
    let mut patience = common::Patience::new(Duration::from_secs(30));
    // By transport position (playing starts once the tiers are primed).
    let mut out = [vec![0.0f32; Q + 8192], vec![0.0f32; Q + 8192]];
    let mut played = 0;
    while played < Q + 8192 {
        patience.wait(&o.controller, block);
        let got: Vec<Vec<f32>> = {
            let bufs = o.step();
            (0..2)
                .map(|c| bufs.output_ref(c)[..block].to_vec())
                .collect()
        };
        let t = o.controller.transport_snapshot();
        if !t.playing {
            continue;
        }
        let start = (t.position - block as i64).max(0) as usize;
        for (c, ch) in out.iter_mut().enumerate() {
            for (i, s) in got[c].iter().enumerate() {
                if let Some(x) = ch.get_mut(start + i) {
                    *x = *s;
                }
            }
        }
        played = t.position.max(0) as usize;
    }
    assert_eq!(
        o.controller.ahead_misses(),
        0,
        "nothing rendered ahead was late"
    );
    let l = one_hit(&out[0], "left");
    let rr = one_hit(&out[1], "right");
    assert_eq!(l, Q + 117);
    assert_eq!(l, rr);
}

/// A mono track taking one channel of a stereo output gets that channel
/// alone (not the two averaged), on the same sample as the rest: a drum
/// instrument's microphones on tracks of their own however the plugin
/// grouped them (#52).
#[test]
fn a_track_taking_one_channel_gets_that_channel_alone() {
    // Left: 1.0 at frame 0, right: 0.5 at frame 10.
    let mut r = rig(64);
    let rate = r.tp.project.sample_rate;
    let mut left = vec![0.0; 64];
    left[0] = 1.0;
    let mut right = vec![0.0; 64];
    right[10] = 0.5;
    let source =
        r.tp.source(faderframe_audio_files::AudioData::from_channels(
            rate,
            vec![left, right],
        ));
    let host = r.host;
    // The rig's impulse (on both channels) goes.
    for id in std::mem::take(&mut r.tp.project.track_mut(host).unwrap().clips) {
        r.tp.project.clips.remove(&id);
    }
    r.tp.clip(host, source, MusicalTime::from_quarters_i(1), 64);
    let t = r.tp.project.track_mut(host).unwrap();
    t.pan = 0.0;
    // Only the takers are heard.
    t.output = OutputRouting::None;
    let taker = r.tp.track(TrackKind::Aux, "Mic", ChannelLayout::Mono);
    let mut heard = Vec::new();
    for channel in [0u16, 1] {
        let plugin = r.plugin;
        let tr = r.tp.project.track_mut(taker).unwrap();
        tr.input = InputRouting::Plugin {
            plugin,
            bus: 1,
            channel: Some(channel),
        };
        tr.pan = 1.0;
        let out = render(&r, 128, Q + 4096);
        assert!(
            out[0].iter().all(|s| s.abs() < 1e-6),
            "channel {channel}: panned right"
        );
        let at = one_hit(&out[1], &format!("channel {channel}"));
        heard.push((at, out[1][at]));
    }
    assert_eq!(heard[0].0, Q + 64, "the left, as the plugin gave it");
    assert_eq!(heard[1].0, Q + 64 + 10, "the right alone");
    assert!(
        (heard[1].1 / heard[0].1 - 0.5).abs() < 1e-5,
        "each at its own level: {heard:?}"
    );
}

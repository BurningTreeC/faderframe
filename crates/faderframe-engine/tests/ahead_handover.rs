#![allow(clippy::unwrap_used)]
//! Buses rendered ahead across a new sequence (a locate): the shallow
//! tier holds only a device callback and two small blocks of what plays,
//! so it renders the old sequence on from the deep tier's rings until the
//! deep tier has started the new one — however long that takes (here a
//! plugin keeps the deep tier from it for a while), nothing goes missing.

mod common;

use common::TestProject;
use faderframe_audio::OwnedBuffers;
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, PluginHost};
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{Impact, PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const SR: u32 = 48_000;
const BLOCK: usize = 256;

/// Blocks the stalling plugin still holds up (each for [`STALL`]).
static STALLS: AtomicU32 = AtomicU32::new(0);
const STALL: Duration = Duration::from_millis(50);

// --- a pass-through effect that can keep its thread busy --------------------------------

struct Factory;

struct Instance {
    descriptor: PluginDescriptor,
}

struct Stall;

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
                name: "Stall".into(),
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
        Ok(Box::new(Stall))
    }
}

/// One of the [`STALLS`] left, if any (a compare-and-swap loop:
/// `fetch_update` is deprecated on newer toolchains, `try_update` missing
/// on older ones).
fn take_stall() -> bool {
    let mut left = STALLS.load(Ordering::Acquire);
    while left > 0 {
        match STALLS.compare_exchange_weak(left, left - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(now) => left = now,
        }
    }
    false
}

impl PluginProcessor for Stall {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        if take_stall() {
            // The deep tier's thread, kept from rendering ahead.
            std::thread::sleep(STALL);
        }
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

// --- the rig ---------------------------------------------------------------------------

/// A steady tone into the stalling plugin, on to a bus with a utility, on
/// to the master: the source track and the bus are rendered ahead, the
/// source's strip in the shallow tier.
fn rig() -> (
    OfflineRenderer,
    faderframe_core::TrackId,
    faderframe_core::TrackId,
) {
    let mut tp = TestProject::new(SR);
    let source = tp.track(TrackKind::Audio, "Source", ChannelLayout::Stereo);
    let bus = tp.track(TrackKind::Bus, "Bus", ChannelLayout::Stereo);
    tp.route(source, bus);
    let seconds = 20;
    let tone = tp.dc(2, 0.25, SR as usize * seconds);
    tp.clip(source, tone, MusicalTime::ZERO, SR as i64 * seconds as i64);
    tp.project
        .track_mut(source)
        .unwrap()
        .inserts
        .push(PluginSlot {
            id: PluginInstanceId(9_800),
            plugin: PluginRef {
                format: faderframe_project::PluginFormat::Clap,
                id: "org.faderframe.test.stall".into(),
                name: "Stall".into(),
            },
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        });
    tp.project.track_mut(bus).unwrap().inserts.push(PluginSlot {
        id: PluginInstanceId(9_801),
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    *r.controller.plugins() = PluginHost::new(registry);
    r.controller.set_render_ahead_buses(true);
    r.controller
        .set_render_ahead(Some(Duration::from_millis(200)), 1);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    (r, source, bus)
}

/// `blocks` callbacks at the device's pace; how many of them, playing,
/// were silent.
fn play(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers, blocks: usize) -> usize {
    let mut silent = 0;
    for _ in 0..blocks {
        r.processor.process_device(bufs);
        let t = r.controller.transport_snapshot();
        if t.playing && bufs.output_ref(0).iter().all(|s| s.abs() < 0.05) {
            silent += 1;
        }
        std::thread::sleep(Duration::from_secs_f64(BLOCK as f64 / f64::from(SR)));
        r.controller.collect_garbage();
    }
    silent
}

/// A locate while the deep tier is kept from the new sequence for two of
/// its blocks (100 ms, far beyond the shallow tier's few milliseconds):
/// the bus plays the old sequence on without a gap, then the new one.
/// Tried again when the machine kept the anticipators from a deadline
/// elsewhere (a busy machine: misses before the locate).
#[test]
fn a_locate_with_buses_ahead_has_no_gap_while_the_deep_tier_starts() {
    for tries in 1..=3 {
        let (mut r, source, bus) = rig();
        let c = &r.controller;
        assert!(c.ahead_tracks().contains(&bus), "the bus is rendered ahead");
        assert!(c.ahead_strips().contains(&source), "its source's strip too");
        let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
        play(&mut r, &mut bufs, 20);
        r.controller.transport(TransportCommand::Play).unwrap();
        play(&mut r, &mut bufs, SR as usize / 2 / BLOCK);
        let before = r.controller.ahead_misses();
        // Kept from the new sequence's first blocks.
        STALLS.store(2, Ordering::Release);
        r.controller
            .transport(TransportCommand::Locate(SR as i64 * 5))
            .unwrap();
        let silent = play(&mut r, &mut bufs, SR as usize / BLOCK);
        STALLS.store(0, Ordering::Release);
        let misses = r.controller.ahead_misses();
        let position = r.controller.transport_snapshot().position;
        assert!(
            position > SR as i64 * 5,
            "played on from the locate: {position}"
        );
        if before > 0 {
            eprintln!("try {tries}: {before} blocks late before the locate (a busy machine)");
            continue;
        }
        assert_eq!(misses, 0, "blocks missing across the hand-over");
        assert_eq!(silent, 0, "silent callbacks across the hand-over");
        return;
    }
    panic!("late before the locate in every try: the anticipators do not keep up here");
}

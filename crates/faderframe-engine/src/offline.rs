//! Offline (faster-than-realtime) rendering through the exact same
//! processing path as live playback.

use crate::EngineError;
use crate::engine::{EngineConfig, EngineController, EngineProcessor, create};
use crate::snapshot::SourceMap;
use faderframe_audio::OwnedBuffers;
use faderframe_audio_files::{PAGE_FRAMES, Page};
use faderframe_project::{Impact, Project};
use faderframe_realtime::Reclaimer;
use faderframe_transport::TransportCommand;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How long a render waits for ARA plug-ins' analyses.
const ARA_SETTLE_SECONDS: u64 = 120;

/// The program's choice until the user makes another (its preferences
/// start with it): see [`set_plugin_warm_up`].
pub const DEFAULT_PLUGIN_WARM_UP_MS: u32 = 2_000;
/// The longest warm-up a render takes.
pub const MAX_PLUGIN_WARM_UP_MS: u32 = 30_000;

static PLUGIN_WARM_UP_MS: AtomicU32 = AtomicU32::new(0);

/// The real time third-party plug-ins get before an offline render's first
/// frame (see [`OfflineRenderer`]), for every render this process makes
/// from now on; 0: renders start at once (what a program that never sets
/// it gets). Up to [`MAX_PLUGIN_WARM_UP_MS`].
pub fn set_plugin_warm_up(ms: u32) {
    PLUGIN_WARM_UP_MS.store(ms.min(MAX_PLUGIN_WARM_UP_MS), Ordering::Relaxed);
}

/// The warm-up renders take now ([`set_plugin_warm_up`]), in milliseconds.
pub fn plugin_warm_up_ms() -> u32 {
    PLUGIN_WARM_UP_MS.load(Ordering::Relaxed)
}

/// Drives an engine without an audio device.
///
/// **The plug-ins' warm-up.** A render hosts plug-in instances of its own
/// and runs faster than real time. A plug-in that prepares what its state
/// names on threads of its own after the state has loaded (a neural amp
/// model, samples, an impulse response) plays silence or a stand-in until
/// it is ready — by then a render from the first frame is seconds into
/// the music (a self-frozen track's cache, an export, a bounce silent at
/// its start). Neither CLAP, VST3, LV2 nor Audio Units let a plug-in tell
/// its host it is ready, so a render hosting any third-party plug-in gives
/// them [`plugin_warm_up_ms`] of real time before its first frame, as a
/// device would: blocks of silence with the transport stopped, paced from
/// when the newest such instance was made (the time setting the render up
/// took counts), and their main-thread requests served in between (a
/// restart — a latency known only once a model is loaded — rebuilds the
/// graph, so the render compensates it). The number of blocks follows
/// from the time, the rate and the block alone: renders of the same
/// project stay alike (a plug-in's free-running state moves as far each
/// time). What the warm-up captured is forgotten ([`Self::with_capture`]).
/// Built-in devices load what they need before a render starts, so a
/// render of built-ins alone never waits.
pub struct OfflineRenderer {
    pub controller: EngineController,
    pub processor: EngineProcessor,
    buffers: OwnedBuffers,
    block: usize,
    /// Pages of streamed sources are loaded synchronously right before the
    /// blocks that need them (this thread is both reader and loader).
    reclaimer: Reclaimer<Page>,
    scratch: Vec<u8>,
    steps: u64,
    /// I/O errors while streaming (the affected frames render as silence).
    pub stream_errors: u64,
}

impl OfflineRenderer {
    /// Prepare an engine for `project` at `sample_rate`, delivering
    /// "device" callbacks of `block` frames (any size; the engine chunks
    /// internally at `config.max_block_size`).
    pub fn new(
        project: &Project,
        sources: &SourceMap,
        config: EngineConfig,
        block: usize,
        output_channels: usize,
    ) -> Result<Self, EngineError> {
        Self::with_capture(project, sources, config, block, output_channels, None)
    }

    /// [`Self::new`] capturing what `plan` asks for (its block is this
    /// renderer's: the taps hold one callback; see [`crate::capture`]).
    /// Read the taps after every [`Self::step`]: the controller's
    /// [`EngineController::capture`]. They start empty and the probes at
    /// zero, whatever the plug-ins' warm-up played.
    pub fn with_capture(
        project: &Project,
        sources: &SourceMap,
        config: EngineConfig,
        block: usize,
        output_channels: usize,
        plan: Option<crate::capture::CapturePlan>,
    ) -> Result<Self, EngineError> {
        let (mut controller, mut processor) = create(config);
        if let Some(mut plan) = plan {
            plan.block = plan.block.max(block.max(1));
            controller.set_capture(Some(plan));
        }
        controller.plugins().set_realtime(false);
        faderframe_audio::AudioCallback::prepare(
            &mut processor,
            &faderframe_audio::StreamInfo {
                backend: "offline",
                device: "offline".into(),
                sample_rate: config.sample_rate,
                buffer_size: block as u32,
                input_channels: 2,
                output_channels: output_channels as u16,
                input_latency: 0,
                output_latency: 0,
            },
        );
        controller.sync(project, sources, Impact::Graph)?;
        // ARA plug-ins still analysing their sources would render them
        // half-done.
        if !controller
            .plugins()
            .settle_ara(std::time::Duration::from_secs(ARA_SETTLE_SECONDS))
        {
            tracing::warn!("an ARA plug-in is still analysing: rendering anyway");
        }
        let mut r = Self {
            controller,
            processor,
            buffers: OwnedBuffers::new(2, output_channels, block.max(1)),
            block: block.max(1),
            reclaimer: Reclaimer::default(),
            scratch: Vec::new(),
            steps: 0,
            stream_errors: 0,
        };
        if let Some(made) = r.controller.plugins().third_party_since() {
            let time = Duration::from_millis(u64::from(plugin_warm_up_ms()));
            r.warm_up(project, sources, made, time)?;
        }
        Ok(r)
    }

    /// Give third-party plug-ins `time` of real time from `made` on before
    /// the first frame (see the type's docs).
    fn warm_up(
        &mut self,
        project: &Project,
        sources: &SourceMap,
        made: Instant,
        time: Duration,
    ) -> Result<(), EngineError> {
        if time.is_zero() {
            return Ok(());
        }
        let rate = f64::from(self.controller.config().sample_rate.max(1));
        let each = self.block as f64 / rate;
        let blocks = (time.as_secs_f64() / each).ceil() as u64;
        for i in 1..=blocks {
            self.buffers.set_frames(self.block);
            self.processor.process_device(&mut self.buffers);
            self.controller.collect_garbage();
            if self.controller.poll_plugins().restart {
                self.controller.sync(project, sources, Impact::Graph)?;
            }
            let due = made + Duration::from_secs_f64(each * i as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        // None of it is the render's.
        if let Some(capture) = self.controller.capture() {
            capture.reset();
        }
        Ok(())
    }

    /// Locate and start playback.
    pub fn play_from(&mut self, sample: i64) -> Result<(), EngineError> {
        self.controller
            .transport(TransportCommand::Locate(sample))?;
        self.controller.transport(TransportCommand::Play)
    }

    fn prefetch(&mut self) {
        let plan = self.controller.stream_plan();
        if plan.is_empty() {
            return;
        }
        // Apply pending transport commands so the position is current.
        let mut none = OwnedBuffers::new(0, 0, 0);
        none.set_frames(0);
        self.processor.process_device(&mut none);
        let pos = self.controller.transport_snapshot().position;
        let shared = self.controller.shared();
        let ahead = (self.block + PAGE_FRAMES) as i64;
        if plan
            .ensure(
                &[(pos, pos + ahead)],
                &shared.epoch,
                &mut self.reclaimer,
                &mut self.scratch,
            )
            .is_err()
        {
            self.stream_errors += 1;
        }
        if self.steps.is_multiple_of(64) {
            let keep = [(pos - PAGE_FRAMES as i64, pos + 2 * ahead)];
            plan.evict_outside(&keep, &shared.epoch, &mut self.reclaimer);
        }
        self.reclaimer.collect(&shared.epoch);
    }

    /// Process one callback and return the output buffers.
    pub fn step(&mut self) -> &OwnedBuffers {
        self.prefetch();
        self.buffers.set_frames(self.block);
        self.processor.process_device(&mut self.buffers);
        self.controller.collect_garbage();
        self.steps += 1;
        &self.buffers
    }

    /// Render `frames` frames into per-channel vectors.
    pub fn render(&mut self, frames: usize) -> Vec<Vec<f32>> {
        let channels = faderframe_audio::DeviceBuffers::output_channels(&self.buffers);
        let mut out = vec![Vec::with_capacity(frames); channels];
        while out[0].len() < frames {
            let need = frames - out[0].len();
            let bufs = self.step();
            for (c, ch) in out.iter_mut().enumerate() {
                let data = bufs.output_ref(c);
                ch.extend_from_slice(&data[..need.min(data.len())]);
            }
        }
        out
    }
}

/// Render `frames` frames of `project` from `start` (bounce helper).
pub fn render_project(
    project: &Project,
    sources: &SourceMap,
    config: EngineConfig,
    block: usize,
    start: i64,
    frames: usize,
) -> Result<Vec<Vec<f32>>, EngineError> {
    let mut r = OfflineRenderer::new(project, sources, config, block, 2)?;
    r.play_from(start)?;
    Ok(r.render(frames))
}

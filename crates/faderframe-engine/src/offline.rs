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

/// Drives an engine without an audio device.
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
        let (mut controller, mut processor) = create(config);
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
        Ok(Self {
            controller,
            processor,
            buffers: OwnedBuffers::new(2, output_channels, block.max(1)),
            block: block.max(1),
            reclaimer: Reclaimer::default(),
            scratch: Vec::new(),
            steps: 0,
            stream_errors: 0,
        })
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

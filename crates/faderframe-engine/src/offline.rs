//! Offline (faster-than-realtime) rendering through the exact same
//! processing path as live playback.

use crate::EngineError;
use crate::engine::{EngineConfig, EngineController, EngineProcessor, create};
use crate::snapshot::SourceMap;
use faderframe_audio::OwnedBuffers;
use faderframe_project::{Impact, Project};
use faderframe_transport::TransportCommand;

/// Drives an engine without an audio device.
pub struct OfflineRenderer {
    pub controller: EngineController,
    pub processor: EngineProcessor,
    buffers: OwnedBuffers,
    block: usize,
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
        })
    }

    /// Locate and start playback.
    pub fn play_from(&mut self, sample: i64) -> Result<(), EngineError> {
        self.controller
            .transport(TransportCommand::Locate(sample))?;
        self.controller.transport(TransportCommand::Play)
    }

    /// Process one callback and return the output buffers.
    pub fn step(&mut self) -> &OwnedBuffers {
        self.buffers.set_frames(self.block);
        self.processor.process_device(&mut self.buffers);
        self.controller.collect_garbage();
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

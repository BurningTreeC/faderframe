use crate::DeviceBuffers;

/// Heap-owned device buffers: used by offline rendering, the dummy backend,
/// benchmarks and tests. Allocate once, then reuse with [`Self::set_frames`].
#[derive(Debug, Clone)]
pub struct OwnedBuffers {
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    frames: usize,
}

impl OwnedBuffers {
    pub fn new(input_channels: usize, output_channels: usize, capacity: usize) -> Self {
        Self {
            inputs: vec![vec![0.0; capacity]; input_channels],
            outputs: vec![vec![0.0; capacity]; output_channels],
            frames: capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.inputs
            .first()
            .or(self.outputs.first())
            .map_or(0, |c| c.len())
    }

    /// Set the frame count of the next callback (`<= capacity`).
    pub fn set_frames(&mut self, frames: usize) {
        self.frames = frames.min(self.capacity());
    }

    pub fn input_mut(&mut self, channel: usize) -> &mut [f32] {
        let n = self.frames;
        &mut self.inputs[channel][..n]
    }

    pub fn output_ref(&self, channel: usize) -> &[f32] {
        &self.outputs[channel][..self.frames]
    }
}

impl DeviceBuffers for OwnedBuffers {
    fn frames(&self) -> usize {
        self.frames
    }

    fn input_channels(&self) -> usize {
        self.inputs.len()
    }

    fn output_channels(&self) -> usize {
        self.outputs.len()
    }

    fn input(&self, channel: usize) -> &[f32] {
        &self.inputs[channel][..self.frames]
    }

    fn output(&mut self, channel: usize) -> &mut [f32] {
        let n = self.frames;
        &mut self.outputs[channel][..n]
    }
}

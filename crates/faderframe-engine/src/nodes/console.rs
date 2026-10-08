//! The console's channel line amplifier on an audio or instrument track
//! (`faderframe_circuit::console::ConsoleStage`, one per channel), between
//! its inserts and its strip — so the pre-fader sends carry it too. The
//! drive comes from a parameter slot (no rebuild), ramped across a block.

use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_circuit::console::ConsoleStage;
use faderframe_realtime::ParamSlot;

pub struct ConsoleChannel {
    stages: Vec<ConsoleStage>,
    drive: ParamSlot,
    /// The drive the stages are at (linear).
    now: f64,
}

impl ConsoleChannel {
    /// Family `family`'s stage on `channels` channels at `rate`.
    pub fn new(family: usize, channels: usize, rate: f64, drive: ParamSlot) -> Self {
        Self {
            stages: (0..channels.max(1))
                .map(|_| ConsoleStage::new(family, 0.0, rate))
                .collect(),
            drive,
            now: 1.0,
        }
    }
}

impl Processor<EngineContext> for ConsoleChannel {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let n = io.frames;
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        let db = f64::from(cx.data.params.get(self.drive));
        let target = if db.is_finite() {
            10f64.powf(db.clamp(-24.0, 24.0) / 20.0)
        } else {
            1.0
        };
        let step = if n > 0 {
            (target - self.now) / n as f64
        } else {
            0.0
        };
        let channels = out.num_channels().min(input.num_channels());
        for (c, stage) in self.stages.iter_mut().enumerate().take(channels) {
            let mut drive = self.now;
            for (o, &x) in out.channel_mut(c)[..n]
                .iter_mut()
                .zip(&input.channel(c)[..n])
            {
                if step != 0.0 {
                    drive += step;
                    stage.set_drive(drive);
                }
                *o = stage.process(f64::from(x)) as f32;
            }
            stage.set_drive(target);
            stage.flush();
        }
        for c in channels..out.num_channels() {
            out.channel_mut(c)[..n].fill(0.0);
        }
        self.now = target;
    }

    fn reset(&mut self) {
        for s in &mut self.stages {
            s.reset();
        }
    }
}

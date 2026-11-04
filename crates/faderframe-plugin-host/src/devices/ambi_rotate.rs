//! Ambisonic Rotator: an ambisonic field (AmbiX, first to third order)
//! turned by yaw, pitch and roll — the scene turned, as IEM's
//! SceneRotator does, e.g. to put a recording's front where the picture's
//! is, or to follow a head tracker's angles from automation.
//!
//! The order comes from the track's channels (4, 9 or 16); other channel
//! counts pass unchanged. The rotation is a block-diagonal matrix per
//! order (`faderframe_core::ambisonics::Rotator`, exact for any turn,
//! made without allocating); a change of angle ramps every entry across
//! the block, so turning never clicks. Yaw is positive to the left, pitch
//! positive up, roll towards the right shoulder, applied yaw first: a
//! source straight ahead comes from (yaw, pitch) afterwards. No latency.

use super::param;
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_core::ambisonics::{AmbiOrder, MAX_CHANNELS, Orientation, Rotator, order_of};

pub mod id {
    pub const YAW: u32 = 0;
    pub const PITCH: u32 = 1;
    pub const ROLL: u32 = 2;
}

/// (Degrees; shown with their unit by [`format`].)
pub fn parameters() -> Vec<ParameterInfo> {
    vec![
        param(id::YAW, "Yaw", -180.0, 180.0, 0.0, ParameterUnit::None),
        param(id::PITCH, "Pitch", -90.0, 90.0, 0.0, ParameterUnit::None),
        param(id::ROLL, "Roll", -180.0, 180.0, 0.0, ParameterUnit::None),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::YAW if v.abs() < 0.05 => "0°".into(),
        id::YAW if v > 0.0 => format!("L {v:.1}°"),
        id::YAW => format!("R {:.1}°", -v),
        id::PITCH | id::ROLL => format!("{v:+.1}°"),
        _ => return None,
    })
}

/// The turn the parameters say.
pub fn orientation(params: &ParamValues) -> Orientation {
    let get = |pid: u32| params.get(pid as usize);
    Orientation::from_ypr(get(id::YAW), get(id::PITCH), get(id::ROLL))
}

pub struct RotatorProcessor {
    params: ParamValues,
    rotator: Box<Rotator>,
    /// The matrix as last applied and the angles it was made for.
    matrix: Box<[[f32; MAX_CHANNELS]; MAX_CHANNELS]>,
    target: Box<[[f32; MAX_CHANNELS]; MAX_CHANNELS]>,
    made_for: Option<(AmbiOrder, Orientation)>,
    /// The input, kept while the output is written (they may share no
    /// buffer, but each output channel needs every input channel).
    scratch: Vec<Vec<f32>>,
}

impl RotatorProcessor {
    pub fn new(params: ParamValues, max_block: usize) -> Self {
        Self {
            params,
            rotator: Box::new(Rotator::new()),
            matrix: Box::new([[0.0; MAX_CHANNELS]; MAX_CHANNELS]),
            target: Box::new([[0.0; MAX_CHANNELS]; MAX_CHANNELS]),
            made_for: None,
            scratch: vec![vec![0.0; max_block.max(1)]; MAX_CHANNELS],
        }
    }
}

impl PluginProcessor for RotatorProcessor {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let n = io.frames;
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return ProcessStatus::Continue;
        };
        let channels = input.num_channels().min(out.num_channels());
        let Some(order) = AmbiOrder::from_channels(channels) else {
            out.copy_from(input);
            return ProcessStatus::Continue;
        };
        let n = n.min(self.scratch[0].len());
        let turn = orientation(&self.params);
        let first = self.made_for.is_none_or(|(o, _)| o != order);
        if self.made_for != Some((order, turn)) {
            self.rotator.matrix(order, &turn, &mut self.target);
            if first {
                *self.matrix = *self.target;
            }
            self.made_for = Some((order, turn));
        }
        for (c, s) in self.scratch.iter_mut().enumerate().take(channels) {
            s[..n].copy_from_slice(&input.channel(c)[..n]);
        }
        let step = 1.0 / n.max(1) as f32;
        for i in 0..channels {
            let o = &mut out.channel_mut(i)[..n];
            o.fill(0.0);
            // Only the same order's channels mix (block-diagonal).
            let ord = order_of(i);
            for j in ord * ord..(ord + 1) * (ord + 1) {
                let (from, to) = (self.matrix[i][j], self.target[i][j]);
                if from == 0.0 && to == 0.0 {
                    continue;
                }
                let x = &self.scratch[j][..n];
                if from == to {
                    for (o, &v) in o.iter_mut().zip(x) {
                        *o += v * to;
                    }
                } else {
                    let d = (to - from) * step;
                    for (k, (o, &v)) in o.iter_mut().zip(x).enumerate() {
                        *o += v * (from + d * (k + 1) as f32);
                    }
                }
            }
        }
        for c in channels..out.num_channels() {
            out.channel_mut(c)[..n].fill(0.0);
        }
        *self.matrix = *self.target;
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.made_for = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_audio_graph::AudioBuffer;
    use faderframe_core::ChannelLayout;
    use faderframe_core::ambisonics::{direction, harmonics};

    fn process(p: &mut RotatorProcessor, ins: &[AudioBuffer], outs: &mut [AudioBuffer], n: usize) {
        let transport = faderframe_transport::TransportInfo::default();
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &crate::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        let mut io = NodeIo {
            frames: n,
            audio_in: ins,
            audio_out: outs,
            events_in: &[],
            events_out: &mut [],
        };
        p.process(&ctx, &mut io);
    }

    fn run(
        p: &mut RotatorProcessor,
        order: AmbiOrder,
        input: &[f32; MAX_CHANNELS],
        n: usize,
    ) -> Vec<Vec<f32>> {
        let layout = ChannelLayout::Ambisonic(order);
        let mut ins = [AudioBuffer::new(layout, n)];
        ins[0].set_len(n);
        for (c, v) in input.iter().enumerate().take(order.channels()) {
            ins[0].channel_mut(c).fill(*v);
        }
        let mut outs = [AudioBuffer::new(layout, n)];
        outs[0].set_len(n);
        process(p, &ins, &mut outs, n);
        (0..order.channels())
            .map(|c| outs[0].channel(c).to_vec())
            .collect()
    }

    /// A plane wave from ahead, turned by yaw and pitch, comes from
    /// there; the first block after a change glides, the next is exact.
    #[test]
    fn a_turned_field_comes_from_the_turned_direction() {
        let params = ParamValues::new(parameters());
        let mut p = RotatorProcessor::new(params.clone(), 256);
        let ahead = harmonics(direction(0.0, 0.0));
        for order in AmbiOrder::ALL {
            params.set_by_id(ParameterId(id::YAW), 0.0).unwrap();
            params.set_by_id(ParameterId(id::PITCH), 0.0).unwrap();
            let out = run(&mut p, order, &ahead, 64);
            for k in 0..order.channels() {
                assert!(
                    (out[k][63] - ahead[k]).abs() < 1e-5,
                    "{order:?} unturned {k}"
                );
            }
            params.set_by_id(ParameterId(id::YAW), 60.0).unwrap();
            params.set_by_id(ParameterId(id::PITCH), 20.0).unwrap();
            let gliding = run(&mut p, order, &ahead, 64);
            let out = run(&mut p, order, &ahead, 64);
            let want = harmonics(direction(60.0, 20.0));
            for k in 0..order.channels() {
                assert!((out[k][0] - want[k]).abs() < 1e-4, "{order:?} turned {k}");
                // On its way, never past where it goes.
                let (a, b) = (ahead[k].min(want[k]), ahead[k].max(want[k]));
                assert!(gliding[k].iter().all(|v| *v >= a - 1e-4 && *v <= b + 1e-4));
            }
        }
    }

    /// Other channel counts pass as they are.
    #[test]
    fn other_layouts_pass() {
        let params = ParamValues::new(parameters());
        params.set_by_id(ParameterId(id::YAW), 90.0).unwrap();
        let mut p = RotatorProcessor::new(params, 64);
        let mut ins = [AudioBuffer::new(ChannelLayout::Stereo, 32)];
        ins[0].set_len(32);
        ins[0].channel_mut(0).fill(0.3);
        ins[0].channel_mut(1).fill(-0.2);
        let mut outs = [AudioBuffer::new(ChannelLayout::Stereo, 32)];
        outs[0].set_len(32);
        process(&mut p, &ins, &mut outs, 32);
        assert!(outs[0].channel(0).iter().all(|v| *v == 0.3));
        assert!(outs[0].channel(1).iter().all(|v| *v == -0.2));
        assert_eq!(format(ParameterId(id::YAW), -30.0).unwrap(), "R 30.0°");
    }
}

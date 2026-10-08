//! Hardware Insert: outboard gear in a track's chain. The engine builds it
//! as a send (to the audio interface's outputs from Send Channel on) and a
//! return (from Return Channel on), declaring the round trip — measured by
//! a ping (`Round Trip`, in samples) — as the return's latency, so the
//! graph's compensation lines the return up with everything else. This
//! processor gets the dry signal (held back as long, its main input) and
//! the return (its second input) and mixes them: the return at its level
//! (and polarity), the dry signal under it as Mix says. Without the return
//! (renders, which have no audio interface) it passes the dry signal.

use super::{fixed, on_off, param, stepped};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;

pub mod id {
    /// The first interface output the send goes to (0: output 1).
    pub const SEND_CHANNEL: u32 = 0;
    /// The first interface input the return comes from.
    pub const RETURN_CHANNEL: u32 = 1;
    pub const SEND: u32 = 2;
    pub const RETURN: u32 = 3;
    /// Wet share (1: the return alone).
    pub const MIX: u32 = 4;
    pub const INVERT: u32 = 5;
    /// The round trip (samples at the session's rate), from a ping.
    pub const ROUND_TRIP: u32 = 6;
}

/// Interface channels offered (outputs and inputs).
pub const CHANNELS: u32 = 64;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        fixed(stepped(
            id::SEND_CHANNEL,
            "Send Channel",
            f64::from(CHANNELS - 1),
            2.0,
        )),
        fixed(stepped(
            id::RETURN_CHANNEL,
            "Return Channel",
            f64::from(CHANNELS - 1),
            2.0,
        )),
        param(id::SEND, "Send", -60.0, 12.0, 0.0, Decibels),
        param(id::RETURN, "Return", -60.0, 24.0, 0.0, Decibels),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        stepped(id::INVERT, "Invert Return", 1.0, 0.0),
        fixed(ParameterInfo {
            stepped: true,
            ..param(id::ROUND_TRIP, "Round Trip", 0.0, 192_000.0, 0.0, None)
        }),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::SEND_CHANNEL => format!("Out {}", v.round() as i64 + 1),
        id::RETURN_CHANNEL => format!("In {}", v.round() as i64 + 1),
        id::SEND | id::RETURN if v <= -59.95 => "−∞ dB".into(),
        id::INVERT => on_off(v),
        id::ROUND_TRIP if v < 0.5 => "not measured".into(),
        id::ROUND_TRIP => format!("{:.0} samples", v),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

fn gain(db: f64) -> f64 {
    if db <= -59.95 {
        0.0
    } else {
        10f64.powf(db / 20.0)
    }
}

/// What the engine needs to build the insert: send and return channels,
/// the send's gain and the round trip (samples).
pub fn routing(params: &ParamValues) -> (u16, u16, f32, u32) {
    (
        get(params, id::SEND_CHANNEL)
            .round()
            .clamp(0.0, f64::from(CHANNELS - 1)) as u16,
        get(params, id::RETURN_CHANNEL)
            .round()
            .clamp(0.0, f64::from(CHANNELS - 1)) as u16,
        gain(get(params, id::SEND)) as f32,
        get(params, id::ROUND_TRIP).round().max(0.0) as u32,
    )
}

pub struct HardwareInsertProcessor {
    params: ParamValues,
    /// The gains for the dry signal and the return at the end of the last
    /// block (ramped across the next, so moves do not click).
    current: (f64, f64),
}

impl HardwareInsertProcessor {
    pub fn new(params: ParamValues, _config: &ProcessConfig) -> Self {
        let current = Self::gains(&params);
        Self { params, current }
    }

    fn gains(params: &ParamValues) -> (f64, f64) {
        let mix = get(params, id::MIX).clamp(0.0, 1.0);
        let sign = if get(params, id::INVERT) >= 0.5 {
            -1.0
        } else {
            1.0
        };
        (1.0 - mix, mix * sign * gain(get(params, id::RETURN)))
    }
}

impl PluginProcessor for HardwareInsertProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames;
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        let Some(dry) = io.audio_in.first() else {
            return ProcessStatus::Continue;
        };
        out.copy_from(dry);
        // No return (a render): the dry signal as it is.
        let Some(ret) = io.audio_in.get(1).filter(|r| r.num_channels() > 0) else {
            return ProcessStatus::Continue;
        };
        let target = Self::gains(&self.params);
        let steps = frames.max(1) as f64;
        let (dd, dw) = (
            (target.0 - self.current.0) / steps,
            (target.1 - self.current.1) / steps,
        );
        let rc = ret.num_channels();
        for c in 0..out.num_channels() {
            let back = ret.channel(c.min(rc - 1));
            let (mut d, mut w) = self.current;
            for (o, r) in out.channel_mut(c).iter_mut().zip(back).take(frames) {
                d += dd;
                w += dw;
                *o = (f64::from(*o) * d + f64::from(*r) * w) as f32;
            }
        }
        self.current = target;
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.current = Self::gains(&self.params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_reads_the_parameters() {
        let p = ParamValues::new(parameters());
        for (i, v) in [
            (id::SEND_CHANNEL, 4.0),
            (id::RETURN_CHANNEL, 6.0),
            (id::SEND, -6.0),
            (id::ROUND_TRIP, 333.0),
        ] {
            p.set_by_id(ParameterId(i), v).unwrap();
        }
        let (s, r, g, rt) = routing(&p);
        assert_eq!((s, r, rt), (4, 6, 333));
        assert!((g - 0.501).abs() < 0.01);
        assert_eq!(format(ParameterId(id::SEND_CHANNEL), 4.0).unwrap(), "Out 5");
        assert_eq!(
            format(ParameterId(id::ROUND_TRIP), 0.0).unwrap(),
            "not measured"
        );
    }
}

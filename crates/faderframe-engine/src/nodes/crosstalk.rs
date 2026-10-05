use super::{AUTOMATION_STEP, MAX_CHANNELS, automation_at, ramp_step};
use crate::{context::EngineContext, slots::StripSlots};
use faderframe_audio_graph::{NodeIo, PrepareConfig, ProcessContext, Processor};
use faderframe_core::TrackId;

/// Generic resistive + capacitive channel coupling, not a measured console.
/// H(s) = 1e-5 + 10^(-65/20) s/(s + 2π·10000): about -85 dB at 1 kHz,
/// rising towards -65 dB at high frequencies. Bilinear discretisation,
/// independent state per channel/direction, no delay or recursive crossfeed.
/// Donor and recipient mute/solo remain effective (including automation).
/// Leakage is post-insert, pre-fader; the recipient strip controls the sum.
pub struct Crosstalk {
    donor: TrackId,
    slots: StripSlots,
    a: f64,
    b: f64,
    x: [f64; MAX_CHANNELS],
    y: [f64; MAX_CHANNELS],
    gate: f32,
}

impl Crosstalk {
    pub fn new(donor: TrackId, slots: StripSlots) -> Self {
        Self {
            donor,
            slots,
            a: 0.0,
            b: 0.0,
            x: [0.0; MAX_CHANNELS],
            y: [0.0; MAX_CHANNELS],
            gate: f32::NAN,
        }
    }

    fn filter(&mut self, channel: usize, input: f32) -> f32 {
        let x = f64::from(input);
        let y = self.a * (x - self.x[channel]) + self.b * self.y[channel];
        self.x[channel] = x;
        self.y[channel] = if y.abs() < 1e-24 { 0.0 } else { y };
        (1e-5 * x + 0.000_562_341_325_190_349_1 * y) as f32
    }
}

impl Processor<EngineContext> for Crosstalk {
    fn prepare(&mut self, config: &PrepareConfig) {
        let k = std::f64::consts::PI * 10_000.0 / config.sample_rate;
        self.a = 1.0 / (1.0 + k);
        self.b = (1.0 - k) / (1.0 + k);
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        if cx.data.discontinuity {
            self.reset();
        }
        let (Some(input), Some(output)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        let params = &cx.data.params;
        let static_mute = params.get(self.slots.mute) >= 0.5;
        let solo_mute = params.get(self.slots.solo_mute) >= 0.5;
        let auto = cx.data.timeline.automation(self.donor);
        let mute_lane = auto.and_then(|a| a.mute.as_ref());
        let vca_mute = auto.map_or(&[][..], |a| a.vca_mute.as_slice());
        let step = if mute_lane.is_some() || !vca_mute.is_empty() {
            AUTOMATION_STEP
        } else {
            io.frames.max(1)
        };
        let mut off = 0;
        while off < io.frames {
            let n = step.min(io.frames - off);
            let at = automation_at(cx, off + n);
            let mute = mute_lane
                .and_then(|l| l.value_at(at))
                .map_or(static_mute, |v| v >= 0.5);
            let vca_muted = vca_mute
                .iter()
                .any(|l| l.value_at(at).is_some_and(|v| v >= 0.5));
            let target = if mute || solo_mute || vca_muted {
                0.0
            } else {
                1.0
            };
            let from = if self.gate.is_nan() {
                target
            } else {
                self.gate
            };
            let increment = ramp_step(from, target, n);
            for c in 0..output.num_channels().min(MAX_CHANNELS) {
                let mut gate = from;
                for (out, &x) in output.channel_mut(c)[off..off + n]
                    .iter_mut()
                    .zip(&input.channel(c)[off..off + n])
                {
                    gate += increment;
                    *out = self.filter(c, x) * gate;
                }
            }
            self.gate = target;
            off += n;
        }
    }

    fn reset(&mut self) {
        self.x.fill(0.0);
        self.y.fill(0.0);
        self.gate = f32::NAN;
    }
}

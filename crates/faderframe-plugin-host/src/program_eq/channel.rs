//! One channel of the Program EQ: the passive network and the tube make-up
//! stage, the oversampling around them, and the latency padding.
//!
//! From PultEQFx by Simon Huber, used in FaderFrame under the MIT licence.

use super::network::{Controls, PassiveNetwork};
use super::tube::TubeStage;
use crate::dsp::oversample::Oversampler;

/// Latency the plugin reports, in samples at the host rate. It is the round
/// trip through the longest oversampling cascade; shorter settings are padded
/// out to match so the reported latency never changes while the plugin is
/// running, which the CLAP specification asks for and hosts are happier with.
pub const LATENCY: u32 = 74;

/// How long a new oversampling setting runs unheard after it is chosen, in
/// seconds, before it is faded in.
///
/// It starts from rest, and has to be where the old setting would have been
/// by the time it is heard. The slowest thing in the unit is the amplifier's
/// 3 Hz input coupling, whose start from rest dies away by a factor of e every
/// 53 ms; a fifth of a second leaves it at a few parts in ten thousand.
///
/// Its charge cannot simply be copied over from the old setting instead. It
/// carries a much reduced copy of the signal itself, and the two settings'
/// oversamplers put them a fraction of a millisecond apart in time, so the
/// copy lands out of step with the signal and makes matters worse.
pub const WARM_UP: f64 = 0.2;
/// How long the fade from the old oversampling setting to the new one takes,
/// in seconds.
pub const CROSSFADE: f64 = 0.01;

/// A fixed delay of at most `LATENCY` samples. It pads the processed path out
/// to whatever the oversampler is not using, and holds the dry signal back by
/// all of it while the power is off.
struct Padding {
    buf: [f32; LATENCY as usize],
    pos: usize,
    len: usize,
}

impl Padding {
    fn new() -> Self {
        Self {
            buf: [0.0; LATENCY as usize],
            pos: 0,
            len: 0,
        }
    }

    fn set_len(&mut self, len: usize) {
        self.len = len.min(self.buf.len());
        self.buf.fill(0.0);
        self.pos = 0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        if self.len == 0 {
            return x;
        }
        let out = self.buf[self.pos];
        self.buf[self.pos] = x;
        self.pos += 1;
        if self.pos == self.len {
            self.pos = 0;
        }
        out
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

/// The part of a channel the oversampling setting decides: the passive
/// network and the amplifier at the oversampled rate, the oversampler around
/// them, and the padding that makes its latency up to `LATENCY`.
struct Path {
    eq: PassiveNetwork,
    tube: TubeStage,
    oversampler: Oversampler,
    padding: Padding,
}

impl Path {
    fn new(sample_rate: f64, factor: usize) -> Self {
        let oversampler = Oversampler::new(factor);
        let internal = sample_rate * oversampler.factor() as f64;
        let mut padding = Padding::new();
        padding.set_len((LATENCY - oversampler.latency()) as usize);
        Self {
            eq: PassiveNetwork::new(internal),
            tube: TubeStage::new(internal),
            oversampler,
            padding,
        }
    }

    /// Allocation free: every stage already exists, this only changes how many
    /// of them are in use. The path starts over from rest.
    fn set_factor(&mut self, sample_rate: f64, factor: usize) {
        self.oversampler.set_factor(factor);
        self.padding
            .set_len((LATENCY - self.oversampler.latency()) as usize);
        let internal = sample_rate * self.oversampler.factor() as f64;
        self.eq.set_sample_rate(internal);
        self.tube.set_sample_rate(internal);
        self.reset();
    }

    fn factor(&self) -> usize {
        self.oversampler.factor()
    }

    #[inline]
    fn process(&mut self, sample: f32, eq_in: bool) -> f32 {
        let Self {
            eq,
            tube,
            oversampler,
            padding,
        } = self;
        let out = oversampler.process(sample as f64, &mut |x| {
            // The network runs whether or not it is switched in. Otherwise its
            // capacitors and inductor would sit holding whatever they held
            // when it was switched out, and switching it back in would play
            // that out as a thump before it caught up with the signal.
            let equalised = eq.process(x);
            tube.process(if eq_in { equalised } else { x })
        });
        padding.process(out as f32)
    }

    fn reset(&mut self) {
        self.eq.reset();
        self.tube.reset();
        self.oversampler.reset();
        self.padding.reset();
    }
}

/// One channel of the unit: the passive network, the make-up amplifier, and
/// the oversampling that surrounds both.
pub struct Channel {
    /// Two, so that a change of oversampling can bring the new setting up
    /// beside the old one rather than silencing the channel while it fills.
    /// Only the one being heard runs, except during such a change.
    paths: [Path; 2],
    /// Which of `paths` is being heard. The other is the incoming one during a
    /// change of oversampling, and idle otherwise.
    heard: usize,
    /// Samples left of a change of oversampling, or zero when none is under
    /// way: `WARM_UP` and then `CROSSFADE`, counting down.
    changing: usize,
    warm_up: usize,
    crossfade: usize,
    /// The panel as last set, for the incoming path to start from.
    controls: Controls,
    drive: f64,
    /// The input held back by the whole `LATENCY`, which is what comes out
    /// while the power is off.
    dry: Padding,
    sample_rate: f64,
}

impl Channel {
    pub fn new(sample_rate: f64, factor: usize) -> Self {
        let mut dry = Padding::new();
        dry.set_len(LATENCY as usize);
        // Both paths start out at the network's and the amplifier's own
        // defaults, which is what these two say.
        Self {
            paths: [
                Path::new(sample_rate, factor),
                Path::new(sample_rate, factor),
            ],
            heard: 0,
            changing: 0,
            warm_up: (WARM_UP * sample_rate).round() as usize,
            crossfade: ((CROSSFADE * sample_rate).round() as usize).max(1),
            controls: Controls::default(),
            drive: 0.0,
            dry,
            sample_rate,
        }
    }

    /// The knob positions and frequencies. Called at control rate.
    pub fn set_controls(&mut self, controls: Controls) {
        self.controls = controls;
        for path in self.running() {
            path.eq.set_controls(controls);
        }
    }

    /// How far the level into the amplifier is raised, in decibels.
    pub fn set_drive(&mut self, drive: f64) {
        self.drive = drive;
        for path in self.running() {
            path.tube.set_drive(drive);
        }
    }

    /// The paths that are running: the one being heard, and the incoming one
    /// while the oversampling is changing.
    fn running(&mut self) -> impl Iterator<Item = &mut Path> {
        let (heard, changing) = (self.heard, self.changing > 0);
        self.paths
            .iter_mut()
            .enumerate()
            .filter(move |(i, _)| *i == heard || changing)
            .map(|(_, path)| path)
    }

    /// Change the oversampling while playing, without a gap.
    ///
    /// Starting the path over at the new setting would silence it for the
    /// length of the latency while its filters refilled, and then play the
    /// network's settling from rest. So the new setting is brought up on the
    /// idle path instead, fed the same input unheard for `WARM_UP`, and then
    /// crossfaded in over `CROSSFADE`. Allocation free: both paths already
    /// exist.
    ///
    /// The dry delay does not depend on the setting, so changing it with the
    /// power off leaves what is coming out alone.
    pub fn set_oversampling(&mut self, factor: usize) {
        let factor = Oversampler::supported(factor);
        let incoming = 1 - self.heard;
        if self.changing > 0 && self.paths[incoming].factor() == factor {
            return;
        }
        if self.paths[self.heard].factor() == factor {
            // Back to what is already being heard, which is simply staying.
            self.changing = 0;
            return;
        }
        let path = &mut self.paths[incoming];
        path.set_factor(self.sample_rate, factor);
        path.eq.set_controls(self.controls);
        path.tube.set_drive(self.drive);
        self.changing = self.warm_up + self.crossfade;
    }

    /// `eq_in` mirrors the front panel EQ IN/OUT switch, which lifts the
    /// passive network out of circuit but leaves the amplifier in it.
    ///
    /// `powered` mirrors the OFF/ON switch, which takes the whole unit out.
    /// The input then comes back unprocessed but still `LATENCY` samples late:
    /// the host went on compensating for the latency it was told at
    /// initialisation, so passing the input straight through would put the
    /// track that far ahead of everything else, and throwing the switch would
    /// jump it back and forth in time.
    ///
    /// The processed path keeps running underneath while the power is off, so
    /// switching back on picks it up mid-stream. Starting it over instead
    /// would leave a gap the length of the latency while it refilled.
    #[inline]
    pub fn process(&mut self, sample: f32, eq_in: bool, powered: bool) -> f32 {
        let dry = self.dry.process(sample);
        let heard = self.paths[self.heard].process(sample, eq_in);
        let processed = if self.changing == 0 {
            heard
        } else {
            let incoming = self.paths[1 - self.heard].process(sample, eq_in);
            self.changing -= 1;
            let left = self.changing;
            let out = if left >= self.crossfade {
                heard
            } else {
                // The two carry the same signal a hair apart, so an equal gain
                // fade holds the level where an equal power one would lift it.
                let t = (self.crossfade - left) as f32 / self.crossfade as f32;
                heard + (incoming - heard) * t
            };
            if left == 0 {
                self.heard = 1 - self.heard;
            }
            out
        };
        if powered { processed } else { dry }
    }

    pub fn reset(&mut self) {
        // Everything starts from rest anyway, so a change under way is simply
        // finished.
        if self.changing > 0 {
            self.heard = 1 - self.heard;
            self.changing = 0;
        }
        self.paths.iter_mut().for_each(Path::reset);
        self.dry.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Switching the network out must not stop it, or switching it back in
    /// plays out whatever it was holding when it went. With the fix, a
    /// channel whose EQ was in and out is left with exactly the network state
    /// of one whose EQ never moved, because the network saw the same input
    /// either way.
    #[test]
    fn the_network_keeps_up_while_it_is_switched_out() {
        let controls = Controls {
            low_boost: 1.0,
            low_atten: 0.6,
            low_freq: 20.0,
            high_boost: 1.0,
            high_boost_freq: 3e3,
            bandwidth: 0.0,
            ..Controls::default()
        };
        for factor in [1, 2, 4, 8] {
            let mut steady = Channel::new(48_000.0, factor);
            let mut switched = Channel::new(48_000.0, factor);
            steady.set_controls(controls);
            switched.set_controls(controls);
            for n in 0..20_000 {
                let x = (0.3 * (n as f64 * 0.013).sin() + 0.1 * (n as f64 * 0.41).sin()) as f32;
                steady.process(x, true, true);
                switched.process(x, n % 5_000 < 2_500, true);
            }
            let (a, b) = (steady.heard, switched.heard);
            for _ in 0..64 {
                assert_eq!(
                    steady.paths[a].eq.process(0.0),
                    switched.paths[b].eq.process(0.0),
                    "at {factor}x the network fell behind while switched out"
                );
            }
        }
    }
}

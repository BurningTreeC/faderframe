//! One place on the Guitar Station's pedal line, for one channel: a pedal or
//! a wah, on a footswitch, at its own oversampling.
//!
//! Upstream runs one wah and one pedal inside the amplifier's oversampler
//! (`Chain::split`'s `FrontPedal`). Here every pedal is a stage of its own,
//! so the line can be as long as the player likes and each stage can run on
//! a thread of its own. What a stage does to the signal is upstream's,
//! operation for operation: a pedal is handed its calibrated guitar volts
//! (`Pedal::input_volts`), a wah the guitar's (`GUITAR_VOLTS`), and what
//! comes out goes on in the same units, so a stage after it sees a pedal's
//! output volts exactly as a cable would hand them on -- with one pedal and
//! the amplifier this is upstream's `pedal_hand_off`. Between stages the
//! signal is decimated back to the host rate, which at 1x (the default) is
//! nothing at all.
//!
//! A stage's circuit is built off the audio thread ([`StompCircuit::build`])
//! and handed in whole ([`PedalStage::swap`]); nothing here allocates.

use crate::circuits::wah;
use crate::dsp::bbd::Brigade;
use crate::dsp::netlist::{Circuit as Netlist, Fault};
use crate::dsp::oversample::Oversampler;
use crate::dsp::time::Simulation;
use crate::voice::{
    master_position, BucketBrigade, Delay, Gain, Pedal, DEFAULT_MASTER_REST, FADE_LEN,
    GUITAR_VOLTS, MODELLED_MAX_OVERSAMPLING, NOMINAL_DBFS, PEDAL_TONES,
};
use std::time::Instant;

/// The deepest oversampling a stage runs at, and the stage's host-sample
/// latency at a quality setting (whatever its pedal runs at, so changing the
/// pedal never changes the line's latency).
pub fn stage_latency(quality: usize) -> u32 {
    Oversampler::latency_of(quality.clamp(1, MODELLED_MAX_OVERSAMPLING))
}

/// What a place on the line holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Stomp {
    #[default]
    Empty,
    Pedal(Pedal),
    Wah(wah::Build),
}

impl Stomp {
    /// Everything the line offers, in the order of the stable parameter
    /// values: append, never reorder.
    pub const ALL: [Stomp; 21] = [
        Stomp::Empty,
        Stomp::Pedal(Pedal::Green808),
        Stomp::Pedal(Pedal::Green9),
        Stomp::Pedal(Pedal::GoldDrive),
        Stomp::Pedal(Pedal::BritDrive),
        Stomp::Pedal(Pedal::CleanBoost),
        Stomp::Pedal(Pedal::TrebleBoost),
        Stomp::Pedal(Pedal::Rodent),
        Stomp::Pedal(Pedal::YellowDist),
        Stomp::Pedal(Pedal::OrangeDist),
        Stomp::Pedal(Pedal::HeavyMetal),
        Stomp::Pedal(Pedal::MetalZone),
        Stomp::Pedal(Pedal::Modern33),
        Stomp::Pedal(Pedal::ModernPurple),
        Stomp::Pedal(Pedal::BigMuff),
        Stomp::Pedal(Pedal::RoundFuzz),
        Stomp::Pedal(Pedal::BassDriver),
        Stomp::Pedal(Pedal::OrangePhase),
        Stomp::Pedal(Pedal::BlueChorus),
        Stomp::Wah(wah::Build::CryBaby),
        Stomp::Wah(wah::Build::V847),
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    /// What the panel calls it (upstream's names).
    pub fn name(self) -> &'static str {
        match self {
            Stomp::Empty => "Empty",
            Stomp::Wah(wah::Build::CryBaby) => "Black Wah",
            Stomp::Wah(wah::Build::V847) => "Chrome Wah",
            Stomp::Pedal(p) => match p {
                Pedal::None => "None",
                Pedal::Green808 => "Green 808",
                Pedal::BigMuff => "Ram Fuzz",
                Pedal::Green9 => "Green 9",
                Pedal::Rodent => "Rodent",
                Pedal::RoundFuzz => "Round Fuzz",
                Pedal::YellowDist => "Yellow Dist",
                Pedal::HeavyMetal => "Heavy Metal",
                Pedal::MetalZone => "Metal Zone",
                Pedal::OrangeDist => "Orange Dist",
                Pedal::TrebleBoost => "Treble Boost",
                Pedal::GoldDrive => "Gold Drive",
                Pedal::BritDrive => "Brit Drive",
                Pedal::CleanBoost => "Clean Boost",
                Pedal::BassDriver => "Bass Driver",
                Pedal::OrangePhase => "Orange Phase",
                Pedal::BlueChorus => "Blue Chorus",
                Pedal::Modern33 => "Modern 33",
                Pedal::ModernPurple => "Modern Purple",
            },
        }
    }

    /// What kind of box it is, for the panel's grouping.
    pub fn family(self) -> &'static str {
        match self {
            Stomp::Empty => "",
            Stomp::Wah(_) => "Wah",
            Stomp::Pedal(p) => match p {
                Pedal::Green808
                | Pedal::Green9
                | Pedal::GoldDrive
                | Pedal::BritDrive
                | Pedal::CleanBoost
                | Pedal::TrebleBoost => "Drive & Boost",
                Pedal::Rodent
                | Pedal::YellowDist
                | Pedal::OrangeDist
                | Pedal::HeavyMetal
                | Pedal::MetalZone
                | Pedal::Modern33
                | Pedal::ModernPurple => "Distortion",
                Pedal::BigMuff | Pedal::RoundFuzz => "Fuzz",
                Pedal::BassDriver => "Preamp",
                Pedal::OrangePhase | Pedal::BlueChorus => "Modulation",
                Pedal::None => "",
            },
        }
    }

    /// Too large to oversample (upstream's `Pedal::is_expensive`): its stage
    /// runs at the host rate whatever the quality.
    pub fn is_expensive(self) -> bool {
        matches!(self, Stomp::Pedal(p) if p.is_expensive())
    }

    /// The pedal's drive and level knobs' names and whether it has them, its
    /// tone knobs' names; a wah's are its treadle, sense and mode.
    pub fn knobs(self) -> Knobs {
        match self {
            Stomp::Pedal(p) => {
                let (drive, level) = p.drive_and_level_labels();
                Knobs {
                    drive: p.has_drive().then_some(drive),
                    level: p.has_level().then_some(level),
                    tones: p.tone_labels(),
                    wah: false,
                }
            }
            Stomp::Wah(_) => Knobs {
                drive: None,
                level: None,
                tones: [None; PEDAL_TONES],
                wah: true,
            },
            Stomp::Empty => Knobs {
                drive: None,
                level: None,
                tones: [None; PEDAL_TONES],
                wah: false,
            },
        }
    }
}

/// Which of a slot's knobs a stomp has, and what its box calls them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Knobs {
    pub drive: Option<&'static str>,
    pub level: Option<&'static str>,
    pub tones: [Option<&'static str>; PEDAL_TONES],
    /// A wah: treadle, sense and the Auto switch.
    pub wah: bool,
}

/// A place on the line's controls, as plain values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StompSettings {
    pub stomp: Stomp,
    /// The footswitch: off is a true bypass.
    pub engaged: bool,
    pub drive: f64,
    pub level: f64,
    pub tone: [f64; PEDAL_TONES],
    /// A wah's pot, 0 heel to 1 toe, and its envelope follower.
    pub treadle: f64,
    pub auto: bool,
    pub sense: f64,
}

impl Default for StompSettings {
    fn default() -> Self {
        Self {
            stomp: Stomp::Empty,
            engaged: true,
            drive: 0.5,
            level: 0.5,
            tone: [0.5; PEDAL_TONES],
            treadle: 0.5,
            auto: false,
            sense: 0.5,
        }
    }
}

/// A bucket brigade resolved to one simulation's unknowns (upstream's
/// `BrigadeNodes`).
#[derive(Clone, Copy, Debug)]
struct BrigadeNodes {
    send: usize,
    ret: usize,
    control: usize,
    delay: fn(f64) -> f64,
}

impl BrigadeNodes {
    fn resolve(netlist: &Netlist, brigade: BucketBrigade) -> Option<Self> {
        Some(Self {
            send: netlist.unknown_named(brigade.send)?,
            ret: brigade.ret,
            control: netlist.unknown_named(brigade.control)?,
            delay: brigade.delay,
        })
    }
}

/// The longest delay a pedal's bucket brigade is given, and the highest rate
/// it can run at, which size its line (upstream's `BRIGADE_*`).
const BRIGADE_LONGEST: f64 = crate::circuits::blue_chorus::LONGEST;
const BRIGADE_MAX_RATE: f64 = 192_000.0 * MODELLED_MAX_OVERSAMPLING as f64;

/// One channel's circuit for a stomp, built and sized off the audio thread.
pub struct StompCircuit {
    stomp: Stomp,
    sim: Simulation,
    /// The stage's oversampling for it: 1 for an expensive pedal.
    factor: usize,
    /// Volts at its input per unit of the line's signal.
    into: f64,
    brigade: Option<(Brigade, BrigadeNodes)>,
}

impl StompCircuit {
    /// The circuit for `stomp` at host rate `rate` and the line's quality;
    /// `None` for an empty place.
    pub fn build(stomp: Stomp, rate: f64, quality: usize) -> Result<Option<Self>, Fault> {
        let nominal = 10f64.powf(NOMINAL_DBFS / 20.0);
        let factor = if stomp.is_expensive() {
            1
        } else {
            quality.clamp(1, MODELLED_MAX_OVERSAMPLING)
        };
        let inner = rate * factor as f64;
        let (netlist, into, brigade) = match stomp {
            Stomp::Empty => return Ok(None),
            Stomp::Wah(b) => (
                wah::build(b, 10_000.0, 470_000.0)?,
                GUITAR_VOLTS / nominal,
                None,
            ),
            Stomp::Pedal(p) => {
                let Some(slot) = p.slot() else {
                    return Ok(None);
                };
                (
                    Pedal::build(slot)?,
                    Pedal::input_volts(slot) / nominal,
                    p.as_circuit().and_then(Gain::bucket_brigade),
                )
            }
        };
        let brigade = brigade.and_then(|b| {
            BrigadeNodes::resolve(&netlist, b)
                .map(|nodes| (Brigade::new(BRIGADE_LONGEST, BRIGADE_MAX_RATE), nodes))
        });
        let sim = Simulation::new(netlist, inner);
        Ok(Some(Self {
            stomp,
            sim,
            factor,
            into,
            brigade,
        }))
    }

    pub fn stomp(&self) -> Stomp {
        self.stomp
    }
}

/// One channel's stage: whichever circuit is installed, the footswitch, the
/// stage's own oversampler and the padding that keeps its latency fixed.
pub struct PedalStage {
    circuit: Option<StompCircuit>,
    over: Oversampler,
    /// After the circuit: up to `latency`.
    pad: Delay,
    /// The bypass path: the input, `latency` late.
    thru: Delay,
    latency: u32,
    rate: f64,
    /// The footswitch, and whether the circuit is in the path (engaged and
    /// installed for the stomp the settings ask for).
    engaged: bool,
    active: bool,
    /// The wah's treadle target, follower and smoothed position.
    treadle: f64,
    auto: bool,
    sense: f64,
    position: f64,
    envelope: f64,
    brigade_return: f64,
    fade_remaining: usize,
    prev_output: f64,
    deadline: Option<Instant>,
}

impl PedalStage {
    pub fn new(rate: f64, quality: usize) -> Self {
        let latency = stage_latency(quality);
        Self {
            circuit: None,
            over: Oversampler::new(1),
            pad: Delay::new(latency as usize),
            thru: Delay::new(latency as usize),
            latency,
            rate: rate.max(1.0),
            engaged: true,
            active: false,
            treadle: 0.5,
            auto: false,
            sense: 0.5,
            position: 0.5,
            envelope: 0.0,
            brigade_return: 0.0,
            fade_remaining: 0,
            prev_output: 0.0,
            deadline: None,
        }
    }

    /// The host samples the stage delays by, active or bypassed.
    pub fn latency(&self) -> u32 {
        self.latency
    }

    /// The stomp whose circuit is installed.
    pub fn installed(&self) -> Stomp {
        self.circuit
            .as_ref()
            .map_or(Stomp::Empty, StompCircuit::stomp)
    }

    /// Put `circuit` in and hand the one that was there back in its place.
    /// The new circuit starts from its operating point, under the switch
    /// fade. Never allocates.
    pub fn swap(&mut self, circuit: &mut Option<StompCircuit>) {
        std::mem::swap(&mut self.circuit, circuit);
        if let Some(c) = &mut self.circuit {
            c.sim.reset_deferred();
            c.sim.set_realtime_deadline(self.deadline);
            if let Some((line, _)) = &mut c.brigade {
                line.reset();
            }
            self.over.set_factor(c.factor);
            self.pad
                .set_len((self.latency - self.over.latency().min(self.latency)) as usize);
        }
        self.over.reset();
        self.pad.reset();
        self.brigade_return = 0.0;
        self.envelope = 0.0;
        self.position = self.treadle;
        self.place_treadle();
        self.active = false;
        self.fade_remaining = FADE_LEN;
    }

    /// The place's knobs. Only reach the circuit when it is the one the
    /// settings name; until it has been handed in the place is bypassed.
    pub fn apply(&mut self, s: &StompSettings) {
        let installed = self.installed();
        let active = s.engaged && s.stomp != Stomp::Empty && installed == s.stomp;
        if active && !self.active {
            // Stepping on it: the circuit sat out of the path with whatever
            // charge it had, so it starts again from its operating point.
            if let Some(c) = &mut self.circuit {
                c.sim.reset_deferred();
                if let Some((line, _)) = &mut c.brigade {
                    line.reset();
                }
            }
            self.over.reset();
            self.pad.reset();
            // What the pad holds until the circuit's first output reaches
            // its end: the input, as the bypass had it, rather than silence
            // (the switch fade covers the difference).
            if self.over.latency() == 0 {
                self.pad.copy_runtime_state_from(&self.thru);
            }
            self.brigade_return = 0.0;
            self.envelope = 0.0;
            self.position = s.treadle.clamp(0.0, 1.0);
        }
        if active != self.active {
            self.fade_remaining = FADE_LEN;
        }
        self.active = active;
        self.engaged = s.engaged;
        self.treadle = s.treadle.clamp(0.0, 1.0);
        self.auto = s.auto;
        self.sense = s.sense.clamp(0.0, 1.0);
        let Some(c) = self.circuit.as_mut().filter(|c| c.stomp == s.stomp) else {
            return;
        };
        match s.stomp {
            Stomp::Pedal(p) => {
                let Some(slot) = p.slot() else { return };
                let controls = Pedal::controls(slot);
                if let Some(drive) = controls.drive {
                    c.sim.set_control(drive, s.drive.clamp(0.0, 1.0));
                }
                for (knob, &value) in controls.tones.iter().zip(s.tone.iter()) {
                    let Some(knob) = knob else { continue };
                    let value = value.clamp(0.0, 1.0);
                    c.sim.set_control(
                        knob.control,
                        if knob.inverted { 1.0 - value } else { value },
                    );
                }
                if let Some(level) = controls.level {
                    let rest = c.sim.resting_position(level).unwrap_or(DEFAULT_MASTER_REST);
                    c.sim.set_control(level, master_position(rest, s.level));
                }
            }
            // A wah's pot follows the treadle sample by sample (`process`).
            Stomp::Wah(_) | Stomp::Empty => {}
        }
    }

    /// The wah's pot where its treadle rests (upstream's `place_wah_treadle`).
    fn place_treadle(&mut self) {
        if let Some(c) = &mut self.circuit {
            if matches!(c.stomp, Stomp::Wah(_)) {
                let (top, bottom) = wah::treadle_halves(self.position);
                c.sim.set_realtime_value(wah::TREADLE_TOP, top);
                c.sim.set_realtime_value(wah::TREADLE_BOTTOM, bottom);
            }
        }
    }

    /// Where the wah's treadle is now, 0 heel to 1 toe, while one is in.
    pub fn wah_position(&self) -> Option<f64> {
        (self.active && matches!(self.installed(), Stomp::Wah(_))).then_some(self.position)
    }

    /// The treadle's target for one host sample of input `x` (upstream's
    /// `WahTap::target`).
    fn wah_target(&mut self, x: f64) -> f64 {
        let attack = 1.0 - (-1.0 / (0.005 * self.rate)).exp();
        let release = 1.0 - (-1.0 / (0.150 * self.rate)).exp();
        let level = x.abs();
        let k = if level > self.envelope {
            attack
        } else {
            release
        };
        self.envelope += k * (level - self.envelope);
        if self.auto {
            let full =
                0.25 * 10f64.powf(NOMINAL_DBFS / 20.0) * 10f64.powf(-2.0 * (self.sense - 0.5));
            self.treadle + (1.0 - self.treadle) * (self.envelope / full).min(1.0)
        } else {
            self.treadle
        }
    }

    /// One host sample through the stage.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let x = if x.is_finite() { x } else { 0.0 };
        let bypassed = self.thru.process(x);
        let mut y = bypassed;
        if self.active && self.circuit.is_some() {
            let wah = matches!(self.installed(), Stomp::Wah(_));
            let target = if wah { Some(self.wah_target(x)) } else { None };
            if let Some(c) = self.circuit.as_mut() {
                let inner = self.rate * c.factor as f64;
                let glide = 1.0 - (-1.0 / (0.020 * inner)).exp();
                let into = c.into;
                let position = &mut self.position;
                let returned = &mut self.brigade_return;
                let sim = &mut c.sim;
                let brigade = &mut c.brigade;
                let out = self.over.process(x * into, &mut |v| {
                    if let Some(target) = target {
                        let before = *position;
                        *position += glide * (target - *position);
                        if (*position - before).abs() > 1e-7 {
                            let (top, bottom) = wah::treadle_halves(*position);
                            sim.set_realtime_value(wah::TREADLE_TOP, top);
                            sim.set_realtime_value(wah::TREADLE_BOTTOM, bottom);
                        }
                    }
                    if let Some((_, nodes)) = brigade.as_ref() {
                        sim.set_aux_input(nodes.ret, *returned);
                    }
                    let out = sim.process(v);
                    if let Some((line, nodes)) = brigade.as_mut() {
                        let delay = (nodes.delay)(sim.voltage_at(nodes.control)) * inner;
                        *returned = line.process(sim.voltage_at(nodes.send), delay, inner);
                    }
                    out
                });
                y = self.pad.process(out / into);
            }
        }
        if self.fade_remaining > 0 {
            let t = self.fade_remaining as f64 / FADE_LEN as f64;
            self.fade_remaining -= 1;
            y = self.prev_output * t + y * (1.0 - t);
        }
        let y = if y.is_finite() { y } else { 0.0 };
        self.prev_output = y;
        y
    }

    pub fn reset(&mut self) {
        if let Some(c) = &mut self.circuit {
            c.sim.reset_deferred();
            if let Some((line, _)) = &mut c.brigade {
                line.reset();
            }
        }
        self.over.reset();
        self.pad.reset();
        self.thru.reset();
        self.brigade_return = 0.0;
        self.envelope = 0.0;
        self.position = self.treadle;
        self.place_treadle();
        self.fade_remaining = 0;
        self.prev_output = 0.0;
    }

    /// When the audio being solved is due (live); `None` renders.
    pub fn set_realtime_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
        if let Some(c) = &mut self.circuit {
            c.sim.set_realtime_deadline(deadline);
        }
    }

    pub fn deadline_aborts(&self) -> u64 {
        self.circuit.as_ref().map_or(0, |c| c.sim.deadline_aborts())
    }

    pub fn needs_operating_point(&self) -> bool {
        self.active
            && self
                .circuit
                .as_ref()
                .is_some_and(|c| c.sim.needs_operating_point())
    }

    pub fn find_operating_point(&mut self) -> bool {
        match &mut self.circuit {
            Some(c) if self.active && c.sim.needs_operating_point() => c.sim.find_operating_point(),
            _ => true,
        }
    }

    pub fn operating_point(&self) -> Option<&[f64]> {
        self.circuit.as_ref().map(|c| c.sim.operating_point())
    }

    pub fn share_operating_point_from(&mut self, op: &[f64]) {
        if let Some(c) = &mut self.circuit {
            if self.active
                && c.sim.needs_operating_point()
                && c.sim.operating_point().len() == op.len()
            {
                c.sim.apply_operating_point(op);
            }
        }
    }

    /// Wake an identically configured channel from this one's exact history.
    /// Copies into existing storage only.
    pub fn copy_runtime_state_from(&mut self, source: &Self) {
        if let (Some(dst), Some(src)) = (&mut self.circuit, &source.circuit) {
            if dst.stomp == src.stomp {
                dst.sim.copy_runtime_state_from(&src.sim);
                if let (Some((a, _)), Some((b, _))) = (&mut dst.brigade, &src.brigade) {
                    a.copy_runtime_state_from(b);
                }
            }
        }
        self.over.copy_runtime_state_from(&source.over);
        self.pad.copy_runtime_state_from(&source.pad);
        self.thru.copy_runtime_state_from(&source.thru);
        self.engaged = source.engaged;
        self.active = source.active;
        self.treadle = source.treadle;
        self.auto = source.auto;
        self.sense = source.sense;
        self.position = source.position;
        self.envelope = source.envelope;
        self.brigade_return = source.brigade_return;
        self.fade_remaining = source.fade_remaining;
        self.prev_output = source.prev_output;
    }
}

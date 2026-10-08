//! The amplifier stage of the Guitar Station, for one channel: a modelled
//! guitar or bass amplifier's preamplifier, its power stage working into the
//! loudspeaker (or a resistor), the cabinet and the two microphones, and the
//! DI taps.
//!
//! This is upstream's `Chain` (GainStageFx `voice.rs`) with its pedal slot,
//! wah, output iron, microphone preamplifiers, plugin tone stack, pipelining
//! and speculation taken out: the pedals are stages of their own
//! ([`crate::pedal`]), and the stages run on threads of their own instead of
//! a block being split across two. Every step that remains is upstream's,
//! operation for operation, and the comments that explain them are kept
//! where they were. The oversampling is fixed when a chain is made (a
//! different quality is a different chain), so the latency never moves.

use crate::acoustics::cabinet::Horn as CabinetHorn;
use crate::acoustics::filters::{Biquad as AcousticBiquad, OnePole};
use crate::acoustics::speaker::{self, LoadSlots, LoadValues, Mounting, SpeakerProfile};
use crate::acoustics::stage::AcousticStage;
use crate::circuits::{american_vt40, deluxe, markiic, power, twin};
use crate::dsp::bbd::Bbd;
use crate::dsp::netlist::Fault;
use crate::dsp::oversample::Oversampler;
use crate::dsp::spring::Tank;
use crate::dsp::time::Simulation;
use crate::dsp::tremolo::Tremolo;
use crate::voice::{
    build_power, build_voice, effective_oversampling, knots_on, master_lift, master_position,
    peak_gain, voice_at, voice_index, AcousticSettings, Amplifier, Cabinet, CabinetChoice, Delay,
    Diode, DrySource, Gain, Level, Pedal, PowerAmp, PowerModel, Throw, CALIBRATION,
    CHANNEL_VOLUME_CALIBRATION_REFERENCE, DEFAULT_MASTER_REST, DIRECT_OUT_DB, EXTRA_POWERS,
    EXTRA_POWER_SPECS, FADE_LEN, LOAD, MASTER_MIDDLE, NOMINAL_DBFS, OVERRIDE_MASTER_REST, POINTS,
    POWER_TRIM_DB, SOURCE, VOICES,
};
use std::time::Instant;

const MAX_OVERSAMPLING: usize = 8;

/// The amplifiers the Guitar Station offers, in the order of their stable
/// parameter values: append, never reorder.
pub const AMPS: [Gain; 20] = [
    Gain::Plexi,
    Gain::Brit45,
    Gain::PlexiBass,
    Gain::Brit800,
    Gain::Brit2205,
    Gain::AC30,
    Gain::DR103,
    Gain::Brum100,
    Gain::Twin,
    Gain::Deluxe,
    Gain::DeluxeNormal,
    Gain::Jazz120,
    Gain::Boogie,
    Gain::Recto,
    Gain::Peavey,
    Gain::OregonT,
    Gain::AmericanVt40,
    Gain::AmericanSvt,
    Gain::AmericanV4b,
    Gain::American800RB,
];

/// An amplifier's name on the panel (upstream's).
pub fn amp_name(amp: Gain) -> &'static str {
    match amp {
        Gain::Plexi => "Brit Plexi",
        Gain::Brit45 => "Brit 45",
        Gain::PlexiBass => "Brit Plexi Bass",
        Gain::Brit800 => "Brit 800",
        Gain::Brit2205 => "Brit 2205",
        Gain::AC30 => "Brit AC30",
        Gain::DR103 => "Brit DR103",
        Gain::Brum100 => "Brum 100",
        Gain::Twin => "American Twin",
        Gain::Deluxe => "American Deluxe",
        Gain::DeluxeNormal => "American Deluxe Normal",
        Gain::Jazz120 => "Jazz 120",
        Gain::Boogie => "Cali IIC+",
        Gain::Recto => "Cali Rectifier",
        Gain::Peavey => "American 5150",
        Gain::OregonT => "Oregon T",
        Gain::AmericanVt40 => "American VT-40",
        Gain::AmericanSvt => "American SVT",
        Gain::AmericanV4b => "American V-4B",
        Gain::American800RB => "American 800RB",
        _ => "",
    }
}

/// A power selection's name on the panel (upstream's).
pub fn power_name(p: PowerAmp) -> &'static str {
    match p {
        PowerAmp::Matched => "Matched",
        PowerAmp::Bypass => "Bypass",
        PowerAmp::Cali6L6 => "Cali 6L6",
        PowerAmp::American6L6Clean => "American 6L6 Clean",
        PowerAmp::American6L6HighGain => "American 6L6 High-Gain",
        PowerAmp::BritEL34 => "Brit EL34",
        PowerAmp::BritPlexiEL34 => "Brit Plexi EL34",
        PowerAmp::AC30EL84 => "AC30 EL84",
        PowerAmp::DR103EL34 => "DR103 EL34",
        PowerAmp::Recto6L6 => "Recto 6L6",
        PowerAmp::Recto6L6Tube => "Recto 6L6 Tube",
        PowerAmp::AmericanDeluxe6V6 => "American Deluxe 6V6",
        PowerAmp::Brit2205EL34 => "Brit 2205 EL34",
        PowerAmp::BritPlexiBassEL34 => "Brit Plexi Bass EL34",
        PowerAmp::BrumEL34 => "Brum EL34",
        PowerAmp::Oregon6550 => "Oregon 6550",
        PowerAmp::Svt6550 => "American 6550",
        PowerAmp::AmericanSS800 => "American SS 800",
        PowerAmp::Brit45KT66 => "Brit 45 KT66",
        PowerAmp::American7027A => "American 7027A",
        PowerAmp::AmericanV4b7027A => "American V-4B 7027A",
    }
}

/// The host-sample latency of the amplifier stage at a quality setting.
pub fn amp_latency(quality: usize) -> u32 {
    crate::pedal::stage_latency(quality)
}

/// The amplifier stage's controls, as plain values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub amp: Gain,
    pub power_amp: PowerAmp,
    pub acoustic: AcousticSettings,
    /// The old baked cabinet filter, on the legacy path.
    pub legacy_cabinet: Cabinet,
    pub drive: f64,
    pub master: f64,
    pub presence: f64,
    pub graphic: [f64; 5],
    pub bass: f64,
    pub mid: f64,
    pub treble: f64,
    /// A circuit's own fourth control (the 800RB's high mid).
    pub sweep: f64,
    /// The second input jack (AB763, JC-120, 800RB's pad).
    pub low_input: bool,
    /// The circuit's bright switch, where it has one.
    pub bright: bool,
    pub low_switch: Throw,
    pub mid_switch: Throw,
    pub reverb: f64,
    pub speed: f64,
    pub intensity: f64,
    pub chorus: f64,
    /// A fraction of the amplifier's mains (a variac).
    pub mains: f64,
    pub dry_source: DrySource,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            amp: Gain::Brit800,
            power_amp: PowerAmp::Matched,
            acoustic: AcousticSettings::default(),
            legacy_cabinet: Cabinet::Off,
            drive: 0.5,
            master: MASTER_MIDDLE,
            presence: 0.5,
            graphic: [0.5; 5],
            bass: 0.5,
            mid: 0.5,
            treble: 0.5,
            sweep: 0.5,
            low_input: false,
            bright: true,
            low_switch: Throw::Centre,
            mid_switch: Throw::Centre,
            reverb: 0.0,
            speed: 0.4,
            intensity: 0.0,
            chorus: 0.0,
            mains: 1.0,
            dry_source: DrySource::Input,
        }
    }
}

/// One host sample out of the stage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub left: f64,
    /// The right microphone's side while one chain carries a mono source on
    /// a stereo bus, the Jazz Chorus's delayed speaker; else `left`.
    pub right: f64,
    /// The DI: the chain's input, the guitar's or the preamplifier's, in step.
    pub dry: f64,
    /// Each microphone on its own (A, B), before the blend and the pan, at
    /// the output's level; without microphones (a direct or legacy
    /// cabinet) both are the output.
    pub mics: [f64; 2],
}

/// A speaker-loaded simulation and where to read its cone.
struct Loaded {
    sim: Simulation,
    slots: LoadSlots,
    motional: usize,
    /// The speaker's terminals: what a horn's crossover is fed from.
    terminal: usize,
}

pub struct Chain {
    /// Each offered amplifier's preamplifier, at its catalogue voice index.
    gains: Vec<Option<Simulation>>,
    /// Each guitar power circuit at its own amplifier's catalogue index, and
    /// the stages that belong to no voice after them.
    powers: Vec<Option<Simulation>>,
    power: usize,
    power_selection: PowerAmp,
    /// Each output circuit again, driving a loudspeaker instead of a resistor,
    /// indexed by `PowerModel::slot` (the 73P's is never built here).
    loaded: Vec<Option<Loaded>>,
    /// A loudspeaker driven straight from the preamplifier, for chains with no
    /// power stage.
    driven: Box<Loaded>,
    acoustic: Box<AcousticStage>,
    acoustic_settings: AcousticSettings,
    radiating: bool,
    pressure_scale: f64,
    motional_previous: f64,
    mains: f64,
    /// The old baked cabinet filters (Combo, Stack) with their trims.
    cabinets: Vec<(Simulation, f64)>,
    cabinet: Option<usize>,
    gain: usize,
    voice: Gain,
    over: Oversampler,
    /// The stage's latency: the oversampler's, fixed with the chain.
    latency: u32,
    pad: Delay,
    /// The DI from the chain's input, and from the guitar's input lanes.
    dry: Delay,
    raw: Delay,
    dry_source: DrySource,
    /// The preamplifier's tap: its decimator, padding, level and value.
    tap_over: Oversampler,
    tap_pad: Delay,
    tap_value: f64,
    tap_scale: f64,
    tap_scale_target: f64,
    direct_outs: Vec<Option<(usize, &'static [f64; POINTS])>>,
    horn: Option<CabinetHorn>,
    horn_level: f64,
    horn_low: OnePole,
    horn_high: AcousticBiquad,
    horn_over: Oversampler,
    horn_pad: Delay,
    twin_tank_send: usize,
    deluxe_tank_send: usize,
    vt40_tank_send: usize,
    twin_tank_drive_previous: f64,
    tank: Tank,
    tremolo: Tremolo,
    reverb: f64,
    speed: f64,
    intensity: f64,
    bbd: Bbd,
    chorus: f64,
    has_chorus: bool,
    rate: f64,
    drive: f64,
    master: f64,
    presence: f64,
    master_lift: f64,
    graphic: Simulation,
    into: f64,
    out_of: f64,
    out_of_target: f64,
    fade_remaining: usize,
    prev_output: f64,
    prev_output_right: f64,
    prev_mics: [f64; 2],
    deadline: Option<Instant>,
}

fn built<T>(r: Result<T, Fault>, what: &str) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e:?}"))
}

impl Chain {
    /// Builds every offered amplifier and power stage (so switching while
    /// playing allocates nothing) at host rate `rate` and the quality's
    /// oversampling. Off the audio thread.
    pub fn new(rate: f64, quality: usize) -> Result<Self, String> {
        let rate = rate.max(1.0);
        let factor =
            effective_oversampling(AMPS[0], Pedal::None, PowerAmp::Matched, quality.max(1))
                .clamp(1, MAX_OVERSAMPLING);
        let inner = rate * factor as f64;
        let offered = |i: usize| AMPS.contains(&voice_at(i).0);
        let mut gains = Vec::with_capacity(VOICES);
        let mut powers = Vec::with_capacity(VOICES + EXTRA_POWERS);
        for i in 0..VOICES {
            let (gain, diode, amplifier) = voice_at(i);
            if !offered(i) {
                gains.push(None);
                powers.push(None);
                continue;
            }
            gains.push(Some(Simulation::new(
                built(build_voice(gain, diode, amplifier), "amplifier")?,
                inner,
            )));
            powers.push(match build_power(gain) {
                Some(net) => {
                    let mut sim = Simulation::new(built(net, "power stage")?, inner);
                    // Upstream's measured Twin solver settings.
                    if gain == Gain::Twin {
                        sim.set_backtracks(4);
                        sim.set_late_continuation(true);
                    }
                    Some(sim)
                }
                None => None,
            });
        }
        for spec in EXTRA_POWER_SPECS {
            powers.push(Some(Simulation::new(
                built(power::build(spec, 10_000.0), "power stage")?,
                inner,
            )));
        }
        let mut loaded = Vec::with_capacity(PowerModel::ALL.len());
        for model in PowerModel::ALL {
            if model == PowerModel::British73Out {
                loaded.push(None);
                continue;
            }
            let values = LoadValues::new(
                &SpeakerProfile::BRIT_V30,
                &Mounting::BAFFLE,
                model.speaker_scale(),
            );
            let (circuit, slots) = built(model.build_loaded(&values), "loaded power stage")?;
            let motional = circuit
                .unknown_named(speaker::MOTIONAL)
                .ok_or("the driver has no motional node")?;
            let terminal = circuit
                .unknown_named(model.speaker_terminal())
                .ok_or("the driver has no terminals")?;
            let mut sim = Simulation::new(circuit, inner);
            if model == PowerModel::American6L6Clean {
                sim.set_backtracks(4);
                sim.set_late_continuation(true);
            }
            loaded.push(Some(Loaded {
                sim,
                slots,
                motional,
                terminal,
            }));
        }
        let initial = LoadValues::new(&SpeakerProfile::BRIT_V30, &Mounting::BAFFLE, 1.0);
        let (driven_circuit, driven_slots) =
            built(speaker::voltage_driven(&initial), "driven speaker")?;
        let driven_motional = driven_circuit.output;
        let driven_terminal = driven_circuit
            .unknown_named("spk")
            .ok_or("the driven speaker has no terminals")?;
        let twin_tank_send = built(twin::build(10_000.0, 1_000_000.0), "Twin")?
            .unknown_named(twin::SEND)
            .ok_or("the Twin has no reverb send")?;
        let deluxe_tank_send = built(deluxe::build(10_000.0, 2_000_000.0), "Deluxe")?
            .unknown_named(deluxe::SEND)
            .ok_or("the Deluxe has no reverb send")?;
        let vt40_tank_send = built(american_vt40::build(10_000.0, 1_000_000.0), "VT-40")?
            .unknown_named(american_vt40::SEND)
            .ok_or("the VT-40 has no tank coil")?;
        let mut direct_outs = Vec::with_capacity(VOICES);
        for i in 0..VOICES {
            let (gain, diode, amplifier) = voice_at(i);
            let out = match gain.direct_out().filter(|_| offered(i)) {
                Some(node) => {
                    let index = built(build_voice(gain, diode, amplifier), "amplifier")?
                        .unknown_named(node)
                        .ok_or("a direct out names no node")?;
                    DIRECT_OUT_DB
                        .iter()
                        .find(|(measured, _)| *measured == gain)
                        .map(|(_, table)| (index, table))
                }
                None => None,
            };
            direct_outs.push(out);
        }
        let mut cabinets = Vec::new();
        for c in [Cabinet::Combo, Cabinet::Stack] {
            let netlist = match c.build() {
                Some(n) => built(n, "cabinet")?,
                None => continue,
            };
            let controls = vec![0.5; netlist.controls];
            let trim = 1.0 / peak_gain(&netlist, &controls);
            let mut sim = Simulation::new(netlist, rate);
            for (i, p) in controls.iter().enumerate() {
                sim.set_control(i, *p);
            }
            cabinets.push((sim, trim));
        }
        let mut over = Oversampler::new(factor);
        over.set_factor(factor);
        let latency = over.latency();
        let mut chain = Self {
            gains,
            powers,
            power: 0,
            power_selection: PowerAmp::Matched,
            loaded,
            driven: Box::new(Loaded {
                sim: Simulation::new(driven_circuit, inner),
                slots: driven_slots,
                motional: driven_motional,
                terminal: driven_terminal,
            }),
            acoustic: Box::new(AcousticStage::new(rate)),
            acoustic_settings: AcousticSettings::default(),
            radiating: false,
            pressure_scale: 1.0 / initial.bl,
            motional_previous: 0.0,
            mains: 1.0,
            cabinets,
            cabinet: None,
            gain: usize::MAX,
            voice: AMPS[0],
            over,
            latency,
            pad: Delay::new(0),
            dry: Delay::new(latency as usize),
            raw: Delay::new(latency as usize),
            dry_source: DrySource::Input,
            tap_over: Oversampler::new(factor),
            tap_pad: Delay::new(0),
            tap_value: 0.0,
            tap_scale: 1.0,
            tap_scale_target: 1.0,
            direct_outs,
            horn: None,
            horn_level: 0.0,
            horn_low: OnePole::open(),
            horn_high: AcousticBiquad::IDENTITY,
            horn_over: Oversampler::new(factor),
            horn_pad: Delay::new(0),
            twin_tank_send,
            deluxe_tank_send,
            vt40_tank_send,
            twin_tank_drive_previous: 0.0,
            tank: Tank::accutronics(rate),
            tremolo: Tremolo::new(rate),
            reverb: 0.0,
            speed: 0.5,
            intensity: 0.0,
            bbd: Bbd::new(rate),
            chorus: 0.0,
            has_chorus: false,
            rate,
            drive: 0.5,
            master: MASTER_MIDDLE,
            presence: 0.5,
            master_lift: 1.0,
            graphic: Simulation::new(
                built(markiic::graphic(SOURCE, LOAD), "graphic equaliser")?,
                inner,
            ),
            into: 1.0,
            out_of: 1.0,
            out_of_target: 1.0,
            fade_remaining: 0,
            prev_output: 0.0,
            prev_output_right: 0.0,
            prev_mics: [0.0; 2],
            deadline: None,
        };
        chain.tap_over.set_factor(factor);
        chain.horn_over.set_factor(factor);
        chain.set_voice(AMPS[0]);
        chain.apply(&Settings::default());
        chain.out_of = chain.out_of_target;
        chain.fade_remaining = 0;
        Ok(chain)
    }

    pub fn latency(&self) -> u32 {
        self.latency
    }

    fn gain_sim(&mut self) -> &mut Simulation {
        self.gains[self.gain]
            .as_mut()
            .expect("only offered amplifiers are selected")
    }

    /// Which amplifier. Switching resets the one switched to, and its power
    /// stage, under the switch fade (upstream's `set_voice`).
    pub fn set_voice(&mut self, gain: Gain) {
        let gain = if AMPS.contains(&gain) { gain } else { AMPS[0] };
        let index = voice_index(gain, Diode::Silicon, Amplifier::Valve);
        self.voice = gain;
        self.has_chorus = gain.has_chorus();
        if !self.has_chorus {
            self.chorus = 0.0;
        }
        if index != self.gain {
            self.gain = index;
            if gain.has_reverb() {
                self.sync_twin_effect_controls();
            }
            self.twin_tank_drive_previous = 0.0;
            self.tank.reset();
            self.tremolo.reset();
            self.bbd.reset();
            let deadline = self.deadline;
            let sim = self.gain_sim();
            sim.reset_deferred();
            sim.set_realtime_deadline(deadline);
            self.update_power(true);
            self.graphic.reset_deferred();
            self.set_drive(self.drive);
            self.out_of = self.out_of_target;
            self.fade_remaining = FADE_LEN;
        }
    }

    fn update_power(&mut self, reset: bool) {
        let next = self
            .power_selection
            .resolved(self.voice)
            .map(PowerModel::index)
            .unwrap_or(0);
        if next != self.power || reset {
            self.power = next;
            if let Some(sim) = self.powers.get_mut(next).and_then(Option::as_mut) {
                sim.reset_deferred();
            }
            match self.power_selection.resolved(self.voice) {
                Some(model) => {
                    if let Some(l) = self.loaded[model.slot()].as_mut() {
                        l.sim.reset_deferred();
                    }
                }
                None => self.driven.sim.reset_deferred(),
            }
            self.motional_previous = 0.0;
            self.fade_remaining = FADE_LEN;
        }
    }

    fn resistive_power(&mut self) -> Option<&mut Simulation> {
        self.powers.get_mut(self.power).and_then(Option::as_mut)
    }

    /// The power simulation actually in the path: the speaker-loaded one when the
    /// physical path is in use, the resistor-loaded one otherwise.
    fn active_power(&self) -> Option<&Simulation> {
        if self.radiating {
            self.resolved_power_amp()
                .and_then(|model| self.loaded[model.slot()].as_ref())
                .map(|l| &l.sim)
        } else {
            self.powers.get(self.power).and_then(Option::as_ref)
        }
    }

    fn active_power_mut(&mut self) -> Option<&mut Simulation> {
        if self.radiating {
            match self.power_selection.resolved(self.voice) {
                Some(model) => self.loaded[model.slot()].as_mut().map(|l| &mut l.sim),
                None => None,
            }
        } else {
            self.resistive_power()
        }
    }

    fn active_driven(&self) -> bool {
        self.radiating && self.resolved_power_amp().is_none()
    }

    pub fn resolved_power_amp(&self) -> Option<PowerModel> {
        self.power_selection.resolved(self.voice)
    }

    pub fn is_radiating(&self) -> bool {
        self.radiating
    }

    /// Speaker, cabinet and microphones (upstream's `set_acoustic`).
    pub fn set_acoustic(&mut self, a: &AcousticSettings) {
        let speaker = a.resolved_speaker();
        let previous = self.acoustic_settings;
        let domain_changed = speaker.is_some() != self.radiating;
        let load_changed =
            domain_changed || previous.cabinet != a.cabinet || previous.speaker != a.speaker;
        if load_changed {
            if domain_changed {
                self.over.reset();
                self.pad.reset();
            }
            self.radiating = speaker.is_some();
            if let Some(profile) = speaker {
                let mounting = a.mounting();
                for (model, loaded) in PowerModel::ALL.iter().zip(self.loaded.iter_mut()) {
                    let Some(loaded) = loaded else { continue };
                    let values = LoadValues::new(profile, &mounting, model.speaker_scale());
                    loaded.slots.apply(&mut loaded.sim, &values);
                    loaded.sim.reset_deferred();
                }
                let values = LoadValues::new(profile, &mounting, 1.0);
                self.driven.slots.apply(&mut self.driven.sim, &values);
                self.driven.sim.reset_deferred();
                self.pressure_scale = 1.0 / values.bl;
            }
            if let Some(sim) = self.resistive_power() {
                sim.reset_deferred();
            }
            self.motional_previous = 0.0;
            self.fade_remaining = FADE_LEN;
        }
        if let Some(profile) = speaker {
            let before = (previous.mic_a, previous.mic_b);
            self.acoustic
                .configure(a.resolved_cabinet(), profile, a.mic_a, a.mic_b);
            if load_changed || before != (a.mic_a, a.mic_b) {
                self.fade_remaining = FADE_LEN;
            }
            self.acoustic.set_placement(
                a.place_a, a.place_b, a.blend, a.pan_a, a.pan_b, a.invert_b, a.align,
            );
        }
        let horn = a.resolved_cabinet().and_then(|cab| cab.horn);
        if horn != self.horn {
            self.horn = horn;
            self.horn_low.reset();
            self.horn_high.reset();
            self.horn_over.reset();
            self.horn_pad.reset();
        }
        self.horn_level = a.horn.clamp(0.0, 1.0).powi(2);
        self.acoustic_settings = *a;
        self.set_master(self.master);
        self.set_presence(self.presence);
    }

    pub fn set_power_amp(&mut self, selection: PowerAmp) {
        if selection != self.power_selection {
            self.power_selection = selection;
            self.update_power(false);
            self.set_drive(self.drive);
            self.out_of = self.out_of_target;
        }
    }

    /// The make-up correction for a power stage other than the voice's own
    /// (upstream's `power_trim`).
    fn power_trim(&self) -> f64 {
        match self.power_column() {
            Some(column) => self.trim_for(column),
            None => 1.0,
        }
    }

    fn trim_for(&self, column: usize) -> f64 {
        let row = Gain::ALL.iter().position(|g| *g == self.voice).unwrap_or(0);
        10f64.powf(-POWER_TRIM_DB[row][column] / 20.0)
    }

    fn power_column(&self) -> Option<usize> {
        let column = match self.power_selection {
            PowerAmp::Matched => return None,
            PowerAmp::Bypass => 0,
            PowerAmp::Cali6L6 => 1,
            PowerAmp::American6L6Clean => 2,
            PowerAmp::American6L6HighGain => 3,
            PowerAmp::BritEL34 => 4,
            PowerAmp::BritPlexiEL34 => 5,
            PowerAmp::AC30EL84 => 6,
            PowerAmp::DR103EL34 => 7,
            PowerAmp::Recto6L6 => 8,
            PowerAmp::Recto6L6Tube => 9,
            PowerAmp::AmericanDeluxe6V6 => 10,
            PowerAmp::Brit2205EL34 => 11,
            PowerAmp::BritPlexiBassEL34 => 12,
            PowerAmp::BrumEL34 => 13,
            PowerAmp::Oregon6550 => 14,
            PowerAmp::Svt6550 => 15,
            PowerAmp::AmericanSS800 => 16,
            PowerAmp::Brit45KT66 => 17,
            PowerAmp::American7027A => 18,
            PowerAmp::AmericanV4b7027A => 19,
        };
        Some(column)
    }

    pub fn set_cabinet(&mut self, cabinet: Cabinet) {
        let next = match cabinet {
            Cabinet::Off => None,
            Cabinet::Combo => Some(0),
            Cabinet::Stack => Some(1),
        };
        if next != self.cabinet {
            if let Some(i) = next {
                self.cabinets[i].0.reset_deferred();
            }
            self.cabinet = next;
            self.fade_remaining = FADE_LEN;
        }
    }

    pub fn set_graphic(&mut self, bands: [f64; 5]) {
        if !self.voice.has_graphic() {
            return;
        }
        for (band, &position) in bands.iter().enumerate() {
            self.graphic.set_control(band, position);
        }
    }

    /// The circuit's own output level control, from the panel's Master knob
    /// (upstream's `set_master`, whose comments explain each step).
    pub fn set_master(&mut self, knob: f64) {
        self.master = knob;
        let voice = self.voice;
        let overridden = self.power_selection != PowerAmp::Matched && self.power != 0;
        let level = if overridden {
            if let Some(Level::Circuit(which)) = voice.level_control() {
                let sim = self.gain_sim();
                let rest = sim.resting_position(which).unwrap_or(DEFAULT_MASTER_REST);
                sim.set_control(which, rest);
            }
            Some(Level::Power(power::MASTER))
        } else {
            voice.level_control()
        };
        let resolved = self.power_selection.resolved(self.voice);
        if !matches!(level, Some(Level::Power(_))) {
            let power = self.power;
            let loaded = resolved
                .and_then(|model| self.loaded[model.slot()].as_mut())
                .map(|l| &mut l.sim);
            let resistive = self.powers.get_mut(power).and_then(Option::as_mut);
            for sim in resistive.into_iter().chain(loaded) {
                if let Some(rest) = sim.resting_position(power::MASTER) {
                    sim.set_control(power::MASTER, rest);
                }
            }
        }
        let Some(level) = level else {
            self.master_lift = 1.0;
            return;
        };
        let (sim, which) = match level {
            Level::Circuit(which) => (self.gains[self.gain].as_mut(), which),
            Level::Power(which) => (
                self.powers.get_mut(self.power).and_then(Option::as_mut),
                which,
            ),
        };
        let Some(sim) = sim else {
            self.master_lift = 1.0;
            return;
        };
        let lift = master_lift(knob);
        let mut rest = sim.resting_position(which).unwrap_or(DEFAULT_MASTER_REST);
        if overridden && matches!(level, Level::Power(_)) && rest >= 0.99 {
            rest = OVERRIDE_MASTER_REST;
        }
        let position = master_position(rest, knob);
        sim.set_control(which, position);
        self.master_lift = lift;
        if let (Level::Power(which), Some(model)) = (level, resolved) {
            if let Some(l) = self.loaded[model.slot()].as_mut() {
                l.sim.set_control(which, position);
            }
        }
    }

    /// The power stage's presence control, from the panel's Presence knob
    /// (upstream's `set_presence`).
    pub fn set_presence(&mut self, knob: f64) {
        self.presence = knob.clamp(0.0, 1.0);
        let own = self.voice.own_presence();
        if let Some(which) = own {
            let presence = self.presence;
            let sim = self.gain_sim();
            let rest = sim.resting_position(which).unwrap_or(0.5);
            sim.set_control(which, master_position(rest, presence));
        }
        let Some(model) = self.power_selection.resolved(self.voice) else {
            return;
        };
        if model.presence_name().is_none() {
            return;
        }
        let knob = if own.is_some() { 0.5 } else { self.presence };
        let power = self.power;
        let loaded = self.loaded[model.slot()].as_mut().map(|l| &mut l.sim);
        let resistive = self.powers.get_mut(power).and_then(Option::as_mut);
        for sim in resistive.into_iter().chain(loaded) {
            let rest = sim.resting_position(power::PRESENCE).unwrap_or(0.5);
            sim.set_control(power::PRESENCE, master_position(rest, knob));
        }
    }

    /// The drive control, which moves both the circuit and the make-up
    /// (upstream's `set_drive`). Once a block, not once a sample.
    pub fn set_drive(&mut self, drive: f64) {
        self.drive = drive.clamp(0.0, 1.0);
        let which = self.voice.drive_control();
        let d = self.drive;
        self.gain_sim().set_control(which, d);
        let calibration = CALIBRATION[self.gain];
        let nominal = 10f64.powf(NOMINAL_DBFS / 20.0);
        self.into = calibration.drive_volts / nominal;
        let trim = self.power_trim();
        let make_up_drive = if self.voice.drive_is_channel_volume() {
            CHANNEL_VOLUME_CALIBRATION_REFERENCE
        } else {
            self.drive
        };
        let steps = self.gain_sim().control_steps(which);
        let make_up = 10f64.powf(calibration.make_up_db_on(make_up_drive, steps) / 20.0);
        self.out_of_target = make_up / self.into * self.master_lift * trim;
        self.tap_scale_target = make_up / self.into * self.trim_for(0);
        if let Some((_, table)) = self.direct_outs[self.gain] {
            let gain = 10f64.powf(knots_on(table, make_up_drive, steps) / 20.0);
            self.tap_scale_target = 1.0 / (self.into * gain);
        }
    }

    /// The three tone knobs and the fourth, on the circuit's own stack.
    fn set_tone_knobs(&mut self, s: &Settings) {
        let voice = self.voice;
        let sim = self.gain_sim();
        if let Some((b, m, t)) = voice.own_tone() {
            for (which, value) in [(b, s.bass), (m, s.mid), (t, s.treble)] {
                if which != usize::MAX {
                    sim.set_control(which, value.clamp(0.0, 1.0));
                }
            }
        }
        if let Some((which, _)) = voice.own_sweep() {
            sim.set_control(which, s.sweep.clamp(0.0, 1.0));
        }
    }

    /// Puts a whole panel's worth of settings onto the chain (upstream's
    /// `apply`, in its order).
    pub fn apply(&mut self, s: &Settings) {
        self.set_mains(s.mains);
        self.set_voice(s.amp);
        self.set_input_jack(s.low_input);
        self.set_bright(s.bright);
        self.set_switches(s.low_switch, s.mid_switch);
        self.set_dry_source(s.dry_source);
        self.set_power_amp(s.power_amp);
        self.set_acoustic(&s.acoustic);
        self.set_master(s.master);
        self.set_presence(s.presence);
        self.set_graphic(s.graphic);
        self.set_cabinet(s.legacy_cabinet);
        self.set_drive(s.drive);
        self.set_tone_knobs(s);
        self.set_reverb_and_tremolo(s);
        self.chorus = if self.has_chorus {
            s.chorus.clamp(0.0, 1.0)
        } else {
            0.0
        };
    }

    /// What the amplifier is plugged into, as a fraction of its own mains
    /// (upstream's `set_mains`). The pedals keep their batteries.
    pub fn set_mains(&mut self, fraction: f64) {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.5, 1.2)
        } else {
            1.0
        };
        if (self.mains - fraction).abs() < 1e-9 {
            return;
        }
        self.mains = fraction;
        for sim in self
            .gains
            .iter_mut()
            .flatten()
            .chain(self.powers.iter_mut().flatten())
            .chain(self.loaded.iter_mut().flatten().map(|l| &mut l.sim))
            .chain(std::iter::once(&mut self.driven.sim))
        {
            sim.set_supply_scale(fraction);
        }
        self.fade_remaining = FADE_LEN;
    }

    fn set_input_jack(&mut self, low: bool) {
        let Some(jacks) = self.voice.input_jacks() else {
            return;
        };
        let sim = self.gain_sim();
        sim.set_value(
            jacks.series,
            if low {
                jacks.low_series
            } else {
                jacks.high_series
            },
        );
        if let Some((slot, in_use, open)) = jacks.jack_load {
            sim.set_value(slot, if low { open } else { in_use });
        }
        if let Some((slot, in_use, open)) = jacks.grid_shunt {
            sim.set_value(slot, if low { in_use } else { open });
        }
    }

    fn set_bright(&mut self, bright: bool) {
        if let Some(sw) = self.voice.bright_switch() {
            self.gain_sim()
                .set_value(sw.slot, if bright { sw.on } else { sw.off });
        }
    }

    fn set_switches(&mut self, low: Throw, mid: Throw) {
        for (switch, throw) in [
            (self.voice.low_switch(), low),
            (self.voice.mid_switch(), mid),
        ] {
            if let Some(sw) = switch {
                let sim = self.gain_sim();
                for (&slot, &value) in sw.slots.iter().zip(sw.at(throw)) {
                    sim.set_value(slot, value);
                }
            }
        }
    }

    fn set_reverb_and_tremolo(&mut self, s: &Settings) {
        if self.voice.spring_reverb().is_none() {
            return;
        }
        self.reverb = s.reverb.clamp(0.0, 1.0);
        self.speed = s.speed.clamp(0.0, 1.0);
        self.intensity = s.intensity.clamp(0.0, 1.0);
        self.sync_twin_effect_controls();
    }

    fn tank_send(&self) -> usize {
        match self.voice {
            Gain::Deluxe => self.deluxe_tank_send,
            Gain::AmericanVt40 => self.vt40_tank_send,
            _ => self.twin_tank_send,
        }
    }

    fn sync_twin_effect_controls(&mut self) {
        let (reverb, intensity) = (self.reverb, self.intensity);
        if let Some(r) = self.voice.spring_reverb() {
            self.gain_sim().set_control(r.reverb, reverb);
        }
        if let Some(ab763) = self.voice.ab763() {
            self.gain_sim()
                .set_control(ab763.intensity, 1.0 - intensity);
        }
    }

    /// Where the DI comes from. A change starts the tap from silence.
    pub fn set_dry_source(&mut self, source: DrySource) {
        if source != self.dry_source {
            self.dry_source = source;
            self.tap_over.reset();
            self.tap_pad.reset();
            self.tap_value = 0.0;
            self.tap_scale = self.tap_scale_target;
        }
    }

    /// One host sample: `x` the line's signal into the amplifier, `raw` the
    /// guitar's own (for the DI), both in the line's units. `stereo`: one
    /// chain carries a mono source on a stereo bus, so its microphones are
    /// placed apart.
    #[inline]
    pub fn process(&mut self, x: f64, raw: f64, stereo: bool) -> Frame {
        let x = if x.is_finite() { x } else { 0.0 };
        let raw = if raw.is_finite() { raw } else { 0.0 };
        let dry_input = self.dry.process(x);
        let dry_raw = self.raw.process(raw);
        let inner_rate = self.rate * self.over.factor() as f64;
        let tank_send = self.tank_send();
        let power_model = self.power_selection.resolved(self.voice);
        let radiating = self.radiating;
        let horn_corner = self
            .acoustic_settings
            .resolved_cabinet()
            .and_then(|cab| cab.crossover_hz)
            .unwrap_or(3_000.0);
        self.horn_low.set_lowpass(inner_rate, horn_corner);
        self.horn_high.set_highpass(inner_rate, horn_corner, 1.0);
        let horn_wanted = radiating && self.horn.is_some();
        let horn_terminal = match power_model {
            Some(model) => self.loaded[model.slot()].as_ref().map_or(0, |l| l.terminal),
            None => self.driven.terminal,
        };
        let tapping = self.dry_source == DrySource::Preamp;
        let tap_node = self.direct_outs[self.gain].map(|(node, _)| node);
        self.tap_scale += (self.tap_scale_target - self.tap_scale) * 0.02;
        let tap_scale = self.tap_scale;

        // --- the first half: the preamplifier (upstream's `FrontGain`) ---
        let reverb = self.voice.spring_reverb();
        let ab763 = self.voice.ab763();
        let (mut up, mut down) = self.over.split();
        let mut upsampled = [0.0; MAX_OVERSAMPLING];
        let n = up.push(x * self.into, &mut upsampled);
        let gain = self.gains[self.gain]
            .as_mut()
            .expect("only offered amplifiers are selected");
        let tank_pickup = if reverb.is_some() {
            self.tank.process(self.twin_tank_drive_previous)
        } else {
            0.0
        };
        if let Some(reverb) = reverb {
            gain.set_aux_input(reverb.tank_return_aux, tank_pickup);
        }
        if let Some(ab763) = ab763 {
            let ldr = self.tremolo.resistance(self.speed, self.intensity);
            gain.set_realtime_value(ab763.ldr_slot, ldr);
        }
        let mut next_tank_drive = self.twin_tank_drive_previous;
        let mut mid = [0.0; MAX_OVERSAMPLING];
        let mut tapped = [0.0; MAX_OVERSAMPLING];
        for k in 0..n {
            let mut amplified = gain.process(upsampled[k]);
            if let Some(reverb) = reverb {
                next_tank_drive = gain.voltage_at(tank_send) * reverb.drive_scale;
            }
            if self.voice.has_graphic() {
                amplified = self.graphic.process(amplified);
            }
            if tapping {
                let at = tap_node.map_or(amplified, |node| gain.voltage_at(node));
                tapped[k] = at * tap_scale;
            }
            mid[k] = amplified;
        }
        if reverb.is_some() {
            self.twin_tank_drive_previous = next_tank_drive;
        }
        if tapping {
            let (_, mut tap_down) = self.tap_over.split();
            let decimated = tap_down.pull(&tapped[..n]);
            self.tap_value = self.tap_pad.process(decimated);
        }

        // --- the power stage, or the speaker it drives (`BackPower`) ---
        self.out_of += (self.out_of_target - self.out_of) * 0.02;
        let out_of = self.out_of;
        let pressure_scale = self.pressure_scale
            / power_model
                .map(|model| model.speaker_scale().sqrt())
                .unwrap_or(1.0);
        let mut processed = [0.0; MAX_OVERSAMPLING];
        let mut horns = [0.0; MAX_OVERSAMPLING];
        {
            let (power, driven, motional) = if radiating {
                match power_model {
                    Some(model) => match self.loaded[model.slot()].as_mut() {
                        Some(l) => (Some(&mut l.sim), None, l.motional),
                        None => (None, None, 0),
                    },
                    None => (None, Some(&mut self.driven.sim), self.driven.motional),
                }
            } else {
                (
                    self.powers.get_mut(self.power).and_then(Option::as_mut),
                    None,
                    0,
                )
            };
            let mut power = power;
            let mut driven = driven;
            for k in 0..n {
                let amplified = mid[k];
                if radiating {
                    let (cone, terminal) = match (power.as_deref_mut(), driven.as_deref_mut()) {
                        (Some(sim), _) => {
                            sim.process(amplified);
                            let terminal = if horn_wanted {
                                sim.voltage_at(horn_terminal)
                            } else {
                                0.0
                            };
                            (sim.voltage_at(motional), terminal)
                        }
                        (None, Some(sim)) => {
                            let cone = sim.process(amplified);
                            let terminal = if horn_wanted {
                                sim.voltage_at(horn_terminal)
                            } else {
                                0.0
                            };
                            (cone, terminal)
                        }
                        (None, None) => (0.0, 0.0),
                    };
                    horns[k] = if horn_wanted {
                        let first = terminal - self.horn_low.process(terminal);
                        self.horn_high.process(first) * self.horn_level
                    } else {
                        0.0
                    };
                    // Physical cone acceleration. Keep the amplifier's output
                    // calibration outside the acoustic pressure calculation.
                    processed[k] = pressure_scale * (cone - self.motional_previous) * inner_rate;
                    self.motional_previous = cone;
                } else {
                    let v = match power.as_deref_mut() {
                        Some(sim) => sim.process(amplified),
                        None => amplified,
                    };
                    processed[k] = v * out_of;
                }
            }
        }
        let horn = if horn_wanted {
            let (_, mut horn_down) = self.horn_over.split();
            horn_down.pull(&horns[..n])
        } else {
            0.0
        };
        let mut y = down.pull(&processed[..n]);

        // --- the host-rate end (`BackCabinet`) ---
        let horn = self.horn_pad.process(horn);
        y = self.pad.process(y);
        let stereo = stereo && radiating && !self.has_chorus;
        let mut right = if stereo {
            let (left, right) = self.acoustic.process_stereo_with_horn(y, horn);
            y = left;
            right
        } else if radiating {
            y = self.acoustic.process_with_horn(y, horn);
            y
        } else {
            y
        };
        if radiating {
            y *= out_of;
            right *= out_of;
        }
        if !radiating && matches!(self.acoustic_settings.cabinet, CabinetChoice::Legacy) {
            if let Some(i) = self.cabinet {
                let (sim, trim) = &mut self.cabinets[i];
                y = sim.process(y) * *trim;
                right = y;
            }
        }
        if self.has_chorus {
            let wet = self.bbd.process(y);
            right = y + (wet - y) * self.chorus;
        }
        // The microphones on their own (or, without any, the output).
        let mut mics = if radiating {
            self.acoustic.mics().map(|m| m * out_of)
        } else {
            [y, y]
        };
        if self.fade_remaining > 0 {
            let t = self.fade_remaining as f64 / FADE_LEN as f64;
            self.fade_remaining -= 1;
            y = self.prev_output * t + y * (1.0 - t);
            right = self.prev_output_right * t + right * (1.0 - t);
            for (m, p) in mics.iter_mut().zip(self.prev_mics) {
                *m = p * t + *m * (1.0 - t);
            }
        }
        let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
        let (y, right) = (finite(y), finite(right));
        let mics = mics.map(finite);
        self.prev_output = y;
        self.prev_output_right = right;
        self.prev_mics = mics;
        let dry = match self.dry_source {
            DrySource::Input => dry_raw,
            DrySource::Pedal => dry_input,
            DrySource::Preamp => self.tap_value,
        };
        Frame {
            left: y,
            right,
            dry: finite(dry),
            mics,
        }
    }

    /// Give every nonlinear circuit the same absolute realtime cutoff;
    /// `None` renders.
    pub fn set_realtime_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
        for sim in self
            .gains
            .iter_mut()
            .flatten()
            .chain(self.powers.iter_mut().flatten())
            .chain(self.loaded.iter_mut().flatten().map(|l| &mut l.sim))
            .chain(std::iter::once(&mut self.driven.sim))
        {
            sim.set_realtime_deadline(deadline);
        }
    }

    /// Deadline aborts in the circuits in the path.
    pub fn deadline_aborts(&self) -> u64 {
        let mut aborts = self.gains[self.gain]
            .as_ref()
            .map_or(0, Simulation::deadline_aborts);
        if self.active_driven() {
            aborts += self.driven.sim.deadline_aborts();
        } else if let Some(power) = self.active_power() {
            aborts += power.deadline_aborts();
        }
        aborts
    }

    /// Solver work so far in the circuits in the path: (solves, Newton
    /// passes, samples left unsettled, rebuilds).
    pub fn statistics(&self) -> (u64, u64, u64, u64) {
        let mut sum = (0, 0, 0, 0);
        let sims = self.gains[self.gain]
            .as_ref()
            .into_iter()
            .chain(self.active_power())
            .chain(self.active_driven().then_some(&self.driven.sim));
        for sim in sims {
            let (a, b, c, d) = sim.statistics();
            sum = (sum.0 + a, sum.1 + b, sum.2 + c, sum.3 + d);
        }
        sum
    }

    /// Whether an active nonlinear section is at rest with a dirty matrix
    /// and needs its operating point before audio starts.
    pub fn needs_operating_point(&self) -> bool {
        self.gains[self.gain]
            .as_ref()
            .is_some_and(Simulation::needs_operating_point)
            || self
                .active_power()
                .is_some_and(Simulation::needs_operating_point)
            || (self.active_driven() && self.driven.sim.needs_operating_point())
    }

    /// Hunts only the operating points that are actually pending.
    pub fn find_operating_point(&mut self) -> bool {
        let mut settled = true;
        if let Some(sim) = self.gains[self.gain].as_mut() {
            if sim.needs_operating_point() {
                settled &= sim.find_operating_point();
            }
        }
        if let Some(sim) = self.active_power_mut() {
            if sim.needs_operating_point() {
                settled &= sim.find_operating_point();
            }
        }
        if self.active_driven() && self.driven.sim.needs_operating_point() {
            settled &= self.driven.sim.find_operating_point();
        }
        settled
    }

    pub fn operating_point(&self) -> &[f64] {
        self.gains[self.gain]
            .as_ref()
            .map_or(&[], Simulation::operating_point)
    }

    pub fn power_operating_point(&self) -> Option<&[f64]> {
        if self.active_driven() {
            return Some(self.driven.sim.operating_point());
        }
        self.active_power().map(Simulation::operating_point)
    }

    pub fn share_operating_point_from(&mut self, gain_op: &[f64]) {
        if let Some(sim) = self.gains[self.gain].as_mut() {
            if sim.needs_operating_point() && sim.operating_point().len() == gain_op.len() {
                sim.apply_operating_point(gain_op);
            }
        }
    }

    pub fn share_power_operating_point_from(&mut self, op: &[f64]) {
        let sim = if self.active_driven() {
            Some(&mut self.driven.sim)
        } else {
            self.active_power_mut()
        };
        if let Some(sim) = sim {
            if sim.needs_operating_point() && sim.operating_point().len() == op.len() {
                sim.apply_operating_point(op);
            }
        }
    }

    /// Wake a dormant, identically configured channel from this stream's
    /// exact history (upstream's `copy_runtime_state_from`). Never
    /// allocates.
    pub fn copy_runtime_state_from(&mut self, source: &Self) {
        debug_assert_eq!(self.gain, source.gain);
        debug_assert_eq!(self.power, source.power);
        self.over.copy_runtime_state_from(&source.over);
        self.pad.copy_runtime_state_from(&source.pad);
        self.dry.copy_runtime_state_from(&source.dry);
        self.raw.copy_runtime_state_from(&source.raw);
        self.tap_over.copy_runtime_state_from(&source.tap_over);
        self.tap_pad.copy_runtime_state_from(&source.tap_pad);
        self.horn_over.copy_runtime_state_from(&source.horn_over);
        self.horn_pad.copy_runtime_state_from(&source.horn_pad);
        self.horn_low = source.horn_low;
        self.horn_high = source.horn_high;
        self.tap_value = source.tap_value;
        self.tap_scale = source.tap_scale;
        self.tap_scale_target = source.tap_scale_target;
        if let (Some(dst), Some(src)) = (
            self.gains[self.gain].as_mut(),
            source.gains[source.gain].as_ref(),
        ) {
            dst.copy_runtime_state_from(src);
        }
        if let (Some(dst), Some(src)) = (self.active_power_mut(), source.active_power()) {
            dst.copy_runtime_state_from(src);
        }
        if self.radiating {
            if self.resolved_power_amp().is_none() {
                self.driven.sim.copy_runtime_state_from(&source.driven.sim);
            }
            self.acoustic.copy_runtime_state_from(&source.acoustic);
            self.motional_previous = source.motional_previous;
        }
        if let Some(i) = self.cabinet {
            self.cabinets[i]
                .0
                .copy_runtime_state_from(&source.cabinets[i].0);
        }
        if self.voice.has_graphic() {
            self.graphic.copy_runtime_state_from(&source.graphic);
        }
        if self.voice.has_reverb() {
            self.twin_tank_drive_previous = source.twin_tank_drive_previous;
            self.tank.copy_runtime_state_from(&source.tank);
            self.tremolo.copy_runtime_state_from(&source.tremolo);
        }
        self.out_of = source.out_of;
        self.fade_remaining = source.fade_remaining;
        self.prev_output = source.prev_output;
        self.prev_output_right = source.prev_output_right;
        self.prev_mics = source.prev_mics;
    }

    pub fn reset(&mut self) {
        self.out_of = self.out_of_target;
        self.fade_remaining = 0;
        self.prev_output = 0.0;
        self.prev_output_right = 0.0;
        self.prev_mics = [0.0; 2];
        for sim in self
            .gains
            .iter_mut()
            .flatten()
            .chain(self.powers.iter_mut().flatten())
            .chain(self.loaded.iter_mut().flatten().map(|l| &mut l.sim))
            .chain(std::iter::once(&mut self.driven.sim))
        {
            sim.reset_deferred();
        }
        self.acoustic.reset();
        self.motional_previous = 0.0;
        for (sim, _) in self.cabinets.iter_mut() {
            sim.reset_deferred();
        }
        self.graphic.reset_deferred();
        self.twin_tank_drive_previous = 0.0;
        self.tank.reset();
        self.tremolo.reset();
        self.bbd.reset();
        self.over.reset();
        self.pad.reset();
        self.dry.reset();
        self.raw.reset();
        self.tap_over.reset();
        self.tap_pad.reset();
        self.tap_value = 0.0;
        self.tap_scale = self.tap_scale_target;
        self.horn_low.reset();
        self.horn_high.reset();
        self.horn_over.reset();
        self.horn_pad.reset();
    }
}

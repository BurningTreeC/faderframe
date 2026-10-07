//! A circuit made playable.
//!
//! Between a netlist and a plugin there is a gap that the first attempt at
//! this fell into twice, so both sides of it are settled here before anything
//! is wired to a knob.
//!
//! **A circuit does not run at digital levels.** A guitar arrives at an
//! interface at a peak of a few tenths of a volt and leaves as a number near
//! one. Feed that number to a valve grid and it is a hundred times too large;
//! feed the same number to a three stage cascade with a small signal gain of
//! nearly six thousand and there is nothing to hear but a square wave. Each
//! voice therefore states the voltage a nominal digital signal should arrive
//! at, and that is the only place the two worlds are joined.
//!
//! **The make-up cannot be measured while playing.** It has to follow the
//! drive control, because otherwise every comparison between two settings is
//! just picking the louder one -- but measuring it costs thousands of solves,
//! and the first attempt spent about fifty five times the entire audio budget
//! doing exactly that on every block. The measurement is real work and it is
//! done here in an example, printed as a table, and pasted in as constants;
//! `tests/voice.rs` re-measures the table and fails if the circuits have moved
//! away from it. So the numbers are measured, and the audio thread only ever
//! interpolates five of them.

use crate::acoustics::cabinet::CabinetProfile;
use crate::acoustics::mic::{MicPlacement, MicProfile};
use crate::acoustics::speaker::{self, LoadValues, Mounting, SpeakerProfile};
use crate::acoustics::stage::MicSlot;
use crate::circuits::{
    ac30, american312, american_800rb, american_ss800, american_svt, american_v4b, american_vt40,
    bass_driver, bigmuff, blue_chorus, brit2205, brit800, brit_drive, british_47, brum100, cabinet,
    clean_boost, clipper, console_e, deluxe, distortion_plus, dr103, evh5150, german_76,
    gold_drive, heavy_metal, iron, jazz120, jc120_power, jtm45, markiic, metal_zone, modern_33,
    modern_purple, neve, orange_dist, orange_phase, oregon_t, plexi, plexi_bass, power, preamp,
    rectifier, rodent, round_fuzz, studio, tone, treble_boost, ts808, tube610, twin, wah,
};
use crate::dsp::ac;
use crate::dsp::netlist::{Circuit as Netlist, DiodeSpec, Fault};
use crate::dsp::oversample::Oversampler;

/// The level a plugin should be set up around: hot enough to be well clear of
/// the noise floor, quiet enough to leave headroom for a peak. A guitar
/// tracked sensibly sits about here.
pub const NOMINAL_DBFS: f64 = -18.0;

/// The impedance a stage is driven from and drives into. Real values, so that
/// a stage loads the one before it the way the hardware does rather than
/// every stage pretending it is driven by a perfect source.
pub const SOURCE: f64 = 10_000.0;
pub const LOAD: f64 = 470_000.0;

/// A circuit control index and its panel label.
pub type NamedControl = (usize, &'static str);

/// What makes the gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gain {
    /// One valve stage barely working: the sound of a signal having been
    /// through something rather than the sound of distortion.
    Clean,
    /// Two stages, the second driven by the first.
    Crunch,
    /// Three stages. This is where the gain stops being a texture.
    HighGain,
    /// Diodes in the feedback loop, which lower the gain rather than stopping
    /// the output.
    Overdrive,
    /// Diodes to ground, which are a ceiling.
    Distortion,
    /// A step-up transformer into a discrete stage. Not a guitar
    /// preamplifier turned down: built so as *not* to run out of room, so
    /// everything interesting happens in the last few decibels before it does.
    Console,
    /// The same channel without the input transformer, and with far more
    /// headroom. The one to reach for when the point is not to hear the
    /// preamplifier.
    Studio,
    // --- modelled from schematics ---------------------------------------
    // The ones above are topologies: a valve cascade, a clipper, a channel.
    // These are particular circuits, parts and values off a drawing, and each
    // is checked against arithmetic done from that drawing.
    /// Ibanez TS808.
    Screamer,
    /// Electro-Harmonix Big Muff Pi, the 1973 Ram's Head.
    Muff,
    /// Mesa Boogie Mark IIC+, the lead channel preamplifier through its
    /// recovery stage.
    Boogie,
    /// Peavey EVH 5150, the lead channel preamplifier.
    Peavey,
    /// Neve 73P microphone preamplifier, two cascaded transistor stages.
    Neve,
    /// Fender Twin Reverb, AB763 -- the circuit the '65 reissue reissues.
    /// Vibrato channel, with the spring tank and the tremolo.
    Twin,
    /// Marshall JCM800 2203, the 1981 master-volume preamplifier. Its matched
    /// power stage is the Brit EL34. See `circuits::brit800`.
    Brit800,
    /// API 312 microphone preamplifier card. See `circuits::american312`.
    American312,
    /// SSL SL 4000 E channel mic amplifier (82E01). See `circuits::console_e`.
    ConsoleE,
    /// Universal Audio 610-A modular console preamplifier. See `circuits::tube610`.
    Tube610,
    /// Marshall 1959 Super Lead (Unicord drawing, 1970), bright channel. Its
    /// matched power stage is the Brit Plexi EL34. See `circuits::plexi`.
    Plexi,
    /// Vox AC30/6 Top Boost, brilliant channel. Cathode-biased EL84s with no
    /// feedback loop behind it. See `circuits::ac30`.
    AC30,
    /// Hiwatt Custom 100 DR103, brilliant channel through its master volume.
    /// See `circuits::dr103`.
    DR103,
    // --- the pedals, as circuits in their own right ----------------------
    // Every pedal in the slot is also a circuit you can select on its own,
    // with no amplifier behind it. The Green 808 and the Ram Fuzz were here
    // first -- they were catalogue voices before the pedal slot existed -- and
    // these are the rest of them, appended so the voice indices above do not
    // move and `CALIBRATION` keeps its meaning.
    /// Ibanez TS9 (`ts808::TS9`).
    Green9,
    /// Pro Co RAT (`circuits::rodent`).
    Rat,
    /// Arbiter Fuzz Face (`circuits::round_fuzz`).
    FuzzFace,
    /// MXR Distortion+ (`circuits::distortion_plus`).
    DistPlus,
    /// Boss HM-2 (`circuits::heavy_metal`).
    Hm2,
    /// Boss MT-2 (`circuits::metal_zone`).
    Mt2,
    /// Mesa/Boogie Dual Rectifier, Rev F, the red channel in its modern setting.
    /// See `circuits::rectifier`.
    Recto,
    /// Fender Deluxe Reverb, AB763 -- the same circuit family as the Twin at
    /// a quarter of the power. Vibrato channel, with the spring tank and the
    /// optical tremolo. See `circuits::deluxe`.
    Deluxe,
    /// Roland JC-120 Jazz Chorus, CH-1, the clean channel. The
    /// current-production JC-120UT/JT. See `circuits::jazz120`.
    ///
    /// The first solid-state guitar amplifier in the catalogue, and the first
    /// with no valve power stage behind it: its two 60 W transistor amplifiers
    /// and its bucket-brigade chorus are separate work, so `Matched` resolves
    /// to nothing here and the channel hands straight over to the tone
    /// section. What is modelled is the part that makes the sound -- two
    /// 2SK184 stages around a passive network with far more authority than a
    /// Fender stack has.
    Jazz120,
    /// The AB763 Deluxe's *other* channel: two knobs, a Volume, no bright
    /// capacitor, and neither the reverb nor the tremolo -- both of those hang
    /// on the Vibrato channel's second stage and join this one only at the
    /// mixer, after the intensity tap. See `circuits::deluxe`.
    DeluxeNormal,
    /// Marshall JCM800 2205, the 50 W split-channel head: its boost channel,
    /// in the later (1985-89) circuit. A diode-biased second stage and a
    /// bridge-rectifier clipper the single-channel 2203 does not have, and its
    /// own 50 W power stage, the Brit 2205 EL34. See `circuits::brit2205`.
    Brit2205,
    /// Boss DS-1, the TA7136P original (`circuits::orange_dist`). A pedal, so
    /// it sits with the others in spirit; it is appended here, not beside
    /// them, so no voice index above it moves.
    Ds1,
    /// Dallas Rangemaster, the stock OC44 unit (`circuits::treble_boost`).
    /// A pedal, appended like the DS-1.
    TrebleBoost,
    /// Marshall 1992 Super Bass (Unicord drawing, 1970): the Brit Plexi's
    /// sibling, with a shared V1 cathode, no bright capacitor, an unbypassed
    /// V2a and a 250 pF / 56 k stack. Its matched power stage is the Brit
    /// Plexi Bass EL34. See `circuits::plexi_bass`.
    PlexiBass,
    /// Laney Supergroup 100 Mk I (traced drawing of a 1969 build), TREBLE
    /// channel. Its matched power stage is the Brum EL34, 600 V on four EL34s.
    /// See `circuits::brum100`.
    Brum100,
    /// Sunn Model T (drawing D-1029 A, 1973), BRITE channel through its
    /// master. Its matched power stage is the Oregon 6550, four 6550s in
    /// ultra-linear. See `circuits::oregon_t`.
    OregonT,
    /// Klon Centaur (`circuits::gold_drive`).
    GoldDrive,
    /// Marshall The Guv'nor, the original (`circuits::brit_drive`).
    BritDrive,
    /// MXR MicroAmp (`circuits::clean_boost`).
    CleanBoost,
    /// Ampeg SVT, the 6550 head, channel 1 (D 591719 D, 1975). Six 6550s
    /// behind cathode followers. See `circuits::american_svt`.
    AmericanSvt,
    /// EMI REDD.47, the cassette line amplifier (REDD.47/C1, 1959): EF86 and
    /// paralleled E88CC between a 1:7 and a 7:1 transformer, its gain a
    /// switch in the outer loop. See `circuits::british_47`.
    British47,
    /// The Telefunken / TAB V76 (IRT drawing S 1176, 1959): three EF804S
    /// and an E83F between a 1:30 and a 9:1 transformer, its gain a
    /// twelve-step switch. See `circuits::german_76`.
    German76,
    /// The Tech 21 SansAmp Bass Driver DI, V2, from a trace of a real unit
    /// (`circuits::bass_driver`).
    BassDriver,
    /// The Gallien-Krueger 800RB's preamp, rev C of 1991, into its own 300 W
    /// transistor amplifier (`circuits::american_800rb`,
    /// `circuits::american_ss800`).
    American800RB,
    /// The MXR Phase 90, script logo with the block's R28 on a switch
    /// (`circuits::orange_phase`).
    OrangePhase,
    /// The Boss CE-2, its bucket brigade between two halves of its netlist
    /// (`circuits::blue_chorus`).
    BlueChorus,
    /// The Fortin 33, from a trace of a genuine unit (`circuits::modern_33`).
    Modern33,
    /// The Revv G3, the original, from a trace of a genuine unit
    /// (`circuits::modern_purple`).
    ModernPurple,
    /// The Marshall JTM45 of 1965, from Marshall's own drawings
    /// (`circuits::jtm45`), into its two KT66s.
    Brit45,
    /// The Ampeg VT-40 of 1971, from Ampeg's own drawing and service data
    /// (`circuits::american_vt40`), into its two 7027As.
    AmericanVt40,
    /// The Ampeg V-4B of 1971, the VT-40's bass sibling, from Ampeg's own
    /// drawing (`circuits::american_v4b`), into its four 7027As.
    AmericanV4b,
}

impl Gain {
    // Appended: the calibration table and every chain's circuit slots are laid
    // out in this order.
    pub const ALL: [Gain; 51] = [
        Gain::Clean,
        Gain::Crunch,
        Gain::HighGain,
        Gain::Overdrive,
        Gain::Distortion,
        Gain::Console,
        Gain::Studio,
        Gain::Screamer,
        Gain::Muff,
        Gain::Boogie,
        Gain::Peavey,
        Gain::Neve,
        Gain::Twin,
        Gain::Brit800,
        Gain::American312,
        Gain::ConsoleE,
        Gain::Tube610,
        Gain::Plexi,
        Gain::AC30,
        Gain::DR103,
        Gain::Recto,
        Gain::Green9,
        Gain::Rat,
        Gain::FuzzFace,
        Gain::DistPlus,
        Gain::Hm2,
        Gain::Mt2,
        Gain::Deluxe,
        Gain::Jazz120,
        Gain::DeluxeNormal,
        Gain::Brit2205,
        Gain::Ds1,
        Gain::TrebleBoost,
        Gain::PlexiBass,
        Gain::Brum100,
        Gain::OregonT,
        Gain::GoldDrive,
        Gain::BritDrive,
        Gain::CleanBoost,
        Gain::AmericanSvt,
        Gain::British47,
        Gain::German76,
        Gain::BassDriver,
        Gain::American800RB,
        Gain::OrangePhase,
        Gain::BlueChorus,
        Gain::Modern33,
        Gain::ModernPurple,
        Gain::Brit45,
        Gain::AmericanVt40,
        Gain::AmericanV4b,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Gain::Clean => "Clean",
            Gain::Crunch => "Crunch",
            Gain::HighGain => "High Gain",
            Gain::Overdrive => "Overdrive",
            Gain::Distortion => "Distortion",
            Gain::Console => "Console",
            Gain::Studio => "Studio",
            Gain::Screamer => "TS808",
            Gain::Twin => "Twin Reverb",
            Gain::Muff => "Big Muff",
            Gain::Boogie => "Mark IIC+",
            Gain::Peavey => "5150",
            Gain::Neve => "Neve 73P",
            Gain::Brit800 => "JCM800 2203",
            Gain::American312 => "API 312",
            Gain::ConsoleE => "SSL 4000 E",
            Gain::Tube610 => "UA 610-A",
            Gain::Plexi => "1959 Super Lead",
            Gain::AC30 => "AC30 Top Boost",
            Gain::DR103 => "Hiwatt DR103",
            Gain::Recto => "Dual Rectifier",
            Gain::Green9 => "TS9",
            Gain::Rat => "ProCo RAT",
            Gain::FuzzFace => "Fuzz Face",
            Gain::DistPlus => "MXR Distortion+",
            Gain::Hm2 => "Boss HM-2",
            Gain::Mt2 => "Boss MT-2",
            Gain::Deluxe => "Deluxe Reverb",
            Gain::Jazz120 => "Jazz Chorus JC-120",
            Gain::DeluxeNormal => "Deluxe Reverb, Normal",
            Gain::Brit2205 => "JCM800 2205",
            Gain::Ds1 => "Boss DS-1",
            Gain::TrebleBoost => "Rangemaster",
            Gain::PlexiBass => "1992 Super Bass",
            Gain::Brum100 => "Supergroup 100",
            Gain::OregonT => "Sunn Model T",
            Gain::GoldDrive => "Klon Centaur",
            Gain::BritDrive => "Marshall Guv'nor",
            Gain::CleanBoost => "MXR MicroAmp",
            Gain::AmericanSvt => "Ampeg SVT",
            Gain::British47 => "EMI REDD.47",
            Gain::German76 => "Telefunken V76",
            Gain::BassDriver => "Tech 21 SansAmp Bass Driver DI",
            Gain::American800RB => "Gallien-Krueger 800RB",
            Gain::OrangePhase => "MXR Phase 90",
            Gain::BlueChorus => "Boss CE-2",
            Gain::Modern33 => "Fortin 33",
            Gain::ModernPurple => "Revv G3",
            Gain::Brit45 => "JTM45",
            Gain::AmericanVt40 => "Ampeg VT-40",
            Gain::AmericanV4b => "Ampeg V-4B",
        }
    }

    /// Which control on this circuit the Drive knob turns.
    ///
    /// The topologies all put it first because they were written that way.
    /// A modelled circuit puts its controls where the drawing does, and the
    /// Mark IIC+ has treble, bass and middle before its lead drive -- so a
    /// Drive knob that always turned control zero would be turning its treble.
    pub fn drive_control(self) -> usize {
        match self {
            Gain::Boogie => markiic::LEAD_DRIVE,
            Gain::Peavey => evh5150::PRE,
            Gain::Neve => neve::GAIN,
            Gain::Screamer => ts808::DRIVE,
            Gain::Twin => twin::VOLUME,
            Gain::Deluxe => deluxe::VOLUME,
            Gain::Jazz120 => jazz120::VOLUME,
            Gain::DeluxeNormal => deluxe::VOLUME,
            Gain::Muff => bigmuff::SUSTAIN,
            Gain::Brit800 => brit800::VOLUME,
            Gain::Brit2205 => brit2205::GAIN,
            Gain::American312 => american312::GAIN,
            Gain::ConsoleE => console_e::GAIN,
            Gain::Tube610 => tube610::LEVEL,
            Gain::Plexi => plexi::VOLUME,
            Gain::AC30 => ac30::VOLUME,
            Gain::DR103 => dr103::VOLUME,
            Gain::Recto => rectifier::GAIN,
            Gain::Green9 => ts808::DRIVE,
            Gain::Rat => rodent::DISTORTION,
            Gain::FuzzFace => round_fuzz::FUZZ,
            Gain::DistPlus => distortion_plus::DISTORTION,
            Gain::Hm2 => heavy_metal::DIST,
            Gain::Mt2 => metal_zone::DIST,
            Gain::Ds1 => orange_dist::DIST,
            // Its one pot, which is a volume after a fixed gain: see
            // `drive_is_channel_volume`.
            Gain::TrebleBoost => treble_boost::BOOST,
            Gain::PlexiBass => plexi_bass::VOLUME,
            Gain::Brum100 => brum100::VOLUME,
            Gain::OregonT => oregon_t::VOLUME,
            Gain::GoldDrive => gold_drive::GAIN,
            Gain::BritDrive => brit_drive::GAIN,
            Gain::CleanBoost => clean_boost::GAIN,
            Gain::AmericanSvt => american_svt::VOLUME,
            Gain::British47 => british_47::GAIN,
            Gain::German76 => german_76::GAIN,
            Gain::BassDriver => bass_driver::DRIVE,
            Gain::American800RB => american_800rb::VOLUME,
            Gain::OrangePhase => orange_phase::SPEED,
            Gain::BlueChorus => blue_chorus::RATE,
            // Its one pot, a level after a fixed gain, as the Treble Boost's.
            Gain::Modern33 => modern_33::LEVEL,
            Gain::ModernPurple => modern_purple::GAIN,
            Gain::Brit45 => jtm45::VOLUME,
            Gain::AmericanVt40 => american_vt40::VOLUME,
            Gain::AmericanV4b => american_v4b::VOLUME,
            _ => clipper::GAIN,
        }
    }

    /// Whether this amplifier carries the five band graphic equaliser.
    ///
    /// Only the Mark IIC+. It is the part of that amplifier everyone
    /// recognises -- the scooped middle is this network and not the Fender
    /// stack ahead of it -- and it sits late in the preamplifier, so it is
    /// modelled where the drawing puts it rather than as an equaliser bolted
    /// on the end. See `markiic::graphic` and §9.8.
    pub const fn has_graphic(self) -> bool {
        matches!(self, Gain::Boogie)
    }

    /// What the drive knob is called on the device it is turning.
    ///
    /// The same principle as the pedals' single tone control being labelled
    /// TONE rather than TREBLE: a knob is named after what it turns, and what
    /// it turns here is a specific pot on a specific drawing. "Drive" is the
    /// section's job, not every device's word for it.
    ///
    /// The Twin Reverb is the one that matters. Its drive control **is** its
    /// Volume -- an AB763 has no master, so the one pot both sets the level and
    /// decides how hard the amplifier works -- and calling it DRIVE made it
    /// look as though the amplifier's volume knob was missing. It is not
    /// missing; it is this one.
    pub fn drive_name(self) -> &'static str {
        match self {
            Gain::Twin | Gain::Deluxe | Gain::Jazz120 | Gain::DeluxeNormal => "VOLUME",
            Gain::Muff => "SUSTAIN",
            Gain::Boogie => "LEAD DRIVE",
            Gain::Peavey => "PRE GAIN",
            Gain::Neve => "GAIN",
            Gain::Brit800 => "PREAMP",
            Gain::American312 | Gain::ConsoleE | Gain::British47 | Gain::German76 => "GAIN",
            Gain::Tube610 => "LEVEL",
            Gain::Plexi
            | Gain::AC30
            | Gain::DR103
            | Gain::PlexiBass
            | Gain::Brit45
            | Gain::OregonT
            | Gain::AmericanSvt
            | Gain::AmericanVt40
            | Gain::AmericanV4b
            | Gain::American800RB => "VOLUME",
            // The Laney prints its volumes as gains: this channel's is GAIN TWO.
            Gain::Brum100 => "GAIN",
            Gain::Recto | Gain::Brit2205 => "GAIN",
            // The pedals, as their boxes print them.
            Gain::Screamer => "OVERDRIVE",
            Gain::Rat | Gain::DistPlus | Gain::Hm2 => "DISTORTION",
            Gain::FuzzFace => "FUZZ",
            Gain::TrebleBoost => "BOOST",
            Gain::OrangePhase => "SPEED",
            Gain::BlueChorus => "RATE",
            Gain::Modern33 => "LEVEL",
            Gain::GoldDrive | Gain::BritDrive | Gain::CleanBoost | Gain::ModernPurple => "GAIN",
            Gain::Mt2 | Gain::Ds1 => "DIST",
            _ => "DRIVE",
        }
    }

    /// What the panel calls the level knob (`level_control`) on this circuit:
    /// MASTER on the amplifiers, and on a pedal the name its box prints --
    /// which is what the pedal slot's level knob stands for.
    pub fn level_name(self) -> &'static str {
        match self {
            Gain::Screamer | Gain::Green9 | Gain::Hm2 | Gain::Mt2 | Gain::Ds1 => "LEVEL",
            Gain::Muff | Gain::Rat | Gain::FuzzFace | Gain::ModernPurple => "VOLUME",
            Gain::DistPlus | Gain::GoldDrive => "OUTPUT",
            Gain::BritDrive | Gain::BassDriver => "LEVEL",
            // Its masters sit on 10 as the manual says; the knob is the boost,
            // the preset volume ahead of them.
            Gain::American800RB => "BOOST",
            _ => "MASTER",
        }
    }

    /// The circuit's own output level control, where its drawing has one, and
    /// which of the two simulations it lives in.
    ///
    /// Every device here except the abstract typologies has a knob on its face
    /// that sets how loud it is, and until now every one of them was frozen at
    /// a resting position -- BUG-023's fault, one level up: a control that
    /// exists on the hardware and cannot be reached from the panel.
    ///
    /// Which control counts as "the output" is a judgement per device and it
    /// is written here rather than inferred:
    ///
    /// - the pedals' Level and Volume, which is what they are called;
    /// - the 73P's output trim;
    /// - the 5150's post gain, at its **power stage**: it is after the
    ///   preamplifier, which is the amplifier's whole method -- gain in front,
    ///   level at the back;
    /// - the Mark IIC+'s **Lead Master**, in the preamplifier, because that is
    ///   where the drawing puts it: between V2B and the V2A recovery stage, so
    ///   it sets how hard V2A and the power stage are driven. The amplifier's
    ///   overall MASTER sits after V2A, and the power stage's master pot stands
    ///   in for it at its resting position;
    /// - the Mark IIC+'s Volume 1 is *not* this. It is an input volume and it
    ///   sits ahead of the lead circuit; Lead Drive is already the Drive knob.
    ///
    /// **The Twin Reverb has none**, and that is not an omission. An AB763 has
    /// no master volume and no presence: its channel Volume is the only level
    /// control it owns, and that is already what the Drive knob turns. Giving
    /// it one would hide why its phase inverter sees a hundred volts at Volume
    /// 10 -- see BUG-025.
    pub fn level_control(self) -> Option<Level> {
        match self {
            Gain::Screamer => Some(Level::Circuit(ts808::LEVEL)),
            Gain::Muff => Some(Level::Circuit(bigmuff::VOLUME)),
            Gain::Neve => Some(Level::Circuit(neve::TRIM)),
            Gain::Boogie => Some(Level::Circuit(markiic::LEAD_MASTER)),
            // The 2203's Master Volume is the pot the Brit EL34 stage begins with.
            Gain::Peavey | Gain::Brit800 => Some(Level::Power(power::MASTER)),
            // The DR103's master volume is in the preamplifier, between the
            // stack and the last two triodes, which is where the drawing has it.
            Gain::DR103 => Some(Level::Circuit(dr103::MASTER)),
            // The red channel's master, in the preamplifier as the sheet has it.
            Gain::Recto => Some(Level::Circuit(rectifier::MASTER)),
            // The later 2205's master is VR10, in the preamplifier ahead of the
            // loop and V4A, which is where the drawing has it.
            Gain::Brit2205 => Some(Level::Circuit(brit2205::MASTER)),
            Gain::Green9 => Some(Level::Circuit(ts808::LEVEL)),
            Gain::Rat => Some(Level::Circuit(rodent::VOLUME)),
            Gain::FuzzFace => Some(Level::Circuit(round_fuzz::VOLUME)),
            Gain::DistPlus => Some(Level::Circuit(distortion_plus::VOLUME)),
            Gain::Hm2 => Some(Level::Circuit(heavy_metal::LEVEL)),
            Gain::Mt2 => Some(Level::Circuit(metal_zone::LEVEL)),
            Gain::Ds1 => Some(Level::Circuit(orange_dist::LEVEL)),
            // The Model T's master, R19, in the preamplifier as the drawing has
            // it. The 1992 and the Supergroup have none, like the 1959, and the
            // Rangemaster's only pot is already the Drive knob.
            Gain::OregonT => Some(Level::Circuit(oregon_t::MASTER)),
            Gain::GoldDrive => Some(Level::Circuit(gold_drive::LEVEL)),
            Gain::BritDrive => Some(Level::Circuit(brit_drive::LEVEL)),
            Gain::ModernPurple => Some(Level::Circuit(modern_purple::VOLUME)),
            Gain::BassDriver => Some(Level::Circuit(bass_driver::LEVEL)),
            Gain::American800RB => Some(Level::Circuit(american_800rb::BOOST)),
            // The MicroAmp's one knob is its gain, already the Drive knob.
            _ => None,
        }
    }

    /// The circuit's own tone controls, where it has some: bass, middle,
    /// treble. A Mark IIC+ carries a Fender stack of its own, and using the
    /// plugin's generic one instead of it would be modelling the amplifier
    /// and then ignoring the part everyone recognises.
    pub fn own_tone(self) -> Option<(usize, usize, usize)> {
        match self {
            Gain::Boogie => Some((markiic::BASS, markiic::MIDDLE, markiic::TREBLE)),
            Gain::Twin => Some((twin::BASS, twin::MIDDLE, twin::TREBLE)),
            // No Middle control: the AB763 Deluxe grounds its stack through a
            // fixed 6.8 k resistor, so the panel's Middle knob is greyed out.
            Gain::Deluxe | Gain::DeluxeNormal => Some((deluxe::BASS, usize::MAX, deluxe::TREBLE)),
            // All three, and they have more authority than a Fender stack:
            // the slope resistor feeds two caps into two entry points rather
            // than one. See `circuits::jazz120`.
            Gain::Jazz120 => Some((jazz120::BASS, jazz120::MIDDLE, jazz120::TREBLE)),
            Gain::Brit800 => Some((brit800::BASS, brit800::MIDDLE, brit800::TREBLE)),
            Gain::Plexi => Some((plexi::BASS, plexi::MIDDLE, plexi::TREBLE)),
            // No middle control: the stack has a 10 k resistor where a Fender
            // stack has that pot, so the panel's Middle knob is greyed out.
            Gain::AC30 => Some((ac30::BASS, usize::MAX, ac30::TREBLE)),
            Gain::DR103 => Some((dr103::BASS, dr103::MIDDLE, dr103::TREBLE)),
            Gain::Recto => Some((rectifier::BASS, rectifier::MIDDLE, rectifier::TREBLE)),
            Gain::Brit2205 => Some((brit2205::BASS, brit2205::MIDDLE, brit2205::TREBLE)),
            Gain::PlexiBass => Some((plexi_bass::BASS, plexi_bass::MIDDLE, plexi_bass::TREBLE)),
            Gain::Brit45 => Some((jtm45::BASS, jtm45::MIDDLE, jtm45::TREBLE)),
            Gain::Brum100 => Some((brum100::BASS, brum100::MIDDLE, brum100::TREBLE)),
            Gain::OregonT => Some((oregon_t::BASS, oregon_t::MIDDLE, oregon_t::TREBLE)),
            // BASS, MIDRANGE (inside the V3b-V4 loop) and TREBLE.
            Gain::AmericanSvt => Some((
                american_svt::BASS,
                american_svt::MIDDLE,
                american_svt::TREBLE,
            )),
            // The same three, in the 6K11's loop.
            Gain::AmericanVt40 => Some((
                american_vt40::BASS,
                american_vt40::MIDDLE,
                american_vt40::TREBLE,
            )),
            Gain::AmericanV4b => Some((
                american_v4b::BASS,
                american_v4b::MIDDLE,
                american_v4b::TREBLE,
            )),
            // The original 5150's own stack, off its preamp sheet (2026-09-25).
            Gain::Peavey => Some((evh5150::BASS, evh5150::MIDDLE, evh5150::TREBLE)),
            // The pedals with a single tone control have a knob of their own
            // (`own_single_tone`), not the stack's Treble.
            // The Heavy Metal's Colour Mix has dedicated plugin parameters.
            // Do not multiplex them onto Bass/Treble: those three remain the
            // optional plugin tone stack and the HM-2 pair is independent.
            Gain::Mt2 => Some((metal_zone::LOW, metal_zone::MIDDLE, metal_zone::HIGH)),
            // The Guv'nor's own three, wired as its drawing has them.
            Gain::BritDrive => Some((brit_drive::BASS, brit_drive::MIDDLE, brit_drive::TREBLE)),
            // The G3's passive bass and treble and its active middle.
            Gain::ModernPurple => Some((
                modern_purple::BASS,
                modern_purple::MID,
                modern_purple::TREBLE,
            )),
            // Active, +-12 dB each; the bass and mid with their shifts on the
            // panel's low and mid switches.
            Gain::BassDriver => Some((bass_driver::BASS, bass_driver::MID, bass_driver::TREBLE)),
            // Four bands: BASS, LOW MID and TREBLE here, HIGH MID beside them
            // (`own_sweep`).
            Gain::American800RB => Some((
                american_800rb::BASS,
                american_800rb::LO_MID,
                american_800rb::TREBLE,
            )),
            Gain::Neve => None,
            _ => None,
        }
    }

    /// Which of the three tone knobs this circuit carries one of its own for:
    /// bass, middle, treble.
    ///
    /// The panel needs this and `own_tone` will not do, because a control that
    /// is not there is written `usize::MAX` and a caller that forgets to check
    /// sets control number eighteen quintillion.
    /// A control this circuit has of its own beyond bass, middle and treble,
    /// and what the panel calls it.
    ///
    /// The Metal Zone's **Mid Freq**, which moves where the middle band works
    /// rather than how much it does; and the Bass Driver's **Blend**. Selected as a pedal it
    /// gets a knob in the slot's own row; selected as a circuit it needs one
    /// here, or the control would be reachable from one half of the plugin and
    /// not the other.
    pub fn own_sweep(self) -> Option<(usize, &'static str)> {
        match self {
            Gain::Mt2 => Some((metal_zone::MID_FREQ, "MID FREQ")),
            // Not a tone control but the box's one control beyond the stack's
            // three and presence: the blend of dry against the emulation.
            Gain::BassDriver => Some((bass_driver::BLEND, "BLEND")),
            // The fourth band of its four.
            Gain::American800RB => Some((american_800rb::HI_MID, "HI MID")),
            // Not a tone control but the box's toggle beyond its stack: off,
            // Blue and Red in thirds of the knob.
            Gain::ModernPurple => Some((modern_purple::AGGRESSION, "AGGRESSION")),
            _ => None,
        }
    }

    /// The Heavy Metal circuit's dedicated Colour Mix pair. These are not the
    /// plugin's generic Bass/Treble controls and must keep separate state.
    pub fn own_colour_mix(self) -> Option<(NamedControl, NamedControl)> {
        match self {
            Gain::Hm2 => Some((
                (heavy_metal::LOW, "COLOUR LO"),
                (heavy_metal::HIGH, "COLOUR HI"),
            )),
            _ => None,
        }
    }

    /// A presence control this circuit carries in its own netlist, which the
    /// panel's Presence knob turns instead of the power stage's.
    ///
    /// Only the Cali Rectifier's. In the red channel's Modern mode the Dual
    /// Rectifier's power amplifier runs with no loop, so there is nothing for
    /// a presence to work on there, and the channel's PRSNC is a treble shunt
    /// in the preamplifier (`rectifier::PRESENCE`). Where a circuit has one,
    /// the power stage's own presence -- in a custom chain behind it -- rests
    /// where it was voiced: one knob, one control, as the amplifier has.
    pub fn own_presence(self) -> Option<usize> {
        match self {
            Gain::Recto => Some(rectifier::PRESENCE),
            // In U1A's loop, ahead of the drive stages.
            Gain::BassDriver => Some(bass_driver::PRESENCE),
            _ => None,
        }
    }

    /// A pedal's single tone control, selected as the circuit: which control,
    /// what its box calls it, and whether the knob runs the other way from the
    /// pot.
    ///
    /// It has a knob and a parameter of its own (`Settings::circuit_tone`),
    /// drawn beside the stack's three, the way the pedal slot gives it one.
    /// Until 2026-09-30 it rode on the stack's Treble knob, which the panel
    /// called TREBLE whenever the plugin's stack was in circuit -- the default
    /// -- because the knob turned both. So a TS808 picked as the circuit showed
    /// no Tone knob anywhere, which is how it was reported.
    ///
    /// The Rodent's is a **filter**, whose pot darkens as it turns up; the
    /// knob runs the other way, as the pedal slot's does, so up is brighter on
    /// every one of these.
    pub fn own_single_tone(self) -> Option<(usize, &'static str, bool)> {
        match self {
            Gain::Screamer | Gain::Green9 => Some((ts808::TONE, "TONE", false)),
            Gain::Muff => Some((bigmuff::TONE, "TONE", false)),
            Gain::Rat => Some((rodent::FILTER, "FILTER", true)),
            // A Big-Muff-style blend with a scoop in the middle, like the Muff's.
            Gain::Ds1 => Some((orange_dist::TONE, "TONE", false)),
            // A shelf above 408 Hz, boost and cut: the Klon's TREBLE.
            Gain::GoldDrive => Some((gold_drive::TREBLE, "TREBLE", false)),
            // Not a tone control but the pedal's one switch, on its one
            // knob: down the script (R28 out), up the block (R28 in).
            Gain::OrangePhase => Some((orange_phase::BLOCK, "BLOCK", false)),
            // The CE-2's other knob, which is its depth rather than a tone.
            Gain::BlueChorus => Some((blue_chorus::DEPTH, "DEPTH", false)),
            _ => None,
        }
    }

    pub fn own_tone_knobs(self) -> [bool; 3] {
        match self.own_tone() {
            Some((b, m, t)) => [b != usize::MAX, m != usize::MAX, t != usize::MAX],
            None => [false; 3],
        }
    }

    /// Whether the circuit's only tone control is the single knob a pedal has.
    ///
    /// A TS808 and a Big Muff each have one, and it is not a treble control:
    /// the Screamer's is a shelf either side of a fixed corner and the Muff's
    /// is a blend between a low-pass and a high-pass with a scoop in the
    /// middle. See `own_single_tone`.
    pub fn single_tone(self) -> bool {
        self.own_single_tone().is_some()
    }

    /// The *valve* power amplifier behind this circuit, where it has one.
    ///
    /// Only the three valve guitar amplifiers do. A pedal has no power stage,
    /// and the topologies are shapes rather than particular units, so there
    /// is nothing to put behind them: inventing one would be inventing a
    /// sound. The 73P has an output block of its own and it is not one of
    /// these -- see `build_power`.
    pub fn power_stage(self) -> Option<&'static power::PowerSpec> {
        match self {
            Gain::Boogie => Some(&power::PowerSpec::MARKIIC),
            Gain::Peavey => Some(&power::PowerSpec::EVH5150),
            Gain::Twin => Some(&power::PowerSpec::TWIN),
            Gain::Deluxe | Gain::DeluxeNormal => Some(&power::PowerSpec::DELUXE_6V6),
            Gain::Brit800 => Some(&power::PowerSpec::BRIT_EL34),
            Gain::Plexi => Some(&power::PowerSpec::PLEXI_EL34),
            Gain::AC30 => Some(&power::PowerSpec::AC30_EL84),
            Gain::DR103 => Some(&power::PowerSpec::DR103_EL34),
            Gain::Recto => Some(&power::PowerSpec::RECTO_6L6),
            Gain::Brit2205 => Some(&power::PowerSpec::BRIT_2205_EL34),
            Gain::PlexiBass => Some(&power::PowerSpec::PLEXI_BASS_EL34),
            Gain::Brit45 => Some(&power::PowerSpec::JTM45_KT66),
            Gain::AmericanVt40 => Some(&power::PowerSpec::VT40_7027A),
            Gain::AmericanV4b => Some(&power::PowerSpec::V4B_7027A),
            Gain::Brum100 => Some(&power::PowerSpec::BRUM_EL34),
            Gain::OregonT => Some(&power::PowerSpec::OREGON_6550),
            Gain::AmericanSvt => Some(&power::PowerSpec::SVT_6550),
            _ => None,
        }
    }

    /// Whether this is a model of a particular circuit rather than a
    /// topology. The panel says so, because "an overdrive" and "a TS808" are
    /// different kinds of claim.
    pub fn is_modelled(self) -> bool {
        matches!(
            self,
            Gain::Green9
                | Gain::Rat
                | Gain::FuzzFace
                | Gain::DistPlus
                | Gain::Hm2
                | Gain::Mt2
                | Gain::Screamer
                | Gain::Muff
                | Gain::Boogie
                | Gain::Peavey
                | Gain::Neve
                | Gain::Twin
                | Gain::Brit800
                | Gain::American312
                | Gain::ConsoleE
                | Gain::Tube610
                | Gain::Plexi
                | Gain::AC30
                | Gain::DR103
                | Gain::Recto
                | Gain::Deluxe
                | Gain::Jazz120
                | Gain::DeluxeNormal
                | Gain::Brit2205
                | Gain::Ds1
                | Gain::TrebleBoost
                | Gain::PlexiBass
                | Gain::Brit45
                | Gain::Brum100
                | Gain::OregonT
                | Gain::GoldDrive
                | Gain::BritDrive
                | Gain::CleanBoost
                | Gain::AmericanSvt
                | Gain::British47
                | Gain::German76
                | Gain::BassDriver
                | Gain::American800RB
                | Gain::OrangePhase
                | Gain::BlueChorus
                | Gain::Modern33
                | Gain::ModernPurple
                | Gain::AmericanVt40
                | Gain::AmericanV4b
        )
    }

    /// Whether the choice of amplifying part reaches this circuit.
    ///
    /// Only the preamplifier channels are built around a part that can be
    /// swapped. The guitar circuits are valve cascades by definition -- a
    /// "three cascaded stages" made of op-amps is a different thing with the
    /// same name -- and the pedals are built around their diodes.
    pub const fn has_amplifier(self) -> bool {
        matches!(self, Gain::Console | Gain::Studio)
    }

    /// Whether the diode choice reaches this circuit at all. A valve stage has
    /// no diodes in it, and offering the choice there would be a control that
    /// does nothing -- which is worse than not offering it.
    pub const fn has_diodes(self) -> bool {
        matches!(self, Gain::Overdrive | Gain::Distortion)
    }

    /// What a Fender AB763 channel exposes to the chain, where this voice is
    /// one.
    ///
    /// The American Twin and the American Deluxe are the same design at
    /// different sizes: a switched pair of input jacks, a spring tank driven
    /// from the reverb transformer's secondary and returned through an
    /// independent port, and an optical tremolo whose photoresistor is a real
    /// audio-rate resistor in the netlist. The chain drives all three
    /// identically, so only the slot and control numbers live here rather than
    /// in a `match` that grows a case per amplifier.
    pub fn ab763(self) -> Option<Ab763> {
        match self {
            Gain::Twin => Some(Ab763 {
                input_series: twin::INPUT_SERIES_SLOT,
                input_jack_load: twin::INPUT_JACK_LOAD_SLOT,
                input_grid_shunt: twin::INPUT_GRID_SHUNT_SLOT,
                high_series: twin::INPUT_HIGH_SERIES_OHMS,
                high_jack_load: twin::INPUT_HIGH_JACK_LOAD_OHMS,
                low_series: twin::INPUT_LOW_SERIES_OHMS,
                low_grid_shunt: twin::INPUT_LOW_GRID_SHUNT_OHMS,
                open: twin::INPUT_OPEN_OHMS,
                bright: Some((
                    twin::BRIGHT_CAP_SLOT,
                    twin::BRIGHT_CAP_FARADS,
                    twin::BRIGHT_OFF_FARADS,
                )),
                ldr_slot: twin::LDR_SLOT,
                tank_return_aux: twin::TANK_RETURN_AUX,
                reverb: twin::REVERB,
                intensity: twin::INTENSITY,
            }),
            Gain::Deluxe => Some(Ab763 {
                input_series: deluxe::INPUT_SERIES_SLOT,
                input_jack_load: deluxe::INPUT_JACK_LOAD_SLOT,
                input_grid_shunt: deluxe::INPUT_GRID_SHUNT_SLOT,
                high_series: deluxe::INPUT_HIGH_SERIES_OHMS,
                high_jack_load: deluxe::INPUT_HIGH_JACK_LOAD_OHMS,
                low_series: deluxe::INPUT_LOW_SERIES_OHMS,
                low_grid_shunt: deluxe::INPUT_LOW_GRID_SHUNT_OHMS,
                open: deluxe::INPUT_OPEN_OHMS,
                // The Deluxe's 47 pF bright capacitor is soldered in: the
                // panel has no switch for it, so offering one would be a
                // control the amplifier has not got.
                bright: None,
                ldr_slot: deluxe::LDR_SLOT,
                tank_return_aux: deluxe::TANK_RETURN_AUX,
                reverb: deluxe::REVERB,
                intensity: deluxe::INTENSITY,
            }),
            _ => None,
        }
    }

    /// The switched input jacks, where the circuit has a pair.
    ///
    /// Both AB763s and the Jazz 120. The AB763s answer out of `ab763()` so
    /// their numbers stay in one place; the Jazz 120 is the simpler case and
    /// states its own.
    pub fn input_jacks(self) -> Option<InputJacks> {
        if self == Gain::American800RB {
            // One jack and the -10 dB switch: R3 across R5.
            return Some(InputJacks {
                series: american_800rb::PAD_SLOT,
                high_series: american_800rb::SWITCH_OPEN,
                low_series: american_800rb::SWITCH_CLOSED,
                jack_load: None,
                grid_shunt: None,
                high_label: "Normal",
                low_label: "-10 dB",
            });
        }
        if self == Gain::Jazz120 {
            // R1 33 k on HIGH and R2 68 k on LOW into the same node, and the
            // sheet's own sensitivities beside them.
            return Some(InputJacks {
                series: jazz120::INPUT_SERIES_SLOT,
                high_series: jazz120::INPUT_HIGH_SERIES_OHMS,
                low_series: jazz120::INPUT_LOW_SERIES_OHMS,
                jack_load: None,
                grid_shunt: None,
                high_label: "High (-30 dBm)",
                low_label: "Low (-20 dBm)",
            });
        }
        if self == Gain::DeluxeNormal {
            // The Normal channel's jacks are the Vibrato channel's, part for
            // part; only the channel behind them differs.
            return Some(InputJacks {
                series: deluxe::INPUT_SERIES_SLOT,
                high_series: deluxe::INPUT_HIGH_SERIES_OHMS,
                low_series: deluxe::INPUT_LOW_SERIES_OHMS,
                jack_load: Some((
                    deluxe::INPUT_JACK_LOAD_SLOT,
                    deluxe::INPUT_HIGH_JACK_LOAD_OHMS,
                    deluxe::INPUT_OPEN_OHMS,
                )),
                grid_shunt: Some((
                    deluxe::INPUT_GRID_SHUNT_SLOT,
                    deluxe::INPUT_LOW_GRID_SHUNT_OHMS,
                    deluxe::INPUT_OPEN_OHMS,
                )),
                high_label: "High 1 (1 MOhm)",
                low_label: "Low 2 (-6 dB)",
            });
        }
        let a = self.ab763()?;
        Some(InputJacks {
            series: a.input_series,
            high_series: a.high_series,
            low_series: a.low_series,
            jack_load: Some((a.input_jack_load, a.high_jack_load, a.open)),
            grid_shunt: Some((a.input_grid_shunt, a.low_grid_shunt, a.open)),
            high_label: "High 1 (1 MOhm)",
            low_label: "Low 2 (-6 dB)",
        })
    }

    /// The Bright switch, where the panel has one rather than a capacitor
    /// soldered in. The Deluxe has the capacitor and no switch, so it is
    /// `None` there and the panel greys the control.
    /// Where the circuit's own direct out is taken, when it has one ahead of
    /// its output: the node the Preamp DI reads instead of the output. The
    /// 800RB's is "after the effects loop", ahead of the boost that is the
    /// circuit's last stage here. Levelled by `DIRECT_OUT_DB`, which
    /// `examples/directout.rs` measures. See `docs/DI.md`.
    pub fn direct_out(self) -> Option<&'static str> {
        match self {
            Gain::American800RB => Some(american_800rb::DIRECT_OUT),
            _ => None,
        }
    }

    pub fn bright_switch(self) -> Option<BrightSwitch> {
        if self == Gain::American800RB {
            // HI BOOST: shorts R23 so C26 and R28 bridge the volume.
            return Some(BrightSwitch {
                slot: american_800rb::HI_BOOST_SLOT,
                on: american_800rb::SWITCH_CLOSED,
                off: american_800rb::SWITCH_OPEN,
                on_label: "Hi Boost",
            });
        }
        if self == Gain::AmericanSvt {
            // ULTRA HI: C6 from VR1's top to its wiper. See `circuits::american_svt`.
            return Some(BrightSwitch {
                slot: american_svt::ULTRA_HI_SLOT,
                on: american_svt::ULTRA_HI_FARADS,
                off: american_svt::ULTRA_HI_OFF_FARADS,
                on_label: "Ultra Hi (500 pF)",
            });
        }
        if self == Gain::AmericanVt40 {
            // SW3 ULTRA HI, up: C102 from VR101's top to its wiper. Its third
            // position is not offered. See `circuits::american_vt40`.
            return Some(BrightSwitch {
                slot: american_vt40::ULTRA_HI_SLOT,
                on: american_vt40::ULTRA_HI_FARADS,
                off: american_vt40::ULTRA_HI_OFF_FARADS,
                on_label: "Ultra Hi (120 pF)",
            });
        }
        if self == Gain::AmericanV4b {
            // SW2 ULTRA HI: C105 from VR101's top to its wiper.
            return Some(BrightSwitch {
                slot: american_v4b::ULTRA_HI_SLOT,
                on: american_v4b::ULTRA_HI_FARADS,
                off: american_v4b::ULTRA_HI_OFF_FARADS,
                on_label: "Ultra Hi (500 pF)",
            });
        }
        if self == Gain::Jazz120 {
            // SW2 does not switch C7 in and out: it shorts R4 so the 330 pF
            // couples fully. See `circuits::jazz120`.
            return Some(BrightSwitch {
                slot: jazz120::BRIGHT_SLOT,
                on: jazz120::BRIGHT_ON_OHMS,
                off: jazz120::BRIGHT_OFF_OHMS,
                on_label: "On (330 pF)",
            });
        }
        let (slot, on, off) = self.ab763()?.bright?;
        Some(BrightSwitch {
            slot,
            on,
            off,
            on_label: "On (120 pF)",
        })
    }

    /// The circuit's low-frequency three-position switch, where it has one:
    /// the SVT's BASS CUT / OFF / ULTRA LO. The netlist is built in the centre
    /// position, which is where it is calibrated.
    pub fn low_switch(self) -> Option<CircuitSwitch> {
        match self {
            Gain::AmericanSvt => Some(CircuitSwitch {
                slots: &american_svt::BASS_SELECT_SLOTS,
                values: &[
                    &american_svt::BASS_SELECT[0],
                    &american_svt::BASS_SELECT[1],
                    &american_svt::BASS_SELECT[2],
                ],
                labels: &["Bass Cut", "Off", "Ultra Lo"],
            }),
            // The V76's low cut, its flat position second as every circuit's
            // default is: 80 Hz, flat, 300 Hz, 80 + 300 Hz.
            // LO CUT, flat second.
            Gain::American800RB => Some(CircuitSwitch {
                slots: &american_800rb::LO_CUT_SLOTS,
                values: &[&american_800rb::LO_CUT[0], &american_800rb::LO_CUT[1]],
                labels: &["Lo Cut", "Flat"],
            }),
            // The V-4B's ULTRA LO, off second, as it is built.
            Gain::AmericanV4b => Some(CircuitSwitch {
                slots: &american_v4b::ULTRA_LO_SLOTS,
                values: &[&american_v4b::ULTRA_LO[0], &american_v4b::ULTRA_LO[1]],
                labels: &["Ultra Lo", "Off"],
            }),
            // BASS SHIFT, the built position second: 40 Hz, 80 Hz.
            Gain::BassDriver => Some(CircuitSwitch {
                slots: &bass_driver::BASS_SHIFT_SLOTS,
                values: &[&bass_driver::BASS_SHIFT[0], &bass_driver::BASS_SHIFT[1]],
                labels: &["40 Hz", "80 Hz"],
            }),
            Gain::German76 => Some(CircuitSwitch {
                slots: &german_76::LOW_CUT_SLOTS,
                values: &[
                    &german_76::LOW_CUT[0],
                    &german_76::LOW_CUT[1],
                    &german_76::LOW_CUT[2],
                    &german_76::LOW_CUT[3],
                ],
                labels: &["80 Hz", "Flat", "300 Hz", "80+300"],
            }),
            _ => None,
        }
    }

    /// The circuit's midrange three-position switch, where it has one: the
    /// SVT's 1-2-3, 220 Hz, 800 Hz and 3 kHz. Built in the centre position.
    pub fn mid_switch(self) -> Option<CircuitSwitch> {
        match self {
            Gain::AmericanSvt => Some(CircuitSwitch {
                slots: &american_svt::MID_SELECT_SLOTS,
                values: &[
                    &american_svt::MID_SELECT[0],
                    &american_svt::MID_SELECT[1],
                    &american_svt::MID_SELECT[2],
                ],
                labels: &["220 Hz", "800 Hz", "3 kHz"],
            }),
            // The VT-40's SW5, the same switch around another toroid.
            Gain::AmericanVt40 => Some(CircuitSwitch {
                slots: &american_vt40::MID_SELECT_SLOTS,
                values: &[
                    &american_vt40::MID_SELECT[0],
                    &american_vt40::MID_SELECT[1],
                    &american_vt40::MID_SELECT[2],
                ],
                labels: &["300 Hz", "800 Hz", "3 kHz"],
            }),
            Gain::AmericanV4b => Some(CircuitSwitch {
                slots: &american_v4b::MID_SELECT_SLOTS,
                values: &[
                    &american_v4b::MID_SELECT[0],
                    &american_v4b::MID_SELECT[1],
                    &american_v4b::MID_SELECT[2],
                ],
                labels: &["300 Hz", "800 Hz", "3 kHz"],
            }),
            // The V76's "Gerade / 3 kHz", flat second.
            // MID CONTOUR, flat second.
            Gain::American800RB => Some(CircuitSwitch {
                slots: &american_800rb::CONTOUR_SLOTS,
                values: &[&american_800rb::CONTOUR[0], &american_800rb::CONTOUR[1]],
                labels: &["Contour", "Flat"],
            }),
            // MID SHIFT, the built position second: 1 kHz, 500 Hz.
            Gain::BassDriver => Some(CircuitSwitch {
                slots: &bass_driver::MID_SHIFT_SLOTS,
                values: &[&bass_driver::MID_SHIFT[0], &bass_driver::MID_SHIFT[1]],
                labels: &["1 kHz", "500 Hz"],
            }),
            Gain::German76 => Some(CircuitSwitch {
                slots: &german_76::TREBLE_CUT_SLOTS,
                values: &[&german_76::TREBLE_CUT[0], &german_76::TREBLE_CUT[1]],
                labels: &["3 kHz", "Flat"],
            }),
            _ => None,
        }
    }

    /// The spring reverb this voice carries in its own netlist, where it has
    /// one: both AB763s and the VT-40. The panel greys the Reverb knob
    /// everywhere else rather than leaving a knob that turns nothing -- which
    /// is the defect BUG-023 was about, from the other side.
    pub fn spring_reverb(self) -> Option<SpringReverb> {
        if let Some(a) = self.ab763() {
            return Some(SpringReverb {
                tank_return_aux: a.tank_return_aux,
                reverb: a.reverb,
                drive_scale: 1.0,
            });
        }
        match self {
            Gain::AmericanVt40 => Some(SpringReverb {
                tank_return_aux: american_vt40::TANK_RETURN_AUX,
                reverb: american_vt40::REVERB,
                drive_scale: american_vt40::TANK_DRIVE_SCALE,
            }),
            _ => None,
        }
    }

    /// Whether this voice has a spring reverb of its own. See `spring_reverb`.
    pub fn has_reverb(self) -> bool {
        self.spring_reverb().is_some()
    }

    /// Whether this voice has a tremolo of its own: only the AB763s' Vibrato
    /// channels. The panel greys Speed and Intensity everywhere else.
    pub fn has_tremolo(self) -> bool {
        self.ab763().is_some()
    }

    /// Whether this voice's Drive parameter **is** the amplifier's own channel
    /// Volume control, modelled in the netlist, rather than a gain control
    /// ahead of the distortion.
    ///
    /// Where it is, the drive-dependent make-up must not be applied. The
    /// make-up curve is measured by `examples/calibrate` sweeping this very
    /// control, so applying it back cancels the control almost exactly: the
    /// guitar stays near one loudness while the make-up swings tens of
    /// decibels end to end, and the large boost at the bottom of the travel
    /// exaggerates tremolo, noise and switching transients. `set_drive` freezes
    /// the conversion at `CHANNEL_VOLUME_CALIBRATION_REFERENCE` for these and
    /// lets the modelled pot decide the level, as the hardware does.
    ///
    /// Both AB763s and the Jazz 120. The Jazz 120 was missing here until
    /// 2026-09-23 and the symptom was concrete: the make-up cancelled VR1, so
    /// turning Drive down did not quieten the amplifier and did not stop it
    /// driving its own power stage 3.7 times past full output. That is the
    /// same shape of defect as `power_stage` against `build_power` -- one
    /// question with two answers, and the amplifier caught between them.
    ///
    /// The Treble Boost too, and for the plainest reason of all: its one pot is
    /// the collector load of a stage whose gain does not change, so the pot is
    /// a volume and nothing else, and a make-up measured across it would make
    /// the knob do nothing whatever.
    ///
    /// The Brit 800, Plexi, AC30 and DR103 also put Drive on a Volume pot and
    /// are deliberately **not** here. Their make-up curves are what their
    /// shipped presets were trimmed against, so moving them is a separate,
    /// deliberate change with its own re-trimming, not a tidy-up to fold into
    /// this one.
    ///
    /// And the Orange Phase and the Blue Chorus, whose Drive knob is their
    /// Speed and Rate: they change no gain at all, and a make-up measured
    /// across them would only follow where the sweep happened to be when each
    /// point was taken.
    pub fn drive_is_channel_volume(self) -> bool {
        self.ab763().is_some()
            || matches!(
                self,
                Gain::Jazz120
                    | Gain::TrebleBoost
                    | Gain::CleanBoost
                    | Gain::OrangePhase
                    | Gain::BlueChorus
                    | Gain::Modern33
            )
    }

    /// A bucket brigade the circuit sends to and takes back from, when it has
    /// one between two halves of its own netlist: the node its input is
    /// driven at, the auxiliary input its output drives, the node its clock
    /// follows, and the delay that node's voltage sets. The delay itself is a
    /// `dsp::bbd::Brigade` held by the chain; everything either side of it is
    /// solved. See `circuits::blue_chorus`.
    pub fn bucket_brigade(self) -> Option<BucketBrigade> {
        match self {
            Gain::BlueChorus => Some(BucketBrigade {
                send: blue_chorus::BBD_IN,
                ret: blue_chorus::BBD_RETURN,
                control: blue_chorus::CLOCK_CONTROL,
                delay: blue_chorus::delay_seconds,
            }),
            _ => None,
        }
    }

    /// Whether this voice has a bucket-brigade chorus of its own.
    ///
    /// Only the Jazz 120 does, and on that amplifier it is not an effect hung
    /// on the end: `CN6` carries the delayed signal to one of the two power
    /// amplifiers and its 12", and the other gets the dry. See
    /// `Chain::process` for where the split is taken and why.
    pub const fn has_chorus(self) -> bool {
        matches!(self, Gain::Jazz120)
    }

    pub const fn variants(self) -> usize {
        if self.has_diodes() || self.has_amplifier() {
            3
        } else {
            1
        }
    }
}

/// Where a voice's spring tank meets its netlist. See `Gain::spring_reverb`.
#[derive(Clone, Copy, Debug)]
pub struct SpringReverb {
    /// The netlist's auxiliary input the tank's pickup returns through.
    pub tank_return_aux: usize,
    /// The REVERB pot's control number.
    pub reverb: usize,
    /// What the send node's volts are multiplied by to make the drive
    /// `dsp::spring::Tank` is calibrated for: volts across the 4AB3C1B's 8 ohm
    /// coil. One for the AB763s, which drive that tank; see
    /// `american_vt40::TANK_DRIVE_SCALE` for the VT-40's.
    pub drive_scale: f64,
}

/// The slot and control numbers of a Fender AB763 channel. See `Gain::ab763`.
#[derive(Clone, Copy, Debug)]
pub struct Ab763 {
    pub input_series: usize,
    pub input_jack_load: usize,
    pub input_grid_shunt: usize,
    pub high_series: f64,
    pub high_jack_load: f64,
    pub low_series: f64,
    pub low_grid_shunt: f64,
    pub open: f64,
    /// Slot, fitted value and open value of the Bright capacitor, for the
    /// panels that switch it. `None` where it is soldered in.
    pub bright: Option<(usize, f64, f64)>,
    pub ldr_slot: usize,
    pub tank_return_aux: usize,
    pub reverb: usize,
    pub intensity: usize,
}

/// A switched pair of input jacks. See `Gain::input_jacks`.
///
/// Every amplifier that has two jacks has them for the same reason and wires
/// them differently. An AB763 switches three parts at once -- the series
/// resistance, the jack's own load and the grid shunt -- because the unused
/// jack's normalling rearranges the pair of 68 k stoppers. A JC-120 has two
/// resistors into one node and nothing else, so the last two are `None` there
/// rather than slots that would have to be invented.
#[derive(Clone, Copy, Debug)]
pub struct InputJacks {
    pub series: usize,
    pub high_series: f64,
    pub low_series: f64,
    /// Slot, value with this jack in use, value with it open.
    pub jack_load: Option<(usize, f64, f64)>,
    pub grid_shunt: Option<(usize, f64, f64)>,
    /// What the panel calls them.
    pub high_label: &'static str,
    pub low_label: &'static str,
}

/// A Bright switch that is a real part in the netlist. See
/// `Gain::bright_switch`.
#[derive(Clone, Copy, Debug)]
pub struct BrightSwitch {
    pub slot: usize,
    /// The value with the switch closed, and with it open.
    pub on: f64,
    pub off: f64,
    pub on_label: &'static str,
}

/// Where one of the panel's circuit switches is thrown: up to four
/// positions. Every circuit puts its own default in the second, `Centre`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Throw {
    Left,
    #[default]
    Centre,
    Right,
    Far,
}

impl Throw {
    pub fn index(self) -> usize {
        match self {
            Throw::Left => 0,
            Throw::Centre => 1,
            Throw::Right => 2,
            Throw::Far => 3,
        }
    }
}

/// A circuit's own switch that is real parts in its netlist, as the panel's
/// low and mid switch selectors reach it: the adjustable parts it moves, the
/// value each takes in each of its two to four positions, and what the panel
/// calls them. A position past the switch's last is its last. The netlist is
/// built in position two, which is where the circuit is calibrated. See
/// `Gain::low_switch` and `Gain::mid_switch`.
#[derive(Clone, Copy, Debug)]
pub struct CircuitSwitch {
    pub slots: &'static [usize],
    pub values: &'static [&'static [f64]],
    pub labels: &'static [&'static str],
}

impl CircuitSwitch {
    /// The values for `throw`, the last position's past the end.
    pub fn at(&self, throw: Throw) -> &'static [f64] {
        self.values[throw.index().min(self.values.len() - 1)]
    }
}

/// Where the dry signal -- what the Mix knob blends against the amplifier --
/// is taken from. The input, as it always was: a DI box between the bass and
/// the amplifier. The pedal's output: a DI pedal's balanced output, while the
/// amplifier gets the pedal's signal. Or the circuit's output, ahead of the
/// power stage: an amplifier's own direct out, as the GK 800RB's is. None of
/// them carries the power stage, the speaker or a microphone, which is what
/// makes it a DI. See `docs/DI.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DrySource {
    #[default]
    Input,
    Pedal,
    Preamp,
}

/// Which part does the amplifying, for the channels built around one.
///
/// The axis the hardware actually varies along, and the one the panel has to
/// expose: a console channel with a bottle in it instead of a transistor is a
/// different and much-argued-about box built from the same schematic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amplifier {
    Valve,
    Jfet,
    OpAmp,
}

impl Amplifier {
    pub const ALL: [Amplifier; 3] = [Amplifier::Valve, Amplifier::Jfet, Amplifier::OpAmp];

    pub fn name(self) -> &'static str {
        match self {
            Amplifier::Valve => "Valve",
            Amplifier::Jfet => "JFET",
            Amplifier::OpAmp => "Op-amp",
        }
    }

    fn index(self) -> usize {
        match self {
            Amplifier::Valve => 0,
            Amplifier::Jfet => 1,
            Amplifier::OpAmp => 2,
        }
    }

    fn spec(self) -> studio::Amplifier {
        match self {
            Amplifier::Valve => studio::Amplifier::Valve,
            Amplifier::Jfet => studio::Amplifier::Jfet,
            Amplifier::OpAmp => studio::Amplifier::OpAmp,
        }
    }
}

/// Which diodes, for the circuits that have any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diode {
    Silicon,
    Germanium,
    Led,
}

impl Diode {
    pub const ALL: [Diode; 3] = [Diode::Silicon, Diode::Germanium, Diode::Led];

    pub fn name(self) -> &'static str {
        match self {
            Diode::Silicon => "Silicon",
            Diode::Germanium => "Germanium",
            Diode::Led => "LED",
        }
    }

    fn index(self) -> usize {
        match self {
            Diode::Silicon => 0,
            Diode::Germanium => 1,
            Diode::Led => 2,
        }
    }

    fn spec(self) -> DiodeSpec {
        match self {
            Diode::Silicon => DiodeSpec::SILICON,
            Diode::Germanium => DiodeSpec::GERMANIUM,
            Diode::Led => DiodeSpec::LED,
        }
    }
}

/// Every gain circuit that can be selected, as one flat list.
///
/// The plugin builds all of them once and then switches by index, so changing
/// the circuit while playing allocates nothing. Laid out by walking `Gain::ALL`
/// and giving each topology one slot per part it can be built with, so adding
/// a circuit does not move the ones already there.
/// Counted from `Gain::ALL` rather than written down, so that adding a
/// circuit cannot leave this behind. It was a hand-kept `20`, and adding the
/// Twin made it wrong: the calibration table is `[Calibration; VOICES]`, so a
/// stale count is a table with a missing row and every voice after the new one
/// reading its neighbour's make-up.
/// How many entries the circuit list has, which is what `POWER_TRIM_DB` is
/// indexed by. Tied to the list so that adding a circuit fails to compile
/// rather than indexing past the end of the table at runtime, which is what it
/// did when six were appended.
pub const GAINS: usize = Gain::ALL.len();

pub const VOICES: usize = {
    let mut total = 0;
    let mut i = 0;
    while i < Gain::ALL.len() {
        total += Gain::ALL[i].variants();
        i += 1;
    }
    total
};

/// Where a topology's first slot is.
fn first_of(gain: Gain) -> usize {
    Gain::ALL
        .iter()
        .take_while(|g| **g != gain)
        .map(|g| g.variants())
        .sum()
}

/// Where a (circuit, part) combination lives in that list.
pub fn voice_index(gain: Gain, diode: Diode, amplifier: Amplifier) -> usize {
    let within = if gain.has_diodes() {
        diode.index()
    } else if gain.has_amplifier() {
        amplifier.index()
    } else {
        0
    };
    first_of(gain) + within
}

/// The combination at an index, which is the inverse of the above and exists
/// so a table can be built by walking the list.
/// How far the modelled circuits follow the Oversampling control.
///
/// A nonlinear stage makes harmonics, and the ones above half the sample rate
/// fold back down onto frequencies that have nothing to do with the note. That
/// fold-back is the hash a high-gain amplifier adds at 48 kHz, and the only
/// real cure is to solve the circuit faster than the host.
///
/// These circuits are big nonlinear solves and the cure is priced by the
/// factor. Measured at full drive on a 1760 Hz note, as inharmonic energy
/// against the fundamental, alongside what one channel costs of its real-time
/// budget (`examples/oversampling.rs`):
///
/// | | 1x | 2x | 4x | 8x |
/// |---|---|---|---|---|
/// | American 5150 | 18.6 % / 34 % | 6.5 % / 67 % | 3.4 % / 114 % | 1.7 % / 193 % |
/// | Brit 800 | 12.1 % / 21 % | 2.7 % / 40 % | 1.3 % / 71 % | 1.2 % / 142 % |
/// | Cali Rectifier | 7.2 % / 25 % | 2.9 % / 45 % | 0.8 % / 85 % | 0.2 % / 162 % |
/// | Cali IIC+ | 5.1 % / 32 % | 1.0 % / 57 % | 0.3 % / 109 % | 0.2 % / 212 % |
///
/// Two is where the trade sits. It takes about two thirds of the fold-back
/// away for about double the work, and it is the last factor that fits: past
/// it every one of these circuits costs more than the time there is, which is
/// a DAW missing its deadline -- crackle, stuttering live input, playback
/// falling behind. That was what pinning them to 1x was avoiding when the
/// control was first made to skip them; the pin also meant the control did
/// nothing at all for exactly the circuits that alias most, which is this cap
/// instead.
///
/// Every shipped preset on a modelled circuit asks for 1x, so none of them
/// costs any more than it did; this is what the control does when a player
/// turns it up.
///
/// A topology with a power stage behind it is capped here too
/// (`caps_oversampling`). Measured the same way with the cap lifted, fold-back
/// at 1760 Hz and cost a channel:
///
/// | | 1x | 2x | 4x | 8x |
/// |---|---|---|---|---|
/// | High Gain + American 6550 | 17.3 % / 33 % | 7.0 % / 46 % | 3.7 % / 81 % | 1.9 % / 125 % |
/// | Crunch + Brit EL34 | 2.0 % / 11 % | 0.2 % / 21 % | 0.1 % / 41 % | 0.1 % / 79 % |
/// | Distortion + American 6L6 | 7.5 % / 12 % | 0.7 % / 22 % | 0.3 % / 43 % | 0.3 % / 83 % |
///
/// No shipped preset puts a power stage behind a topology.
pub const MODELLED_MAX_OVERSAMPLING: usize = 2;

/// Whether a chain follows the Oversampling control only as far as
/// `MODELLED_MAX_OVERSAMPLING`: a modelled circuit, or any chain with a power
/// stage in it -- the voice's own or one chosen in its place.
///
/// The power stage runs inside the oversampler, after the preamplifier, and it
/// is the heaviest solve in any chain that has one, so a topology with a power
/// stage behind it is priced like a modelled amplifier: the worst rig a player
/// could dial in, Treble Boost, High Gain and the American 6550 stage, took
/// 4.75 ms a 1.33 ms callback at 8x, 3.96 of them in the power stage
/// (`examples/stress.rs`, `docs/realtime-catalogue.md`). A topology on its own
/// is cheap enough to follow the control all the way.
pub fn caps_oversampling(gain: Gain, power: PowerAmp) -> bool {
    gain.is_modelled() || power.resolved(gain).is_some()
}

/// The oversampling factor a chain actually runs at for a requested one.
///
/// A modelled circuit, and a chain with a power stage, are capped at
/// `MODELLED_MAX_OVERSAMPLING` (`caps_oversampling`), and an expensive pedal in
/// the slot takes the whole oversampled path down to 1x, because the pedal runs
/// inside it. See `Chain::set_oversampling`, which applies this.
pub fn effective_oversampling(
    gain: Gain,
    pedal: Pedal,
    power: PowerAmp,
    requested: usize,
) -> usize {
    if pedal != Pedal::None && pedal.is_expensive() {
        1
    } else if caps_oversampling(gain, power) {
        requested.min(MODELLED_MAX_OVERSAMPLING)
    } else {
        requested
    }
}

/// The true latency, in host samples, of a chain running `gain` with `pedal`
/// and `power` at the `requested` oversampling: what `Chain::latency` reports
/// once the chain is in true-latency mode and those settings have been applied.
pub fn true_latency(gain: Gain, pedal: Pedal, power: PowerAmp, requested: usize) -> u32 {
    Oversampler::latency_of(effective_oversampling(gain, pedal, power, requested))
}

pub fn voice_at(index: usize) -> (Gain, Diode, Amplifier) {
    let mut at = 0;
    for gain in Gain::ALL {
        let n = gain.variants();
        if index < at + n {
            let within = index - at;
            return (
                gain,
                if gain.has_diodes() {
                    Diode::ALL[within]
                } else {
                    Diode::Silicon
                },
                if gain.has_amplifier() {
                    Amplifier::ALL[within]
                } else {
                    Amplifier::Valve
                },
            );
        }
        at += n;
    }
    (Gain::Clean, Diode::Silicon, Amplifier::Valve)
}

/// Output stages that are nobody's matched stage, and so have no voice to
/// borrow a resistor-loaded simulation from. They sit after the catalogue in
/// `Chain::powers`, in this order.
pub const EXTRA_POWER_SPECS: [&power::PowerSpec; 2] = [
    &power::PowerSpec::RECTO_6L6_TUBE,
    &power::PowerSpec::DR103_EL34_RETURN,
];

/// How many of them there are.
pub const EXTRA_POWERS: usize = EXTRA_POWER_SPECS.len();

/// Physical power circuit identity, independent of the preamp catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerModel {
    Cali6L6,
    American6L6Clean,
    American6L6HighGain,
    BritEL34,
    BritPlexiEL34,
    AC30EL84,
    DR103EL34,
    Recto6L6,
    Recto6L6Tube,
    /// The AB763 Deluxe's two 6V6GT behind a GZ34.
    AmericanDeluxe6V6,
    /// The JC-120's 60 W transistor amplifier, and the first output stage here
    /// that is not a valve one. See `circuits::jc120_power`.
    Jazz120SS,
    /// The 73P's OUTPUT block: two BC109C into a TIP3055 and the VTB1148. Not
    /// a power amplifier at all -- it drives a line -- but it is the block
    /// behind that voice, and a voice's block has to be in its path or its
    /// calibration is describing something that is not being played.
    British73Out,
    /// The JCM800 2205's two EL34s. See `power::PowerSpec::BRIT_2205_EL34`.
    Brit2205EL34,
    /// The DR103's stage as another preamplifier meets it: from the inverter,
    /// without the Hiwatt's own V3a. What `PowerAmp::DR103EL34` means behind
    /// anything but the DR103. See `power::PowerSpec::DR103_EL34_RETURN`.
    DR103EL34Return,
    /// The 1992 Super Bass's four EL34s. See `power::PowerSpec::PLEXI_BASS_EL34`.
    BritPlexiBassEL34,
    /// The Laney Supergroup's four EL34s on 600 V. See
    /// `power::PowerSpec::BRUM_EL34`.
    BrumEL34,
    /// The Sunn Model T's four 6550s, ultra-linear. See
    /// `power::PowerSpec::OREGON_6550`.
    Oregon6550,
    /// The Ampeg SVT's six 6550s behind a cathodyne and two direct-coupled
    /// followers. See `power::PowerSpec::SVT_6550`.
    Svt6550,
    /// The GK 800RB's 300 W transistor amplifier. See `circuits::american_ss800`.
    AmericanSS800,
    /// The JTM45's two KT66s behind a GZ34. See `power::PowerSpec::JTM45_KT66`.
    Brit45KT66,
    /// The VT-40's two 7027As behind a floating paraphase. See
    /// `power::PowerSpec::VT40_7027A`.
    American7027A,
    /// The V-4B's four 7027As. See `power::PowerSpec::V4B_7027A`.
    AmericanV4b7027A,
}

impl PowerModel {
    pub const ALL: [PowerModel; 22] = [
        PowerModel::Cali6L6,
        PowerModel::American6L6Clean,
        PowerModel::American6L6HighGain,
        PowerModel::BritEL34,
        PowerModel::BritPlexiEL34,
        PowerModel::AC30EL84,
        PowerModel::DR103EL34,
        PowerModel::Recto6L6,
        PowerModel::Recto6L6Tube,
        PowerModel::AmericanDeluxe6V6,
        PowerModel::Jazz120SS,
        PowerModel::British73Out,
        PowerModel::Brit2205EL34,
        PowerModel::DR103EL34Return,
        PowerModel::BritPlexiBassEL34,
        PowerModel::BrumEL34,
        PowerModel::Oregon6550,
        PowerModel::Svt6550,
        PowerModel::AmericanSS800,
        PowerModel::Brit45KT66,
        PowerModel::American7027A,
        PowerModel::AmericanV4b7027A,
    ];

    /// The valve stage this model is, where it is one.
    ///
    /// `None` for the Jazz 120, and that is the point of the option rather
    /// than an oversight: a `PowerSpec` is a phase inverter, a bias supply and
    /// an output transformer, and a complementary transistor amplifier has
    /// none of the three. Every caller has to say what it does about that,
    /// which is how the two definitions of "the block behind this voice" stop
    /// drifting apart again.
    pub fn spec(self) -> Option<&'static power::PowerSpec> {
        Some(match self {
            Self::Cali6L6 => &power::PowerSpec::MARKIIC,
            Self::American6L6Clean => &power::PowerSpec::TWIN,
            Self::American6L6HighGain => &power::PowerSpec::EVH5150,
            Self::BritEL34 => &power::PowerSpec::BRIT_EL34,
            Self::BritPlexiEL34 => &power::PowerSpec::PLEXI_EL34,
            Self::AC30EL84 => &power::PowerSpec::AC30_EL84,
            Self::DR103EL34 => &power::PowerSpec::DR103_EL34,
            Self::Recto6L6 => &power::PowerSpec::RECTO_6L6,
            Self::Recto6L6Tube => &power::PowerSpec::RECTO_6L6_TUBE,
            Self::AmericanDeluxe6V6 => &power::PowerSpec::DELUXE_6V6,
            Self::Brit2205EL34 => &power::PowerSpec::BRIT_2205_EL34,
            Self::DR103EL34Return => &power::PowerSpec::DR103_EL34_RETURN,
            Self::BritPlexiBassEL34 => &power::PowerSpec::PLEXI_BASS_EL34,
            Self::BrumEL34 => &power::PowerSpec::BRUM_EL34,
            Self::Oregon6550 => &power::PowerSpec::OREGON_6550,
            Self::Svt6550 => &power::PowerSpec::SVT_6550,
            Self::Brit45KT66 => &power::PowerSpec::JTM45_KT66,
            Self::American7027A => &power::PowerSpec::VT40_7027A,
            Self::AmericanV4b7027A => &power::PowerSpec::V4B_7027A,
            Self::Jazz120SS | Self::British73Out | Self::AmericanSS800 => return None,
        })
    }

    /// What the stage's presence slot is called on its panel, if it has one.
    /// See `power::PowerSpec::presence_name`.
    pub fn presence_name(self) -> Option<&'static str> {
        self.spec().and_then(power::PowerSpec::presence_name)
    }

    /// The speaker-loaded circuit for this stage, whatever kind it is. One
    /// place that knows how each kind is built, rather than a `match` at every
    /// call site that wants one.
    /// The node the speaker is stamped at in `build_loaded`'s circuit: its
    /// terminals, which a horn's crossover is fed from.
    pub fn speaker_terminal(self) -> &'static str {
        match self.spec() {
            Some(_) => "spk",
            None if self == Self::British73Out => "spk",
            None if self == Self::AmericanSS800 => american_ss800::OUTPUT,
            None => jc120_power::OUTPUT,
        }
    }

    pub fn build_loaded(self, load: &LoadValues) -> Result<(Netlist, speaker::LoadSlots), Fault> {
        match self.spec() {
            Some(spec) => power::build_with_speaker(spec, 10_000.0, load),
            None if self == Self::British73Out => neve::output_with_speaker(LOAD, load),
            None if self == Self::AmericanSS800 => {
                american_ss800::build_with_speaker(3_300.0, load)
            }
            None => jc120_power::build_with_speaker(1_000.0, load),
        }
    }

    /// How far this stage's nominal load is from the profile's own impedance,
    /// so one measured driver can stand for a 4, 8 or 16 ohm one.
    pub fn speaker_scale(self) -> f64 {
        match self.spec() {
            Some(spec) => power::speaker_scale(spec),
            // The JC-120 drives one 8 ohm speaker a side, which is the
            // profile's own impedance; the 73P's transformer is wound for a
            // line and for no impedance of speaker at all. Both take the
            // profile as it is. The 800RB's 300 W is rated into 4 ohm.
            None if self == Self::AmericanSS800 => {
                4.0 / crate::acoustics::speaker::SpeakerProfile::NOMINAL_OHMS
            }
            None => 1.0,
        }
    }

    pub(crate) fn slot(self) -> usize {
        match self {
            Self::Cali6L6 => 0,
            Self::American6L6Clean => 1,
            Self::American6L6HighGain => 2,
            Self::BritEL34 => 3,
            Self::BritPlexiEL34 => 4,
            Self::AC30EL84 => 5,
            Self::DR103EL34 => 6,
            Self::Recto6L6 => 7,
            Self::Recto6L6Tube => 8,
            Self::AmericanDeluxe6V6 => 9,
            Self::Jazz120SS => 10,
            Self::British73Out => 11,
            Self::Brit2205EL34 => 12,
            Self::DR103EL34Return => 13,
            Self::BritPlexiBassEL34 => 14,
            Self::BrumEL34 => 15,
            Self::Oregon6550 => 16,
            Self::Svt6550 => 17,
            Self::AmericanSS800 => 18,
            Self::Brit45KT66 => 19,
            Self::American7027A => 20,
            Self::AmericanV4b7027A => 21,
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Cali6L6 => voice_index(Gain::Boogie, Diode::Silicon, Amplifier::Valve),
            Self::American6L6Clean => voice_index(Gain::Twin, Diode::Silicon, Amplifier::Valve),
            Self::American6L6HighGain => {
                voice_index(Gain::Peavey, Diode::Silicon, Amplifier::Valve)
            }
            Self::BritEL34 => voice_index(Gain::Brit800, Diode::Silicon, Amplifier::Valve),
            Self::BritPlexiEL34 => voice_index(Gain::Plexi, Diode::Silicon, Amplifier::Valve),
            Self::AC30EL84 => voice_index(Gain::AC30, Diode::Silicon, Amplifier::Valve),
            Self::DR103EL34 => voice_index(Gain::DR103, Diode::Silicon, Amplifier::Valve),
            Self::Recto6L6 => voice_index(Gain::Recto, Diode::Silicon, Amplifier::Valve),
            // No voice has this one as its own, so it lives in the slot after
            // the catalogue. See `EXTRA_POWER_SPECS`.
            Self::Recto6L6Tube => VOICES,
            Self::AmericanDeluxe6V6 => voice_index(Gain::Deluxe, Diode::Silicon, Amplifier::Valve),
            Self::Jazz120SS => voice_index(Gain::Jazz120, Diode::Silicon, Amplifier::Valve),
            Self::British73Out => voice_index(Gain::Neve, Diode::Silicon, Amplifier::Valve),
            Self::Brit2205EL34 => voice_index(Gain::Brit2205, Diode::Silicon, Amplifier::Valve),
            // After the Recto's valve-rectifier stage in `EXTRA_POWER_SPECS`.
            Self::DR103EL34Return => VOICES + 1,
            Self::BritPlexiBassEL34 => {
                voice_index(Gain::PlexiBass, Diode::Silicon, Amplifier::Valve)
            }
            Self::BrumEL34 => voice_index(Gain::Brum100, Diode::Silicon, Amplifier::Valve),
            Self::Oregon6550 => voice_index(Gain::OregonT, Diode::Silicon, Amplifier::Valve),
            Self::Svt6550 => voice_index(Gain::AmericanSvt, Diode::Silicon, Amplifier::Valve),
            Self::AmericanSS800 => {
                voice_index(Gain::American800RB, Diode::Silicon, Amplifier::Valve)
            }
            Self::Brit45KT66 => voice_index(Gain::Brit45, Diode::Silicon, Amplifier::Valve),
            Self::American7027A => {
                voice_index(Gain::AmericanVt40, Diode::Silicon, Amplifier::Valve)
            }
            Self::AmericanV4b7027A => {
                voice_index(Gain::AmericanV4b, Diode::Silicon, Amplifier::Valve)
            }
        }
    }
}

/// Independent selection of complete output circuits; see params for stable IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PowerAmp {
    #[default]
    Matched,
    Bypass,
    Cali6L6,
    American6L6Clean,
    American6L6HighGain,
    BritEL34,
    BritPlexiEL34,
    AC30EL84,
    DR103EL34,
    Recto6L6,
    Recto6L6Tube,
    AmericanDeluxe6V6,
    Brit2205EL34,
    BritPlexiBassEL34,
    BrumEL34,
    Oregon6550,
    Svt6550,
    AmericanSS800,
    Brit45KT66,
    American7027A,
    AmericanV4b7027A,
}

impl PowerAmp {
    pub const ALL: [Self; 21] = [
        Self::Matched,
        Self::Bypass,
        Self::Cali6L6,
        Self::American6L6Clean,
        Self::American6L6HighGain,
        Self::BritEL34,
        Self::BritPlexiEL34,
        Self::AC30EL84,
        Self::DR103EL34,
        Self::Recto6L6,
        Self::Recto6L6Tube,
        Self::AmericanDeluxe6V6,
        Self::Brit2205EL34,
        Self::BritPlexiBassEL34,
        Self::BrumEL34,
        Self::Oregon6550,
        Self::Svt6550,
        Self::AmericanSS800,
        Self::Brit45KT66,
        Self::American7027A,
        Self::AmericanV4b7027A,
    ];

    pub fn resolved(self, preamp: Gain) -> Option<PowerModel> {
        match self {
            Self::Matched => match preamp {
                Gain::Boogie => Some(PowerModel::Cali6L6),
                Gain::Twin => Some(PowerModel::American6L6Clean),
                Gain::Peavey => Some(PowerModel::American6L6HighGain),
                Gain::Brit800 => Some(PowerModel::BritEL34),
                Gain::Plexi => Some(PowerModel::BritPlexiEL34),
                Gain::AC30 => Some(PowerModel::AC30EL84),
                Gain::DR103 => Some(PowerModel::DR103EL34),
                Gain::Recto => Some(PowerModel::Recto6L6),
                Gain::Deluxe | Gain::DeluxeNormal => Some(PowerModel::AmericanDeluxe6V6),
                Gain::Jazz120 => Some(PowerModel::Jazz120SS),
                Gain::Neve => Some(PowerModel::British73Out),
                Gain::Brit2205 => Some(PowerModel::Brit2205EL34),
                Gain::PlexiBass => Some(PowerModel::BritPlexiBassEL34),
                Gain::Brum100 => Some(PowerModel::BrumEL34),
                Gain::OregonT => Some(PowerModel::Oregon6550),
                Gain::AmericanSvt => Some(PowerModel::Svt6550),
                Gain::American800RB => Some(PowerModel::AmericanSS800),
                Gain::Brit45 => Some(PowerModel::Brit45KT66),
                Gain::AmericanVt40 => Some(PowerModel::American7027A),
                Gain::AmericanV4b => Some(PowerModel::AmericanV4b7027A),
                _ => None,
            },
            Self::Bypass => None,
            Self::Cali6L6 => Some(PowerModel::Cali6L6),
            Self::American6L6Clean => Some(PowerModel::American6L6Clean),
            Self::American6L6HighGain => Some(PowerModel::American6L6HighGain),
            Self::BritEL34 => Some(PowerModel::BritEL34),
            Self::BritPlexiEL34 => Some(PowerModel::BritPlexiEL34),
            Self::AC30EL84 => Some(PowerModel::AC30EL84),
            // Behind the DR103 it is that amplifier's own stage, V3a and all;
            // behind anything else, the stage from its inverter on.
            Self::DR103EL34 => Some(if preamp == Gain::DR103 {
                PowerModel::DR103EL34
            } else {
                PowerModel::DR103EL34Return
            }),
            Self::Recto6L6 => Some(PowerModel::Recto6L6),
            Self::Recto6L6Tube => Some(PowerModel::Recto6L6Tube),
            Self::AmericanDeluxe6V6 => Some(PowerModel::AmericanDeluxe6V6),
            Self::Brit2205EL34 => Some(PowerModel::Brit2205EL34),
            Self::BritPlexiBassEL34 => Some(PowerModel::BritPlexiBassEL34),
            Self::BrumEL34 => Some(PowerModel::BrumEL34),
            Self::Oregon6550 => Some(PowerModel::Oregon6550),
            Self::Svt6550 => Some(PowerModel::Svt6550),
            Self::AmericanSS800 => Some(PowerModel::AmericanSS800),
            Self::Brit45KT66 => Some(PowerModel::Brit45KT66),
            Self::American7027A => Some(PowerModel::American7027A),
            Self::AmericanV4b7027A => Some(PowerModel::AmericanV4b7027A),
        }
    }
}

/// A pedal in front of the preamplifier, independent of the circuit selection.
/// Only modelled pedals are offered; see `docs/MODEL_INVENTORY.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pedal {
    #[default]
    None,
    /// The Ibanez TS-808 with the values its drawings agree on (`ts808::TS808`).
    Green808,
    /// The 1973 Ram's Head Big Muff Pi netlist (`Gain::Muff`).
    BigMuff,
    /// The Ibanez TS9 (`ts808::TS9`).
    Green9,
    /// The Pro Co RAT, LM308 version (`circuits::rodent`).
    Rodent,
    /// The Arbiter Fuzz Face, germanium (`circuits::round_fuzz`).
    RoundFuzz,
    /// The MXR Distortion+ (`circuits::distortion_plus`).
    YellowDist,
    /// The Boss HM-2 (`circuits::heavy_metal`). Four knobs: the first pedal
    /// here with more than one tone control.
    HeavyMetal,
    /// The Boss MT-2 (`circuits::metal_zone`). Five, and three of them tone.
    MetalZone,
    /// The Boss DS-1, TA7136P original (`circuits::orange_dist`).
    OrangeDist,
    /// The Dallas Rangemaster, stock OC44 (`circuits::treble_boost`). One
    /// knob, and it is the level: the slot's drive knob is greyed.
    TrebleBoost,
    /// The Klon Centaur (`circuits::gold_drive`).
    GoldDrive,
    /// The Marshall Guv'nor (`circuits::brit_drive`). Five knobs.
    BritDrive,
    /// The MXR MicroAmp (`circuits::clean_boost`). One knob, its gain: the
    /// slot's level knob is greyed.
    CleanBoost,
    /// The Tech 21 SansAmp Bass Driver DI, V2 (`circuits::bass_driver`).
    /// Seven knobs, five of them in the tone row; its two shift switches
    /// stay at 80 Hz and 500 Hz in the slot.
    BassDriver,
    /// The MXR Phase 90 (`circuits::orange_phase`). One knob, Speed, on the
    /// slot's drive; R28 on the tone knob as a switch; no level, so the
    /// slot's level knob is greyed.
    OrangePhase,
    /// The Boss CE-2 (`circuits::blue_chorus`). Rate on the slot's drive,
    /// Depth on its tone knob, no level.
    BlueChorus,
    /// The Fortin 33 (`circuits::modern_33`). One knob, Level, on the slot's
    /// level; no drive, so the slot's drive knob is greyed.
    Modern33,
    /// The Revv G3 (`circuits::modern_purple`). Gain, bass, mid, treble, the
    /// Aggression toggle as a fourth tone knob in thirds, and volume.
    ModernPurple,
}

/// What a guitar puts out for a nominal digital signal: the level every circuit
/// with a guitar in front of it is calibrated at (`examples/calibrate.rs`).
pub const GUITAR_VOLTS: f64 = 0.122;

/// The most tone controls any pedal in the list has: the Bass Driver's five
/// (presence, bass, mid, treble, blend). It was the Metal Zone's four.
///
/// The slot used to carry one, which was fine while every pedal in it had one
/// tone knob or none. It is not fine for a pedal whose entire point is its
/// equaliser -- a Metal Zone with three of its six controls missing is not that
/// pedal -- so the slot carries what the boxes carry.
pub const PEDAL_TONES: usize = 5;

/// One of a pedal's tone controls.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ToneKnob {
    /// Where it is in the pedal's netlist.
    pub(crate) control: usize,
    /// What the panel prints under it. The pedals do not agree on a name --
    /// tone, filter, colour, middle -- and the knob should say what the box
    /// says rather than flattening them all to "tone".
    pub(crate) label: &'static str,
    /// The pedal's control runs the other way from the knob: the Rodent's
    /// FILTER darkens as it turns up.
    pub(crate) inverted: bool,
}

/// Where a pedal's knobs land in its netlist. `None` is a knob it does not have.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PedalControls {
    /// `None` where the pedal has no drive control (`Pedal::has_drive`).
    pub(crate) drive: Option<usize>,
    pub(crate) tones: [Option<ToneKnob>; PEDAL_TONES],
    /// `None` where the pedal has no level control (`Pedal::has_level`).
    pub(crate) level: Option<usize>,
}

/// A pedal with one tone control, which is most of them.
const fn one_tone(
    control: usize,
    label: &'static str,
    inverted: bool,
) -> [Option<ToneKnob>; PEDAL_TONES] {
    [
        Some(ToneKnob {
            control,
            label,
            inverted,
        }),
        None,
        None,
        None,
        None,
    ]
}

/// A pedal with none.
const NO_TONES: [Option<ToneKnob>; PEDAL_TONES] = [None, None, None, None, None];

impl Pedal {
    /// What the slot calls this pedal's drive and level knobs: the names its
    /// box prints, as the same pedal selected as the circuit has them
    /// (`Gain::drive_name`, `Gain::level_name`), in the slot's lower case.
    pub fn drive_and_level_labels(self) -> (&'static str, &'static str) {
        match self {
            Pedal::None | Pedal::Green9 => ("drive", "level"),
            Pedal::Green808 => ("overdrive", "level"),
            Pedal::BigMuff => ("sustain", "volume"),
            Pedal::Rodent => ("distortion", "volume"),
            Pedal::RoundFuzz => ("fuzz", "volume"),
            Pedal::YellowDist => ("distortion", "output"),
            Pedal::HeavyMetal => ("distortion", "level"),
            Pedal::MetalZone | Pedal::OrangeDist => ("dist", "level"),
            // Its one pot sets the boost, which is the level; there is no drive.
            Pedal::TrebleBoost => ("drive", "boost"),
            Pedal::GoldDrive => ("gain", "output"),
            Pedal::BritDrive => ("gain", "level"),
            Pedal::CleanBoost => ("gain", "level"),
            Pedal::BassDriver => ("drive", "level"),
            Pedal::OrangePhase => ("speed", "level"),
            Pedal::BlueChorus => ("rate", "level"),
            Pedal::Modern33 => ("drive", "level"),
            Pedal::ModernPurple => ("gain", "volume"),
        }
    }

    /// The same pedal as a circuit of its own.
    pub fn as_circuit(self) -> Option<Gain> {
        match self {
            Pedal::None => None,
            Pedal::Green808 => Some(Gain::Screamer),
            Pedal::BigMuff => Some(Gain::Muff),
            Pedal::Green9 => Some(Gain::Green9),
            Pedal::Rodent => Some(Gain::Rat),
            Pedal::RoundFuzz => Some(Gain::FuzzFace),
            Pedal::YellowDist => Some(Gain::DistPlus),
            Pedal::HeavyMetal => Some(Gain::Hm2),
            Pedal::MetalZone => Some(Gain::Mt2),
            Pedal::OrangeDist => Some(Gain::Ds1),
            Pedal::TrebleBoost => Some(Gain::TrebleBoost),
            Pedal::GoldDrive => Some(Gain::GoldDrive),
            Pedal::BritDrive => Some(Gain::BritDrive),
            Pedal::CleanBoost => Some(Gain::CleanBoost),
            Pedal::BassDriver => Some(Gain::BassDriver),
            Pedal::OrangePhase => Some(Gain::OrangePhase),
            Pedal::BlueChorus => Some(Gain::BlueChorus),
            Pedal::Modern33 => Some(Gain::Modern33),
            Pedal::ModernPurple => Some(Gain::ModernPurple),
        }
    }

    pub const ALL: [Pedal; 19] = [
        Pedal::None,
        Pedal::Green808,
        Pedal::BigMuff,
        Pedal::Green9,
        Pedal::Rodent,
        Pedal::RoundFuzz,
        Pedal::YellowDist,
        Pedal::HeavyMetal,
        Pedal::MetalZone,
        Pedal::OrangeDist,
        Pedal::TrebleBoost,
        Pedal::GoldDrive,
        Pedal::BritDrive,
        Pedal::CleanBoost,
        Pedal::BassDriver,
        Pedal::OrangePhase,
        Pedal::BlueChorus,
        Pedal::Modern33,
        Pedal::ModernPurple,
    ];
    /// How many pedal circuits a chain holds.
    #[allow(dead_code)]
    pub(crate) const SLOTS: usize = 18;

    pub(crate) fn slot(self) -> Option<usize> {
        match self {
            Pedal::None => None,
            Pedal::Green808 => Some(0),
            Pedal::BigMuff => Some(1),
            Pedal::Green9 => Some(2),
            Pedal::Rodent => Some(3),
            Pedal::RoundFuzz => Some(4),
            Pedal::YellowDist => Some(5),
            Pedal::HeavyMetal => Some(6),
            Pedal::MetalZone => Some(7),
            Pedal::OrangeDist => Some(8),
            Pedal::TrebleBoost => Some(9),
            Pedal::GoldDrive => Some(10),
            Pedal::BritDrive => Some(11),
            Pedal::CleanBoost => Some(12),
            Pedal::BassDriver => Some(13),
            Pedal::OrangePhase => Some(14),
            Pedal::BlueChorus => Some(15),
            Pedal::Modern33 => Some(16),
            Pedal::ModernPurple => Some(17),
        }
    }

    /// Whether this pedal is large enough that the chain cannot afford to
    /// oversample it.
    ///
    /// Measured with `examples/pedalcost.rs`, as a percentage of one channel's
    /// realtime budget at 48 kHz, in front of the Cali IIC+:
    ///
    /// | | alone | with the amplifier, 1x | at 2x |
    /// |---|---|---|---|
    /// | Green 808 | 11.6 | 40.8 | 74.5 |
    /// | Rodent | 10.5 | 39.6 | 74.6 |
    /// | Metal Zone | 22.4 | 55.5 | **100.7** |
    /// | Heavy Metal | 41.6 | 75.1 | **132.6** |
    ///
    /// The last two did not fit at twice the host rate in that measurement, and
    /// a chain that does not fit is a DAW missing its deadline: crackle,
    /// stuttering, dropouts. HM-2 keeps only the Colour Mix input follower in
    /// the exact linear Schur interior. The actual boost/cut amplifier remains
    /// rail-aware because the service-specified +21 dB resonant boost can drive
    /// it into its finite output swing at all-knobs-max settings.
    /// MT-2 deliberately keeps its post-distortion op-amps rail-aware: the first
    /// attempt to linearise them changed the response, and the later rail-fallback
    /// shortcut was much slower than the original solver. Keep this host-rate
    /// guard until each MT-2 stage is independently proven safe or is split into
    /// a dedicated stage-level processor. Its factory Middle stage (2026-09-25)
    /// added two more rail-aware amplifiers and took the boundary from 22 to 29:
    /// the same passes a sample, and on a loaded machine 17.8 to 24.1 % alone and
    /// 46.8 to 54.0 % in front of the Cali IIC+ at 1x.
    ///
    /// So the chain keeps them at the host rate for now, exactly as it keeps the
    /// modelled amplifiers under `MODELLED_MAX_OVERSAMPLING`, and for the same
    /// reason. See `Chain::set_oversampling`.
    ///
    /// The Modern Purple (2026-10-02) joins them: six rail-aware op-amps and
    /// six clipping diodes, 5.76 Newton passes a sample in front of the Clean
    /// circuit (the Metal Zone 4.56). With its kernels compiled, 23.9 % alone
    /// and 47.1 % in front of the Cali IIC+ at 1x, against the Metal Zone's
    /// 20.2 and 45.0 measured beside it; before them it took 109.6 % at 2x.
    pub fn is_expensive(self) -> bool {
        matches!(
            self,
            Pedal::HeavyMetal | Pedal::MetalZone | Pedal::ModernPurple
        )
    }

    /// Whether the pedal has a drive control. Every one but the Treble Boost
    /// does: its one pot is a volume after a fixed gain, so it is the slot's
    /// level knob and the drive knob is greyed rather than turning nothing.
    pub fn has_drive(self) -> bool {
        self.slot()
            .is_some_and(|slot| Self::controls(slot).drive.is_some())
    }

    /// Whether the pedal has a level control. Every one but the Clean Boost
    /// does: its one knob is GAIN, the slot's drive, and the level knob is
    /// greyed rather than turning nothing.
    pub fn has_level(self) -> bool {
        self.slot()
            .is_some_and(|slot| Self::controls(slot).level.is_some())
    }

    /// Whether the pedal has any tone control at all.
    pub fn has_tone(self) -> bool {
        self.tone_labels()[0].is_some()
    }

    /// What this pedal's tone controls are called, in panel order. The editor
    /// draws a knob for each one that is there and nothing for the rest.
    pub fn tone_labels(self) -> [Option<&'static str>; PEDAL_TONES] {
        let mut labels = [None; PEDAL_TONES];
        if let Some(slot) = self.slot() {
            for (label, knob) in labels.iter_mut().zip(Self::controls(slot).tones) {
                *label = knob.map(|k| k.label);
            }
        }
        labels
    }

    pub(crate) fn build(slot: usize) -> Result<Netlist, Fault> {
        match slot {
            0 => ts808::build_with(&ts808::TS808, 10_000.0, 470_000.0),
            1 => bigmuff::build(&bigmuff::RAMS_HEAD, 10_000.0, 470_000.0),
            2 => ts808::build_with(&ts808::TS9, 10_000.0, 470_000.0),
            3 => rodent::build(10_000.0, 470_000.0),
            4 => round_fuzz::build(10_000.0, 470_000.0),
            5 => distortion_plus::build(10_000.0, 470_000.0),
            6 => heavy_metal::build(10_000.0, 470_000.0),
            7 => metal_zone::build(10_000.0, 470_000.0),
            8 => orange_dist::build(10_000.0, 470_000.0),
            9 => treble_boost::build(10_000.0, 470_000.0),
            10 => gold_drive::build(10_000.0, 470_000.0),
            11 => brit_drive::build(10_000.0, 470_000.0),
            13 => bass_driver::build(10_000.0, 470_000.0),
            14 => orange_phase::build(10_000.0, 470_000.0),
            15 => blue_chorus::build(10_000.0, 470_000.0),
            16 => modern_33::build(10_000.0, 470_000.0),
            17 => modern_purple::build(10_000.0, 470_000.0),
            _ => clean_boost::build(10_000.0, 470_000.0),
        }
    }

    pub(crate) fn controls(slot: usize) -> PedalControls {
        match slot {
            0 | 2 => PedalControls {
                drive: Some(ts808::DRIVE),
                tones: one_tone(ts808::TONE, "tone", false),
                level: Some(ts808::LEVEL),
            },
            1 => PedalControls {
                drive: Some(bigmuff::SUSTAIN),
                tones: one_tone(bigmuff::TONE, "tone", false),
                level: Some(bigmuff::VOLUME),
            },
            3 => PedalControls {
                drive: Some(rodent::DISTORTION),
                tones: one_tone(rodent::FILTER, "filter", true),
                level: Some(rodent::VOLUME),
            },
            4 => PedalControls {
                drive: Some(round_fuzz::FUZZ),
                tones: NO_TONES,
                level: Some(round_fuzz::VOLUME),
            },
            5 => PedalControls {
                drive: Some(distortion_plus::DISTORTION),
                tones: NO_TONES,
                level: Some(distortion_plus::VOLUME),
            },
            // The first pedal here with two tone controls, and they are a
            // boost-and-cut pair rather than a passive tone: "colour mix" is
            // what the box calls them.
            6 => PedalControls {
                drive: Some(heavy_metal::DIST),
                tones: [
                    Some(ToneKnob {
                        control: heavy_metal::LOW,
                        label: "colour lo",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: heavy_metal::HIGH,
                        label: "colour hi",
                        inverted: false,
                    }),
                    None,
                    None,
                    None,
                ],
                level: Some(heavy_metal::LEVEL),
            },
            // Four tone controls, which is what the slot was widened to carry:
            // three bands and the sweep that moves the middle one.
            7 => PedalControls {
                drive: Some(metal_zone::DIST),
                tones: [
                    Some(ToneKnob {
                        control: metal_zone::LOW,
                        label: "low",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: metal_zone::MIDDLE,
                        label: "middle",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: metal_zone::MID_FREQ,
                        label: "mid freq",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: metal_zone::HIGH,
                        label: "high",
                        inverted: false,
                    }),
                    None,
                ],
                level: Some(metal_zone::LEVEL),
            },
            // One tone control, a blend with a scoop in the middle; the box
            // calls it TONE.
            8 => PedalControls {
                drive: Some(orange_dist::DIST),
                tones: one_tone(orange_dist::TONE, "tone", false),
                level: Some(orange_dist::LEVEL),
            },
            // One pot, the collector load, which is a volume: the level.
            9 => PedalControls {
                drive: None,
                tones: NO_TONES,
                level: Some(treble_boost::BOOST),
            },
            // Gain, Treble and Output.
            10 => PedalControls {
                drive: Some(gold_drive::GAIN),
                tones: one_tone(gold_drive::TREBLE, "treble", false),
                level: Some(gold_drive::LEVEL),
            },
            // Gain, three tone controls, and Level.
            11 => PedalControls {
                drive: Some(brit_drive::GAIN),
                tones: [
                    Some(ToneKnob {
                        control: brit_drive::BASS,
                        label: "bass",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: brit_drive::MIDDLE,
                        label: "middle",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: brit_drive::TREBLE,
                        label: "treble",
                        inverted: false,
                    }),
                    None,
                    None,
                ],
                level: Some(brit_drive::LEVEL),
            },
            // Drive, then the box's knobs right to left -- presence, bass, mid,
            // treble, blend -- and level.
            13 => PedalControls {
                drive: Some(bass_driver::DRIVE),
                tones: [
                    Some(ToneKnob {
                        control: bass_driver::PRESENCE,
                        label: "presence",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: bass_driver::BASS,
                        label: "bass",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: bass_driver::MID,
                        label: "mid",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: bass_driver::TREBLE,
                        label: "treble",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: bass_driver::BLEND,
                        label: "blend",
                        inverted: false,
                    }),
                ],
                level: Some(bass_driver::LEVEL),
            },
            // One knob and one switch: Speed on the drive, R28 on the tone
            // knob, and no level control at all.
            14 => PedalControls {
                drive: Some(orange_phase::SPEED),
                tones: one_tone(orange_phase::BLOCK, "block", false),
                level: None,
            },
            // Rate and Depth, and no level control.
            15 => PedalControls {
                drive: Some(blue_chorus::RATE),
                tones: one_tone(blue_chorus::DEPTH, "depth", false),
                level: None,
            },
            // One knob, LEVEL on the box: the level, and no drive.
            16 => PedalControls {
                drive: None,
                tones: NO_TONES,
                level: Some(modern_33::LEVEL),
            },
            // Gain, the three of its stack and the Aggression toggle, volume.
            17 => PedalControls {
                drive: Some(modern_purple::GAIN),
                tones: [
                    Some(ToneKnob {
                        control: modern_purple::BASS,
                        label: "bass",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: modern_purple::MID,
                        label: "mid",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: modern_purple::TREBLE,
                        label: "treble",
                        inverted: false,
                    }),
                    Some(ToneKnob {
                        control: modern_purple::AGGRESSION,
                        label: "aggr.",
                        inverted: false,
                    }),
                    None,
                ],
                level: Some(modern_purple::VOLUME),
            },
            // One knob, GAIN on the box: the drive, and no level.
            _ => PedalControls {
                drive: Some(clean_boost::GAIN),
                tones: NO_TONES,
                level: None,
            },
        }
    }

    /// Volts at the pedal's input for a nominal digital signal. The three older
    /// pedals keep the calibration of the catalogue voice they share a netlist
    /// with; the newer ones take a guitar's level directly.
    pub(crate) fn input_volts(slot: usize) -> f64 {
        let of = |gain: Gain| {
            CALIBRATION[voice_index(gain, Diode::Silicon, Amplifier::Valve)].drive_volts
        };
        match slot {
            0 | 2 => of(Gain::Screamer),
            1 => of(Gain::Muff),
            _ => GUITAR_VOLTS,
        }
    }
}

/// The wah ahead of the pedal: which one, if any, where its treadle is, and
/// whether an envelope follower moves it.
///
/// Manual puts the pot where `treadle` says -- a host-automated lane, or an
/// expression pedal's controller linked to the parameter. Auto leaves it
/// resting at `treadle` and pushes it toward the toe as the player picks
/// harder, as an auto-wah's Manual knob sets where its sweep starts; `sense`
/// says how hard full travel takes. See `docs/models/wahs.md`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WahSettings {
    pub wah: Option<wah::Build>,
    pub treadle: f64,
    pub auto: bool,
    pub sense: f64,
}

impl Default for WahSettings {
    fn default() -> Self {
        Self {
            wah: None,
            treadle: 0.5,
            auto: false,
            sense: 0.5,
        }
    }
}

/// The pedal and its knobs. Level's middle is the pedal's calibrated resting
/// position, exactly as the Master knob's is. See `Chain::master_position`.
///
/// `tone` carries as many as the pedal has, in the order its panel has them;
/// entries past that are ignored. See `PEDAL_TONES`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PedalSettings {
    pub pedal: Pedal,
    pub drive: f64,
    pub tone: [f64; PEDAL_TONES],
    pub level: f64,
}

impl PedalSettings {
    /// A pedal with every tone control in the middle, which is what a knob the
    /// caller does not care about should be.
    pub fn centred(pedal: Pedal, drive: f64, level: f64) -> Self {
        Self {
            pedal,
            drive,
            level,
            ..Self::default()
        }
    }
}

impl Default for PedalSettings {
    fn default() -> Self {
        Self {
            pedal: Pedal::None,
            drive: 0.5,
            tone: [0.5; PEDAL_TONES],
            level: 0.5,
        }
    }
}

/// Which cabinet is after the power stage. `Legacy` is the old resistive load and
/// baked Combo/Stack filter, exactly as before any of this existed; every old
/// session resolves to it.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum CabinetChoice {
    #[default]
    Legacy,
    /// A driver on an infinite baffle: speaker and microphone, no box.
    Bypass,
    Model(&'static CabinetProfile),
}

/// Which driver. `Matched` is the cabinet's own; `Bypass` is a resistor and the
/// terminal voltage (a DI of the power stage).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum SpeakerChoice {
    #[default]
    Matched,
    Bypass,
    Model(&'static SpeakerProfile),
}

/// Everything after the power stage, as plain values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcousticSettings {
    pub cabinet: CabinetChoice,
    pub speaker: SpeakerChoice,
    /// `Ideal` is an ideal omni at the placement ("Bypass"); it can never be `Off`.
    pub mic_a: MicSlot,
    pub mic_b: MicSlot,
    pub place_a: MicPlacement,
    pub place_b: MicPlacement,
    pub blend: f64,
    /// Where each microphone sits in the stereo field, -1 hard left to +1 hard
    /// right. Centre is the whole signal on both sides, which is what this
    /// plugin's duplicated-mono output already does, so the default changes
    /// nothing. See `AcousticStage::process_stereo`.
    pub pan_a: f64,
    pub pan_b: f64,
    pub invert_b: bool,
    pub align: bool,
    /// The cabinet's horn attenuator, 0..1, for a cabinet with a horn: off at
    /// zero, the horn's full level at one.
    pub horn: f64,
}

impl Default for AcousticSettings {
    fn default() -> Self {
        Self {
            cabinet: CabinetChoice::Legacy,
            speaker: SpeakerChoice::Matched,
            mic_a: MicSlot::Profile(&MicProfile::DYNAMIC_57),
            mic_b: MicSlot::Off,
            place_a: MicPlacement::default(),
            place_b: MicPlacement::default(),
            blend: 0.5,
            pan_a: 0.0,
            pan_b: 0.0,
            invert_b: false,
            align: false,
            horn: 0.5,
        }
    }
}

impl AcousticSettings {
    /// The driver actually radiating, if the physical path is in use.
    pub fn resolved_speaker(&self) -> Option<&'static SpeakerProfile> {
        let cabinet = match self.cabinet {
            CabinetChoice::Legacy => return None,
            CabinetChoice::Bypass => None,
            CabinetChoice::Model(cab) => Some(cab),
        };
        match self.speaker {
            SpeakerChoice::Bypass => None,
            SpeakerChoice::Model(profile) => Some(profile),
            SpeakerChoice::Matched => Some(
                cabinet
                    .map(|cab| cab.default_speaker)
                    .unwrap_or(&SpeakerProfile::BRIT_V30),
            ),
        }
    }

    pub fn resolved_cabinet(&self) -> Option<&'static CabinetProfile> {
        match self.cabinet {
            CabinetChoice::Model(cab) => Some(cab),
            _ => None,
        }
    }

    pub(crate) fn mounting(&self) -> Mounting {
        self.resolved_cabinet()
            .map(CabinetProfile::mounting)
            .unwrap_or(Mounting::BAFFLE)
    }
}

/// The block behind a voice's gain circuit, where it has one.
///
/// Four voices do, and they are not the same kind of thing. The three valve
/// guitar amplifiers get a push-pull power stage from `power.rs`, built from
/// a `PowerSpec`. The 73P gets its own OUTPUT block -- two BC109C into a
/// TIP3055 and the VTB1148 -- because a microphone preamplifier's line driver
/// is not a power amplifier and sharing the machinery would mean sharing a
/// topology it does not have.
///
/// Separate netlists rather than one joined onto the end of the other, and the
/// reason is cost. Solving one circuit of thirty-five nodes is not the same
/// work as solving two of eighteen: the factorisation goes as the cube of the
/// size, so joining them costs about three times as much as keeping them
/// apart. What that separation gives up is the loading between the two, and
/// there is almost none to give up -- a coupling capacitor into a high
/// impedance, with a level control in between.
pub fn build_power(gain: Gain) -> Option<Result<Netlist, Fault>> {
    match gain {
        // The card's own load is the line it drives. Ten kilohms is what a
        // modern input presents; the 73P was designed for six hundred, and
        // R43's 1.5 k across the secondary means the difference is small.
        Gain::Neve => Some(neve::output(LOAD, 10_000.0)),
        // A complementary transistor amplifier into one 8 ohm speaker. It has
        // no `PowerSpec` because `PowerSpec` is valve-shaped throughout and
        // this has no inverter, no grid leaks and no output transformer -- the
        // same reason the 73P's line driver is its own block. Driven from the
        // main amplifier board's own follower, so the source is low.
        Gain::Jazz120 => Some(jc120_power::build(1_000.0, 8.0)),
        // The 800RB's 300 W amplifier into its 4 ohm, driven from the boost
        // stage's drain through the LO master; also not a `PowerSpec`.
        Gain::American800RB => Some(american_ss800::build(3_300.0, 4.0)),
        _ => gain.power_stage().map(|spec| power::build(spec, 10_000.0)),
    }
}

pub fn build_voice(gain: Gain, diode: Diode, amplifier: Amplifier) -> Result<Netlist, Fault> {
    match gain {
        Gain::Clean => preamp::build(&preamp::CLEAN, SOURCE, LOAD),
        Gain::Crunch => preamp::build(&preamp::CRUNCH, SOURCE, LOAD),
        Gain::HighGain => preamp::build(&preamp::HIGH_GAIN, SOURCE, LOAD),
        Gain::Overdrive | Gain::Distortion => {
            let mut v = if gain == Gain::Overdrive {
                clipper::OVERDRIVE
            } else {
                clipper::DISTORTION
            };
            v.diode = diode.spec();
            clipper::build(&v, SOURCE, LOAD)
        }
        Gain::Console | Gain::Studio => {
            let mut v = if gain == Gain::Console {
                studio::CONSOLE
            } else {
                studio::STUDIO
            };
            v.amplifier = amplifier.spec();
            studio::build(&v, SOURCE, LOAD)
        }
        // The modelled circuits take their own source and load, because those
        // are part of what the drawing specifies.
        Gain::Screamer => ts808::build(10_000.0, 470_000.0),
        // Loaded by the phase inverter's grid leak, which is where the
        // drawing hands over. See `twin.rs`.
        Gain::Twin => twin::build(10_000.0, 1_000_000.0),
        // 2 MOhm, not the usual 1 MOhm: what the AB763 Deluxe's 0.001 uF works
        // into is the inverter's 1 M + 1 M grid-leak chain, and that pair sets
        // the bottom corner of the hand-off.
        Gain::Deluxe => deluxe::build(10_000.0, 2_000_000.0),
        // CN3 into the main amplifier board, which is where the sheet hands
        // over. See `circuits::jazz120`.
        Gain::Jazz120 => jazz120::build(10_000.0, 1_000_000.0),
        // The same amplifier through its other channel, and the same hand-off.
        Gain::DeluxeNormal => deluxe::build_channel(deluxe::Channel::Normal, 10_000.0, 2_000_000.0),
        Gain::Muff => bigmuff::build(&bigmuff::RAMS_HEAD, 10_000.0, 470_000.0),
        Gain::Boogie => markiic::build(10_000.0, 1_000_000.0),
        // Not a nominal load: the 5150's tone stack really does hang 33 k on
        // the far side of R89, and leaving it out would flatter the model by
        // twenty four decibels.
        Gain::Peavey => evh5150::build(10_000.0, evh5150::POST_LOAD),
        Gain::Neve => neve::build(150.0, 10_000.0),
        // Loaded by the Master Volume's 1 M, which the power stage begins with.
        Gain::Brit800 => brit800::build(10_000.0, 1_000_000.0),
        // Loaded by what the inverter presents through C25: R41's 330 k in
        // parallel with the power netlist's wide-open 1 M master track.
        Gain::Brit2205 => brit2205::build(10_000.0, 250_000.0),
        // A 150 ohm microphone into the card, and a modern line input after it.
        Gain::American312 => american312::build(150.0, 10_000.0),
        Gain::ConsoleE => console_e::build(150.0, 10_000.0),
        // The 610-A's microphone connection is the O-1's 50 ohm winding, and its
        // output transformer is wound for a 600 ohm line.
        Gain::Tube610 => tube610::build(50.0, 600.0),
        Gain::Plexi => plexi::build(10_000.0, 1_000_000.0),
        Gain::AC30 => ac30::build(10_000.0, 1_000_000.0),
        Gain::DR103 => dr103::build(10_000.0, 1_000_000.0),
        Gain::Recto => rectifier::build(10_000.0, 1_000_000.0),
        // The pedals as circuits: the same netlists the slot builds, on the
        // same source and load their drawings specify.
        Gain::Green9 => ts808::build_with(&ts808::TS9, 10_000.0, 470_000.0),
        Gain::Rat => rodent::build(10_000.0, 470_000.0),
        Gain::FuzzFace => round_fuzz::build(10_000.0, 470_000.0),
        Gain::DistPlus => distortion_plus::build(10_000.0, 470_000.0),
        Gain::Hm2 => heavy_metal::build(10_000.0, 470_000.0),
        Gain::Mt2 => metal_zone::build(10_000.0, 470_000.0),
        Gain::Ds1 => orange_dist::build(10_000.0, 470_000.0),
        Gain::TrebleBoost => treble_boost::build(10_000.0, 470_000.0),
        // Loaded by the power stage's wide-open master track and the
        // inverter's leak, as the Brit Plexi is.
        Gain::PlexiBass => plexi_bass::build(10_000.0, 1_000_000.0),
        Gain::Brum100 => brum100::build(10_000.0, 1_000_000.0),
        // R19's wiper into C8: the power stage's wide-open 1 M master track in
        // parallel with the inverter's 1 M leak behind C8.
        Gain::OregonT => oregon_t::build(10_000.0, 500_000.0),
        Gain::GoldDrive => gold_drive::build(10_000.0, 470_000.0),
        Gain::BritDrive => brit_drive::build(10_000.0, 470_000.0),
        Gain::CleanBoost => clean_boost::build(10_000.0, 470_000.0),
        Gain::AmericanSvt => american_svt::build(10_000.0, 1_000_000.0),
        // At EMI's own impedances: a 200 ohm source, a 200 ohm load.
        Gain::British47 => british_47::build(200.0, 200.0),
        // At the Braunbuch's own: a 200 ohm generator, a 300 ohm load.
        Gain::German76 => german_76::build(200.0, 300.0),
        Gain::BassDriver => bass_driver::build(10_000.0, 470_000.0),
        Gain::American800RB => american_800rb::build(10_000.0, 1_000_000.0),
        Gain::OrangePhase => orange_phase::build(10_000.0, 470_000.0),
        Gain::BlueChorus => blue_chorus::build(10_000.0, 470_000.0),
        Gain::Modern33 => modern_33::build(10_000.0, 470_000.0),
        Gain::ModernPurple => modern_purple::build(10_000.0, 470_000.0),
        // Loaded by the inverter's 1 M leak behind its .02, as the Plexis are.
        Gain::Brit45 => jtm45::build(10_000.0, 1_000_000.0),
        Gain::AmericanVt40 => american_vt40::build(10_000.0, 1_000_000.0),
        Gain::AmericanV4b => american_v4b::build(10_000.0, 1_000_000.0),
    }
}

/// The output transformer, which is a control rather than a property of a
/// circuit: iron belongs after a distortion pedal exactly as much as after a
/// console channel, and there is no reason to offer it on one and not the
/// other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Iron {
    Off,
    /// Bends earliest and most gently -- the only one that colours a quiet
    /// signal at all.
    Nickel,
    /// Stays out of the way and then arrives hard, and goes furthest.
    Steel,
    /// Still clean where the other two are well into it.
    Amorphous,
}

impl Iron {
    pub const ALL: [Iron; 4] = [Iron::Off, Iron::Nickel, Iron::Steel, Iron::Amorphous];

    pub fn name(self) -> &'static str {
        match self {
            Iron::Off => "Off",
            Iron::Nickel => "Nickel",
            Iron::Steel => "Steel",
            Iron::Amorphous => "Amorphous",
        }
    }

    #[allow(dead_code)]
    fn index(self) -> Option<usize> {
        match self {
            Iron::Off => None,
            Iron::Nickel => Some(0),
            Iron::Steel => Some(1),
            Iron::Amorphous => Some(2),
        }
    }

    fn values(self) -> Option<iron::Values> {
        let core = match self {
            Iron::Off => return None,
            Iron::Nickel => crate::dsp::netlist::CoreSpec::NICKEL,
            Iron::Steel => crate::dsp::netlist::CoreSpec::STEEL,
            Iron::Amorphous => crate::dsp::netlist::CoreSpec::AMORPHOUS,
        };
        Some(iron::Values {
            core,
            ..iron::OUTPUT
        })
    }
}

/// One output transformer as a netlist, for the calibration example and the
/// chain, which have to build the same thing.
pub fn build_iron(material: Iron) -> Result<Netlist, Fault> {
    let values = material.values().unwrap_or(iron::OUTPUT);
    iron::build(&values, 600.0, 10_000.0)
}

/// How many volts a unit of digital signal becomes at the iron stage.
///
/// The one place in the plugin where a level has to be chosen rather than
/// measured. Flux is the integral of voltage, so what the core does depends
/// on how many volts it is handed -- and unlike the gain circuits, which are
/// calibrated so a nominal signal drives them the way their name says, the
/// iron stage sits after the make-up and sees a known level already.
///
/// Set so that a nominal signal at a guitar's **low E** lands about at
/// steel's knee.
///
/// It was set at 40 Hz, and 40 Hz is below the lowest note the instrument
/// has. Flux goes as `V / f`, so by low E (82 Hz) there was half that flux
/// and by A (110 Hz) less again -- and the Iron control did nothing audible
/// anywhere in the instrument's range. Measured on the iron alone at a
/// nominal signal, the spread between the three cores was:
///
/// | | 40 Hz | 82 Hz | 110 Hz | 220 Hz |
/// |---|---|---|---|---|
/// | at 24 V | 0.8 pts | **0.2** | **0.1** | **0.0** |
/// | at 96 V | 42.4 pts | **20.5** | **4.8** | 0.1 |
///
/// Reported from a DAW as "the iron models seem not to change anything", and
/// they did not.
///
/// Four times, not more. At six the cores still differ by 22.9 points at
/// 110 Hz, which is a transformer that never stops saturating; at four they
/// separate on the bottom two strings and are out of the way above them. The
/// reason not to chase an audible difference at 220 Hz is that no transformer
/// has one -- getting flux to the knee there needs five times again, which
/// would put 40 Hz past 75 per cent distortion. It would stop being iron and
/// start being a waveshaper.
///
/// See `examples/ironvolts.rs` for the measurement behind the choice.
pub const IRON_VOLTS: f64 = 96.0;

/// Where on the Drive control the iron is matched to the stage driving it.
///
/// The transformer is handed `IRON_VOLTS` exactly here, more above and less
/// below -- see `Chain::iron_drive`. Three quarters rather than the middle,
/// because that is where a player who has reached for a transformer is likely
/// to be, and because it leaves the top of the travel with somewhere to go.
pub const IRON_REFERENCE_DRIVE: f64 = 0.75;

/// The channel Volume position used to anchor the fixed digital output
/// conversion of every voice whose Drive parameter *is* a modelled Volume pot.
///
/// Unlike a generic distortion Drive control, these knobs must be allowed to
/// change level naturally. Freezing the post-circuit make-up at one position
/// keeps the circuit calibration anchored without cancelling the knob's real
/// gain law. See `Gain::drive_is_channel_volume`.
///
/// Named for the Twin until 2026-09-23, when the Jazz 120 joined it and the
/// name stopped being true.
pub const CHANNEL_VOLUME_CALIBRATION_REFERENCE: f64 = 0.24;

/// The tone section, which can be out of circuit entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Off,
    /// Wide open and roughly flat in the middle: a tone control rather than a
    /// voicing.
    Wide,
    /// The scooped voicing, which is the one that makes the sound the whole
    /// exercise is aimed at.
    Scooping,
}

impl Tone {
    pub const ALL: [Tone; 3] = [Tone::Off, Tone::Wide, Tone::Scooping];

    pub fn name(self) -> &'static str {
        match self {
            Tone::Off => "Off",
            Tone::Wide => "Wide",
            Tone::Scooping => "Scooping",
        }
    }

    pub fn build(self) -> Option<Result<Netlist, Fault>> {
        match self {
            Tone::Off => None,
            Tone::Wide => Some(tone::build(&tone::WIDE, SOURCE, LOAD)),
            Tone::Scooping => Some(tone::build(&tone::SCOOPING, SOURCE, LOAD)),
        }
    }
}

/// The speaker, which can also be out of circuit -- and has to be, because a
/// cabinet in front of a preamplifier sound is wrong and a high gain sound
/// without one is unlistenable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cabinet {
    Off,
    Combo,
    Stack,
}

impl Cabinet {
    pub const ALL: [Cabinet; 3] = [Cabinet::Off, Cabinet::Combo, Cabinet::Stack];

    pub fn name(self) -> &'static str {
        match self {
            Cabinet::Off => "Off",
            Cabinet::Combo => "Combo",
            Cabinet::Stack => "Stack",
        }
    }

    pub fn build(self) -> Option<Result<Netlist, Fault>> {
        match self {
            Cabinet::Off => None,
            Cabinet::Combo => Some(cabinet::build(&cabinet::COMBO, SOURCE)),
            Cabinet::Stack => Some(cabinet::build(&cabinet::STACK, SOURCE)),
        }
    }
}

/// How many points the make-up curve is measured at.
///
/// The make-up is a curve across the drive control rather than one number,
/// because the drive control moves the gain of these circuits by about eighty
/// decibels end to end. Where those points sit across the control is what
/// `KNOT_SHAPE` decides; the *number* of them decides how much of the curve a
/// straight line between two of them has to describe. It was nine, and then
/// seventeen, and the increase at each step was because a curve that goes as
/// `log(position)` near the bottom and flattens near the top cannot be
/// described by a straight line between evenly spaced samples -- see
/// `KNOT_SHAPE` for the shape. `tests/voice.rs` checks the interpolation
/// between the points, not just the points themselves.
pub const POINTS: usize = 33;

/// How the make-up's sample points are spread across the drive control.
///
/// Not evenly, and the reason is that no drive control is even. A pot's law is
/// logarithmic and the stage after it saturates, so a circuit's gain climbs
/// most of its range inside the first eighth of the travel and then flattens:
/// the Mark IIC+ moves 47 dB between a shut Lead Drive and an eighth of a turn,
/// and 9 dB over the remaining seven eighths. Evenly spaced points sample that
/// first cliff too sparsely for a straight line to describe it -- with nine
/// of them, the line between the two samples nearest the bottom was **24 dB**
/// away from the curve at a knob position of 0.03, which is heard as the level
/// lurching as the knob comes off its stop, and was reported as "at 0% Gain
/// the meter goes way UP".
///
/// More even points do not fix it. The curve goes as `log(position)` near the
/// bottom, so a straight line across the first segment is wrong by an amount
/// that does not shrink usefully however narrow the segment gets.
///
/// So the points are spread by a cube law: knot `i` sits at
/// `(i / (POINTS - 1))^3`. That puts seventeen of the thirty-three inside the
/// first eighth of the travel, where the gain is, and leaves the flat top end
/// sampled coarsely, where coarse is all it needs.
const KNOT_SHAPE: f64 = 3.0;

/// Where the make-up's `i`th sample point sits on the drive control.
pub fn knot_position(i: usize) -> f64 {
    (i as f64 / (POINTS - 1) as f64).powf(KNOT_SHAPE)
}

#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    /// Volts at the circuit's input for a signal at `NOMINAL_DBFS`.
    pub drive_volts: f64,
    /// Output make-up in dB at the drive positions `knot_position(i)` gives,
    /// the first at 0 and the last at 1.
    pub make_up_db: [f64; POINTS],
}

/// A table sampled at the `knot_position`s, at a drive position between them.
pub fn knots_at(table: &[f64; POINTS], drive: f64) -> f64 {
    // Into knot space, which is where the points are evenly spaced. The cube
    // root is the inverse of `knot_position`.
    let x = drive.clamp(0.0, 1.0).powf(1.0 / KNOT_SHAPE) * (POINTS - 1) as f64;
    let i = (x as usize).min(POINTS - 2);
    let f = x - i as f64;
    table[i] * (1.0 - f) + table[i + 1] * f
}

/// The same on a control that is a switch of `steps` positions: the
/// position's own value, from the knot inside it nearest the knob. See
/// `Calibration::make_up_db_on`.
pub fn knots_on(table: &[f64; POINTS], drive: f64, steps: Option<usize>) -> f64 {
    let Some(n) = steps.filter(|&n| n > 1) else {
        return knots_at(table, drive);
    };
    let drive = drive.clamp(0.0, 1.0);
    let step = |d: f64| ((d * n as f64) as usize).min(n - 1);
    (0..POINTS)
        .filter(|&i| step(knot_position(i)) == step(drive))
        .min_by(|&a, &b| {
            (knot_position(a) - drive)
                .abs()
                .total_cmp(&(knot_position(b) - drive).abs())
        })
        .map_or_else(|| knots_at(table, drive), |i| table[i])
}

impl Calibration {
    /// The make-up at a drive position, between the measured points.
    pub fn make_up_db_at(&self, drive: f64) -> f64 {
        knots_at(&self.make_up_db, drive)
    }

    /// The make-up at a drive position on a control that is a switch of
    /// `steps` positions, or between the points on one that is not.
    ///
    /// A switch's gain is flat across each position and jumps between them,
    /// and a straight line between two points either side of a jump is wrong
    /// by up to the whole jump: the German 76's 46 dB position read 3.5 dB
    /// loud at the knob's middle, half way from its 40 dB neighbour. So the
    /// position's own value is taken instead, from the point inside it
    /// nearest the knob -- every position of the twelve the V76 has holds at
    /// least one of the thirty-three.
    pub fn make_up_db_on(&self, drive: f64, steps: Option<usize>) -> f64 {
        knots_on(&self.make_up_db, drive, steps)
    }
}

include!("calibration.rs");
include!("power_trim.rs");
include!("direct_out.rs");

/// The peak gain a linear section has anywhere in the audio band, at the
/// control positions given.
///
/// A passive tone stack and a speaker are both nothing but loss -- a stack can
/// only ever cut, which is why an amplifier has a gain stage after it -- and
/// switching one in would otherwise drop the whole plugin by twenty decibels.
/// Because they are linear, this is not a matter of playing audio through them
/// and taking a spectrum: the solver can be asked for the answer directly, one
/// exact complex number per frequency, in microseconds.
///
/// It is the peak rather than the level at some nominal frequency because the
/// point is headroom: normalising to a dip would push the peak into clipping.
/// This runs when a *section* is chosen, never when a knob moves, so the tone
/// controls shift the level as they do on the hardware.
pub(crate) fn peak_gain(circuit: &Netlist, controls: &[f64]) -> f64 {
    const STEPS: usize = 120;
    let (low, high) = (20.0f64, 16_000.0f64);
    let mut peak: f64 = 0.0;
    for i in 0..=STEPS {
        let hz = low * (high / low).powf(i as f64 / STEPS as f64);
        peak = peak.max(ac::solve(circuit, controls, hz).magnitude());
    }
    peak
}

/// What the plugin tells the host it delays by, whatever the oversampling is
/// set to.
///
/// The filters are shorter at the lower settings -- nothing at all with
/// oversampling off, 56 samples at two times, 64 at four, 66 at eight -- so
/// the honest figure would change as the control moves. The CLAP specification
/// asks that it does not, and a host that has to renegotiate its delay
/// compensation mid-stream will click. So one figure is reported and the
/// shorter settings are padded up to it.
///
/// 66 samples is 1.38 ms at 48 kHz and 1.50 ms at 44.1 kHz, both inside the
/// plugin's latency budget and inside the 9 ms ceiling everywhere. The
/// filters that get there are shorter and less steep than the ones this used
/// to carry -- 56 taps with a 90 dB stopband against 160 with 136 -- and the
/// reason the trade is worth making is that the plugin is a guitar amplifier.
/// What the extra attenuation bought was the octave above fifteen kilohertz,
/// which a speaker cone never reaches, and what it cost was two and a half
/// milliseconds of a three-point-seven-five millisecond budget.
pub const LATENCY: u32 = 66;

/// Number of samples to crossfade when switching circuits or presets.
/// At 48 kHz this is about 5.3 ms -- long enough to mask the capacitor
/// reset discontinuity even for high-gain circuits like the Mark IIC+,
/// short enough to be imperceptible as a level dip.
pub(crate) const FADE_LEN: usize = 256;

/// A whole number of samples of delay.
pub(crate) struct Delay {
    // A dormant channel may still have its old padding length when it wakes.
    // Reserve the maximum inline so copying any legal history cannot allocate.
    buf: [f64; LATENCY as usize],
    len: usize,
    pos: usize,
}

impl Delay {
    pub(crate) fn new(len: usize) -> Self {
        assert!(len <= LATENCY as usize);
        Self {
            buf: [0.0; LATENCY as usize],
            len,
            pos: 0,
        }
    }

    pub(crate) fn copy_runtime_state_from(&mut self, source: &Self) {
        self.buf.copy_from_slice(&source.buf);
        self.len = source.len;
        self.pos = source.pos;
    }

    /// A length of zero means no delay at all, and has to mean that.
    ///
    /// Rounding it up to one sample instead is not a rounding: at eight times
    /// oversampling the padding is exactly zero, so the wet path came out one
    /// sample later than the dry path it is mixed against and one later than
    /// the host had been told. A one sample offset between two copies of the
    /// same signal is a comb filter, and the reported latency was wrong at
    /// that setting and right at every other.
    pub(crate) fn set_len(&mut self, len: usize) {
        assert!(len <= self.buf.len());
        if len != self.len {
            self.buf[..len].fill(0.0);
            self.len = len;
            self.pos = 0;
        }
    }

    #[inline]
    pub(crate) fn process(&mut self, x: f64) -> f64 {
        if self.len == 0 {
            return x;
        }
        let out = self.buf[self.pos];
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % self.len;
        out
    }

    pub(crate) fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.pos = 0;
    }
}

/// Where a Master or Level knob puts a level pot that rests at `rest`: its
/// middle is the resting position, its ends the ends of the track
/// (upstream's `Chain::master_position`, whose comment explains why).
pub(crate) fn master_position(rest: f64, knob: f64) -> f64 {
    let knob = knob.clamp(0.0, 1.0);
    if knob <= 0.5 {
        rest * knob * 2.0
    } else {
        rest + (1.0 - rest) * (knob - 0.5) * 2.0
    }
}

/// What the Master knob adds above its middle once the pot has nothing left
/// to give (upstream's `Chain::master_lift`).
pub(crate) fn master_lift(knob: f64) -> f64 {
    let above = (knob.clamp(0.0, 1.0) - 0.5).max(0.0) * 2.0;
    10f64.powf(above * MASTER_LIFT_DB / 20.0)
}

/// The middle of the Master knob: the position each circuit was voiced at.
pub const MASTER_MIDDLE: f64 = 0.5;

/// How much the top of the Master knob adds once the pot has run out.
///
/// Six decibels, because the pot's own contribution above the voicing ranges
/// from three to eight across the catalogue and this brings every voice's
/// upward half into the same order as its downward one, which is about twenty.
/// Larger would make the knob mostly digital gain; smaller would leave the
/// 73P's upper half doing nothing. See `Chain::master_lift`.
pub(crate) const MASTER_LIFT_DB: f64 = 6.0;

/// Where a level control sits when the circuit declares one but rests it
/// nowhere. Nothing in the catalogue does; this is so a circuit added later
/// that forgets `Netlist::rest` is quiet rather than wrong.
pub(crate) const DEFAULT_MASTER_REST: f64 = 0.7;

/// Where the Master knob's middle puts a power stage that has no master of its
/// own, when it is driven by a different preamplifier. See `Chain::set_master`.
pub const OVERRIDE_MASTER_REST: f64 = 0.30;

/// Where a circuit's output level control lives. See `Gain::level_control`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// In the gain circuit: a pedal's Level or Volume, the 73P's trim.
    Circuit(usize),
    /// In the power amplifier: a master volume.
    Power(usize),
}

/// A circuit's bucket brigade, by name: see `Gain::bucket_brigade`.
#[derive(Clone, Copy, Debug)]
pub struct BucketBrigade {
    /// The node the brigade's input is driven at.
    pub send: &'static str,
    /// The auxiliary input its output drives.
    pub ret: usize,
    /// The node its clock follows.
    pub control: &'static str,
    /// The delay, in seconds, for a voltage at `control`.
    pub delay: fn(f64) -> f64,
}

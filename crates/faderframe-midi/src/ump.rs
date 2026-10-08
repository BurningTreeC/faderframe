//! Universal MIDI Packets (MIDI 2.0, M2-104-UM 1.1).
//!
//! A packet is one to four 32-bit words; the message type in the top four
//! bits of the first word gives its size. [`Ump`] holds one packet,
//! [`packets`] splits a word stream, [`Message::parse`] / [`Message::to_ump`]
//! read and write the messages a DAW deals with (utility, system, MIDI 1.0
//! and MIDI 2.0 channel voice, SysEx7, flex data, UMP stream); anything
//! else is kept as [`Message::Other`].
//!
//! [`scale_up`] / [`scale_down`] are the specification's min-centre-max
//! value scaling, [`Midi1ToMidi2`] translates MIDI 1.0 byte messages into
//! MIDI 2.0 channel voice messages (bank select and RPN/NRPN folded in),
//! [`Voice2::to_midi1`] goes the other way.

use crate::NoteExpressionKind;

/// Words in a packet of message type `mt` (0–15).
pub const fn words_for(mt: u8) -> usize {
    match mt & 0xF {
        0x0 | 0x1 | 0x2 | 0x6 | 0x7 => 1,
        0x3 | 0x4 | 0x8 | 0x9 | 0xA => 2,
        0xB | 0xC => 3,
        _ => 4,
    }
}

/// One Universal MIDI Packet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ump {
    words: [u32; 4],
    len: u8,
}

impl Ump {
    /// A packet from its words (as many as its message type takes).
    pub fn new(words: &[u32]) -> Option<Self> {
        let first = *words.first()?;
        let len = words_for((first >> 28) as u8);
        if words.len() != len {
            return None;
        }
        let mut w = [0u32; 4];
        w[..len].copy_from_slice(words);
        Some(Self {
            words: w,
            len: len as u8,
        })
    }

    fn from_array<const N: usize>(words: [u32; N]) -> Self {
        let mut w = [0u32; 4];
        w[..N].copy_from_slice(&words);
        Self {
            words: w,
            len: N as u8,
        }
    }

    pub fn words(&self) -> &[u32] {
        &self.words[..self.len as usize]
    }

    pub fn message_type(&self) -> u8 {
        (self.words[0] >> 28) as u8
    }

    /// The group (meaningless for utility and stream messages).
    pub fn group(&self) -> u8 {
        ((self.words[0] >> 24) & 0xF) as u8
    }

    /// The same packet on another group.
    pub fn with_group(mut self, group: u8) -> Self {
        if !matches!(self.message_type(), 0x0 | 0xF) {
            self.words[0] = (self.words[0] & !0x0F00_0000) | (u32::from(group & 0xF) << 24);
        }
        self
    }
}

/// The packets of a word stream (a truncated last packet is left out).
pub fn packets(words: &[u32]) -> impl Iterator<Item = Ump> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        let first = *words.get(at)?;
        let n = words_for((first >> 28) as u8);
        let p = Ump::new(words.get(at..at + n)?)?;
        at += n;
        Some(p)
    })
}

// --- value scaling ------------------------------------------------------------

/// Min-centre-max upscaling of a `src_bits` value to `dst_bits`: 0 stays 0,
/// the centre stays the centre, the maximum becomes the maximum.
pub fn scale_up(value: u32, src_bits: u32, dst_bits: u32) -> u32 {
    let scale_bits = dst_bits - src_bits;
    let shifted = value << scale_bits;
    let centre = 1u32 << (src_bits - 1);
    if value <= centre {
        return shifted;
    }
    let repeat_bits = src_bits - 1;
    let repeat_mask = (1u32 << repeat_bits) - 1;
    let mut repeat = value & repeat_mask;
    if scale_bits > repeat_bits {
        repeat <<= scale_bits - repeat_bits;
    } else {
        repeat >>= repeat_bits - scale_bits;
    }
    let mut out = shifted;
    while repeat != 0 {
        out |= repeat;
        repeat >>= repeat_bits;
    }
    out
}

/// Downscaling: the top bits.
pub fn scale_down(value: u32, src_bits: u32, dst_bits: u32) -> u32 {
    value >> (src_bits - dst_bits)
}

/// A 32-bit controller value as 0…1.
pub fn unit_of(value: u32) -> f64 {
    f64::from(value) / f64::from(u32::MAX)
}

/// 0…1 as a 32-bit controller value.
pub fn of_unit(x: f64) -> u32 {
    (x.clamp(0.0, 1.0) * f64::from(u32::MAX)).round() as u32
}

/// A 32-bit bend (centre 0x8000_0000) as −1…1.
pub fn bend_of(value: u32) -> f64 {
    let v = f64::from(value) - 2_147_483_648.0;
    if v < 0.0 {
        v / 2_147_483_648.0
    } else {
        v / 2_147_483_647.0
    }
}

/// −1…1 as a 32-bit bend.
pub fn of_bend(x: f64) -> u32 {
    let x = x.clamp(-1.0, 1.0);
    let v = if x < 0.0 {
        2_147_483_648.0 + x * 2_147_483_648.0
    } else {
        2_147_483_648.0 + x * 2_147_483_647.0
    };
    v.round() as u32
}

// --- messages -----------------------------------------------------------------

/// The place of a packet in a multi-packet message (SysEx7, text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Complete,
    Start,
    Continue,
    End,
}

impl Form {
    fn of(bits: u32) -> Self {
        match bits & 3 {
            0 => Form::Complete,
            1 => Form::Start,
            2 => Form::Continue,
            _ => Form::End,
        }
    }

    fn bits(self) -> u32 {
        match self {
            Form::Complete => 0,
            Form::Start => 1,
            Form::Continue => 2,
            Form::End => 3,
        }
    }

    /// The forms of `n` pieces in order.
    pub fn of_piece(i: usize, n: usize) -> Self {
        match (i, n) {
            (_, 0 | 1) => Form::Complete,
            (0, _) => Form::Start,
            (i, n) if i + 1 == n => Form::End,
            _ => Form::Continue,
        }
    }
}

/// Utility messages (no group).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Utility {
    Noop,
    JrClock(u16),
    JrTimestamp(u16),
    /// Delta Clockstamp ticks per quarter note (MIDI Clip Files).
    TicksPerQuarter(u16),
    /// Ticks since the previous message (20 bits).
    DeltaClockstamp(u32),
}

/// A note's attribute (MIDI 2.0 note on/off).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attribute {
    /// 0 none, 1 manufacturer, 2 profile, 3 pitch 7.9.
    pub kind: u8,
    pub data: u16,
}

impl Attribute {
    pub const PITCH_7_9: u8 = 3;

    /// The pitch of a pitch 7.9 attribute (semitones as a MIDI key).
    pub fn pitch(self) -> Option<f64> {
        (self.kind == Self::PITCH_7_9).then(|| f64::from(self.data) / 512.0)
    }
}

/// MIDI 2.0 channel voice messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Voice2 {
    NoteOff {
        note: u8,
        velocity: u16,
        attribute: Attribute,
    },
    NoteOn {
        note: u8,
        velocity: u16,
        attribute: Attribute,
    },
    PolyPressure {
        note: u8,
        value: u32,
    },
    RegisteredPerNote {
        note: u8,
        index: u8,
        value: u32,
    },
    AssignablePerNote {
        note: u8,
        index: u8,
        value: u32,
    },
    PerNoteManagement {
        note: u8,
        detach: bool,
        reset: bool,
    },
    PerNotePitchBend {
        note: u8,
        value: u32,
    },
    ControlChange {
        index: u8,
        value: u32,
    },
    /// RPN.
    Registered {
        bank: u8,
        index: u8,
        value: u32,
    },
    /// NRPN.
    Assignable {
        bank: u8,
        index: u8,
        value: u32,
    },
    RelativeRegistered {
        bank: u8,
        index: u8,
        delta: i32,
    },
    RelativeAssignable {
        bank: u8,
        index: u8,
        delta: i32,
    },
    ProgramChange {
        program: u8,
        /// Bank MSB, LSB.
        bank: Option<(u8, u8)>,
    },
    ChannelPressure(u32),
    PitchBend(u32),
}

/// Registered per-note controllers the DAW knows.
pub mod per_note {
    pub const MODULATION: u8 = 1;
    pub const BREATH: u8 = 2;
    /// Absolute pitch 7.25.
    pub const PITCH: u8 = 3;
    pub const VOLUME: u8 = 7;
    pub const BALANCE: u8 = 8;
    pub const PAN: u8 = 10;
    pub const EXPRESSION: u8 = 11;
    /// Sound controller 5 (brightness, MPE's timbre).
    pub const BRIGHTNESS: u8 = 74;
}

impl Voice2 {
    /// The MIDI 1.0 messages it becomes (bytes, how many). Per-note
    /// controllers, per-note bends and management have none.
    pub fn to_midi1(&self, channel: u8) -> ([[u8; 3]; 4], usize) {
        let ch = channel & 0xF;
        let mut out = [[0u8; 3]; 4];
        let mut n = 0;
        let mut push = |b: [u8; 3]| {
            out[n] = b;
            n += 1;
        };
        let seven = |v: u32| scale_down(v, 32, 7) as u8;
        match *self {
            Voice2::NoteOff { note, velocity, .. } => push([
                0x80 | ch,
                note & 0x7F,
                scale_down(u32::from(velocity), 16, 7) as u8,
            ]),
            Voice2::NoteOn { note, velocity, .. } => push([
                0x90 | ch,
                note & 0x7F,
                (scale_down(u32::from(velocity), 16, 7) as u8).max(1),
            ]),
            Voice2::PolyPressure { note, value } => push([0xA0 | ch, note & 0x7F, seven(value)]),
            Voice2::ControlChange { index, value } => push([0xB0 | ch, index & 0x7F, seven(value)]),
            Voice2::Registered { bank, index, value }
            | Voice2::Assignable { bank, index, value } => {
                let (msb, lsb) = if matches!(self, Voice2::Registered { .. }) {
                    (101, 100)
                } else {
                    (99, 98)
                };
                push([0xB0 | ch, msb, bank & 0x7F]);
                push([0xB0 | ch, lsb, index & 0x7F]);
                push([0xB0 | ch, 6, (value >> 25) as u8]);
                push([0xB0 | ch, 38, ((value >> 18) & 0x7F) as u8]);
            }
            Voice2::ProgramChange { program, bank } => {
                if let Some((msb, lsb)) = bank {
                    push([0xB0 | ch, 0, msb & 0x7F]);
                    push([0xB0 | ch, 32, lsb & 0x7F]);
                }
                push([0xC0 | ch, program & 0x7F, 0]);
            }
            Voice2::ChannelPressure(v) => push([0xD0 | ch, seven(v), 0]),
            Voice2::PitchBend(v) => {
                let b = scale_down(v, 32, 14);
                push([0xE0 | ch, (b & 0x7F) as u8, (b >> 7) as u8]);
            }
            _ => {}
        }
        (out, n)
    }
}

/// Flex data the DAW reads (tempo, meter, key, text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flex {
    /// Ten-nanosecond units per quarter note.
    Tempo { group: u8, tens_of_ns: u32 },
    TimeSignature {
        group: u8,
        numerator: u8,
        /// The denominator as a power of two (2 = quarters).
        denominator_power: u8,
    },
    KeySignature {
        group: u8,
        /// Sharps (positive) or flats (negative).
        sharps: i8,
        /// 1 = A … 7 = G, 0 unknown.
        tonic: u8,
    },
    /// A piece of text: `bank` 1 = metadata, 2 = performance (lyrics);
    /// `status` its kind (bank 1: 1 project name, 2 song name, 3 clip name,
    /// 4 copyright, 5 composer, …; bank 2: 1 lyrics).
    Text {
        group: u8,
        form: Form,
        bank: u8,
        status: u8,
        bytes: [u8; 12],
    },
}

impl Flex {
    /// Beats per minute of a tempo.
    pub fn bpm(tens_of_ns: u32) -> f64 {
        6_000_000_000.0 / f64::from(tens_of_ns.max(1))
    }

    /// Ten-nanosecond units of a tempo.
    pub fn tens_of_ns(bpm: f64) -> u32 {
        (6_000_000_000.0 / bpm.max(1.0)).round() as u32
    }
}

/// UMP stream messages (endpoint and function block discovery, clips).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    EndpointDiscovery {
        major: u8,
        minor: u8,
        filter: u8,
    },
    EndpointInfo {
        major: u8,
        minor: u8,
        static_blocks: bool,
        blocks: u8,
        midi2: bool,
        midi1: bool,
    },
    /// A piece of the endpoint's name (14 bytes).
    EndpointName {
        form: Form,
        bytes: [u8; 14],
    },
    /// A piece of the product instance id (14 bytes).
    ProductInstanceId {
        form: Form,
        bytes: [u8; 14],
    },
    /// The protocol asked for or in use (1 MIDI 1.0, 2 MIDI 2.0).
    ConfigurationRequest {
        protocol: u8,
    },
    ConfigurationNotification {
        protocol: u8,
    },
    FunctionBlockDiscovery {
        block: u8,
        filter: u8,
    },
    FunctionBlockInfo {
        active: bool,
        block: u8,
        /// 1 input, 2 output, 3 both.
        direction: u8,
        first_group: u8,
        groups: u8,
    },
    /// A piece of a function block's name (13 bytes).
    FunctionBlockName {
        form: Form,
        block: u8,
        bytes: [u8; 13],
    },
    StartOfClip,
    EndOfClip,
}

/// A message read from (or written as) one packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    Utility(Utility),
    /// System common and real time (status 0xF1–0xFF, two data bytes).
    System {
        group: u8,
        status: u8,
        data: [u8; 2],
    },
    /// A MIDI 1.0 channel voice message (status with channel, two data
    /// bytes).
    Midi1 {
        group: u8,
        bytes: [u8; 3],
    },
    /// A piece of a SysEx (without F0/F7).
    Sysex7 {
        group: u8,
        form: Form,
        bytes: [u8; 6],
        len: u8,
    },
    Midi2 {
        group: u8,
        channel: u8,
        voice: Voice2,
    },
    Flex(Flex),
    Stream(Stream),
    /// Anything else (SysEx8, mixed data sets, reserved types), as it came.
    Other(Ump),
}

fn byte(w: u32, i: u32) -> u8 {
    (w >> (24 - 8 * i)) as u8
}

fn bytes_of(words: &[u32], out: &mut [u8]) {
    for (i, b) in out.iter_mut().enumerate() {
        *b = byte(words[i / 4], (i % 4) as u32);
    }
}

fn words_of(bytes: &[u8]) -> [u32; 3] {
    let mut w = [0u32; 3];
    for (i, b) in bytes.iter().take(12).enumerate() {
        w[i / 4] |= u32::from(*b) << (24 - 8 * (i % 4));
    }
    w
}

impl Message {
    pub fn parse(p: &Ump) -> Message {
        let w = p.words();
        let w0 = w[0];
        let group = p.group();
        match p.message_type() {
            0x0 => {
                let status = (w0 >> 20) & 0xF;
                Message::Utility(match status {
                    0x0 => Utility::Noop,
                    0x1 => Utility::JrClock(w0 as u16),
                    0x2 => Utility::JrTimestamp(w0 as u16),
                    0x3 => Utility::TicksPerQuarter(w0 as u16),
                    0x4 => Utility::DeltaClockstamp(w0 & 0xF_FFFF),
                    _ => return Message::Other(*p),
                })
            }
            0x1 => Message::System {
                group,
                status: byte(w0, 1),
                data: [byte(w0, 2) & 0x7F, byte(w0, 3) & 0x7F],
            },
            0x2 => Message::Midi1 {
                group,
                bytes: [byte(w0, 1), byte(w0, 2) & 0x7F, byte(w0, 3) & 0x7F],
            },
            0x3 => {
                let len = (((w0 >> 16) & 0xF) as u8).min(6);
                let mut bytes = [0u8; 6];
                bytes[0] = byte(w0, 2);
                bytes[1] = byte(w0, 3);
                bytes_of(&w[1..2], &mut bytes[2..6]);
                Message::Sysex7 {
                    group,
                    form: Form::of(w0 >> 20),
                    bytes,
                    len,
                }
            }
            0x4 => {
                let channel = ((w0 >> 16) & 0xF) as u8;
                let (b2, b3, data) = (byte(w0, 2), byte(w0, 3), w[1]);
                let voice = match (w0 >> 20) & 0xF {
                    0x0 => Voice2::RegisteredPerNote {
                        note: b2 & 0x7F,
                        index: b3,
                        value: data,
                    },
                    0x1 => Voice2::AssignablePerNote {
                        note: b2 & 0x7F,
                        index: b3,
                        value: data,
                    },
                    0x2 => Voice2::Registered {
                        bank: b2 & 0x7F,
                        index: b3 & 0x7F,
                        value: data,
                    },
                    0x3 => Voice2::Assignable {
                        bank: b2 & 0x7F,
                        index: b3 & 0x7F,
                        value: data,
                    },
                    0x4 => Voice2::RelativeRegistered {
                        bank: b2 & 0x7F,
                        index: b3 & 0x7F,
                        delta: data as i32,
                    },
                    0x5 => Voice2::RelativeAssignable {
                        bank: b2 & 0x7F,
                        index: b3 & 0x7F,
                        delta: data as i32,
                    },
                    0x6 => Voice2::PerNotePitchBend {
                        note: b2 & 0x7F,
                        value: data,
                    },
                    0x8 | 0x9 => {
                        let note = b2 & 0x7F;
                        let velocity = (data >> 16) as u16;
                        let attribute = Attribute {
                            kind: b3,
                            data: data as u16,
                        };
                        if (w0 >> 20) & 0xF == 0x8 {
                            Voice2::NoteOff {
                                note,
                                velocity,
                                attribute,
                            }
                        } else {
                            Voice2::NoteOn {
                                note,
                                velocity,
                                attribute,
                            }
                        }
                    }
                    0xA => Voice2::PolyPressure {
                        note: b2 & 0x7F,
                        value: data,
                    },
                    0xB => Voice2::ControlChange {
                        index: b2 & 0x7F,
                        value: data,
                    },
                    0xC => Voice2::ProgramChange {
                        program: (data >> 24) as u8 & 0x7F,
                        bank: (b3 & 1 == 1)
                            .then_some((((data >> 8) & 0x7F) as u8, (data & 0x7F) as u8)),
                    },
                    0xD => Voice2::ChannelPressure(data),
                    0xE => Voice2::PitchBend(data),
                    0xF => Voice2::PerNoteManagement {
                        note: b2 & 0x7F,
                        detach: b3 & 2 != 0,
                        reset: b3 & 1 != 0,
                    },
                    _ => return Message::Other(*p),
                };
                Message::Midi2 {
                    group,
                    channel,
                    voice,
                }
            }
            0xD => {
                let form = Form::of(w0 >> 22);
                let (bank, status) = (byte(w0, 2), byte(w0, 3));
                Message::Flex(match (bank, status) {
                    (0, 0) => Flex::Tempo {
                        group,
                        tens_of_ns: w[1],
                    },
                    (0, 1) => Flex::TimeSignature {
                        group,
                        numerator: byte(w[1], 0),
                        denominator_power: byte(w[1], 1),
                    },
                    (0, 5) => Flex::KeySignature {
                        group,
                        sharps: ((w[1] >> 28) as u8 as i8) << 4 >> 4,
                        tonic: ((w[1] >> 24) & 0xF) as u8,
                    },
                    (1 | 2, _) => {
                        let mut bytes = [0u8; 12];
                        bytes_of(&w[1..4], &mut bytes);
                        Flex::Text {
                            group,
                            form,
                            bank,
                            status,
                            bytes,
                        }
                    }
                    _ => return Message::Other(*p),
                })
            }
            0xF => {
                let form = Form::of(w0 >> 26);
                let status = (w0 >> 16) & 0x3FF;
                let hi = (w0 >> 8) as u8;
                let lo = w0 as u8;
                let text14 = || {
                    let mut b = [0u8; 14];
                    b[0] = hi;
                    b[1] = lo;
                    bytes_of(&w[1..4], &mut b[2..]);
                    b
                };
                Message::Stream(match status {
                    0x000 => Stream::EndpointDiscovery {
                        major: hi,
                        minor: lo,
                        filter: w[1] as u8,
                    },
                    0x001 => Stream::EndpointInfo {
                        major: hi,
                        minor: lo,
                        static_blocks: w[1] >> 31 != 0,
                        blocks: ((w[1] >> 24) & 0x7F) as u8,
                        midi2: w[1] & (1 << 9) != 0,
                        midi1: w[1] & (1 << 8) != 0,
                    },
                    0x003 => Stream::EndpointName {
                        form,
                        bytes: text14(),
                    },
                    0x004 => Stream::ProductInstanceId {
                        form,
                        bytes: text14(),
                    },
                    0x005 => Stream::ConfigurationRequest { protocol: hi },
                    0x006 => Stream::ConfigurationNotification { protocol: hi },
                    0x010 => Stream::FunctionBlockDiscovery {
                        block: hi,
                        filter: lo,
                    },
                    0x011 => Stream::FunctionBlockInfo {
                        active: hi & 0x80 != 0,
                        block: hi & 0x7F,
                        direction: lo & 3,
                        first_group: byte(w[1], 0),
                        groups: byte(w[1], 1),
                    },
                    0x012 => {
                        let mut bytes = [0u8; 13];
                        bytes[0] = lo;
                        bytes_of(&w[1..4], &mut bytes[1..]);
                        Stream::FunctionBlockName {
                            form,
                            block: hi,
                            bytes,
                        }
                    }
                    0x020 => Stream::StartOfClip,
                    0x021 => Stream::EndOfClip,
                    _ => return Message::Other(*p),
                })
            }
            _ => Message::Other(*p),
        }
    }

    pub fn to_ump(&self) -> Ump {
        let g = |group: u8| u32::from(group & 0xF) << 24;
        match *self {
            Message::Utility(u) => Ump::from_array([match u {
                Utility::Noop => 0,
                Utility::JrClock(t) => 0x0010_0000 | u32::from(t),
                Utility::JrTimestamp(t) => 0x0020_0000 | u32::from(t),
                Utility::TicksPerQuarter(t) => 0x0030_0000 | u32::from(t),
                Utility::DeltaClockstamp(t) => 0x0040_0000 | (t & 0xF_FFFF),
            }]),
            Message::System {
                group,
                status,
                data,
            } => Ump::from_array([0x1000_0000
                | g(group)
                | u32::from(status) << 16
                | u32::from(data[0] & 0x7F) << 8
                | u32::from(data[1] & 0x7F)]),
            Message::Midi1 { group, bytes } => Ump::from_array([0x2000_0000
                | g(group)
                | u32::from(bytes[0]) << 16
                | u32::from(bytes[1] & 0x7F) << 8
                | u32::from(bytes[2] & 0x7F)]),
            Message::Sysex7 {
                group,
                form,
                bytes,
                len,
            } => Ump::from_array([
                0x3000_0000
                    | g(group)
                    | form.bits() << 20
                    | u32::from(len.min(6)) << 16
                    | u32::from(bytes[0]) << 8
                    | u32::from(bytes[1]),
                u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
            ]),
            Message::Midi2 {
                group,
                channel,
                voice,
            } => {
                let head = |op: u32, b2: u8, b3: u8| {
                    0x4000_0000
                        | g(group)
                        | op << 20
                        | u32::from(channel & 0xF) << 16
                        | u32::from(b2) << 8
                        | u32::from(b3)
                };
                let (w0, w1) = match voice {
                    Voice2::RegisteredPerNote { note, index, value } => {
                        (head(0x0, note & 0x7F, index), value)
                    }
                    Voice2::AssignablePerNote { note, index, value } => {
                        (head(0x1, note & 0x7F, index), value)
                    }
                    Voice2::Registered { bank, index, value } => {
                        (head(0x2, bank & 0x7F, index & 0x7F), value)
                    }
                    Voice2::Assignable { bank, index, value } => {
                        (head(0x3, bank & 0x7F, index & 0x7F), value)
                    }
                    Voice2::RelativeRegistered { bank, index, delta } => {
                        (head(0x4, bank & 0x7F, index & 0x7F), delta as u32)
                    }
                    Voice2::RelativeAssignable { bank, index, delta } => {
                        (head(0x5, bank & 0x7F, index & 0x7F), delta as u32)
                    }
                    Voice2::PerNotePitchBend { note, value } => (head(0x6, note & 0x7F, 0), value),
                    Voice2::NoteOff {
                        note,
                        velocity,
                        attribute,
                    } => (
                        head(0x8, note & 0x7F, attribute.kind),
                        u32::from(velocity) << 16 | u32::from(attribute.data),
                    ),
                    Voice2::NoteOn {
                        note,
                        velocity,
                        attribute,
                    } => (
                        head(0x9, note & 0x7F, attribute.kind),
                        u32::from(velocity) << 16 | u32::from(attribute.data),
                    ),
                    Voice2::PolyPressure { note, value } => (head(0xA, note & 0x7F, 0), value),
                    Voice2::ControlChange { index, value } => (head(0xB, index & 0x7F, 0), value),
                    Voice2::ProgramChange { program, bank } => {
                        let (flag, b) = match bank {
                            Some((m, l)) => (1, u32::from(m & 0x7F) << 8 | u32::from(l & 0x7F)),
                            None => (0, 0),
                        };
                        (head(0xC, 0, flag), u32::from(program & 0x7F) << 24 | b)
                    }
                    Voice2::ChannelPressure(v) => (head(0xD, 0, 0), v),
                    Voice2::PitchBend(v) => (head(0xE, 0, 0), v),
                    Voice2::PerNoteManagement {
                        note,
                        detach,
                        reset,
                    } => (
                        head(0xF, note & 0x7F, u8::from(detach) << 1 | u8::from(reset)),
                        0,
                    ),
                };
                Ump::from_array([w0, w1])
            }
            Message::Flex(f) => {
                // Form, address (1 = group), status bank, status.
                let head = |group: u8, form: Form, bank: u8, status: u8| {
                    0xD000_0000
                        | g(group)
                        | form.bits() << 22
                        | 1 << 20
                        | u32::from(bank) << 8
                        | u32::from(status)
                };
                match f {
                    Flex::Tempo { group, tens_of_ns } => {
                        Ump::from_array([head(group, Form::Complete, 0, 0), tens_of_ns, 0, 0])
                    }
                    Flex::TimeSignature {
                        group,
                        numerator,
                        denominator_power,
                    } => Ump::from_array([
                        head(group, Form::Complete, 0, 1),
                        u32::from(numerator) << 24 | u32::from(denominator_power) << 16,
                        0,
                        0,
                    ]),
                    Flex::KeySignature {
                        group,
                        sharps,
                        tonic,
                    } => Ump::from_array([
                        head(group, Form::Complete, 0, 5),
                        (u32::from(sharps as u8) & 0xF) << 28 | u32::from(tonic & 0xF) << 24,
                        0,
                        0,
                    ]),
                    Flex::Text {
                        group,
                        form,
                        bank,
                        status,
                        bytes,
                    } => {
                        let w = words_of(&bytes);
                        Ump::from_array([head(group, form, bank, status), w[0], w[1], w[2]])
                    }
                }
            }
            Message::Stream(s) => {
                let head = |form: Form, status: u32, data: u32| {
                    0xF000_0000 | form.bits() << 26 | status << 16 | (data & 0xFFFF)
                };
                let text14 = |form: Form, status: u32, b: &[u8; 14]| {
                    let w = words_of(&b[2..]);
                    Ump::from_array([
                        head(form, status, u32::from(b[0]) << 8 | u32::from(b[1])),
                        w[0],
                        w[1],
                        w[2],
                    ])
                };
                let one = |w0: u32, w1: u32| Ump::from_array([w0, w1, 0, 0]);
                match s {
                    Stream::EndpointDiscovery {
                        major,
                        minor,
                        filter,
                    } => one(
                        head(
                            Form::Complete,
                            0x000,
                            u32::from(major) << 8 | u32::from(minor),
                        ),
                        u32::from(filter),
                    ),
                    Stream::EndpointInfo {
                        major,
                        minor,
                        static_blocks,
                        blocks,
                        midi2,
                        midi1,
                    } => one(
                        head(
                            Form::Complete,
                            0x001,
                            u32::from(major) << 8 | u32::from(minor),
                        ),
                        u32::from(static_blocks) << 31
                            | u32::from(blocks & 0x7F) << 24
                            | u32::from(midi2) << 9
                            | u32::from(midi1) << 8,
                    ),
                    Stream::EndpointName { form, bytes } => text14(form, 0x003, &bytes),
                    Stream::ProductInstanceId { form, bytes } => text14(form, 0x004, &bytes),
                    Stream::ConfigurationRequest { protocol } => {
                        one(head(Form::Complete, 0x005, u32::from(protocol) << 8), 0)
                    }
                    Stream::ConfigurationNotification { protocol } => {
                        one(head(Form::Complete, 0x006, u32::from(protocol) << 8), 0)
                    }
                    Stream::FunctionBlockDiscovery { block, filter } => one(
                        head(
                            Form::Complete,
                            0x010,
                            u32::from(block) << 8 | u32::from(filter),
                        ),
                        0,
                    ),
                    Stream::FunctionBlockInfo {
                        active,
                        block,
                        direction,
                        first_group,
                        groups,
                    } => one(
                        head(
                            Form::Complete,
                            0x011,
                            (u32::from(active) << 7 | u32::from(block & 0x7F)) << 8
                                | u32::from(direction & 3),
                        ),
                        u32::from(first_group) << 24 | u32::from(groups) << 16,
                    ),
                    Stream::FunctionBlockName { form, block, bytes } => {
                        let w = words_of(&bytes[1..]);
                        Ump::from_array([
                            head(form, 0x012, u32::from(block) << 8 | u32::from(bytes[0])),
                            w[0],
                            w[1],
                            w[2],
                        ])
                    }
                    Stream::StartOfClip => one(head(Form::Complete, 0x020, 0), 0),
                    Stream::EndOfClip => one(head(Form::Complete, 0x021, 0), 0),
                }
            }
            Message::Other(p) => p,
        }
    }
}

/// UTF-8 text cut into pieces of `size` bytes (zero padded), with their
/// forms; an empty text is one empty piece.
pub fn text_pieces<const N: usize>(text: &str) -> Vec<(Form, [u8; N])> {
    let bytes = text.as_bytes();
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[][..]]
    } else {
        bytes.chunks(N).collect()
    };
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let mut b = [0u8; N];
            b[..c.len()].copy_from_slice(c);
            (Form::of_piece(i, n), b)
        })
        .collect()
}

/// Text from pieces' bytes (zero padding dropped).
pub fn text_of(bytes: &[u8]) -> String {
    let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// SysEx bytes (without F0/F7) as SysEx7 packets.
pub fn sysex7(group: u8, data: &[u8]) -> Vec<Ump> {
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(6).collect()
    };
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let mut bytes = [0u8; 6];
            bytes[..c.len()].copy_from_slice(c);
            Message::Sysex7 {
                group,
                form: Form::of_piece(i, n),
                bytes,
                len: c.len() as u8,
            }
            .to_ump()
        })
        .collect()
}

// --- per-note expression -----------------------------------------------------

/// The range of a per-note pitch bend (semitones each way): MIDI 2.0's
/// default, as MPE's.
pub const PER_NOTE_BEND_RANGE: f64 = 48.0;

/// Per-note volume 0…1 as dB: the General MIDI volume curve, 100/127 = as
/// played.
fn volume_db(unit: f64) -> f64 {
    if unit <= 0.0 {
        return -60.0;
    }
    (40.0 * (unit * 127.0 / 100.0).log10()).clamp(-60.0, 12.0)
}

fn volume_unit(db: f64) -> f64 {
    (100.0 / 127.0 * 10f64.powf(db / 40.0)).clamp(0.0, 1.0)
}

/// A MIDI 2.0 per-note message as the host's per-note expression (key,
/// kind, value in the kind's units): per-note pitch bend and absolute pitch
/// as tuning, the registered per-note controllers for volume, pan/balance,
/// modulation (vibrato), expression and brightness.
pub fn per_note_expression(voice: &Voice2) -> Option<(u8, NoteExpressionKind, f64)> {
    use NoteExpressionKind as K;
    Some(match *voice {
        Voice2::PerNotePitchBend { note, value } => {
            (note, K::Tuning, bend_of(value) * PER_NOTE_BEND_RANGE)
        }
        Voice2::RegisteredPerNote { note, index, value } => {
            let u = unit_of(value);
            match index {
                per_note::PITCH => (
                    note,
                    K::Tuning,
                    f64::from(value) / f64::from(1u32 << 25) - f64::from(note),
                ),
                per_note::VOLUME => (note, K::Volume, volume_db(u)),
                per_note::PAN | per_note::BALANCE => (note, K::Pan, 2.0 * u - 1.0),
                per_note::MODULATION => (note, K::Vibrato, u),
                per_note::EXPRESSION => (note, K::Expression, u),
                per_note::BRIGHTNESS => (note, K::Brightness, u),
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// The host's per-note expression as a MIDI 2.0 message (pressure as poly
/// pressure).
pub fn per_note_message(key: u8, kind: NoteExpressionKind, value: f64) -> Voice2 {
    use NoteExpressionKind as K;
    let note = key & 0x7F;
    let registered = |index: u8, u: f64| Voice2::RegisteredPerNote {
        note,
        index,
        value: of_unit(u),
    };
    match kind {
        K::Tuning => Voice2::PerNotePitchBend {
            note,
            value: of_bend(value / PER_NOTE_BEND_RANGE),
        },
        K::Volume => registered(per_note::VOLUME, volume_unit(value)),
        K::Pan => registered(per_note::PAN, (value + 1.0) / 2.0),
        K::Vibrato => registered(per_note::MODULATION, value),
        K::Expression => registered(per_note::EXPRESSION, value),
        K::Brightness => registered(per_note::BRIGHTNESS, value),
        K::Pressure => Voice2::PolyPressure {
            note,
            value: of_unit(value),
        },
    }
}

// --- MIDI 1.0 → MIDI 2.0 ------------------------------------------------------

/// Translates MIDI 1.0 channel voice messages into MIDI 2.0 ones, as the
/// specification's default translation does: values scaled up, bank select
/// held for the next program change, RPN/NRPN data entry as registered or
/// assignable controllers.
#[derive(Clone, Debug, Default)]
pub struct Midi1ToMidi2 {
    channels: [ChannelState; 16],
}

#[derive(Clone, Copy, Debug, Default)]
struct ChannelState {
    bank: (Option<u8>, Option<u8>),
    /// (registered, msb, lsb) of the parameter selected.
    parameter: Option<(bool, u8, u8)>,
    parameter_msb: Option<u8>,
    parameter_lsb: Option<u8>,
    registered: bool,
    data_msb: u8,
}

impl Midi1ToMidi2 {
    pub fn new() -> Self {
        Self::default()
    }

    /// The MIDI 2.0 message of MIDI 1.0 bytes (none for bank select and
    /// parameter numbers: they wait for what they belong to).
    pub fn translate(&mut self, bytes: [u8; 3]) -> Option<Voice2> {
        let ch = &mut self.channels[usize::from(bytes[0] & 0xF)];
        let (d1, d2) = (bytes[1] & 0x7F, bytes[2] & 0x7F);
        let up = |v: u8, bits| scale_up(u32::from(v), 7, bits);
        Some(match bytes[0] & 0xF0 {
            0x80 => Voice2::NoteOff {
                note: d1,
                velocity: up(d2, 16) as u16,
                attribute: Attribute::default(),
            },
            0x90 if d2 == 0 => Voice2::NoteOff {
                note: d1,
                velocity: 0x8000,
                attribute: Attribute::default(),
            },
            0x90 => Voice2::NoteOn {
                note: d1,
                velocity: up(d2, 16) as u16,
                attribute: Attribute::default(),
            },
            0xA0 => Voice2::PolyPressure {
                note: d1,
                value: up(d2, 32),
            },
            0xB0 => match d1 {
                0 => {
                    ch.bank.0 = Some(d2);
                    return None;
                }
                32 => {
                    ch.bank.1 = Some(d2);
                    return None;
                }
                99 | 101 => {
                    ch.registered = d1 == 101;
                    ch.parameter_msb = Some(d2);
                    ch.parameter = None;
                    return None;
                }
                98 | 100 => {
                    ch.registered = d1 == 100;
                    ch.parameter_lsb = Some(d2);
                    ch.parameter = match (ch.parameter_msb, ch.parameter_lsb) {
                        (Some(m), Some(l)) if !(m == 127 && l == 127) => {
                            Some((ch.registered, m, l))
                        }
                        _ => None,
                    };
                    return None;
                }
                6 | 38 => {
                    let (registered, bank, index) = ch.parameter?;
                    let value = if d1 == 6 {
                        ch.data_msb = d2;
                        scale_up(u32::from(d2) << 7, 14, 32)
                    } else {
                        scale_up(u32::from(ch.data_msb) << 7 | u32::from(d2), 14, 32)
                    };
                    if registered {
                        Voice2::Registered { bank, index, value }
                    } else {
                        Voice2::Assignable { bank, index, value }
                    }
                }
                _ => Voice2::ControlChange {
                    index: d1,
                    value: up(d2, 32),
                },
            },
            0xC0 => {
                let bank = match ch.bank {
                    (Some(m), l) => Some((m, l.unwrap_or(0))),
                    (None, Some(l)) => Some((0, l)),
                    (None, None) => None,
                };
                Voice2::ProgramChange { program: d1, bank }
            }
            0xD0 => Voice2::ChannelPressure(up(d1, 32)),
            0xE0 => Voice2::PitchBend(scale_up(u32::from(d2) << 7 | u32::from(d1), 14, 32)),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_keeps_min_centre_and_max() {
        assert_eq!(scale_up(0, 7, 32), 0);
        assert_eq!(scale_up(64, 7, 32), 0x8000_0000);
        assert_eq!(scale_up(127, 7, 32), 0xFFFF_FFFF);
        assert_eq!(scale_up(127, 7, 16), 0xFFFF);
        assert_eq!(scale_up(64, 7, 16), 0x8000);
        assert_eq!(scale_up(0x2000, 14, 32), 0x8000_0000);
        assert_eq!(scale_up(0x3FFF, 14, 32), 0xFFFF_FFFF);
        for v in 0..128u32 {
            assert_eq!(scale_down(scale_up(v, 7, 32), 32, 7), v);
            assert_eq!(scale_down(scale_up(v, 7, 16), 16, 7), v);
        }
        // Monotonic.
        let mut last = 0;
        for v in 1..128u32 {
            let x = scale_up(v, 7, 32);
            assert!(x > last);
            last = x;
        }
        assert_eq!(of_bend(0.0), 0x8000_0000);
        assert!((bend_of(of_bend(-0.25)) + 0.25).abs() < 1e-9);
        assert!((unit_of(of_unit(0.3)) - 0.3).abs() < 1e-9);
    }

    #[test]
    fn packets_split_a_stream_by_message_type() {
        let words = [
            0x0040_0010, // DC
            0x4090_3C00,
            0xC000_0000, // MIDI 2.0 note on
            0x2090_3C40, // MIDI 1.0 note on
            0xF020_0000,
            0,
            0,
            0,           // start of clip
            0x4080_3C00, // truncated
        ];
        let p: Vec<Ump> = packets(&words).collect();
        assert_eq!(p.len(), 4);
        assert_eq!(p[1].words(), &[0x4090_3C00, 0xC000_0000]);
        assert_eq!(Message::parse(&p[3]), Message::Stream(Stream::StartOfClip));
    }

    #[test]
    fn every_message_reads_back_as_written() {
        let attr = Attribute {
            kind: Attribute::PITCH_7_9,
            data: 60 * 512 + 256,
        };
        let messages = [
            Message::Utility(Utility::DeltaClockstamp(0xF_FFFF)),
            Message::Utility(Utility::TicksPerQuarter(960)),
            Message::Utility(Utility::JrTimestamp(1234)),
            Message::System {
                group: 3,
                status: 0xF2,
                data: [0x10, 0x20],
            },
            Message::Midi1 {
                group: 1,
                bytes: [0x93, 60, 100],
            },
            Message::Sysex7 {
                group: 0,
                form: Form::Start,
                bytes: [0x7E, 0x7F, 6, 1, 0, 0],
                len: 4,
            },
            Message::Midi2 {
                group: 2,
                channel: 5,
                voice: Voice2::NoteOn {
                    note: 61,
                    velocity: 0xABCD,
                    attribute: attr,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 0,
                voice: Voice2::NoteOff {
                    note: 61,
                    velocity: 0x1234,
                    attribute: Attribute::default(),
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 15,
                voice: Voice2::RegisteredPerNote {
                    note: 64,
                    index: per_note::BRIGHTNESS,
                    value: 0xDEAD_BEEF,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 1,
                voice: Voice2::AssignablePerNote {
                    note: 1,
                    index: 200,
                    value: 7,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 1,
                voice: Voice2::PerNotePitchBend {
                    note: 70,
                    value: 0x9000_0000,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 1,
                voice: Voice2::PerNoteManagement {
                    note: 70,
                    detach: true,
                    reset: false,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::ControlChange {
                    index: 74,
                    value: 0x1234_5678,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::Registered {
                    bank: 0,
                    index: 0,
                    value: 0x0C00_0000,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::RelativeAssignable {
                    bank: 3,
                    index: 4,
                    delta: -5,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::ProgramChange {
                    program: 12,
                    bank: Some((1, 2)),
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::ProgramChange {
                    program: 12,
                    bank: None,
                },
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::ChannelPressure(77),
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::PitchBend(0x8000_0001),
            },
            Message::Midi2 {
                group: 0,
                channel: 2,
                voice: Voice2::PolyPressure {
                    note: 9,
                    value: u32::MAX,
                },
            },
            Message::Flex(Flex::Tempo {
                group: 0,
                tens_of_ns: Flex::tens_of_ns(120.0),
            }),
            Message::Flex(Flex::TimeSignature {
                group: 0,
                numerator: 6,
                denominator_power: 3,
            }),
            Message::Flex(Flex::KeySignature {
                group: 0,
                sharps: -3,
                tonic: 3,
            }),
            Message::Flex(Flex::Text {
                group: 0,
                form: Form::Complete,
                bank: 2,
                status: 1,
                bytes: *b"Hello world!",
            }),
            Message::Stream(Stream::EndpointDiscovery {
                major: 1,
                minor: 1,
                filter: 0x1F,
            }),
            Message::Stream(Stream::EndpointInfo {
                major: 1,
                minor: 1,
                static_blocks: true,
                blocks: 1,
                midi2: true,
                midi1: true,
            }),
            Message::Stream(Stream::EndpointName {
                form: Form::Complete,
                bytes: *b"FaderFrame\0\0\0\0",
            }),
            Message::Stream(Stream::FunctionBlockInfo {
                active: true,
                block: 0,
                direction: 3,
                first_group: 0,
                groups: 1,
            }),
            Message::Stream(Stream::FunctionBlockName {
                form: Form::End,
                block: 0,
                bytes: *b"Main\0\0\0\0\0\0\0\0\0",
            }),
            Message::Stream(Stream::ConfigurationNotification { protocol: 2 }),
            Message::Stream(Stream::EndOfClip),
        ];
        for m in messages {
            let p = m.to_ump();
            assert_eq!(Message::parse(&p), m, "{m:?} as {:08X?}", p.words());
            assert_eq!(Ump::new(p.words()), Some(p));
        }
        assert!((Flex::bpm(Flex::tens_of_ns(93.5)) - 93.5).abs() < 1e-6);
    }

    #[test]
    fn midi1_translates_to_midi2_and_back() {
        let mut t = Midi1ToMidi2::new();
        let on = t.translate([0x91, 60, 100]).unwrap();
        assert_eq!(
            on,
            Voice2::NoteOn {
                note: 60,
                velocity: scale_up(100, 7, 16) as u16,
                attribute: Attribute::default()
            }
        );
        assert_eq!(on.to_midi1(1).0[0], [0x91, 60, 100]);
        // Velocity 0 is a note off.
        assert!(matches!(
            t.translate([0x91, 60, 0]),
            Some(Voice2::NoteOff { note: 60, .. })
        ));
        // Bank select waits for the program change.
        assert_eq!(t.translate([0xB0, 0, 5]), None);
        assert_eq!(t.translate([0xB0, 32, 3]), None);
        let pc = t.translate([0xC0, 7, 0]).unwrap();
        assert_eq!(
            pc,
            Voice2::ProgramChange {
                program: 7,
                bank: Some((5, 3))
            }
        );
        let (m1, n) = pc.to_midi1(0);
        assert_eq!(&m1[..n], &[[0xB0, 0, 5], [0xB0, 32, 3], [0xC0, 7, 0]]);
        // RPN 0 (pitch bend range) = 12 semitones.
        assert_eq!(t.translate([0xB2, 101, 0]), None);
        assert_eq!(t.translate([0xB2, 100, 0]), None);
        let rpn = t.translate([0xB2, 6, 12]).unwrap();
        assert!(matches!(
            rpn,
            Voice2::Registered {
                bank: 0,
                index: 0,
                ..
            }
        ));
        let (m1, n) = rpn.to_midi1(2);
        assert_eq!(n, 4);
        assert_eq!(m1[2], [0xB2, 6, 12]);
        // A plain CC and pitch bend.
        let cc = t.translate([0xB0, 74, 127]).unwrap();
        assert_eq!(
            cc,
            Voice2::ControlChange {
                index: 74,
                value: u32::MAX
            }
        );
        let pb = t.translate([0xE0, 0, 0x40]).unwrap();
        assert_eq!(pb, Voice2::PitchBend(0x8000_0000));
        assert_eq!(pb.to_midi1(0).0[0], [0xE0, 0, 0x40]);
        // A MIDI 2.0 note on at velocity 1/65535 still sounds in MIDI 1.0.
        let soft = Voice2::NoteOn {
            note: 1,
            velocity: 1,
            attribute: Attribute::default(),
        };
        assert_eq!(soft.to_midi1(0).0[0][2], 1);
        // Per-note controllers have no MIDI 1.0 message.
        let pn = Voice2::PerNotePitchBend { note: 1, value: 0 };
        assert_eq!(pn.to_midi1(0).1, 0);
    }

    #[test]
    fn per_note_expression_goes_both_ways() {
        use NoteExpressionKind as K;
        for (kind, v) in [
            (K::Tuning, 2.5),
            (K::Tuning, -0.25),
            (K::Volume, -6.0),
            (K::Volume, 0.0),
            (K::Pan, -0.5),
            (K::Vibrato, 0.3),
            (K::Expression, 0.9),
            (K::Brightness, 0.6),
        ] {
            let m = per_note_message(60, kind, v);
            let (key, k, back) = per_note_expression(&m).unwrap();
            assert_eq!((key, k), (60, kind));
            assert!((back - v).abs() < 1e-6, "{kind:?} {v} → {back}");
        }
        // Absolute pitch 7.25: a quarter tone above the note.
        let pitch = Voice2::RegisteredPerNote {
            note: 60,
            index: per_note::PITCH,
            value: (60 << 25) + (1 << 23),
        };
        let (_, k, v) = per_note_expression(&pitch).unwrap();
        assert_eq!(k, K::Tuning);
        assert!((v - 0.25).abs() < 1e-9);
        assert!(matches!(
            per_note_message(61, K::Pressure, 1.0),
            Voice2::PolyPressure {
                note: 61,
                value: u32::MAX
            }
        ));
    }

    #[test]
    fn texts_and_sysex_go_in_pieces() {
        let pieces = text_pieces::<12>("A rather long lyric line");
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].0, Form::Start);
        assert_eq!(pieces[1].0, Form::End);
        let joined: Vec<u8> = pieces.iter().flat_map(|p| p.1).collect();
        assert_eq!(text_of(&joined), "A rather long lyric line");
        let sx = sysex7(0, &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(sx.len(), 2);
        assert!(matches!(
            Message::parse(&sx[1]),
            Message::Sysex7 {
                form: Form::End,
                len: 1,
                ..
            }
        ));
    }
}

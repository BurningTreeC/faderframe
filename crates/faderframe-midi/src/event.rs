/// A dimension of per-note expression: the set CLAP and VST3 share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NoteExpressionKind {
    /// dB (0 = unchanged; at most +12).
    Volume,
    /// −1 (left) … 1 (right).
    Pan,
    /// Semitones (−120 … 120).
    Tuning,
    /// 0 … 1.
    Vibrato,
    /// 0 … 1.
    Expression,
    /// 0 … 1.
    Brightness,
    /// 0 … 1.
    Pressure,
}

impl NoteExpressionKind {
    pub const ALL: [NoteExpressionKind; 7] = [
        Self::Volume,
        Self::Pan,
        Self::Tuning,
        Self::Vibrato,
        Self::Expression,
        Self::Brightness,
        Self::Pressure,
    ];
}

/// The plain value of a note expression in millionths (fixed point keeps
/// [`MidiEvent`] `Eq` and `Hash`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExpressionValue(i32);

impl ExpressionValue {
    pub fn new(plain: f64) -> Self {
        Self(
            (plain * 1e6)
                .round()
                .clamp(i32::MIN as f64, i32::MAX as f64) as i32,
        )
    }

    pub fn get(self) -> f64 {
        self.0 as f64 / 1e6
    }

    /// The fixed-point representation (for shared memory).
    pub fn raw(self) -> i32 {
        self.0
    }

    pub fn from_raw(raw: i32) -> Self {
        Self(raw)
    }
}

/// A system exclusive message held by a [`MidiBuffer`](crate::MidiBuffer):
/// where its bytes are in that buffer. Only the buffer that holds the bytes
/// can resolve it ([`MidiBuffer::sysex`](crate::MidiBuffer::sysex)); copy
/// such events between buffers with
/// [`MidiBuffer::push_from`](crate::MidiBuffer::push_from).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SysexRef {
    pub(crate) buffer: u32,
    pub(crate) start: u32,
    pub(crate) len: u32,
}

impl SysexRef {
    /// Bytes in the message (`F0 … F7`).
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// A channel-voice MIDI message (channels are 0-based, 0..=15), a per-note
/// expression for hosted plugins, or a system exclusive message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MidiEvent {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    PolyPressure {
        channel: u8,
        key: u8,
        pressure: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    /// 14-bit value, 8192 = centre.
    PitchBend {
        channel: u8,
        value: u16,
    },
    /// Per-note expression of the sounding note `key` on `channel` (CLAP
    /// note expressions, VST3 note expression values). Not a MIDI message:
    /// MIDI outputs drop it.
    NoteExpression {
        channel: u8,
        key: u8,
        kind: NoteExpressionKind,
        value: ExpressionValue,
    },
    /// A system exclusive message (`F0 … F7`) for hosted plugins; its bytes
    /// are in the buffer holding the event. External MIDI outputs get clip
    /// SysEx from the control side instead and drop it here.
    SysEx(SysexRef),
}

impl MidiEvent {
    pub const PITCH_BEND_CENTRE: u16 = 8192;
    pub const CC_ALL_NOTES_OFF: u8 = 123;
    pub const CC_ALL_SOUND_OFF: u8 = 120;

    /// Parse a raw channel-voice message. A note-on with velocity 0 is
    /// normalised to a note-off. Returns `None` for system/incomplete messages.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let status = *bytes.first()?;
        let channel = status & 0x0F;
        let d1 = || bytes.get(1).map(|b| b & 0x7F);
        let d2 = || bytes.get(2).map(|b| b & 0x7F);
        Some(match status & 0xF0 {
            0x80 => MidiEvent::NoteOff {
                channel,
                key: d1()?,
                velocity: d2()?,
            },
            0x90 => {
                let (key, velocity) = (d1()?, d2()?);
                if velocity == 0 {
                    MidiEvent::NoteOff {
                        channel,
                        key,
                        velocity: 64,
                    }
                } else {
                    MidiEvent::NoteOn {
                        channel,
                        key,
                        velocity,
                    }
                }
            }
            0xA0 => MidiEvent::PolyPressure {
                channel,
                key: d1()?,
                pressure: d2()?,
            },
            0xB0 => MidiEvent::ControlChange {
                channel,
                controller: d1()?,
                value: d2()?,
            },
            0xC0 => MidiEvent::ProgramChange {
                channel,
                program: d1()?,
            },
            0xD0 => MidiEvent::ChannelPressure {
                channel,
                pressure: d1()?,
            },
            0xE0 => MidiEvent::PitchBend {
                channel,
                value: (d1()? as u16) | ((d2()? as u16) << 7),
            },
            _ => return None,
        })
    }

    /// Encode to raw bytes; returns the buffer and the number of valid bytes
    /// (0 for a note expression, which has no MIDI form).
    pub fn to_bytes(self) -> ([u8; 3], usize) {
        let ch = |c: u8| c & 0x0F;
        match self {
            MidiEvent::NoteOff {
                channel,
                key,
                velocity,
            } => ([0x80 | ch(channel), key & 0x7F, velocity & 0x7F], 3),
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } => ([0x90 | ch(channel), key & 0x7F, velocity & 0x7F], 3),
            MidiEvent::PolyPressure {
                channel,
                key,
                pressure,
            } => ([0xA0 | ch(channel), key & 0x7F, pressure & 0x7F], 3),
            MidiEvent::ControlChange {
                channel,
                controller,
                value,
            } => ([0xB0 | ch(channel), controller & 0x7F, value & 0x7F], 3),
            MidiEvent::ProgramChange { channel, program } => {
                ([0xC0 | ch(channel), program & 0x7F, 0], 2)
            }
            MidiEvent::ChannelPressure { channel, pressure } => {
                ([0xD0 | ch(channel), pressure & 0x7F, 0], 2)
            }
            MidiEvent::PitchBend { channel, value } => (
                [
                    0xE0 | ch(channel),
                    (value & 0x7F) as u8,
                    ((value >> 7) & 0x7F) as u8,
                ],
                3,
            ),
            MidiEvent::NoteExpression { .. } | MidiEvent::SysEx(_) => ([0; 3], 0),
        }
    }

    /// The channel (SysEx has none: 0).
    pub fn channel(self) -> u8 {
        match self {
            MidiEvent::SysEx(_) => 0,
            MidiEvent::NoteOn { channel, .. }
            | MidiEvent::NoteOff { channel, .. }
            | MidiEvent::PolyPressure { channel, .. }
            | MidiEvent::ControlChange { channel, .. }
            | MidiEvent::ProgramChange { channel, .. }
            | MidiEvent::ChannelPressure { channel, .. }
            | MidiEvent::PitchBend { channel, .. }
            | MidiEvent::NoteExpression { channel, .. } => channel,
        }
    }

    /// `fine`'s value when it agrees with the event: when its top bits are
    /// the MIDI 1.0 value (a note-on's 7-bit velocity is at least 1).
    pub fn fine_value(self, fine: Fine) -> Option<u32> {
        let (coarse, bits, wide) = widths(self)?;
        let v = fine.value?;
        if wide == 16 && v > 0xFFFF {
            return None;
        }
        let top = v >> (wide - bits);
        let agrees = match self {
            MidiEvent::NoteOn { .. } => top.max(1) == coarse,
            _ => top == coarse,
        };
        agrees.then_some(v)
    }

    /// The event's value at MIDI 2.0's resolution — a note's 16-bit
    /// velocity, 32 bits for a controller, pressure or pitch bend: `fine`'s
    /// value when it agrees ([`Self::fine_value`]), else the MIDI 1.0
    /// value scaled up the specification's way. `None` for program
    /// changes, note expressions and SysEx.
    pub fn full_value(self, fine: Fine) -> Option<u32> {
        let (coarse, bits, wide) = widths(self)?;
        Some(
            self.fine_value(fine)
                .unwrap_or_else(|| crate::ump::scale_up(coarse, bits, wide)),
        )
    }

    /// The value as 0…1 (pitch bend: 0 down, 1 up): `fine`'s value over
    /// its range when it agrees, else the MIDI 1.0 value over its own (as
    /// plugins have always been given it).
    pub fn full_unit(self, fine: Fine) -> Option<f64> {
        let (coarse, bits, wide) = widths(self)?;
        let max = |b: u32| ((1u64 << b) - 1) as f64;
        Some(match self.fine_value(fine) {
            Some(v) => f64::from(v) / max(wide),
            None => f64::from(coarse) / max(bits),
        })
    }

    /// A note-on's or note-off's 16-bit velocity at full resolution.
    pub fn velocity16(self, fine: Fine) -> Option<u16> {
        match self {
            MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. } => {
                self.full_value(fine).map(|v| v.min(0xFFFF) as u16)
            }
            _ => None,
        }
    }

    /// Ordering priority for events sharing a sample offset: note-offs go
    /// first so a retriggered note at the same instant is not cut off;
    /// note expressions follow their note-on (they address a sounding
    /// note).
    #[inline]
    pub fn same_time_priority(self) -> u8 {
        match self {
            MidiEvent::NoteOff { .. } => 0,
            MidiEvent::NoteOn { .. } => 2,
            MidiEvent::NoteExpression { .. } => 3,
            _ => 1,
        }
    }
}

/// What MIDI 2.0 adds to an event's MIDI 1.0 form: its value at full
/// resolution — a note's 16-bit velocity, 32 bits for a controller,
/// pressure or pitch bend — and a note's attribute.
///
/// It rides beside the event ([`TimedMidiEvent::fine`], the input and
/// output queues' events) and counts only while it agrees with it: while
/// its top bits are the event's own value ([`MidiEvent::full_value`]).
/// Whatever changes an event's value at MIDI 1.0's resolution (a velocity
/// curve, a MIDI effect's transform) so leaves an old fine value behind by
/// itself and the event plays at what it says; moving the event in time,
/// to another key or channel keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fine {
    /// The value at MIDI 2.0's width (`None`: the MIDI 1.0 value scaled
    /// up).
    pub value: Option<u32>,
    /// A note's attribute (manufacturer or profile specific data; a pitch
    /// 7.9 attribute travels as a tuning expression instead). Kind 0: none.
    pub attribute: crate::ump::Attribute,
}

impl Fine {
    /// Nothing beyond the MIDI 1.0 form.
    pub const NONE: Fine = Fine {
        value: None,
        attribute: crate::ump::Attribute { kind: 0, data: 0 },
    };

    /// A value at MIDI 2.0's width.
    pub const fn value(value: u32) -> Fine {
        Fine {
            value: Some(value),
            attribute: crate::ump::Attribute { kind: 0, data: 0 },
        }
    }

    pub const fn with_attribute(mut self, attribute: crate::ump::Attribute) -> Fine {
        self.attribute = attribute;
        self
    }

    pub fn is_none(&self) -> bool {
        self.value.is_none() && self.attribute.kind == 0
    }

    /// A note's 16-bit velocity as its MIDI 1.0 velocity and this (the
    /// note-on's MIDI 1.0 velocity is at least 1: 0 would end it).
    pub fn of_velocity(velocity: u16, note_on: bool) -> (u8, Fine) {
        let seven = (velocity >> 9) as u8;
        (
            if note_on { seven.max(1) } else { seven },
            Fine::value(u32::from(velocity)),
        )
    }

    /// A velocity of 0…1 (a plugin's) as [`Self::of_velocity`].
    pub fn of_unit_velocity(unit: f64, note_on: bool) -> (u8, Fine) {
        let v = (unit.clamp(0.0, 1.0) * 65535.0).round() as u16;
        Self::of_velocity(v, note_on)
    }

    /// A 32-bit value as a `bits`-wide MIDI 1.0 value (7, or 14 for pitch
    /// bend) and this.
    pub fn of_value(value: u32, bits: u32) -> (u16, Fine) {
        ((value >> (32 - bits)) as u16, Fine::value(value))
    }
}

/// The MIDI 1.0 value of an event and MIDI 2.0's widths: (value, its bits,
/// the full width).
fn widths(event: MidiEvent) -> Option<(u32, u32, u32)> {
    Some(match event {
        MidiEvent::NoteOn { velocity, .. } | MidiEvent::NoteOff { velocity, .. } => {
            (u32::from(velocity), 7, 16)
        }
        MidiEvent::PolyPressure { pressure, .. } | MidiEvent::ChannelPressure { pressure, .. } => {
            (u32::from(pressure), 7, 32)
        }
        MidiEvent::ControlChange { value, .. } => (u32::from(value), 7, 32),
        MidiEvent::PitchBend { value, .. } => (u32::from(value), 14, 32),
        _ => return None,
    })
}

/// A MIDI event at a frame offset within the current processing block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedMidiEvent {
    pub sample_offset: u32,
    pub event: MidiEvent,
    /// MIDI 2.0's resolution of the event's value (see [`Fine`]).
    pub fine: Fine,
}

impl TimedMidiEvent {
    #[inline]
    pub fn new(sample_offset: u32, event: MidiEvent) -> Self {
        Self {
            sample_offset,
            event,
            fine: Fine::NONE,
        }
    }

    /// An event with its MIDI 2.0 resolution.
    #[inline]
    pub fn with_fine(sample_offset: u32, event: MidiEvent, fine: Fine) -> Self {
        Self {
            sample_offset,
            event,
            fine,
        }
    }

    /// The same event (and resolution) at another offset.
    #[inline]
    pub fn at(self, sample_offset: u32) -> Self {
        Self {
            sample_offset,
            ..self
        }
    }

    /// The event's value at MIDI 2.0's resolution (see
    /// [`MidiEvent::full_value`]).
    #[inline]
    pub fn full_value(&self) -> Option<u32> {
        self.event.full_value(self.fine)
    }

    /// The value as 0…1 at full resolution: a note's velocity, a
    /// controller's or pressure's value (pitch bend: 0 = down, 1 = up).
    #[inline]
    pub fn unit(&self) -> Option<f64> {
        self.event.full_unit(self.fine)
    }

    #[inline]
    pub(crate) fn sort_key(&self) -> (u32, u8) {
        (self.sample_offset, self.event.same_time_priority())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_expressions_are_fixed_point_and_have_no_midi_form() {
        let v = ExpressionValue::new(-1.234_567_8);
        assert!((v.get() - -1.234_568).abs() < 1e-9);
        let e = MidiEvent::NoteExpression {
            channel: 2,
            key: 64,
            kind: NoteExpressionKind::Tuning,
            value: v,
        };
        assert_eq!(e.to_bytes().1, 0);
        assert_eq!(e.channel(), 2);
        assert!(
            e.same_time_priority()
                > MidiEvent::NoteOn {
                    channel: 2,
                    key: 64,
                    velocity: 1
                }
                .same_time_priority()
        );
    }

    #[test]
    fn byte_round_trip() {
        let events = [
            MidiEvent::NoteOn {
                channel: 3,
                key: 60,
                velocity: 100,
            },
            MidiEvent::NoteOff {
                channel: 0,
                key: 61,
                velocity: 0,
            },
            MidiEvent::ControlChange {
                channel: 15,
                controller: 7,
                value: 127,
            },
            MidiEvent::ProgramChange {
                channel: 2,
                program: 5,
            },
            MidiEvent::PitchBend {
                channel: 1,
                value: 12345,
            },
            MidiEvent::ChannelPressure {
                channel: 9,
                pressure: 44,
            },
        ];
        for e in events {
            let (bytes, n) = e.to_bytes();
            assert_eq!(MidiEvent::from_bytes(&bytes[..n]), Some(e));
        }
    }

    #[test]
    fn a_fine_value_counts_while_it_agrees() {
        let on = |velocity| MidiEvent::NoteOn {
            channel: 0,
            key: 60,
            velocity,
        };
        // 0x1234 has 9 as its top seven bits.
        let (v7, fine) = Fine::of_velocity(0x1234, true);
        assert_eq!(v7, 9);
        assert_eq!(on(v7).velocity16(fine), Some(0x1234));
        assert_eq!(
            TimedMidiEvent::with_fine(3, on(v7), fine)
                .at(7)
                .full_value(),
            Some(0x1234),
            "moved in time, it keeps its resolution"
        );
        // Changed at seven bits: the MIDI 1.0 value, scaled up.
        assert_eq!(on(10).velocity16(fine), Some(10 << 9));
        assert_eq!(on(127).velocity16(Fine::NONE), Some(0xFFFF));
        // A MIDI 2.0 note-on below 1/128 is a note-on of velocity 1.
        let (v7, fine) = Fine::of_velocity(100, true);
        assert_eq!((v7, on(v7).velocity16(fine)), (1, Some(100)));
        // Controllers and pitch bend at 32 bits.
        let (v, fine) = Fine::of_value(0xDEAD_BEEF, 7);
        let cc = MidiEvent::ControlChange {
            channel: 1,
            controller: 1,
            value: v as u8,
        };
        assert_eq!(cc.full_value(fine), Some(0xDEAD_BEEF));
        let (v, fine) = Fine::of_value(0x8000_1234, 14);
        let bend = MidiEvent::PitchBend {
            channel: 1,
            value: v,
        };
        assert_eq!(v, 8192);
        assert_eq!(bend.full_value(fine), Some(0x8000_1234));
        assert_eq!(bend.full_value(Fine::NONE), Some(0x8000_0000));
        assert!((bend.full_unit(fine).unwrap() - 0.5).abs() < 1e-4);
        assert_eq!(
            bend.full_unit(Fine::NONE),
            Some(8192.0 / 16383.0),
            "MIDI 1.0 as it was"
        );
        assert_eq!(on(127).full_unit(Fine::NONE), Some(1.0));
        // Not for program changes, expressions or SysEx.
        assert_eq!(
            MidiEvent::ProgramChange {
                channel: 0,
                program: 3
            }
            .full_value(Fine::value(7)),
            None
        );
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        assert_eq!(
            MidiEvent::from_bytes(&[0x92, 64, 0]),
            Some(MidiEvent::NoteOff {
                channel: 2,
                key: 64,
                velocity: 64
            })
        );
        assert_eq!(MidiEvent::from_bytes(&[0xF8]), None);
        assert_eq!(MidiEvent::from_bytes(&[0x90, 60]), None);
    }
}

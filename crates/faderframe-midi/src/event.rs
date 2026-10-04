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

/// A MIDI event at a frame offset within the current processing block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedMidiEvent {
    pub sample_offset: u32,
    pub event: MidiEvent,
}

impl TimedMidiEvent {
    #[inline]
    pub fn new(sample_offset: u32, event: MidiEvent) -> Self {
        Self {
            sample_offset,
            event,
        }
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

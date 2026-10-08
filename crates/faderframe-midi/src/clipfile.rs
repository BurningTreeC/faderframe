//! MIDI Clip Files (SMF2, M2-116-U 1.0): `SMF2CLIP`, then Universal MIDI
//! Packets as big-endian words, each after a Delta Clockstamp — the clip
//! header (ticks per quarter note, configuration), Start of Clip, the
//! clip's messages, End of Clip (whose clockstamp is the clip's length).
//! A gap longer than a clockstamp holds (20 bits) is several clockstamps,
//! with no-ops between them.
//!
//! [`ClipFile`] is the container only: ticks and packets; what they mean is
//! [`crate::ump`]'s.

use crate::ump::{Message, Stream, Ump, Utility, packets};
use std::fmt;

/// The first eight bytes.
pub const MAGIC: &[u8; 8] = b"SMF2CLIP";

/// The largest Delta Clockstamp.
pub const MAX_DELTA: u32 = 0xF_FFFF;

/// A clip: its resolution, its packets at their ticks, its length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipFile {
    pub ticks_per_quarter: u16,
    /// Sorted by tick (equal ticks in the order they play).
    pub events: Vec<(u64, Ump)>,
    /// The tick of End of Clip.
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipFileError {
    /// Not `SMF2CLIP`.
    NotAClipFile,
    /// No Delta Clockstamp Ticks Per Quarter Note in the header.
    NoResolution,
}

impl fmt::Display for ClipFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ClipFileError::NotAClipFile => "not a MIDI 2.0 clip file (no SMF2CLIP header)",
            ClipFileError::NoResolution => "the clip file gives no ticks per quarter note",
        })
    }
}

impl std::error::Error for ClipFileError {}

fn push(words: &mut Vec<u32>, m: Message) {
    words.extend_from_slice(m.to_ump().words());
}

fn push_delta(words: &mut Vec<u32>, mut delta: u64) {
    while delta > u64::from(MAX_DELTA) {
        push(words, Message::Utility(Utility::DeltaClockstamp(MAX_DELTA)));
        push(words, Message::Utility(Utility::Noop));
        delta -= u64::from(MAX_DELTA);
    }
    push(
        words,
        Message::Utility(Utility::DeltaClockstamp(delta as u32)),
    );
}

impl ClipFile {
    /// The file's bytes.
    pub fn write(&self) -> Vec<u8> {
        let mut words = Vec::with_capacity(8 + self.events.len() * 3);
        push_delta(&mut words, 0);
        push(
            &mut words,
            Message::Utility(Utility::TicksPerQuarter(self.ticks_per_quarter.max(1))),
        );
        push_delta(&mut words, 0);
        push(&mut words, Message::Stream(Stream::StartOfClip));
        let mut now = 0u64;
        let mut events: Vec<&(u64, Ump)> = self.events.iter().collect();
        events.sort_by_key(|(t, _)| *t);
        for (t, p) in events {
            push_delta(&mut words, t.saturating_sub(now));
            now = now.max(*t);
            words.extend_from_slice(p.words());
        }
        push_delta(&mut words, self.length.saturating_sub(now));
        push(&mut words, Message::Stream(Stream::EndOfClip));
        let mut out = Vec::with_capacity(8 + words.len() * 4);
        out.extend_from_slice(MAGIC);
        for w in words {
            out.extend_from_slice(&w.to_be_bytes());
        }
        out
    }

    /// Read a file (leniently: a file without Start of Clip has every
    /// message after its header in the clip; without End of Clip it ends
    /// at its last message).
    pub fn read(bytes: &[u8]) -> Result<ClipFile, ClipFileError> {
        let body = bytes
            .strip_prefix(MAGIC.as_slice())
            .ok_or(ClipFileError::NotAClipFile)?;
        let words: Vec<u32> = body
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_be_bytes(*c))
            .collect();
        let all: Vec<Ump> = packets(&words).collect();
        let has_start = all
            .iter()
            .any(|p| Message::parse(p) == Message::Stream(Stream::StartOfClip));
        let mut tick = 0u64;
        let mut tpq = None;
        let mut started = !has_start;
        let mut events = Vec::new();
        let mut length = None;
        for p in all {
            match Message::parse(&p) {
                Message::Utility(Utility::DeltaClockstamp(d)) => tick += u64::from(d),
                Message::Utility(Utility::TicksPerQuarter(t)) => tpq = Some(t),
                Message::Utility(_) => {}
                Message::Stream(Stream::StartOfClip) => {
                    started = true;
                    tick = 0;
                }
                Message::Stream(Stream::EndOfClip) if started => {
                    length = Some(tick);
                    break;
                }
                // Configuration (the header's stream messages) is not the
                // clip's.
                Message::Stream(_) => {}
                _ if started => events.push((tick, p)),
                _ => {}
            }
        }
        let ticks_per_quarter = tpq.filter(|t| *t > 0).ok_or(ClipFileError::NoResolution)?;
        let last = events.last().map_or(0, |(t, _)| *t);
        Ok(ClipFile {
            ticks_per_quarter,
            events,
            length: length.unwrap_or(last).max(last),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ump::{Attribute, Voice2};

    fn note(on: bool, key: u8) -> Ump {
        let attribute = Attribute::default();
        Message::Midi2 {
            group: 0,
            channel: 0,
            voice: if on {
                Voice2::NoteOn {
                    note: key,
                    velocity: 0x8000,
                    attribute,
                }
            } else {
                Voice2::NoteOff {
                    note: key,
                    velocity: 0,
                    attribute,
                }
            },
        }
        .to_ump()
    }

    #[test]
    fn a_clip_reads_back_as_written() {
        let clip = ClipFile {
            ticks_per_quarter: 960,
            events: vec![
                (0, note(true, 60)),
                (0, note(true, 64)),
                (960, note(false, 60)),
                (960, note(false, 64)),
                // Past what one clockstamp holds.
                (3_000_000, note(true, 67)),
                (3_000_480, note(false, 67)),
            ],
            length: 3_840_000,
        };
        let bytes = clip.write();
        assert_eq!(&bytes[..8], b"SMF2CLIP");
        assert_eq!(ClipFile::read(&bytes), Ok(clip));
    }

    #[test]
    fn the_file_is_laid_out_as_the_specification_asks() {
        let clip = ClipFile {
            ticks_per_quarter: 480,
            events: vec![(0, note(true, 60)), (480, note(false, 60))],
            length: 1920,
        };
        let bytes = clip.write();
        let words: Vec<u32> = bytes[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_be_bytes(*c))
            .collect();
        let messages: Vec<Message> = packets(&words).map(|p| Message::parse(&p)).collect();
        use Message::{Stream as S, Utility as U};
        assert_eq!(messages[0], U(Utility::DeltaClockstamp(0)));
        assert_eq!(messages[1], U(Utility::TicksPerQuarter(480)));
        assert_eq!(messages[2], U(Utility::DeltaClockstamp(0)));
        assert_eq!(messages[3], S(Stream::StartOfClip));
        assert_eq!(messages[4], U(Utility::DeltaClockstamp(0)));
        assert_eq!(messages[6], U(Utility::DeltaClockstamp(480)));
        assert_eq!(messages[8], U(Utility::DeltaClockstamp(1440)));
        assert_eq!(messages[9], S(Stream::EndOfClip));
        assert_eq!(messages.len(), 10);
        assert_eq!(
            ClipFile::read(b"MThd\0\0\0\x06"),
            Err(ClipFileError::NotAClipFile)
        );
    }
}

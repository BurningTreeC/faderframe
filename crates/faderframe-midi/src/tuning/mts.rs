//! MIDI Tuning Standard SysEx (the MMA's MTS, 1992 and the 2006
//! amendment): what each key sounds as three bytes — the equal-tempered
//! semitone at A4 = 440 Hz below the pitch, then a 14-bit fraction of a
//! semitone (100/16384 cent steps). `7F 7F 7F` means "leave the key as it
//! is".
//!
//! * Bulk tuning dump (non-real-time, `F0 7E <dev> 08 01`): a named table
//!   of all 128 keys with a checksum; instruments take it for the notes
//!   they start next.
//! * Single note tuning change (real-time, `F0 7F <dev> 08 02`): up to 127
//!   keys; sounding notes move at once.
//!
//! MTS cannot silence a key: unmapped keys go out at their own
//! equal-tempered pitch.

use super::TuningTable;

/// The device id every receiver answers to.
pub const ALL_DEVICES: u8 = 0x7F;
/// Bytes of a bulk tuning dump.
pub const BULK_LEN: usize = 408;
/// Keys one real-time change carries in [`note_changes`] (two messages
/// cover the keyboard).
pub const KEYS_PER_CHANGE: usize = 64;

/// A pitch (fractional MIDI note number at A4 = 440 Hz) as MTS frequency
/// data; pitches outside the keyboard are clamped to its ends (the top
/// stops short of `7F 7F 7F`, which means "no change").
pub fn frequency_bytes(pitch: f64) -> [u8; 3] {
    if !pitch.is_finite() || pitch <= 0.0 {
        return [0, 0, 0];
    }
    let mut semitone = pitch.floor();
    let mut fraction = ((pitch - semitone) * 16384.0).round();
    if fraction >= 16384.0 {
        semitone += 1.0;
        fraction = 0.0;
    }
    if semitone > 127.0 || (semitone == 127.0 && fraction >= 16383.0) {
        return [0x7F, 0x7F, 0x7E];
    }
    let fraction = fraction as u16;
    [
        semitone as u8,
        (fraction >> 7) as u8,
        (fraction & 0x7F) as u8,
    ]
}

/// The pitch MTS frequency data names (`None`: no change).
pub fn pitch_of(bytes: [u8; 3]) -> Option<f64> {
    if bytes == [0x7F, 0x7F, 0x7F] {
        return None;
    }
    let fraction = (u16::from(bytes[1] & 0x7F) << 7) | u16::from(bytes[2] & 0x7F);
    Some(f64::from(bytes[0] & 0x7F) + f64::from(fraction) / 16384.0)
}

/// A bulk tuning dump of `table` into tuning program `program`, named
/// `name` (16 ASCII characters; others become `?`).
pub fn bulk_dump(table: &TuningTable, program: u8, name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(BULK_LEN);
    out.extend_from_slice(&[0xF0, 0x7E, ALL_DEVICES, 0x08, 0x01, program & 0x7F]);
    let mut chars = name.chars();
    for _ in 0..16 {
        let c = chars.next().unwrap_or(' ');
        out.push(if c.is_ascii() && !c.is_ascii_control() {
            c as u8
        } else {
            b'?'
        });
    }
    for key in 0..128u8 {
        out.extend_from_slice(&frequency_bytes(table.pitch_or_key(key)));
    }
    let checksum = out[1..].iter().fold(0u8, |c, b| c ^ b) & 0x7F;
    out.push(checksum);
    out.push(0xF7);
    out
}

/// One real-time single note tuning change: `changes` = (key, pitch) —
/// `None` leaves the key as it is — at most 127 of them.
pub fn note_change(program: u8, changes: &[(u8, Option<f64>)]) -> Vec<u8> {
    let changes = &changes[..changes.len().min(127)];
    let mut out = Vec::with_capacity(8 + 4 * changes.len());
    out.extend_from_slice(&[
        0xF0,
        0x7F,
        ALL_DEVICES,
        0x08,
        0x02,
        program & 0x7F,
        changes.len() as u8,
    ]);
    for (key, pitch) in changes {
        out.push(key & 0x7F);
        out.extend_from_slice(&pitch.map_or([0x7F, 0x7F, 0x7F], frequency_bytes));
    }
    out.push(0xF7);
    out
}

/// The whole keyboard of `table` as real-time single note tuning changes
/// (one message per [`KEYS_PER_CHANGE`] keys).
pub fn note_changes(table: &TuningTable, program: u8) -> Vec<Vec<u8>> {
    (0..128u8)
        .collect::<Vec<_>>()
        .chunks(KEYS_PER_CHANGE)
        .map(|keys| {
            let changes: Vec<(u8, Option<f64>)> = keys
                .iter()
                .map(|&k| (k, Some(table.pitch_or_key(k))))
                .collect();
            note_change(program, &changes)
        })
        .collect()
}

/// An MTS message read back.
#[derive(Clone, Debug, PartialEq)]
pub enum Mts {
    /// A bulk dump (its checksum checked).
    Bulk {
        program: u8,
        name: String,
        pitches: Vec<Option<f64>>,
    },
    /// Single note changes: (key, pitch or no change).
    NoteChange {
        program: u8,
        realtime: bool,
        changes: Vec<(u8, Option<f64>)>,
    },
}

/// Read an MTS bulk dump or single note change (`None`: another message,
/// or a broken one).
pub fn parse(bytes: &[u8]) -> Option<Mts> {
    let body = bytes.strip_prefix(&[0xF0])?.strip_suffix(&[0xF7])?;
    match body {
        [0x7E, _, 0x08, 0x01, program, rest @ ..] if rest.len() == 16 + 384 + 1 => {
            let checksum = body[..body.len() - 1].iter().fold(0u8, |c, b| c ^ b) & 0x7F;
            if checksum != *rest.last()? {
                return None;
            }
            let name = String::from_utf8_lossy(&rest[..16]).trim_end().to_string();
            let pitches = rest[16..16 + 384]
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| pitch_of(*c))
                .collect();
            Some(Mts::Bulk {
                program: *program,
                name,
                pitches,
            })
        }
        [
            kind @ (0x7E | 0x7F),
            _,
            0x08,
            0x02,
            program,
            count,
            rest @ ..,
        ] if rest.len() == 4 * usize::from(*count) => {
            let changes = rest
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&[key, a, b, c]| (key, pitch_of([a, b, c])))
                .collect();
            Some(Mts::NoteChange {
                program: *program,
                realtime: *kind == 0x7F,
                changes,
            })
        }
        _ => None,
    }
}

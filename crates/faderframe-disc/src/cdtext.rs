//! CD-Text: the lead-in packs of 18 bytes that carry the disc's and the
//! tracks' titles, performers, songwriters, composers, arrangers and
//! messages (block 0, ISO 8859-1, English).
//!
//! Per text type, the disc's string and then each track's follow each
//! other NUL-terminated (a track without that text has an empty string),
//! cut into 12-byte payloads; each pack names the string its first
//! payload byte belongs to and that byte's position in it. Three size
//! packs (type 0x8F) close the block. Every pack ends with its inverted
//! CRC-16/CCITT, big-endian.

use crate::hash::crc16;
use crate::{CdText, Disc, latin1};

/// One pack.
pub type Pack = [u8; 18];

const TEXT_TYPES: std::ops::RangeInclusive<u8> = 0x80..=0x85;
const SIZE_INFO: u8 = 0x8F;

fn seal(p: &mut Pack) {
    let crc = !crc16(&p[..16]);
    p[16..].copy_from_slice(&crc.to_be_bytes());
}

/// The CD-Text packs of `disc` (none when it has no text).
pub fn packs(disc: &Disc) -> Vec<Pack> {
    let mut out: Vec<Pack> = Vec::new();
    let mut counts = [0u8; 16];
    for (k, ty) in TEXT_TYPES.enumerate() {
        let strings: Vec<Vec<u8>> = std::iter::once(&disc.text)
            .chain(disc.tracks.iter().map(|t| &t.text))
            .map(|t| latin1(t.fields()[k]))
            .collect();
        if strings.iter().all(Vec::is_empty) {
            continue;
        }
        // Every byte with the string (0 disc, n track) and position it has.
        let mut bytes: Vec<(u8, u8, usize)> = Vec::new();
        for (owner, s) in strings.iter().enumerate() {
            for (pos, &b) in s.iter().chain(std::iter::once(&0)).enumerate() {
                bytes.push((b, owner as u8, pos));
            }
        }
        for chunk in bytes.chunks(12) {
            let mut p: Pack = [0; 18];
            p[0] = ty;
            p[1] = chunk[0].1;
            p[2] = out.len() as u8;
            p[3] = chunk[0].2.min(15) as u8;
            for (i, (b, _, _)) in chunk.iter().enumerate() {
                p[4 + i] = *b;
            }
            seal(&mut p);
            out.push(p);
            counts[k] = counts[k].saturating_add(1);
        }
    }
    if out.is_empty() {
        return out;
    }
    counts[15] = 3;
    let mut info = [0u8; 36];
    info[1] = 1;
    info[2] = disc.tracks.len() as u8;
    info[4..20].copy_from_slice(&counts);
    // The last sequence number of block 0, size packs included.
    info[20] = (out.len() + 2) as u8;
    // English.
    info[28] = 0x09;
    for (i, payload) in info.chunks(12).enumerate() {
        let mut p: Pack = [0; 18];
        p[0] = SIZE_INFO;
        p[1] = i as u8;
        p[2] = out.len() as u8;
        p[4..16].copy_from_slice(payload);
        seal(&mut p);
        out.push(p);
    }
    out
}

/// The text of the disc and of `tracks` tracks from packs (pack CRCs are
/// checked; `Err` names the first bad pack).
pub fn decode(data: &[u8], tracks: usize) -> Result<(CdText, Vec<CdText>), usize> {
    let mut disc = CdText::default();
    let mut per_track = vec![CdText::default(); tracks];
    let mut streams: [Vec<u8>; 6] = Default::default();
    for (i, p) in data.as_chunks::<18>().0.iter().enumerate() {
        let crc = u16::from_be_bytes([p[16], p[17]]);
        if crc != !crc16(&p[..16]) {
            return Err(i);
        }
        if TEXT_TYPES.contains(&p[0]) {
            streams[usize::from(p[0] - 0x80)].extend_from_slice(&p[4..16]);
        }
    }
    for (k, stream) in streams.iter().enumerate() {
        if stream.is_empty() {
            continue;
        }
        let mut previous = String::new();
        for (owner, raw) in stream.split(|b| *b == 0).take(tracks + 1).enumerate() {
            // A single TAB repeats the previous track's string.
            let text: String = if raw == b"\t" {
                previous.clone()
            } else {
                raw.iter().map(|&b| char::from(b)).collect()
            };
            previous.clone_from(&text);
            let target = if owner == 0 {
                &mut disc
            } else {
                &mut per_track[owner - 1]
            };
            let field = match k {
                0 => &mut target.title,
                1 => &mut target.performer,
                2 => &mut target.songwriter,
                3 => &mut target.composer,
                4 => &mut target.arranger,
                _ => &mut target.message,
            };
            *field = text;
        }
    }
    Ok((disc, per_track))
}

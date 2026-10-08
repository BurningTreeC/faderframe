//! The start timecode of a QuickTime or MPEG-4 file's timecode track
//! (`tmcd`), read from its atoms: GStreamer's demuxer parses the track but
//! does not hand the timecode on. The track's sample entry gives the rate
//! and whether it counts drop-frame; its first sample, the frame the file
//! starts at.

use faderframe_core::timecode::{FrameRate, Timecode};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// An atom: its type, where its contents start and how long they are.
struct Atom {
    kind: [u8; 4],
    start: u64,
    len: u64,
}

/// The atoms in `[start, end)` of `f`.
fn atoms(f: &mut File, start: u64, end: u64) -> Vec<Atom> {
    let mut out = Vec::new();
    let mut at = start;
    while at + 8 <= end {
        let mut head = [0u8; 8];
        if f.seek(SeekFrom::Start(at)).is_err() || f.read_exact(&mut head).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let kind = [head[4], head[5], head[6], head[7]];
        let mut header = 8;
        if size == 1 {
            let mut big = [0u8; 8];
            if f.read_exact(&mut big).is_err() {
                break;
            }
            size = u64::from_be_bytes(big);
            header = 16;
        } else if size == 0 {
            size = end - at;
        }
        if size < header || at + size > end {
            break;
        }
        out.push(Atom {
            kind,
            start: at + header,
            len: size - header,
        });
        at += size;
    }
    out
}

fn child(f: &mut File, parent: &Atom, kind: &[u8; 4]) -> Option<Atom> {
    atoms(f, parent.start, parent.start + parent.len)
        .into_iter()
        .find(|a| &a.kind == kind)
}

fn read_at(f: &mut File, at: u64, buf: &mut [u8]) -> Option<()> {
    f.seek(SeekFrom::Start(at)).ok()?;
    f.read_exact(buf).ok()
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// The start timecode of `path`'s timecode track, with its rate.
pub fn start_timecode(path: &Path) -> Option<(Timecode, FrameRate)> {
    let mut f = File::open(path).ok()?;
    let end = f.metadata().ok()?.len();
    let moov = atoms(&mut f, 0, end)
        .into_iter()
        .find(|a| &a.kind == b"moov")?;
    for trak in atoms(&mut f, moov.start, moov.start + moov.len)
        .into_iter()
        .filter(|a| &a.kind == b"trak")
    {
        let Some(mdia) = child(&mut f, &trak, b"mdia") else {
            continue;
        };
        // A timecode track's handler is 'tmcd'.
        let Some(hdlr) = child(&mut f, &mdia, b"hdlr") else {
            continue;
        };
        let mut h = [0u8; 12];
        if read_at(&mut f, hdlr.start, &mut h).is_none() || &h[8..12] != b"tmcd" {
            continue;
        }
        let stbl = child(&mut f, &mdia, b"minf").and_then(|m| child(&mut f, &m, b"stbl"))?;
        // stsd: version/flags, count, then the 'tmcd' sample entry.
        let stsd = child(&mut f, &stbl, b"stsd")?;
        let mut e = [0u8; 8 + 8 + 26];
        read_at(&mut f, stsd.start, &mut e)?;
        if &e[12..16] != b"tmcd" {
            continue;
        }
        // Entry: size, 'tmcd', 6 reserved, data ref (2), 4 reserved,
        // flags (4), timescale (4), frame duration (4), frames a second.
        let body = &e[16..];
        let flags = be32(&body[12..16]);
        let timescale = be32(&body[16..20]);
        let frame_duration = be32(&body[20..24]);
        if timescale == 0 || frame_duration == 0 {
            continue;
        }
        let rate = FrameRate::from_fraction(timescale, frame_duration)?.with_drop(flags & 1 != 0);
        // The first sample: the frame number at the start.
        let offset = if let Some(stco) = child(&mut f, &stbl, b"stco") {
            let mut b = [0u8; 12];
            read_at(&mut f, stco.start, &mut b)?;
            be32(&b[8..12]) as u64
        } else {
            let co64 = child(&mut f, &stbl, b"co64")?;
            let mut b = [0u8; 16];
            read_at(&mut f, co64.start, &mut b)?;
            u64::from_be_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]])
        };
        let mut sample = [0u8; 4];
        read_at(&mut f, offset, &mut sample)?;
        let frames = be32(&sample) as i64;
        return Some((Timecode::from_frames(frames, rate), rate));
    }
    None
}

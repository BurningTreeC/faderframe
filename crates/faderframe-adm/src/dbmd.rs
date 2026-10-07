//! Dolby's audio metadata chunk (`dbmd`, EBU Tech 3285 Supplement 6) as
//! Dolby Atmos masters carry it: the container is EBU's, the Atmos
//! segments (9 and 10) are laid out as Dolby's own parser (`dbmd_atmos_parse`,
//! BSD) and MediaArea's (BSD) read them and as files written by Dolby's
//! tools have them.
//!
//! * version 1.0.0.6, then segments: id, size (u16, payload bytes), the
//!   payload, a checksum (the two's complement of the low byte of the size
//!   plus the payload bytes); id 0 ends it; a pad byte makes the chunk even
//!   (counted in the chunk's size, as Dolby's tools do).
//! * Segment 7 (Dolby Digital Plus): the values Dolby's tools write (3/2
//!   with LFE, −3 dB centre and surround mix levels, Film Light).
//! * Segment 9 (Dolby Atmos, 248 bytes): the creating tool and its version
//!   (FaderFrame's, not Dolby's text), warp mode "not indicated", bed
//!   distribution and the constants Dolby's files share.
//! * Segment 10 (supplemental): automatic trims for all nine speaker
//!   configurations, no object trims bypassed, and each track's binaural
//!   render mode (the LFE off, others as set or "not indicated").

use crate::{BedChannel, Master};

/// How Dolby's binaural render places an element on headphones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinauralMode {
    /// No binaural virtualisation (plain stereo).
    Off,
    Near,
    Mid,
    Far,
}

impl BinauralMode {
    pub const ALL: [BinauralMode; 4] = [
        BinauralMode::Off,
        BinauralMode::Near,
        BinauralMode::Mid,
        BinauralMode::Far,
    ];

    /// Its code in the chunk (Mid and Far are not in the order of the
    /// menu).
    fn code(self) -> u8 {
        match self {
            BinauralMode::Off => 0,
            BinauralMode::Near => 1,
            BinauralMode::Far => 2,
            BinauralMode::Mid => 3,
        }
    }

    fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => BinauralMode::Off,
            1 => BinauralMode::Near,
            2 => BinauralMode::Far,
            3 => BinauralMode::Mid,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            BinauralMode::Off => "Off",
            BinauralMode::Near => "Near",
            BinauralMode::Mid => "Mid",
            BinauralMode::Far => "Far",
        }
    }
}

const VERSION: [u8; 4] = [0x06, 0x00, 0x00, 0x01];
const ATMOS: u8 = 9;
const SUPPLEMENTAL: u8 = 10;
const DDPLUS: u8 = 7;
const SYNC: [u8; 4] = [0xBD, 0x6F, 0x72, 0xF8];
/// head_track_mode "not indicated" with render mode "not indicated".
const HEADPHONE_DEFAULT: u8 = 0x84;

fn segment(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    let size = payload.len() as u16;
    out.push(id);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(payload);
    let mut sum = (size & 0xFF) as u8;
    for &b in payload {
        sum = sum.wrapping_add(b);
    }
    out.push((!sum).wrapping_add(1));
}

fn text(out: &mut [u8], s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(out.len() - 1);
    out[..n].copy_from_slice(&b[..n]);
}

/// The creating tool's version as three bytes (`major.minor.micro`).
fn tool_version(version: &str) -> [u8; 3] {
    let mut v = [0u8; 3];
    for (o, p) in v.iter_mut().zip(version.split('.')) {
        *o = p.parse::<u8>().unwrap_or(0);
    }
    v
}

/// The `dbmd` chunk's payload for `m` (`version`: FaderFrame's, e.g.
/// "0.11.0"); `bed`: the bed channels' mode (the LFE is always Off);
/// `binaural`: each object's mode (missing: not indicated).
pub fn dbmd(
    m: &Master,
    version: &str,
    bed: Option<BinauralMode>,
    binaural: &[Option<BinauralMode>],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(512 + 2 * m.channels());
    out.extend_from_slice(&VERSION);
    // Dolby Digital Plus, as Dolby's tools write it.
    let mut ddp = [0u8; 96];
    ddp[1] = 0x47;
    ddp[5] = 0x60;
    ddp[8] = 0x24;
    ddp[9] = 0x24;
    ddp[14] = 0x02;
    ddp[15] = 0x02;
    segment(&mut out, DDPLUS, &ddp);
    // Dolby Atmos.
    let mut a = [0u8; 248];
    text(&mut a[0x00..0x20], "Created using FaderFrame");
    text(&mut a[0x20..0x60], "FaderFrame");
    a[0x60..0x63].copy_from_slice(&tool_version(version));
    a[0x67] = 0x03;
    a[0x69] = 0x05;
    a[0x6A] = 0x01;
    // Frame rate not indicated; no first frame of action.
    a[0x6F] = 0x20;
    a[0x70] = 0xFF;
    a[0x76] = 0x03;
    a[0x86] = 0xF0;
    a[0x87] = 0x08;
    // Downmix and its phase shift not indicated.
    a[0x8B] = 0x03;
    // Bed distribution 2, warp mode not indicated.
    a[0x98] = 0x84;
    for at in [0xA8, 0xB8, 0xC8] {
        a[at] = 0xF0;
        a[at + 2] = 0x08;
    }
    segment(&mut out, ATMOS, &a);
    // Supplemental: trims and binaural render modes per track.
    let n = m.channels();
    let mut s = Vec::with_capacity(142 + 2 * n);
    s.extend_from_slice(&SYNC);
    s.extend_from_slice(&(n as u16).to_le_bytes());
    s.push(0);
    for _ in 0..9 {
        // Automatic trims.
        s.push(0x01);
        s.extend_from_slice(&[0u8; 14]);
    }
    s.extend(std::iter::repeat_n(0u8, n));
    let byte = |mode: Option<BinauralMode>| mode.map_or(HEADPHONE_DEFAULT, |m| 0x80 | m.code());
    for c in &m.bed {
        s.push(if *c == BedChannel::Lfe {
            byte(Some(BinauralMode::Off))
        } else {
            byte(bed)
        });
    }
    for k in 0..m.objects.len() {
        s.push(byte(binaural.get(k).copied().flatten()));
    }
    segment(&mut out, SUPPLEMENTAL, &s);
    out.push(0);
    if out.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// Each track's binaural render mode from a `dbmd` payload (by file
/// channel; `None` where not indicated), when it has a valid segment 10.
pub fn binaural_modes(payload: &[u8]) -> Option<Vec<Option<BinauralMode>>> {
    let mut at = 4;
    while at + 3 <= payload.len() {
        let id = payload[at];
        if id == 0 {
            break;
        }
        let size = u16::from_le_bytes([payload[at + 1], payload[at + 2]]) as usize;
        let body = payload.get(at + 3..at + 3 + size)?;
        let check = *payload.get(at + 3 + size)?;
        let sum = body
            .iter()
            .fold((size & 0xFF) as u8, |s, &b| s.wrapping_add(b));
        if sum.wrapping_add(check) != 0 {
            return None;
        }
        if id == SUPPLEMENTAL && body.len() >= 142 && body[..4] == SYNC {
            let n = u16::from_le_bytes([body[4], body[5]]) as usize;
            let modes = body.get(142 + n..142 + 2 * n)?;
            return Some(
                modes
                    .iter()
                    .map(|&b| BinauralMode::from_code(b & 0x07))
                    .collect(),
            );
        }
        at += 3 + size + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BEDS, Block, Object, Profile};

    fn master() -> Master {
        Master {
            name: "Song".into(),
            profile: Profile::DolbyAtmos,
            sample_rate: 48_000,
            frames: 48_000,
            bed: BEDS[7].to_vec(),
            objects: (0..3)
                .map(|i| Object {
                    name: format!("O{i}"),
                    blocks: vec![Block {
                        start: 0,
                        length: 48_000,
                        position: [0.0, 1.0, 0.0],
                        size: 0.0,
                        gain: 1.0,
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn the_chunk_is_laid_out_as_dolby_files_are() {
        let m = master();
        let c = dbmd(
            &m,
            "0.11.0",
            None,
            &[Some(BinauralMode::Near), None, Some(BinauralMode::Mid)],
        );
        assert_eq!(&c[..4], &VERSION);
        assert_eq!(c.len() % 2, 0, "even, the pad counted");
        // 4 + (3+96+1) + (3+248+1) + (3+142+2·13+1) + terminator = 529 → pad.
        assert_eq!(c.len(), 530);
        assert_eq!(c[4], DDPLUS);
        assert_eq!(c[4 + 100], ATMOS);
        let atmos = &c[4 + 100 + 3..4 + 100 + 3 + 248];
        assert_eq!(&atmos[0x20..0x2A], b"FaderFrame");
        assert_eq!(&atmos[0x60..0x63], &[0, 11, 0]);
        let modes = binaural_modes(&c).unwrap();
        assert_eq!(modes.len(), 13);
        assert_eq!(modes[3], Some(BinauralMode::Off), "the LFE");
        assert_eq!(modes[0], None);
        let far = binaural_modes(&dbmd(&m, "0.11.0", Some(BinauralMode::Far), &[])).unwrap();
        assert_eq!(far[0], Some(BinauralMode::Far));
        assert_eq!(far[3], Some(BinauralMode::Off), "the LFE");
        assert_eq!(modes[10], Some(BinauralMode::Near));
        assert_eq!(modes[12], Some(BinauralMode::Mid));
        // A broken checksum is refused.
        let mut bad = c.clone();
        bad[4 + 100 + 3 + 10] ^= 1;
        assert_eq!(binaural_modes(&bad), None);
    }
}

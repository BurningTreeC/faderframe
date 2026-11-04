//! WAVE and AIFF headers: what a WAVEDescriptor's or AIFCDescriptor's
//! Summary says about the sound, the Summary of a linked WAVE file, and
//! headers that turn raw embedded PCM into a file a decoder opens.

use crate::timeline::EssenceFormat;
use std::io::{self, Read};
use std::path::Path;

/// Plain PCM: what its samples are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmInfo {
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    pub float: bool,
}

impl PcmInfo {
    pub fn pcm(self, big_endian: bool) -> EssenceFormat {
        EssenceFormat::Pcm {
            bits: self.bits,
            channels: self.channels,
            rate: self.rate,
            big_endian,
            float: self.float,
        }
    }
}

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u16be(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The format a WAVE header (RIFF … fmt) gives.
pub fn wave_format(b: &[u8]) -> Option<PcmInfo> {
    if b.get(..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= b.len() {
        let size = u32le(b, at + 4)? as usize;
        if b.get(at..at + 4)? == b"fmt " {
            let f = at + 8;
            let tag = u16le(b, f)?;
            let bits = u16le(b, f + 14)?;
            let sub = if tag == 0xfffe {
                u16le(b, f + 24)
            } else {
                None
            };
            return Some(PcmInfo {
                channels: u16le(b, f + 2)?,
                rate: u32le(b, f + 4)?,
                bits,
                float: tag == 3 || sub == Some(3),
            });
        }
        at += 8 + size + (size & 1);
    }
    None
}

/// An IEEE 754 80-bit extended number (AIFF sample rates).
fn extended(b: &[u8]) -> Option<f64> {
    let exp = i32::from(u16::from_be_bytes(b.get(..2)?.try_into().ok()?) & 0x7fff);
    let mant = u64::from_be_bytes(b.get(2..10)?.try_into().ok()?);
    if exp == 0 && mant == 0 {
        return Some(0.0);
    }
    Some(mant as f64 * 2f64.powi(exp - 16383 - 63))
}

fn to_extended(v: f64) -> [u8; 10] {
    let mut out = [0u8; 10];
    if v <= 0.0 {
        return out;
    }
    let exp = v.log2().floor() as i32;
    let mant = (v / 2f64.powi(exp - 63)) as u64;
    out[..2].copy_from_slice(&((exp + 16383) as u16).to_be_bytes());
    out[2..].copy_from_slice(&mant.to_be_bytes());
    out
}

/// The format an AIFF/AIFC header (FORM … COMM) gives.
pub fn aiff_format(b: &[u8]) -> Option<PcmInfo> {
    if b.get(..4)? != b"FORM" || !matches!(b.get(8..12)?, b"AIFF" | b"AIFC") {
        return None;
    }
    let mut at = 12;
    while at + 8 <= b.len() {
        let size = u32be(b, at + 4)? as usize;
        if b.get(at..at + 4)? == b"COMM" {
            let c = at + 8;
            let compression = b.get(c + 18..c + 22);
            return Some(PcmInfo {
                channels: u16be(b, c)?,
                rate: extended(b.get(c + 8..c + 18)?)?.round() as u32,
                bits: u16be(b, c + 6)?,
                float: matches!(compression, Some(b"fl32") | Some(b"FL32")),
            });
        }
        at += 8 + size + (size & 1);
    }
    None
}

/// A WAVE file's Summary as AAF wants it: the RIFF header and the fmt
/// chunk.
pub fn wave_summary(path: &Path) -> io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head)?;
    if &head[..4] != b"RIFF" || &head[8..] != b"WAVE" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a WAVE file",
        ));
    }
    loop {
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch)?;
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as usize;
        if &ch[..4] == b"fmt " {
            let mut body = vec![0u8; size];
            f.read_exact(&mut body)?;
            let mut out = head.to_vec();
            out.extend_from_slice(&ch);
            out.extend_from_slice(&body);
            return Ok(out);
        }
        io::copy(
            &mut (&mut f).take((size + (size & 1)) as u64),
            &mut io::sink(),
        )?;
    }
}

/// A header that makes `data_len` bytes of raw PCM a file: WAVE for
/// little-endian samples, AIFF for big-endian ones; the header and the
/// file's extension.
pub fn header_for_pcm(info: PcmInfo, big_endian: bool, data_len: u64) -> (Vec<u8>, &'static str) {
    let block = u32::from(info.channels) * u32::from(info.bits.div_ceil(8));
    if big_endian {
        let frames = data_len / u64::from(block.max(1));
        let mut h = Vec::with_capacity(54);
        h.extend_from_slice(b"FORM");
        let total = 4 + 8 + 18 + 8 + 8 + data_len;
        h.extend_from_slice(&(total.min(u64::from(u32::MAX)) as u32).to_be_bytes());
        h.extend_from_slice(b"AIFF");
        h.extend_from_slice(b"COMM");
        h.extend_from_slice(&18u32.to_be_bytes());
        h.extend_from_slice(&info.channels.to_be_bytes());
        h.extend_from_slice(&(frames.min(u64::from(u32::MAX)) as u32).to_be_bytes());
        h.extend_from_slice(&info.bits.to_be_bytes());
        h.extend_from_slice(&to_extended(f64::from(info.rate)));
        h.extend_from_slice(b"SSND");
        h.extend_from_slice(&((data_len + 8).min(u64::from(u32::MAX)) as u32).to_be_bytes());
        h.extend_from_slice(&[0; 8]);
        (h, "aif")
    } else {
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&((36 + data_len).min(u64::from(u32::MAX)) as u32).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&(if info.float { 3u16 } else { 1u16 }).to_le_bytes());
        h.extend_from_slice(&info.channels.to_le_bytes());
        h.extend_from_slice(&info.rate.to_le_bytes());
        h.extend_from_slice(&(info.rate * block).to_le_bytes());
        h.extend_from_slice(&(block as u16).to_le_bytes());
        h.extend_from_slice(&info.bits.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&(data_len.min(u64::from(u32::MAX)) as u32).to_le_bytes());
        (h, "wav")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_say_what_they_wrap() {
        let info = PcmInfo {
            channels: 2,
            rate: 48_000,
            bits: 24,
            float: false,
        };
        let (wav, ext) = header_for_pcm(info, false, 600);
        assert_eq!(ext, "wav");
        assert_eq!(wave_format(&wav), Some(info));
        let (aif, ext) = header_for_pcm(info, true, 600);
        assert_eq!(ext, "aif");
        assert_eq!(aiff_format(&aif), Some(info));
        let odd = PcmInfo {
            rate: 44_100,
            ..info
        };
        assert_eq!(aiff_format(&header_for_pcm(odd, true, 0).0), Some(odd));
    }
}

//! Minimal RIFF/WAVE writer and reader (PCM 16/24-bit, IEEE float 32-bit).
//!
//! Used by the render/export subsystem and by tests. Decoding of other
//! formats (FLAC, AIFF, compressed) is planned via Symphonia behind the same
//! source abstraction.

use crate::dither::{Dither, Quantizer};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WavFormat {
    Pcm16,
    Pcm24,
    Float32,
}

impl WavFormat {
    pub fn bits(self) -> u16 {
        match self {
            WavFormat::Pcm16 => 16,
            WavFormat::Pcm24 => 24,
            WavFormat::Float32 => 32,
        }
    }

    pub fn is_integer(self) -> bool {
        !matches!(self, WavFormat::Float32)
    }

    pub fn label(self) -> &'static str {
        match self {
            WavFormat::Pcm16 => "WAV 16-bit PCM",
            WavFormat::Pcm24 => "WAV 24-bit PCM",
            WavFormat::Float32 => "WAV 32-bit float",
        }
    }
}

/// Write non-interleaved `channels` (equal lengths) to `path`, with TPDF
/// dither for integer formats when `dither` is set.
pub fn write_wav(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    format: WavFormat,
    dither: bool,
) -> io::Result<()> {
    let dither = if dither { Dither::Tpdf } else { Dither::Off };
    write_wav_with(path, channels, sample_rate, format, dither)
}

/// The `fmt ` chunk (header included) of a file of `channels` channels:
/// `WAVE_FORMAT_EXTENSIBLE` with the speakers of `mask` when there is one
/// (a surround bed), else the plain PCM or float format.
pub(crate) fn fmt_chunk(channels: u16, rate: u32, format: WavFormat, mask: Option<u32>) -> Vec<u8> {
    let bps = u32::from(format.bits() / 8);
    let tag: u16 = if format.is_integer() { 1 } else { 3 };
    let mut c = Vec::with_capacity(48);
    c.extend_from_slice(b"fmt ");
    c.extend_from_slice(&(if mask.is_some() { 40u32 } else { 16 }).to_le_bytes());
    c.extend_from_slice(&(if mask.is_some() { 0xFFFEu16 } else { tag }).to_le_bytes());
    c.extend_from_slice(&channels.to_le_bytes());
    c.extend_from_slice(&rate.to_le_bytes());
    c.extend_from_slice(&(rate * u32::from(channels) * bps).to_le_bytes());
    c.extend_from_slice(&(channels * bps as u16).to_le_bytes());
    c.extend_from_slice(&format.bits().to_le_bytes());
    if let Some(mask) = mask {
        c.extend_from_slice(&22u16.to_le_bytes());
        c.extend_from_slice(&format.bits().to_le_bytes());
        c.extend_from_slice(&mask.to_le_bytes());
        // KSDATAFORMAT_SUBTYPE_PCM / _IEEE_FLOAT.
        c.extend_from_slice(&u32::from(tag).to_le_bytes());
        c.extend_from_slice(&[
            0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    c
}

/// [`write_wav`] with a choice of [`Dither`] (integer formats only).
pub fn write_wav_with(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    format: WavFormat,
    dither: Dither,
) -> io::Result<()> {
    write_wav_mask(path, channels, sample_rate, format, dither, None)
}

/// [`write_wav_with`] carrying a channel mask (`WAVE_FORMAT_EXTENSIBLE`:
/// which speaker each channel is, e.g. a 5.1 bed's 0x60F).
pub fn write_wav_mask(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    format: WavFormat,
    dither: Dither,
    mask: Option<u32>,
) -> io::Result<()> {
    let n_ch = channels.len().max(1) as u16;
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let bytes_per_sample = (format.bits() / 8) as u32;
    let data_len = frames as u64 * n_ch as u64 * bytes_per_sample as u64;
    if data_len > u32::MAX as u64 - 64 {
        return Err(io::Error::other("render exceeds the 4 GiB WAV size limit"));
    }
    let mut w = BufWriter::new(File::create(path)?);
    let fmt = fmt_chunk(n_ch, sample_rate, format, mask);
    // An odd-sized chunk is followed by a pad byte (RIFF).
    let pad = (data_len & 1) as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(4 + fmt.len() as u32 + 8 + data_len as u32 + pad).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(&fmt)?;
    w.write_all(b"data")?;
    w.write_all(&(data_len as u32).to_le_bytes())?;
    let mut quantizer = Quantizer::new(format.bits(), channels.len(), sample_rate, dither);
    for i in 0..frames {
        for (c, ch) in channels.iter().enumerate() {
            let s = ch[i];
            match format {
                WavFormat::Float32 => w.write_all(&s.to_le_bytes())?,
                WavFormat::Pcm16 => {
                    w.write_all(&(quantizer.quantize(c, s) as i16).to_le_bytes())?;
                }
                WavFormat::Pcm24 => {
                    w.write_all(&quantizer.quantize(c, s).to_le_bytes()[..3])?;
                }
            }
        }
    }
    if pad == 1 {
        w.write_all(&[0])?;
    }
    w.flush()
}

/// Decoded WAV contents.
#[derive(Clone, Debug, PartialEq)]
pub struct WavData {
    pub sample_rate: u32,
    pub format: WavFormat,
    pub channels: Vec<Vec<f32>>,
    /// The speakers, from a `WAVE_FORMAT_EXTENSIBLE` header.
    pub channel_mask: Option<u32>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Read a PCM16/PCM24/float32 WAV file.
pub fn read_wav(path: &Path) -> io::Result<WavData> {
    let mut r = BufReader::new(File::open(path)?);
    let mut header = [0u8; 12];
    r.read_exact(&mut header)?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(bad("not a RIFF/WAVE file"));
    }
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut channel_mask = None;
    loop {
        let mut chunk = [0u8; 8];
        r.read_exact(&mut chunk)?;
        let len = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as usize;
        let mut body = vec![0u8; len + (len & 1)];
        r.read_exact(&mut body)?;
        match &chunk[0..4] {
            b"fmt " if len >= 16 => {
                let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
                let sr = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                let mut tag = u16_at(0);
                if tag == 0xFFFE && len >= 40 {
                    // Extensible: the sub-format's first two bytes are the
                    // format, the mask names the speakers.
                    channel_mask =
                        Some(u32::from_le_bytes([body[20], body[21], body[22], body[23]]));
                    tag = u16_at(24);
                }
                fmt = Some((tag, u16_at(2), sr, u16_at(14)));
            }
            b"data" => {
                let (tag, n_ch, sr, bits) = fmt.ok_or_else(|| bad("data before fmt chunk"))?;
                let format = match (tag, bits) {
                    (1, 16) => WavFormat::Pcm16,
                    (1, 24) => WavFormat::Pcm24,
                    (3, 32) => WavFormat::Float32,
                    _ => return Err(bad("unsupported WAV sample format")),
                };
                let bps = (bits / 8) as usize;
                let n = n_ch.max(1) as usize;
                let frames = len / (bps * n);
                let mut channels = vec![Vec::with_capacity(frames); n];
                for f in 0..frames {
                    for (c, ch) in channels.iter_mut().enumerate() {
                        let i = (f * n + c) * bps;
                        let s = match format {
                            WavFormat::Pcm16 => {
                                i16::from_le_bytes([body[i], body[i + 1]]) as f32 / 32768.0
                            }
                            WavFormat::Pcm24 => {
                                let v =
                                    i32::from_le_bytes([0, body[i], body[i + 1], body[i + 2]]) >> 8;
                                v as f32 / 8_388_608.0
                            }
                            WavFormat::Float32 => {
                                f32::from_le_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]])
                            }
                        };
                        ch.push(s);
                    }
                }
                return Ok(WavData {
                    sample_rate: sr,
                    format,
                    channels,
                    channel_mask,
                });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ff-wav-{}-{name}", std::process::id()))
    }

    #[test]
    fn round_trip_all_formats() {
        let left: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.01).sin() * 0.8).collect();
        let right: Vec<f32> = left.iter().map(|v| -v * 0.5).collect();
        for (format, tol) in [
            (WavFormat::Float32, 0.0),
            (WavFormat::Pcm24, 2.0 / 8_388_608.0),
            (WavFormat::Pcm16, 2.0 / 32768.0),
        ] {
            let path = tmp(&format!("{format:?}.wav"));
            write_wav(&path, &[left.clone(), right.clone()], 96_000, format, false).unwrap();
            let d = read_wav(&path).unwrap();
            assert_eq!(d.sample_rate, 96_000);
            assert_eq!(d.format, format);
            assert_eq!(d.channels.len(), 2);
            assert_eq!(d.channels[0].len(), 1000);
            for (a, b) in d.channels[1].iter().zip(&right) {
                assert!((a - b).abs() <= tol + 1e-7, "{format:?}: {a} vs {b}");
            }
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn dither_stays_within_one_lsb_and_clips_safely() {
        let path = tmp("dither.wav");
        write_wav(
            &path,
            &[vec![0.25; 4000], vec![2.0; 4000]],
            44_100,
            WavFormat::Pcm16,
            true,
        )
        .unwrap();
        let d = read_wav(&path).unwrap();
        let lsb = 1.0 / 32768.0;
        assert!(d.channels[0].iter().all(|v| (v - 0.25).abs() <= 2.0 * lsb));
        assert!(
            d.channels[0].iter().any(|v| (v - 0.25).abs() > 1e-9),
            "dither present"
        );
        assert!(d.channels[1].iter().all(|v| *v <= 1.0));
        std::fs::remove_file(&path).unwrap();
    }
}

#[cfg(test)]
mod mask_tests {
    use super::*;

    /// A 5.1 bed's file carries its speakers and reads back with them, in
    /// every format; plain files have none.
    #[test]
    fn a_channel_mask_round_trips() {
        let dir = std::env::temp_dir().join(format!("ff-wavmask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let channels: Vec<Vec<f32>> = (0..6).map(|c| vec![c as f32 * 0.1; 64]).collect();
        for format in [WavFormat::Pcm16, WavFormat::Pcm24, WavFormat::Float32] {
            let p = dir.join("bed.wav");
            write_wav_mask(&p, &channels, 48_000, format, Dither::Off, Some(0x60F)).unwrap();
            let back = read_wav(&p).unwrap();
            assert_eq!(back.channel_mask, Some(0x60F));
            assert_eq!(back.format, format);
            assert_eq!(back.channels.len(), 6);
            assert!((back.channels[5][10] - 0.5).abs() < 1e-3);
            // The stream writer agrees.
            let mut w = crate::wavstream::WavWriter::create_with_mask(
                &p,
                6,
                48_000,
                format,
                Dither::Off,
                Some(0x60F),
            )
            .unwrap();
            let refs: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
            w.write_planar(&refs, 64).unwrap();
            w.finish().unwrap();
            let back = read_wav(&p).unwrap();
            assert_eq!(back.channel_mask, Some(0x60F));
            assert_eq!(back.channels[3].len(), 64);
        }
        let p = dir.join("plain.wav");
        write_wav(&p, &channels[..2], 48_000, WavFormat::Pcm16, false).unwrap();
        assert_eq!(read_wav(&p).unwrap().channel_mask, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

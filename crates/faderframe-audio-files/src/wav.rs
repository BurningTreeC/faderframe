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

/// [`write_wav`] with a choice of [`Dither`] (integer formats only).
pub fn write_wav_with(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    format: WavFormat,
    dither: Dither,
) -> io::Result<()> {
    let n_ch = channels.len().max(1) as u16;
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let bytes_per_sample = (format.bits() / 8) as u32;
    let data_len = frames as u64 * n_ch as u64 * bytes_per_sample as u64;
    if data_len > u32::MAX as u64 - 64 {
        return Err(io::Error::other("render exceeds the 4 GiB WAV size limit"));
    }
    let mut w = BufWriter::new(File::create(path)?);
    let tag: u16 = if format.is_integer() { 1 } else { 3 };
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len as u32).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&tag.to_le_bytes())?;
    w.write_all(&n_ch.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&(sample_rate * n_ch as u32 * bytes_per_sample).to_le_bytes())?;
    w.write_all(&(n_ch * bytes_per_sample as u16).to_le_bytes())?;
    w.write_all(&format.bits().to_le_bytes())?;
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
    w.flush()
}

/// Decoded WAV contents.
#[derive(Clone, Debug, PartialEq)]
pub struct WavData {
    pub sample_rate: u32,
    pub format: WavFormat,
    pub channels: Vec<Vec<f32>>,
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
                fmt = Some((u16_at(0), u16_at(2), sr, u16_at(14)));
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

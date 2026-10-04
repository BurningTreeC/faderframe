//! Streaming WAV writer and random-access WAV reader.
//!
//! Imported and recorded audio is stored as plain RIFF/WAVE so the disk
//! streamer can read any frame range with a single positional read
//! (no decoder state, no seeking tables).

use crate::dither::{Dither, Quantizer};
use crate::wav::WavFormat;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Incrementally written WAV file; sizes are patched in [`WavWriter::finish`].
pub struct WavWriter {
    out: BufWriter<File>,
    path: PathBuf,
    channels: u16,
    sample_rate: u32,
    format: WavFormat,
    quantizer: Quantizer,
    frames: u64,
    scratch: Vec<u8>,
}

impl WavWriter {
    pub fn create(
        path: &Path,
        channels: u16,
        sample_rate: u32,
        format: WavFormat,
        dither: bool,
    ) -> io::Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        Self::write_header(&mut out, channels.max(1), sample_rate, format, 0)?;
        Ok(Self {
            out,
            path: path.to_path_buf(),
            channels: channels.max(1),
            sample_rate,
            format,
            quantizer: Quantizer::new(
                format.bits(),
                channels.max(1) as usize,
                sample_rate,
                if dither { Dither::Tpdf } else { Dither::Off },
            ),
            frames: 0,
            scratch: Vec::new(),
        })
    }

    fn write_header(
        out: &mut impl Write,
        channels: u16,
        rate: u32,
        format: WavFormat,
        frames: u64,
    ) -> io::Result<()> {
        let bps = (format.bits() / 8) as u32;
        let data_len = frames * channels as u64 * bps as u64;
        let data_len = u32::try_from(data_len).unwrap_or(u32::MAX - 64);
        let tag: u16 = if format.is_integer() { 1 } else { 3 };
        out.write_all(b"RIFF")?;
        out.write_all(&(36u32.saturating_add(data_len)).to_le_bytes())?;
        out.write_all(b"WAVEfmt ")?;
        out.write_all(&16u32.to_le_bytes())?;
        out.write_all(&tag.to_le_bytes())?;
        out.write_all(&channels.to_le_bytes())?;
        out.write_all(&rate.to_le_bytes())?;
        out.write_all(&(rate * channels as u32 * bps).to_le_bytes())?;
        out.write_all(&(channels * bps as u16).to_le_bytes())?;
        out.write_all(&format.bits().to_le_bytes())?;
        out.write_all(b"data")?;
        out.write_all(&data_len.to_le_bytes())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Append `frames` frames from non-interleaved channel slices. Missing
    /// channels are written as silence.
    pub fn write_planar(&mut self, channels: &[&[f32]], frames: usize) -> io::Result<()> {
        let bps = (self.format.bits() / 8) as usize;
        let limit = (u32::MAX as u64 - 64) / (self.channels as u64 * bps as u64);
        if self.frames + frames as u64 > limit {
            return Err(io::Error::other("WAV size limit (4 GiB) reached"));
        }
        self.scratch.clear();
        self.scratch.reserve(frames * self.channels as usize * bps);
        for i in 0..frames {
            for c in 0..self.channels as usize {
                let s = channels
                    .get(c)
                    .and_then(|ch| ch.get(i))
                    .copied()
                    .unwrap_or(0.0);
                match self.format {
                    WavFormat::Float32 => self.scratch.extend_from_slice(&s.to_le_bytes()),
                    WavFormat::Pcm16 => {
                        let v = self.quantizer.quantize(c, s) as i16;
                        self.scratch.extend_from_slice(&v.to_le_bytes());
                    }
                    WavFormat::Pcm24 => {
                        let v = self.quantizer.quantize(c, s);
                        self.scratch.extend_from_slice(&v.to_le_bytes()[..3]);
                    }
                }
            }
        }
        self.out.write_all(&self.scratch)?;
        self.frames += frames as u64;
        Ok(())
    }

    /// Flush, patch the header sizes and close.
    pub fn finish(mut self) -> io::Result<PathBuf> {
        self.out.flush()?;
        let mut file = self.out.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        Self::write_header(
            &mut file,
            self.channels,
            self.sample_rate,
            self.format,
            self.frames,
        )?;
        file.sync_data()?;
        Ok(self.path)
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

/// Random-access reader for PCM16/24/32 and float32 WAV files.
///
/// Thread-safe (positional reads), so the disk streamer may read pages
/// while other threads hold the same reader.
#[derive(Debug)]
pub struct WavFile {
    file: File,
    path: PathBuf,
    channels: u16,
    sample_rate: u32,
    format: WavFormat,
    data_offset: u64,
    frames: u64,
}

impl WavFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut header = [0u8; 12];
        read_at(&file, &mut header, 0)?;
        if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
            return Err(bad("not a RIFF/WAVE file"));
        }
        let mut pos = 12u64;
        let mut fmt: Option<(u16, u16, u32, u16)> = None;
        while pos + 8 <= len {
            let mut chunk = [0u8; 8];
            read_at(&file, &mut chunk, pos)?;
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let body = pos + 8;
            match &chunk[0..4] {
                b"fmt " => {
                    let mut f = [0u8; 16];
                    read_at(&file, &mut f, body)?;
                    let u16_at = |i: usize| u16::from_le_bytes([f[i], f[i + 1]]);
                    let mut tag = u16_at(0);
                    if tag == 0xFFFE && size >= 40 {
                        // WAVE_FORMAT_EXTENSIBLE: sub-format GUID starts at 24.
                        let mut ext = [0u8; 2];
                        read_at(&file, &mut ext, body + 24)?;
                        tag = u16::from_le_bytes(ext);
                    }
                    fmt = Some((
                        tag,
                        u16_at(2),
                        u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                        u16_at(14),
                    ));
                }
                b"data" => {
                    let (tag, channels, sample_rate, bits) =
                        fmt.ok_or_else(|| bad("data chunk before fmt chunk"))?;
                    let format = match (tag, bits) {
                        (1, 16) => WavFormat::Pcm16,
                        (1, 24) => WavFormat::Pcm24,
                        (3, 32) => WavFormat::Float32,
                        _ => {
                            return Err(bad(format!(
                                "unsupported WAV format (tag {tag}, {bits} bits)"
                            )));
                        }
                    };
                    // Tolerate files whose size field was never patched.
                    let available = len.saturating_sub(body);
                    let data_len = if size == 0 || size > available {
                        available
                    } else {
                        size
                    };
                    let block = channels.max(1) as u64 * (bits / 8) as u64;
                    return Ok(Self {
                        file,
                        path: path.to_path_buf(),
                        channels: channels.max(1),
                        sample_rate,
                        format,
                        data_offset: body,
                        frames: data_len / block,
                    });
                }
                _ => {}
            }
            pos = body + size + (size & 1);
        }
        Err(bad("no data chunk"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn channels(&self) -> usize {
        self.channels as usize
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn format(&self) -> WavFormat {
        self.format
    }

    /// Read frames `[start, start + out[c].len())` into per-channel slices.
    /// Frames beyond the end of the file are zero-filled.
    pub fn read(
        &self,
        start: u64,
        out: &mut [&mut [f32]],
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        let frames = out.iter().map(|c| c.len()).min().unwrap_or(0);
        for c in out.iter_mut() {
            c.fill(0.0);
        }
        if start >= self.frames || frames == 0 {
            return Ok(());
        }
        let n = (frames as u64).min(self.frames - start) as usize;
        let bps = (self.format.bits() / 8) as usize;
        let block = bps * self.channels as usize;
        scratch.resize(n * block, 0);
        read_at(&self.file, scratch, self.data_offset + start * block as u64)?;
        for i in 0..n {
            for (c, dst) in out.iter_mut().enumerate().take(self.channels as usize) {
                let at = i * block + c * bps;
                let b = &scratch[at..at + bps];
                dst[i] = match self.format {
                    WavFormat::Pcm16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                    WavFormat::Pcm24 => {
                        (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0
                    }
                    WavFormat::Float32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                };
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ff-wavstream-{}-{name}", std::process::id()))
    }

    #[test]
    fn streaming_write_then_random_reads() {
        let path = tmp("s.wav");
        let mut w = WavWriter::create(&path, 2, 44_100, WavFormat::Float32, false).unwrap();
        let ramp: Vec<f32> = (0..10_000).map(|i| i as f32 / 10_000.0).collect();
        let neg: Vec<f32> = ramp.iter().map(|v| -v).collect();
        for chunk in 0..10 {
            let a = chunk * 1000;
            w.write_planar(&[&ramp[a..a + 1000], &neg[a..a + 1000]], 1000)
                .unwrap();
        }
        assert_eq!(w.frames(), 10_000);
        w.finish().unwrap();

        let f = WavFile::open(&path).unwrap();
        assert_eq!(
            (f.channels(), f.sample_rate(), f.frames()),
            (2, 44_100, 10_000)
        );
        let mut l = vec![0.0; 64];
        let mut r = vec![0.0; 64];
        let mut scratch = Vec::new();
        f.read(5_000, &mut [&mut l, &mut r], &mut scratch).unwrap();
        assert_eq!(l[0], 0.5);
        assert_eq!(r[10], -ramp[5_010]);
        // Reading past the end zero-fills.
        f.read(9_990, &mut [&mut l, &mut r], &mut scratch).unwrap();
        assert_eq!(l[9], ramp[9_999]);
        assert!(l[10..].iter().all(|v| *v == 0.0));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn reads_pcm24_written_by_the_simple_writer() {
        let path = tmp("p24.wav");
        let data: Vec<f32> = (0..500).map(|i| ((i as f32) * 0.05).sin() * 0.9).collect();
        crate::write_wav(
            &path,
            std::slice::from_ref(&data),
            96_000,
            WavFormat::Pcm24,
            false,
        )
        .unwrap();
        let f = WavFile::open(&path).unwrap();
        let mut out = vec![0.0; 500];
        f.read(0, &mut [&mut out], &mut Vec::new()).unwrap();
        for (a, b) in out.iter().zip(&data) {
            assert!((a - b).abs() < 2.0 / 8_388_608.0);
        }
        std::fs::remove_file(&path).unwrap();
    }
}

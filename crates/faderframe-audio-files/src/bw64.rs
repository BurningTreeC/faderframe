//! BW64 files (ITU-R BS.2088; RF64 past 4 GiB): WAVE with chunks of its
//! own before and after the audio — an ADM master's `chna` and `axml` — and
//! 64-bit sizes once it outgrows RIFF.
//!
//! The file starts as plain RIFF with a 28-byte `JUNK` chunk after the
//! header; finishing a file larger than 4 GiB turns it into `RF64` (the id
//! the Dolby Atmos master profile names) with that chunk as `ds64`.

use crate::dither::{Dither, Quantizer};
use crate::wav::WavFormat;
use crate::wavstream::interleave;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A chunk: its id and payload.
pub type Chunk = ([u8; 4], Vec<u8>);

/// Incrementally written BW64 file (plain PCM `fmt `; the channels'
/// meaning is in the caller's chunks).
pub struct Bw64Writer {
    out: BufWriter<File>,
    path: PathBuf,
    channels: u16,
    format: WavFormat,
    quantizer: Quantizer,
    frames: u64,
    scratch: Vec<u8>,
    /// Where the `data` chunk's header is.
    data_header: u64,
}

const DS64: u32 = 28;

impl Bw64Writer {
    /// A file of `channels` channels with `before` written ahead of the
    /// audio.
    pub fn create(
        path: &Path,
        channels: u16,
        sample_rate: u32,
        format: WavFormat,
        dither: Dither,
        before: &[Chunk],
    ) -> io::Result<Self> {
        let channels = channels.max(1);
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        out.write_all(b"RIFF\0\0\0\0WAVE")?;
        out.write_all(b"JUNK")?;
        out.write_all(&DS64.to_le_bytes())?;
        out.write_all(&[0; DS64 as usize])?;
        let mut pos = 12 + 8 + u64::from(DS64);
        // Plain PCM or float: the chunks say what each channel is.
        let fmt = crate::wav::fmt_chunk(channels, sample_rate, format, None);
        out.write_all(&fmt)?;
        pos += fmt.len() as u64;
        for (id, payload) in before {
            pos += write_chunk(&mut out, id, payload)?;
        }
        out.write_all(b"data\0\0\0\0")?;
        let quantizer = Quantizer::new(
            format.bits(),
            channels as usize,
            sample_rate,
            if format.is_integer() {
                dither
            } else {
                Dither::Off
            },
        );
        Ok(Self {
            out,
            path: path.to_path_buf(),
            channels,
            format,
            quantizer,
            frames: 0,
            scratch: Vec::new(),
            data_header: pos,
        })
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Append `frames` frames of non-interleaved channels (missing ones are
    /// silent).
    pub fn write_planar(&mut self, channels: &[&[f32]], frames: usize) -> io::Result<()> {
        interleave(
            &mut self.scratch,
            &mut self.quantizer,
            self.format,
            self.channels as usize,
            channels,
            frames,
        );
        self.out.write_all(&self.scratch)?;
        self.frames += frames as u64;
        Ok(())
    }

    /// Write `after` behind the audio, patch the sizes (RF64 with a `ds64`
    /// past 4 GiB) and close.
    pub fn finish(self, after: &[Chunk]) -> io::Result<PathBuf> {
        self.finish_as(after, false)
    }

    /// [`Self::finish`], RF64 whatever the size when `rf64`.
    fn finish_as(mut self, after: &[Chunk], rf64: bool) -> io::Result<PathBuf> {
        let data = self.frames * u64::from(self.channels) * u64::from(self.format.bits() / 8);
        if data & 1 == 1 {
            self.out.write_all(&[0])?;
        }
        for (id, payload) in after {
            write_chunk(&mut self.out, id, payload)?;
        }
        self.out.flush()?;
        let mut file = self.out.into_inner().map_err(|e| e.into_error())?;
        let len = file.seek(SeekFrom::End(0))?;
        let riff = len - 8;
        let large = rf64 || riff > u64::from(u32::MAX) || data > u64::from(u32::MAX);
        file.seek(SeekFrom::Start(0))?;
        if large {
            file.write_all(b"RF64")?;
            file.write_all(&u32::MAX.to_le_bytes())?;
            file.seek(SeekFrom::Start(12))?;
            file.write_all(b"ds64")?;
            file.write_all(&DS64.to_le_bytes())?;
            file.write_all(&riff.to_le_bytes())?;
            file.write_all(&data.to_le_bytes())?;
            file.write_all(&self.frames.to_le_bytes())?;
            file.write_all(&0u32.to_le_bytes())?;
            file.seek(SeekFrom::Start(self.data_header + 4))?;
            file.write_all(&u32::MAX.to_le_bytes())?;
        } else {
            file.write_all(b"RIFF")?;
            file.write_all(&(riff as u32).to_le_bytes())?;
            file.seek(SeekFrom::Start(self.data_header + 4))?;
            file.write_all(&(data as u32).to_le_bytes())?;
        }
        file.sync_data()?;
        Ok(self.path)
    }
}

fn write_chunk(out: &mut impl Write, id: &[u8; 4], payload: &[u8]) -> io::Result<u64> {
    let size = u32::try_from(payload.len()).map_err(|_| io::Error::other("chunk too large"))?;
    out.write_all(id)?;
    out.write_all(&size.to_le_bytes())?;
    out.write_all(payload)?;
    let pad = payload.len() & 1;
    if pad == 1 {
        out.write_all(&[0])?;
    }
    Ok(8 + payload.len() as u64 + pad as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wavstream::{WavFile, chunks, read_chunk};

    #[test]
    fn chunks_before_and_after_the_audio_round_trip() {
        let dir = std::env::temp_dir().join(format!("ff-bw64-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adm.wav");
        let mut w = Bw64Writer::create(
            &path,
            3,
            48_000,
            WavFormat::Pcm24,
            Dither::Off,
            &[(*b"chna", vec![1, 2, 3])],
        )
        .unwrap();
        let a = vec![0.25f32; 1000];
        let b = vec![-0.5f32; 1000];
        w.write_planar(&[&a, &b], 1000).unwrap();
        w.finish(&[(*b"axml", b"<x/>".to_vec())]).unwrap();
        let ids: Vec<[u8; 4]> = chunks(&path).unwrap().into_iter().map(|c| c.0).collect();
        assert_eq!(ids, [*b"JUNK", *b"fmt ", *b"chna", *b"data", *b"axml"]);
        assert_eq!(read_chunk(&path, b"chna").unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(read_chunk(&path, b"axml").unwrap().unwrap(), b"<x/>");
        let f = WavFile::open(&path).unwrap();
        assert_eq!(
            (f.channels(), f.frames(), f.format()),
            (3, 1000, WavFormat::Pcm24)
        );
        let (mut x, mut y, mut z) = (vec![0.0; 10], vec![0.0; 10], vec![1.0; 10]);
        f.read(500, &mut [&mut x, &mut y, &mut z], &mut Vec::new())
            .unwrap();
        assert!((x[0] - 0.25).abs() < 1e-6 && (y[9] + 0.5).abs() < 1e-6 && z[3] == 0.0);
        // The same past 4 GiB: RF64 with the sizes in ds64.
        let mut w =
            Bw64Writer::create(&path, 2, 48_000, WavFormat::Pcm24, Dither::Off, &[]).unwrap();
        w.write_planar(&[&a, &b], 1000).unwrap();
        w.finish_as(&[(*b"axml", b"<y/>".to_vec())], true).unwrap();
        let head = std::fs::read(&path).unwrap();
        assert_eq!(&head[0..4], b"RF64");
        assert_eq!(&head[12..16], b"ds64");
        let f = WavFile::open(&path).unwrap();
        assert_eq!((f.channels(), f.frames()), (2, 1000));
        assert_eq!(read_chunk(&path, b"axml").unwrap().unwrap(), b"<y/>");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! Writing FLAC files (16- or 24-bit, one to eight channels) as the audio
//! comes: [`FlacWriter`] quantises planar float audio (with the chosen
//! [`Dither`]), encodes it a [`BLOCK`] of frames at a time with flacenc
//! (pure Rust, the encoder IAMF's FLAC substreams use) and writes each
//! frame at once. The stream information at the start of the file — its
//! length, frame sizes and the MD5 of the audio, which decoders use to
//! verify it — is written again over the first one when the file is
//! finished ([`FlacWriter::finish`]); it has the same size, so nothing
//! moves.

use crate::dither::{Dither, Quantizer};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream};
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context, Fill, FrameBuf};
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// Frames per FLAC frame (the reference encoder's default).
pub const BLOCK: usize = 4096;

/// The most channels a FLAC stream holds.
pub const MAX_CHANNELS: usize = 8;

/// Bit depths a [`FlacWriter`] writes.
pub const DEPTHS: [u16; 2] = [16, 24];

fn other(e: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("FLAC: {e:?}"))
}

/// An incrementally written FLAC file (see the module docs).
pub struct FlacWriter {
    out: BufWriter<File>,
    /// The stream's information (no frames are kept in it).
    stream: Stream,
    config: Verified<flacenc::config::Encoder>,
    buf: FrameBuf,
    context: Context,
    quantizer: Quantizer,
    channels: usize,
    /// Interleaved samples not yet encoded (less than a block).
    pending: Vec<i32>,
    frame: usize,
    header_len: usize,
    frames: u64,
}

impl FlacWriter {
    /// A file of `channels` channels of `bits` bits (16 or 24) at
    /// `sample_rate`.
    pub fn create(
        path: &Path,
        channels: usize,
        sample_rate: u32,
        bits: u16,
        dither: Dither,
    ) -> io::Result<Self> {
        if !DEPTHS.contains(&bits) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FLAC files are written with 16 or 24 bits",
            ));
        }
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("FLAC holds one to {MAX_CHANNELS} channels, not {channels}"),
            ));
        }
        let mut stream =
            Stream::new(sample_rate as usize, channels, usize::from(bits)).map_err(other)?;
        stream
            .stream_info_mut()
            .set_block_sizes(BLOCK, BLOCK)
            .map_err(other)?;
        let mut config = flacenc::config::Encoder::default();
        config.block_size = BLOCK;
        config.multithread = false;
        let config = config.into_verified().map_err(|(_, e)| other(e))?;
        let header = header_bytes(&stream)?;
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        out.write_all(&header)?;
        Ok(Self {
            out,
            stream,
            config,
            buf: FrameBuf::with_size(channels, BLOCK).map_err(other)?,
            context: Context::new(usize::from(bits), channels),
            quantizer: Quantizer::new(bits, channels, sample_rate, dither),
            channels,
            pending: Vec::with_capacity(BLOCK * channels),
            frame: 0,
            header_len: header.len(),
            frames: 0,
        })
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Append `frames` frames of planar channels (missing ones silent).
    pub fn write_planar(&mut self, channels: &[&[f32]], frames: usize) -> io::Result<()> {
        for i in 0..frames {
            for c in 0..self.channels {
                let s = channels.get(c).and_then(|ch| ch.get(i)).copied();
                self.pending
                    .push(self.quantizer.quantize(c, s.unwrap_or(0.0)));
            }
            if self.pending.len() == BLOCK * self.channels {
                self.encode()?;
            }
        }
        self.frames += frames as u64;
        Ok(())
    }

    /// Encode and write what is pending as one frame.
    fn encode(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        (&mut self.buf, &mut self.context)
            .fill_interleaved(&self.pending)
            .map_err(other)?;
        self.pending.clear();
        let frame = flacenc::encode_fixed_size_frame(
            &self.config,
            &self.buf,
            self.frame,
            self.stream.stream_info(),
        )
        .map_err(other)?;
        self.frame += 1;
        self.stream.stream_info_mut().update_frame_info(&frame);
        let mut sink = ByteSink::new();
        frame.write(&mut sink).map_err(other)?;
        self.out.write_all(sink.as_slice())
    }

    /// Encode the last (shorter) frame, write the stream information with
    /// the length and the MD5 over the first one, and close the file.
    pub fn finish(mut self) -> io::Result<()> {
        self.encode()?;
        let info = self.stream.stream_info_mut();
        // A fixed block size: the last, shorter block does not count
        // (decoders number the frames by it).
        info.set_block_sizes(BLOCK, BLOCK).map_err(other)?;
        info.set_total_samples(self.context.total_samples());
        info.set_md5_digest(&self.context.md5_digest());
        let header = header_bytes(&self.stream)?;
        if header.len() != self.header_len {
            return Err(other("the stream information changed size"));
        }
        self.out.flush()?;
        let mut file = self.out.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.flush()?;
        file.sync_all()
    }
}

/// "fLaC" and the stream information block (the stream holds no frames).
fn header_bytes(stream: &Stream) -> io::Result<Vec<u8>> {
    let mut sink = ByteSink::new();
    stream.write(&mut sink).map_err(other)?;
    Ok(sink.as_slice().to_vec())
}

/// Write planar `channels` to a FLAC file of `bits` bits at once.
pub fn write_flac(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    bits: u16,
    dither: Dither,
) -> io::Result<()> {
    let mut w = FlacWriter::create(path, channels.len(), sample_rate, bits, dither)?;
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let slices: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    w.write_planar(&slices, frames)?;
    w.finish()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::sync::atomic::AtomicBool;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ff-flac-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// What the decoder reads back of a file.
    fn read(path: &Path) -> (crate::decode::ProbeInfo, Vec<Vec<f32>>) {
        let out: std::cell::RefCell<Vec<Vec<f32>>> = Default::default();
        let info = crate::decode::decode_file(
            path,
            &AtomicBool::new(false),
            |i| {
                *out.borrow_mut() = vec![Vec::new(); i.channels];
                Ok(())
            },
            |planes, n| {
                for (o, p) in out.borrow_mut().iter_mut().zip(planes) {
                    o.extend_from_slice(&p[..n]);
                }
                Ok(())
            },
        )
        .unwrap();
        (info, out.into_inner())
    }

    /// 16 and 24 bits, mono and stereo, lengths that end inside a block
    /// and on one: the decoder reads back what was written, to the bit.
    #[test]
    fn what_is_written_reads_back_exactly() {
        let d = dir("exact");
        for (channels, bits, frames) in [(1, 16, 10_000), (2, 24, 3 * BLOCK), (2, 16, 777)] {
            let audio: Vec<Vec<f32>> = (0..channels)
                .map(|c| {
                    (0..frames)
                        .map(|i| 0.5 * ((i as f32 * 0.01 * (c + 1) as f32).sin()))
                        .collect()
                })
                .collect();
            let path = d.join(format!("t{channels}-{bits}-{frames}.flac"));
            write_flac(&path, &audio, 44_100, bits, Dither::Off).unwrap();
            let (info, back) = read(&path);
            assert_eq!(info.sample_rate, 44_100);
            assert_eq!(info.channels, channels);
            assert_eq!(back.len(), channels);
            let scale = (1i64 << (bits - 1)) as f32;
            for (a, b) in audio.iter().zip(&back) {
                assert_eq!(b.len(), frames, "{channels} ch {bits} bits");
                for (x, y) in a.iter().zip(b) {
                    let q = (x * (scale - 1.0)).round() / scale;
                    assert!((q - y).abs() < 1e-6, "{x} {y}");
                }
            }
        }
    }

    /// The stream information carries the length and the audio's MD5.
    #[test]
    fn the_header_holds_the_length_and_the_md5() {
        let d = dir("header");
        let path = d.join("a.flac");
        let audio = vec![vec![0.25f32; 5000], vec![-0.25f32; 5000]];
        write_flac(&path, &audio, 48_000, 16, Dither::Off).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"fLaC");
        // STREAMINFO: the last block, 34 bytes.
        assert_eq!(bytes[4], 0x80);
        let info = &bytes[8..42];
        // 36 bits of total samples after rate (20), channels (3), bits (5).
        let total = (u64::from(info[13] & 0x0F) << 32)
            | u64::from(u32::from_be_bytes([info[14], info[15], info[16], info[17]]));
        assert_eq!(total, 5000);
        assert_ne!(&info[18..34], &[0u8; 16]);
    }

    #[test]
    fn float_and_too_many_channels_are_refused() {
        let d = dir("refused");
        assert!(FlacWriter::create(&d.join("a.flac"), 2, 48_000, 32, Dither::Off).is_err());
        assert!(FlacWriter::create(&d.join("b.flac"), 9, 48_000, 16, Dither::Off).is_err());
    }
}

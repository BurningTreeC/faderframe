//! IAMF in ISO-BMFF (§ 6): one track whose samples are Temporal Units (no
//! delimiters), sample entry `iamf` with the descriptors in an `iacb` box,
//! brands `iamf` and `iso6`, the trimming in an edit list, Opus' roll in a
//! `roll` sample group. Written as it goes: `ftyp`, `mdat` (64-bit size,
//! filled in at the end; every sample in one chunk), then `moov`.

use crate::obu::leb128;
use std::io::{self, Seek, SeekFrom, Write};

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + body.len());
    v.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + body.len());
    b.push(version);
    b.extend_from_slice(&flags.to_be_bytes()[1..]);
    b.extend_from_slice(body);
    bx(kind, &b)
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn matrix(b: &mut Vec<u8>) {
    for m in MATRIX {
        b.extend_from_slice(&m.to_be_bytes());
    }
}

/// What the track says besides its samples.
pub struct Track<'a> {
    /// The descriptor OBUs ([`crate::Master::descriptors`]).
    pub descriptors: &'a [u8],
    pub sample_rate: u32,
    /// Samples trimmed at the start (the edit list skips them).
    pub trim_start: u32,
    /// Samples presented (after both trims).
    pub presented: u64,
    /// Opus' roll distance (`None` for LPCM and FLAC).
    pub roll: Option<i16>,
}

/// Writes an MP4 file sample by sample.
pub struct Mp4Writer<W: Write + Seek> {
    w: W,
    mdat_header: u64,
    data: u64,
    sizes: Vec<u32>,
    durations: Vec<u32>,
}

impl<W: Write + Seek> Mp4Writer<W> {
    pub fn create(mut w: W) -> io::Result<Self> {
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"iamf");
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        for brand in [b"iamf", b"iso6", b"mp41"] {
            ftyp.extend_from_slice(brand);
        }
        w.write_all(&bx(b"ftyp", &ftyp))?;
        let mdat_header = w.stream_position()?;
        // size 1: a 64-bit size follows.
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&0u64.to_be_bytes())?;
        Ok(Self {
            w,
            mdat_header,
            data: 0,
            sizes: Vec::new(),
            durations: Vec::new(),
        })
    }

    /// One IA Sample (a Temporal Unit) lasting `duration` samples (the
    /// trimmed end excluded).
    pub fn sample(&mut self, bytes: &[u8], duration: u32) -> io::Result<()> {
        self.w.write_all(bytes)?;
        self.data += bytes.len() as u64;
        self.sizes.push(bytes.len() as u32);
        self.durations.push(duration);
        Ok(())
    }

    pub fn finish(mut self, t: &Track<'_>) -> io::Result<W> {
        let end = self.w.stream_position()?;
        self.w.seek(SeekFrom::Start(self.mdat_header + 8))?;
        self.w.write_all(&(16 + self.data).to_be_bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        let moov = self.moov(t);
        self.w.write_all(&moov)?;
        self.w.flush()?;
        Ok(self.w)
    }

    fn moov(&self, t: &Track<'_>) -> Vec<u8> {
        let rate = t.sample_rate;
        let media: u64 = self.durations.iter().map(|&d| u64::from(d)).sum();
        let n = self.sizes.len() as u32;
        // mvhd (v1).
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0; 16]);
        mvhd.extend_from_slice(&rate.to_be_bytes());
        mvhd.extend_from_slice(&t.presented.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
        mvhd.extend_from_slice(&[0; 10]);
        matrix(&mut mvhd);
        mvhd.extend_from_slice(&[0; 24]);
        mvhd.extend_from_slice(&2u32.to_be_bytes());
        // tkhd (v1, enabled, in movie and preview).
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0; 16]);
        tkhd.extend_from_slice(&1u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&t.presented.to_be_bytes());
        tkhd.extend_from_slice(&[0; 8]);
        tkhd.extend_from_slice(&0i16.to_be_bytes());
        tkhd.extend_from_slice(&1i16.to_be_bytes());
        tkhd.extend_from_slice(&0x0100u16.to_be_bytes());
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        matrix(&mut tkhd);
        tkhd.extend_from_slice(&[0; 8]);
        // edts/elst (v1): from the first presented sample.
        let mut elst = Vec::new();
        elst.extend_from_slice(&1u32.to_be_bytes());
        elst.extend_from_slice(&t.presented.to_be_bytes());
        elst.extend_from_slice(&i64::from(t.trim_start).to_be_bytes());
        elst.extend_from_slice(&1i16.to_be_bytes());
        elst.extend_from_slice(&0i16.to_be_bytes());
        let edts = bx(b"edts", &full(b"elst", 1, 0, &elst));
        // mdhd (v1), language "und".
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&[0; 16]);
        mdhd.extend_from_slice(&rate.to_be_bytes());
        mdhd.extend_from_slice(&media.to_be_bytes());
        mdhd.extend_from_slice(&0x55C4u16.to_be_bytes());
        mdhd.extend_from_slice(&0u16.to_be_bytes());
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"soun");
        hdlr.extend_from_slice(&[0; 12]);
        hdlr.extend_from_slice(b"SoundHandler\0");
        let smhd = full(b"smhd", 0, 0, &[0; 4]);
        let mut dref = Vec::new();
        dref.extend_from_slice(&1u32.to_be_bytes());
        dref.extend_from_slice(&full(b"url ", 0, 1, &[]));
        let dinf = bx(b"dinf", &full(b"dref", 0, 0, &dref));
        // The sample entry: AudioSampleEntry('iamf') with channelcount and
        // samplerate 0, then the iacb box.
        let mut iacb = vec![1u8];
        leb128(&mut iacb, t.descriptors.len() as u32);
        iacb.extend_from_slice(t.descriptors);
        let mut entry = Vec::new();
        entry.extend_from_slice(&[0; 6]);
        entry.extend_from_slice(&1u16.to_be_bytes());
        entry.extend_from_slice(&[0; 8]);
        entry.extend_from_slice(&0u16.to_be_bytes());
        entry.extend_from_slice(&16u16.to_be_bytes());
        entry.extend_from_slice(&[0; 4]);
        entry.extend_from_slice(&0u32.to_be_bytes());
        entry.extend_from_slice(&bx(b"iacb", &iacb));
        let mut stsd = Vec::new();
        stsd.extend_from_slice(&1u32.to_be_bytes());
        stsd.extend_from_slice(&bx(b"iamf", &entry));
        // stts: runs of equal durations.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for &d in &self.durations {
            match runs.last_mut() {
                Some((count, delta)) if *delta == d => *count += 1,
                _ => runs.push((1, d)),
            }
        }
        let mut stts = Vec::new();
        stts.extend_from_slice(&(runs.len() as u32).to_be_bytes());
        for (count, delta) in runs {
            stts.extend_from_slice(&count.to_be_bytes());
            stts.extend_from_slice(&delta.to_be_bytes());
        }
        // One chunk holds every sample.
        let mut stsc = Vec::new();
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&n.to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
        let mut stsz = Vec::new();
        stsz.extend_from_slice(&0u32.to_be_bytes());
        stsz.extend_from_slice(&n.to_be_bytes());
        for s in &self.sizes {
            stsz.extend_from_slice(&s.to_be_bytes());
        }
        // The one chunk starts right after the mdat header: always a 32-bit
        // offset (stco, which every reader takes).
        let mut stco = Vec::new();
        stco.extend_from_slice(&1u32.to_be_bytes());
        stco.extend_from_slice(&((self.mdat_header + 16) as u32).to_be_bytes());
        let mut stbl = Vec::new();
        stbl.extend_from_slice(&full(b"stsd", 0, 0, &stsd));
        stbl.extend_from_slice(&full(b"stts", 0, 0, &stts));
        stbl.extend_from_slice(&full(b"stsc", 0, 0, &stsc));
        stbl.extend_from_slice(&full(b"stsz", 0, 0, &stsz));
        stbl.extend_from_slice(&full(b"stco", 0, 0, &stco));
        if let Some(roll) = t.roll {
            let mut sgpd = Vec::new();
            sgpd.extend_from_slice(b"roll");
            sgpd.extend_from_slice(&2u32.to_be_bytes());
            sgpd.extend_from_slice(&1u32.to_be_bytes());
            sgpd.extend_from_slice(&roll.to_be_bytes());
            stbl.extend_from_slice(&full(b"sgpd", 1, 0, &sgpd));
            let mut sbgp = Vec::new();
            sbgp.extend_from_slice(b"roll");
            sbgp.extend_from_slice(&1u32.to_be_bytes());
            sbgp.extend_from_slice(&n.to_be_bytes());
            sbgp.extend_from_slice(&1u32.to_be_bytes());
            stbl.extend_from_slice(&full(b"sbgp", 0, 0, &sbgp));
        }
        let minf = bx(b"minf", &[smhd, dinf, bx(b"stbl", &stbl)].concat());
        let mdia = bx(
            b"mdia",
            &[full(b"mdhd", 1, 0, &mdhd), full(b"hdlr", 0, 0, &hdlr), minf].concat(),
        );
        let trak = bx(b"trak", &[full(b"tkhd", 1, 7, &tkhd), edts, mdia].concat());
        bx(b"moov", &[full(b"mvhd", 1, 0, &mvhd), trak].concat())
    }
}

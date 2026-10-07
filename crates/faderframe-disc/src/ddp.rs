//! DDP 2.00 filesets: what a CD replication plant takes as a master.
//!
//! [`DdpWriter`] writes the audio image (`IMAGE.DAT`: raw 16-bit
//! little-endian stereo in 2352-byte sectors, starting with track 1's
//! pregap) as it is produced, hashing it on the way, and then the
//! descriptor files: `DDPID` (identifier, UPC/EAN, master id), `DDPMS`
//! (one 128-byte packet per stream: CD-Text, PQ descriptor, audio), the
//! PQ descriptor (one 64-byte packet per index, with ISRC and flags,
//! between a lead-in carrying the UPC and a lead-out written twice),
//! `CDTEXT.BIN` when there is text, and `CHECKSUM.MD5`/`CHECKSUM.TXT`.
//! All descriptor fields are fixed-width ASCII.
//!
//! [`read`] parses a fileset back into a [`Disc`] and checks its sizes,
//! CD-Text CRCs and checksums — exports are verified with it.

use crate::hash::{Crc32, Md5};
use crate::{CdText, Disc, DiscError, SECTOR_BYTES, SECTOR_FRAMES, Track, TrackFlags, msf};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum DdpError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Disc(#[from] DiscError),
    #[error("not a DDP fileset: {0}")]
    Format(String),
    #[error("{file}: checksum mismatch")]
    Checksum { file: String },
}

/// File names of a fileset.
#[derive(Clone, Debug)]
pub struct DdpNames {
    pub image: String,
    pub pq: String,
    pub cdtext: String,
}

impl Default for DdpNames {
    fn default() -> Self {
        Self {
            image: "IMAGE.DAT".into(),
            pq: "PQDESCR".into(),
            cdtext: "CDTEXT.BIN".into(),
        }
    }
}

/// A file of the fileset with its checksums.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub name: String,
    pub md5: String,
    pub crc32: u32,
}

/// What [`DdpWriter::finish`] wrote.
#[derive(Clone, Debug)]
pub struct Fileset {
    pub dir: PathBuf,
    pub disc: Disc,
    /// In checksum order: DDPID, DDPMS, CD-Text, PQ, image.
    pub files: Vec<Checked>,
}

/// Writes a DDP fileset into a folder: first the audio, then
/// [`Self::finish`] with the disc's description.
pub struct DdpWriter {
    dir: PathBuf,
    names: DdpNames,
    image: BufWriter<File>,
    md5: Md5,
    crc: Crc32,
    frames: u64,
}

/// `text` left-aligned in `width` bytes, space-padded (and cut).
fn field(out: &mut Vec<u8>, text: &[u8], width: usize) {
    let n = text.len().min(width);
    out.extend_from_slice(&text[..n]);
    out.resize(out.len() + width - n, b' ');
}

/// A number right-aligned in `width` bytes, space-padded.
fn number(out: &mut Vec<u8>, value: u64, width: usize) {
    field(out, format!("{value:>width$}").as_bytes(), width);
}

fn blank(out: &mut Vec<u8>, width: usize) {
    field(out, b"", width);
}

fn hashes(data: &[u8]) -> (String, u32) {
    let mut m = Md5::new();
    m.update(data);
    let mut c = Crc32::new();
    c.update(data);
    (m.hex(), c.finish())
}

/// The identifier record.
pub fn ddpid(disc: &Disc) -> Vec<u8> {
    let mut d = Vec::with_capacity(128);
    field(&mut d, b"DDP 2.00", 8);
    field(&mut d, disc.upc.as_deref().unwrap_or("").as_bytes(), 13);
    blank(&mut d, 8 + 8 + 1);
    field(&mut d, &crate::latin1(&disc.master_id), 48);
    blank(&mut d, 1);
    field(&mut d, b"CD", 2);
    let rest = 128 - d.len();
    blank(&mut d, rest);
    d
}

/// One map stream packet.
#[allow(clippy::too_many_arguments)]
fn map_packet(
    dst: &[u8],
    length: u64,
    sub: &[u8],
    cdm: &[u8],
    pregap: Option<u64>,
    trk: &[u8],
    name: &str,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(128);
    field(&mut p, b"VVVM", 4);
    field(&mut p, dst, 2);
    blank(&mut p, 8);
    number(&mut p, length, 8);
    blank(&mut p, 8);
    field(&mut p, sub, 8);
    // CDM, SSM (complete sectors) and SCR as cue2ddp writes them.
    field(&mut p, cdm, 4);
    blank(&mut p, 4);
    match pregap {
        Some(v) => number(&mut p, v, 4),
        None => blank(&mut p, 4),
    }
    blank(&mut p, 4 + 1);
    field(&mut p, trk, 2);
    blank(&mut p, 2 + 12);
    number(&mut p, 17, 3);
    field(&mut p, name.as_bytes(), 17);
    let rest = 128 - p.len();
    blank(&mut p, rest);
    p
}

/// The PQ descriptor: lead-in, every index, lead-out twice.
pub fn pq_descriptor(disc: &Disc) -> Vec<u8> {
    let mut out = Vec::new();
    let mut packet =
        |track: &[u8], index: u32, at: u32, flags: TrackFlags, isrc: &str, upc: &str| {
            let start = out.len();
            field(&mut out, b"VVVS", 4);
            field(&mut out, track, 2);
            field(&mut out, format!("{index:02}").as_bytes(), 2);
            field(&mut out, format!("{:>8}", msf(at)).as_bytes(), 8);
            field(&mut out, &flags.c1(), 2);
            blank(&mut out, 2);
            field(&mut out, isrc.as_bytes(), 12);
            field(&mut out, upc.as_bytes(), 13);
            let rest = 64 - (out.len() - start);
            blank(&mut out, rest);
        };
    packet(
        b"00",
        0,
        0,
        TrackFlags::default(),
        "",
        disc.upc.as_deref().unwrap_or(""),
    );
    for (i, t) in disc.tracks.iter().enumerate() {
        let number = format!("{:02}", i + 1);
        let isrc = t.isrc.as_deref().unwrap_or("");
        let mut first = true;
        for (index, at) in t.pregap.map(|p| (0u32, p)).into_iter().chain(
            t.indexes
                .iter()
                .enumerate()
                .map(|(k, &x)| (k as u32 + 1, x)),
        ) {
            packet(
                number.as_bytes(),
                index,
                at,
                t.flags,
                if first { isrc } else { "" },
                "",
            );
            first = false;
        }
    }
    for _ in 0..2 {
        packet(b"AA", 1, disc.sectors, TrackFlags::default(), "", "");
    }
    out
}

impl DdpWriter {
    /// Start a fileset in `dir` (created; files of an earlier fileset are
    /// replaced).
    pub fn create(dir: &Path, names: DdpNames) -> Result<Self, DdpError> {
        std::fs::create_dir_all(dir)?;
        let image = BufWriter::with_capacity(1 << 20, File::create(dir.join(&names.image))?);
        Ok(Self {
            dir: dir.to_path_buf(),
            names,
            image,
            md5: Md5::new(),
            crc: Crc32::new(),
            frames: 0,
        })
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.md5.update(bytes);
        self.crc.update(bytes);
        self.image.write_all(bytes)
    }

    /// Append stereo frames (interleaved left/right samples).
    pub fn write_interleaved(&mut self, samples: &[i16]) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.frames += (samples.len() / 2) as u64;
        self.put(&bytes)
    }

    /// Append `frames` frames of digital silence.
    pub fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        let zeros = [0u8; 4 * 4096];
        let mut left = frames * 4;
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            self.put(&zeros[..n])?;
            left -= n as u64;
        }
        self.frames += frames;
        Ok(())
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Pad the image to a whole sector, check `disc` (its length is set
    /// from the image) and write the descriptor and checksum files.
    pub fn finish(mut self, disc: &Disc) -> Result<Fileset, DdpError> {
        let partial = self.frames % SECTOR_FRAMES;
        if partial > 0 {
            self.write_silence(SECTOR_FRAMES - partial)?;
        }
        self.image.flush()?;
        let mut disc = disc.clone();
        disc.sectors = (self.frames / SECTOR_FRAMES) as u32;
        disc.validate()?;
        let image = Checked {
            name: self.names.image.clone(),
            md5: self.md5.hex(),
            crc32: self.crc.finish(),
        };
        let mut texts = Vec::new();
        for p in crate::cdtext::packs(&disc) {
            texts.extend_from_slice(&p);
        }
        let pq = pq_descriptor(&disc);
        let mut map = Vec::new();
        if !texts.is_empty() {
            map.extend(map_packet(
                b"S0",
                texts.len() as u64,
                b"CDTEXT",
                b"",
                None,
                b"00",
                &self.names.cdtext,
            ));
        }
        map.extend(map_packet(
            b"S0",
            pq.len() as u64,
            b"PQ DESCR",
            b"",
            None,
            b"",
            &self.names.pq,
        ));
        let pregap = disc.tracks.first().and_then(Track::start).unwrap_or(0);
        map.extend(map_packet(
            b"D0",
            u64::from(disc.sectors),
            b"",
            b"DA71",
            Some(u64::from(pregap)),
            b"",
            &self.names.image,
        ));
        let id = ddpid(&disc);
        let mut files = Vec::new();
        let mut put = |name: &str, data: &[u8]| -> Result<(), DdpError> {
            std::fs::write(self.dir.join(name), data)?;
            let (md5, crc32) = hashes(data);
            files.push(Checked {
                name: name.to_string(),
                md5,
                crc32,
            });
            Ok(())
        };
        put("DDPID", &id)?;
        put("DDPMS", &map)?;
        if !texts.is_empty() {
            put(&self.names.cdtext.clone(), &texts)?;
        }
        put(&self.names.pq.clone(), &pq)?;
        files.push(image);
        let md5: String = files
            .iter()
            .map(|f| format!("{} *{}\n", f.md5, f.name))
            .collect();
        std::fs::write(self.dir.join("CHECKSUM.MD5"), md5)?;
        let mut txt = String::from("[CRC32 Checksum]\n");
        for f in &files {
            txt += &format!("{}={:08X}\n", f.name, f.crc32);
        }
        std::fs::write(self.dir.join("CHECKSUM.TXT"), txt)?;
        Ok(Fileset {
            dir: self.dir,
            disc,
            files,
        })
    }
}

/// A fileset read back.
#[derive(Clone, Debug)]
pub struct ReadFileset {
    pub disc: Disc,
    /// The audio image's path.
    pub image: PathBuf,
    /// The checksums of `CHECKSUM.MD5` matched (`None`: there is none).
    pub checksums: Option<bool>,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim().to_string()
}

fn num(b: &[u8]) -> Option<u64> {
    text(b).parse().ok()
}

fn msf_of(b: &[u8]) -> Option<u32> {
    let t = text(b);
    let t = format!("{t:0>6}");
    let v: Vec<u32> = (0..3)
        .map(|i| t.get(i * 2..i * 2 + 2).and_then(|s| s.parse().ok()))
        .collect::<Option<_>>()?;
    Some((v[0] * 60 + v[1]) * 75 + v[2])
}

/// Read a fileset in `dir` and check it: sizes, PQ against the image,
/// CD-Text CRCs, the Red Book rules, and `CHECKSUM.MD5` when present.
pub fn read(dir: &Path) -> Result<ReadFileset, DdpError> {
    let f = inspect(dir)?;
    f.disc.validate()?;
    Ok(f)
}

/// [`read`] without the Red Book rules (a player shows what breaks them
/// instead of refusing the fileset; `Disc::validate` says).
pub fn inspect(dir: &Path) -> Result<ReadFileset, DdpError> {
    let bad = |what: &str| DdpError::Format(what.to_string());
    let id = std::fs::read(dir.join("DDPID"))?;
    if id.len() != 128 || !id.starts_with(b"DDP 2.0") {
        return Err(bad("DDPID"));
    }
    let map = std::fs::read(dir.join("DDPMS"))?;
    if map.is_empty() || map.len() % 128 != 0 {
        return Err(bad("DDPMS"));
    }
    let (mut image, mut pq, mut cdtext) = (None, None, None);
    let mut image_sectors = 0u64;
    for p in map.as_chunks::<128>().0 {
        if &p[..4] != b"VVVM" {
            return Err(bad("DDPMS packet"));
        }
        let name = text(&p[74..91]);
        match (&p[4..6], text(&p[30..38]).as_str(), &p[38..40]) {
            (b"D0", _, b"DA") => {
                image_sectors = num(&p[14..22]).ok_or_else(|| bad("stream length"))?;
                image = Some(name);
            }
            (b"S0", "PQ DESCR", _) => pq = Some(name),
            (b"S0", "CDTEXT", _) => cdtext = Some(name),
            _ => {}
        }
    }
    let image = dir.join(image.ok_or_else(|| bad("no audio stream"))?);
    let pq = std::fs::read(dir.join(pq.ok_or_else(|| bad("no PQ descriptor"))?))?;
    let size = std::fs::metadata(&image)?.len();
    if size != image_sectors * SECTOR_BYTES as u64 {
        return Err(bad("the image's size does not match its stream length"));
    }
    let upc = Some(text(&id[8..21])).filter(|s| !s.is_empty());
    let mut disc = Disc {
        upc,
        master_id: text(&id[38..86]),
        sectors: image_sectors as u32,
        ..Disc::default()
    };
    if pq.len() % 64 != 0 {
        return Err(bad("PQ descriptor"));
    }
    for p in pq.as_chunks::<64>().0 {
        if &p[..4] != b"VVVS" {
            return Err(bad("PQ packet"));
        }
        let at = msf_of(&p[8..16]).ok_or_else(|| bad("PQ time"))?;
        match &p[4..6] {
            b"00" => {}
            b"AA" => {
                if at != disc.sectors {
                    return Err(bad("the lead-out is not at the end of the image"));
                }
            }
            tk => {
                let n: usize = text(tk).parse().map_err(|_| bad("PQ track"))?;
                if n == 0 || n > disc.tracks.len() + 1 {
                    return Err(bad("PQ tracks out of order"));
                }
                if n > disc.tracks.len() {
                    let c = p[16];
                    let nibble = (c as char).to_digit(16).unwrap_or(0);
                    disc.tracks.push(Track {
                        flags: TrackFlags {
                            pre_emphasis: nibble & 1 != 0,
                            copy_permitted: nibble & 2 != 0,
                            four_channel: nibble & 8 != 0,
                            scms: p[17] == b'S',
                        },
                        ..Track::default()
                    });
                }
                let t = disc.tracks.last_mut().ok_or_else(|| bad("PQ"))?;
                let isrc = text(&p[20..32]);
                if !isrc.is_empty() {
                    t.isrc = Some(isrc);
                }
                match text(&p[6..8]).parse::<u32>().map_err(|_| bad("PQ index"))? {
                    0 => t.pregap = Some(at),
                    _ => t.indexes.push(at),
                }
            }
        }
    }
    if let Some(name) = cdtext {
        let data = std::fs::read(dir.join(name))?;
        let mut blocks = crate::cdtext::decode_blocks(&data, disc.tracks.len())
            .map_err(|i| bad(&format!("CD-Text pack {i} has a bad CRC")))?;
        if !blocks.is_empty() {
            let first = blocks.remove(0);
            disc.text = first.disc;
            disc.text_language = first.language;
            for (t, text) in disc.tracks.iter_mut().zip(first.tracks) {
                t.text = text;
            }
            disc.more_text = blocks;
        }
    } else {
        disc.text = CdText::default();
    }
    let checksums = match std::fs::read_to_string(dir.join("CHECKSUM.MD5")) {
        Ok(list) => {
            let mut ok = true;
            for line in list.lines().filter(|l| !l.trim().is_empty()) {
                let Some((sum, name)) = line.split_once(" *") else {
                    return Err(bad("CHECKSUM.MD5"));
                };
                let mut m = Md5::new();
                let mut f = File::open(dir.join(name.trim()))?;
                let mut buf = vec![0u8; 1 << 20];
                loop {
                    let n = io::Read::read(&mut f, &mut buf)?;
                    if n == 0 {
                        break;
                    }
                    m.update(&buf[..n]);
                }
                if m.hex() != sum.trim().to_ascii_lowercase() {
                    ok = false;
                }
            }
            Some(ok)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    Ok(ReadFileset {
        disc,
        image,
        checksums,
    })
}

/// Whether an image's audio is stored big-endian: DDP images are normally
/// little-endian (Intel order), some tools write the other. Music is
/// smooth from sample to sample in the right order and noise in the wrong
/// one: the order with the smaller sum of steps wins (silence: little).
pub fn image_big_endian(image: &Path) -> io::Result<bool> {
    let len = std::fs::metadata(image)?.len();
    let mut f = File::open(image)?;
    let window = 1 << 16;
    let (mut le, mut be) = (0u64, 0u64);
    for k in 1..=4u64 {
        let at = (len * k / 5) & !3;
        io::Seek::seek(&mut f, io::SeekFrom::Start(at))?;
        let mut buf = vec![0u8; window];
        let n = io::Read::read(&mut f, &mut buf)?;
        let frames = buf[..n & !3].as_chunks::<4>().0;
        for w in frames.windows(2) {
            let step = |a: i16, b: i16| u64::from((i32::from(a) - i32::from(b)).unsigned_abs());
            le += step(
                i16::from_le_bytes([w[0][0], w[0][1]]),
                i16::from_le_bytes([w[1][0], w[1][1]]),
            );
            be += step(
                i16::from_be_bytes([w[0][0], w[0][1]]),
                i16::from_be_bytes([w[1][0], w[1][1]]),
            );
        }
    }
    Ok(be < le)
}

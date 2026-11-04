//! Zip archives as DAWproject files and compressed MusicXML (`.mxl`) use
//! them: entries stored or deflated (method 0 or 8), UTF-8 names, ZIP64
//! where an entry, an offset or the directory passes 4 GiB or the
//! archive holds more than 65535 entries. Encrypted, split and other
//! methods' entries are refused by name.
//!
//! The reader finds the end record (and its ZIP64 locator) from the back,
//! reads the central directory and checks every entry's CRC as it is
//! read; an archive with something in front of it (a self-extracting
//! stub) is read too. The writer streams large entries (media) stored,
//! patching each one's CRC into its local header once written, so a
//! reader that trusts the local headers — Java's `ZipInputStream` —
//! reads it too; small ones (the XML) are deflated in memory.

use crate::{InterchangeError, Result};
use flate2::Crc;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const END64: u32 = 0x0606_4b50;
const LOCATOR64: u32 = 0x0706_4b50;
const MAX32: u64 = 0xFFFF_FFFF;
/// The end record's fixed part.
const END_LEN: usize = 22;
const LOCATOR_LEN: usize = 20;
/// UTF-8 names (general purpose flag bit 11).
const UTF8: u16 = 1 << 11;

/// How an entry's data is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Stored,
    Deflated,
    Other(u16),
}

impl Method {
    fn of(code: u16) -> Self {
        match code {
            0 => Method::Stored,
            8 => Method::Deflated,
            n => Method::Other(n),
        }
    }
}

/// An entry of the central directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub method: Method,
    pub crc: u32,
    pub compressed: u64,
    pub size: u64,
    encrypted: bool,
    /// Its local header (from the archive's start, past any stub).
    offset: u64,
}

impl Entry {
    /// A folder entry (its name ends in a slash).
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

fn bad(what: impl Into<String>) -> InterchangeError {
    InterchangeError::Zip(what.into())
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// A name as written in archives: forward slashes, nothing in front.
pub fn normalize_name(name: &str) -> String {
    let mut n = name.replace('\\', "/");
    while let Some(rest) = n.strip_prefix("./") {
        n = rest.to_string();
    }
    n.trim_start_matches('/').to_string()
}

/// Reads a zip archive's entries.
pub struct ZipReader<R> {
    inner: R,
    entries: Vec<Entry>,
    /// Where the archive starts in `inner` (past a stub in front of it).
    base: u64,
    label: String,
}

impl ZipReader<std::fs::File> {
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            std::fs::File::open(path).map_err(|e| InterchangeError::io(path.display(), e))?;
        Self::new(file, &path.display().to_string())
    }
}

impl<R: Read + Seek> ZipReader<R> {
    /// Read the directory of the archive in `inner` (`label` names it in
    /// errors).
    pub fn new(mut inner: R, label: &str) -> Result<Self> {
        let io = |e| InterchangeError::io(label, e);
        let len = inner.seek(SeekFrom::End(0)).map_err(io)?;
        let tail_len = len.min((END_LEN + 0xFFFF + LOCATOR_LEN) as u64);
        let tail_at = len - tail_len;
        inner.seek(SeekFrom::Start(tail_at)).map_err(io)?;
        let mut tail = vec![0u8; tail_len as usize];
        inner.read_exact(&mut tail).map_err(io)?;
        // The last end record whose comment ends the file (else the last
        // one at all).
        let candidates: Vec<usize> = (0..tail.len().saturating_sub(END_LEN - 1))
            .rev()
            .filter(|&i| u32_at(&tail, i) == Some(END))
            .collect();
        let at = candidates
            .iter()
            .copied()
            .find(|&i| {
                u16_at(&tail, i + 20).is_some_and(|c| i + END_LEN + usize::from(c) == tail.len())
            })
            .or_else(|| candidates.first().copied())
            .ok_or_else(|| bad(format!("{label}: no end of central directory")))?;
        let end = &tail[at..];
        let field = |i| u16_at(end, i).ok_or_else(|| bad(format!("{label}: truncated end")));
        let field32 = |i| u32_at(end, i).ok_or_else(|| bad(format!("{label}: truncated end")));
        let (disk, cd_disk) = (field(4)?, field(6)?);
        let mut count = u64::from(field(10)?);
        let mut cd_size = u64::from(field32(12)?);
        let mut cd_offset = u64::from(field32(16)?);
        let end_pos = tail_at + at as u64;
        let mut directory_end = end_pos;
        // ZIP64: the locator right before the end record.
        if at >= LOCATOR_LEN && u32_at(&tail, at - LOCATOR_LEN) == Some(LOCATOR64) {
            let loc = &tail[at - LOCATOR_LEN..at];
            let end64_at = u64_at(loc, 8).ok_or_else(|| bad("truncated ZIP64 locator"))?;
            let locator_pos = end_pos - LOCATOR_LEN as u64;
            // The record's own place, or (with a stub in front) right
            // before the locator.
            let mut rec = [0u8; 56];
            let mut read_at = |pos: u64| -> Option<[u8; 56]> {
                inner.seek(SeekFrom::Start(pos)).ok()?;
                inner.read_exact(&mut rec).ok()?;
                (u32_at(&rec, 0) == Some(END64)).then_some(rec)
            };
            let r = read_at(end64_at)
                .or_else(|| locator_pos.checked_sub(56).and_then(&mut read_at))
                .ok_or_else(|| bad(format!("{label}: no ZIP64 end record")))?;
            count = u64_at(&r, 32).unwrap_or(count);
            cd_size = u64_at(&r, 40).unwrap_or(cd_size);
            cd_offset = u64_at(&r, 48).unwrap_or(cd_offset);
            directory_end = locator_pos - 56;
        } else if disk != 0 || cd_disk != 0 {
            return Err(bad(format!("{label}: split archives are not read")));
        }
        // Whatever is in front of the archive moves every offset.
        let base = directory_end
            .checked_sub(cd_size)
            .and_then(|d| d.checked_sub(cd_offset))
            .unwrap_or(0);
        if cd_size > len {
            return Err(bad(format!(
                "{label}: the directory is larger than the file"
            )));
        }
        inner.seek(SeekFrom::Start(base + cd_offset)).map_err(io)?;
        let mut cd = vec![0u8; cd_size as usize];
        inner.read_exact(&mut cd).map_err(io)?;
        let mut entries = Vec::with_capacity(count.min(1 << 20) as usize);
        let mut i = 0;
        while i + 46 <= cd.len() && u32_at(&cd, i) == Some(CENTRAL) {
            let get16 = |k| u16_at(&cd, i + k).unwrap_or(0);
            let get32 = |k| u32_at(&cd, i + k).unwrap_or(0);
            let flags = get16(8);
            let method = Method::of(get16(10));
            let crc = get32(16);
            let mut compressed = u64::from(get32(20));
            let mut size = u64::from(get32(24));
            let name_len = usize::from(get16(28));
            let extra_len = usize::from(get16(30));
            let comment_len = usize::from(get16(32));
            let mut offset = u64::from(get32(42));
            let name_at = i + 46;
            let extra_at = name_at + name_len;
            let next = extra_at + extra_len + comment_len;
            if next > cd.len() {
                return Err(bad(format!("{label}: truncated directory entry")));
            }
            let raw_name = &cd[name_at..extra_at];
            let name = normalize_name(&String::from_utf8_lossy(raw_name));
            // ZIP64 extra: the values that overflowed, in order.
            let extra = &cd[extra_at..extra_at + extra_len];
            let mut e = 0;
            while e + 4 <= extra.len() {
                let id = u16_at(extra, e).unwrap_or(0);
                let n = usize::from(u16_at(extra, e + 2).unwrap_or(0));
                let body = extra.get(e + 4..e + 4 + n).unwrap_or(&[]);
                if id == 1 {
                    let mut k = 0;
                    let mut take = |v: &mut u64| {
                        if *v == MAX32
                            && let Some(x) = u64_at(body, k)
                        {
                            *v = x;
                            k += 8;
                        }
                    };
                    take(&mut size);
                    take(&mut compressed);
                    take(&mut offset);
                }
                e += 4 + n;
            }
            entries.push(Entry {
                name,
                method,
                crc,
                compressed,
                size,
                encrypted: flags & 1 != 0,
                offset,
            });
            i = next;
        }
        Ok(Self {
            inner,
            entries,
            base,
            label: label.to_string(),
        })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry called `name`: as written, else with slashes and case
    /// forgiven.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        let n = normalize_name(name);
        self.entries.iter().find(|e| e.name == n).or_else(|| {
            self.entries
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(&n))
        })
    }

    /// The contents of entry `name`.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        let e = self
            .find(name)
            .cloned()
            .ok_or_else(|| bad(format!("{}: no entry {name}", self.label)))?;
        let mut out = Vec::with_capacity(e.size.min(1 << 26) as usize);
        self.copy_entry(&e, &mut out)?;
        Ok(out)
    }

    /// Write entry `name` to `w`; returns its size.
    pub fn copy_to(&mut self, name: &str, w: &mut impl Write) -> Result<u64> {
        let e = self
            .find(name)
            .cloned()
            .ok_or_else(|| bad(format!("{}: no entry {name}", self.label)))?;
        self.copy_entry(&e, w)
    }

    fn copy_entry(&mut self, e: &Entry, w: &mut impl Write) -> Result<u64> {
        let label = format!("{} ({})", self.label, e.name);
        if e.encrypted {
            return Err(bad(format!("{label}: encrypted")));
        }
        let io = |e| InterchangeError::io(&label, e);
        self.inner
            .seek(SeekFrom::Start(self.base + e.offset))
            .map_err(io)?;
        let mut local = [0u8; 30];
        self.inner.read_exact(&mut local).map_err(io)?;
        if u32_at(&local, 0) != Some(LOCAL) {
            return Err(bad(format!("{label}: no local header")));
        }
        let skip =
            u64::from(u16_at(&local, 26).unwrap_or(0)) + u64::from(u16_at(&local, 28).unwrap_or(0));
        self.inner
            .seek(SeekFrom::Current(skip as i64))
            .map_err(io)?;
        let raw = (&mut self.inner).take(e.compressed);
        let mut out = CrcWriter::new(w);
        // One byte more than it says it holds tells a lie (or a bomb).
        match e.method {
            Method::Stored => {
                std::io::copy(&mut raw.take(e.size + 1), &mut out).map_err(io)?;
            }
            Method::Deflated => {
                let mut d = flate2::read::DeflateDecoder::new(raw);
                std::io::copy(&mut (&mut d).take(e.size + 1), &mut out).map_err(io)?;
            }
            Method::Other(m) => {
                return Err(bad(format!("{label}: compression method {m} is not read")));
            }
        }
        if out.count != e.size {
            return Err(bad(format!(
                "{label}: {} bytes where the directory says {}",
                out.count, e.size
            )));
        }
        if out.crc.sum() != e.crc {
            return Err(bad(format!("{label}: CRC mismatch")));
        }
        Ok(out.count)
    }
}

/// Counts and checksums what passes through.
struct CrcWriter<W> {
    inner: W,
    crc: Crc,
    count: u64,
}

impl<W: Write> CrcWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            crc: Crc::new(),
            count: 0,
        }
    }
}

impl<W: Write> Write for CrcWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.crc.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// An entry written.
struct Written {
    name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    offset: u64,
}

/// MS-DOS time and date of `secs` since 1970 (UTC; 1980 at the earliest).
fn dos_time(secs: u64) -> (u16, u16) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    if year < 1980 {
        return (0, (1 << 5) | 1);
    }
    let time = ((rem / 3600) << 11) | (((rem % 3600) / 60) << 5) | ((rem % 60) / 2);
    let date = (((year - 1980).min(127) as u64) << 9) | ((month as u64) << 5) | day as u64;
    (time as u16, date as u16)
}

/// Writes a zip archive entry by entry; [`ZipWriter::finish`] writes the
/// directory.
pub struct ZipWriter<W: Write + Seek> {
    out: W,
    entries: Vec<Written>,
    time: (u16, u16),
    label: String,
}

impl ZipWriter<std::fs::File> {
    /// A new archive at `path` (replacing a file there).
    pub fn create(path: &Path) -> Result<Self> {
        let file =
            std::fs::File::create(path).map_err(|e| InterchangeError::io(path.display(), e))?;
        Ok(Self::new(file, &path.display().to_string()))
    }
}

impl<W: Write + Seek> ZipWriter<W> {
    pub fn new(out: W, label: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self {
            out,
            entries: Vec::new(),
            time: dos_time(now),
            label: label.to_string(),
        }
    }

    fn io(&self) -> impl Fn(std::io::Error) -> InterchangeError + '_ {
        |e| InterchangeError::io(&self.label, e)
    }

    fn check_name(&self, name: &str) -> Result<String> {
        let n = normalize_name(name);
        if n.is_empty() || n.len() > 0xFFFF {
            return Err(bad(format!("{}: bad entry name {name:?}", self.label)));
        }
        if self.entries.iter().any(|e| e.name == n) {
            return Err(bad(format!("{}: {n} written twice", self.label)));
        }
        Ok(n)
    }

    fn local_header(
        &mut self,
        name: &str,
        method: u16,
        crc: u32,
        compressed: u64,
        size: u64,
    ) -> Result<u64> {
        let offset = self.out.stream_position().map_err(self.io())?;
        let zip64 = compressed >= MAX32 || size >= MAX32;
        let mut h = Vec::with_capacity(30 + name.len() + 20);
        h.extend_from_slice(&LOCAL.to_le_bytes());
        h.extend_from_slice(&(if zip64 { 45u16 } else { 20 }).to_le_bytes());
        h.extend_from_slice(&UTF8.to_le_bytes());
        h.extend_from_slice(&method.to_le_bytes());
        h.extend_from_slice(&self.time.0.to_le_bytes());
        h.extend_from_slice(&self.time.1.to_le_bytes());
        h.extend_from_slice(&crc.to_le_bytes());
        let (c32, s32) = if zip64 {
            (MAX32 as u32, MAX32 as u32)
        } else {
            (compressed as u32, size as u32)
        };
        h.extend_from_slice(&c32.to_le_bytes());
        h.extend_from_slice(&s32.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&(if zip64 { 20u16 } else { 0 }).to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        if zip64 {
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(&size.to_le_bytes());
            h.extend_from_slice(&compressed.to_le_bytes());
        }
        self.out.write_all(&h).map_err(self.io())?;
        Ok(offset)
    }

    /// Add `data` as `name`, deflated when `compress` (and it gets
    /// smaller).
    pub fn add(&mut self, name: &str, data: &[u8], compress: bool) -> Result<()> {
        let name = self.check_name(name)?;
        let mut crc = Crc::new();
        crc.update(data);
        let packed = if compress {
            let mut enc =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(data).map_err(self.io())?;
            Some(enc.finish().map_err(self.io())?).filter(|p| p.len() < data.len())
        } else {
            None
        };
        let (method, body) = match &packed {
            Some(p) => (8u16, p.as_slice()),
            None => (0u16, data),
        };
        let offset = self.local_header(
            &name,
            method,
            crc.sum(),
            body.len() as u64,
            data.len() as u64,
        )?;
        self.out.write_all(body).map_err(self.io())?;
        self.entries.push(Written {
            name,
            method,
            crc: crc.sum(),
            compressed: body.len() as u64,
            size: data.len() as u64,
            offset,
        });
        Ok(())
    }

    /// Add `len` bytes read from `from` as `name`, stored (media); the
    /// CRC is written into the local header afterwards.
    pub fn add_stored(&mut self, name: &str, from: &mut impl Read, len: u64) -> Result<()> {
        let name = self.check_name(name)?;
        let offset = self.local_header(&name, 0, 0, len, len)?;
        let label = self.label.clone();
        let mut w = CrcWriter::new(&mut self.out);
        std::io::copy(&mut from.take(len), &mut w).map_err(|e| InterchangeError::io(&label, e))?;
        let (crc, count) = (w.crc.sum(), w.count);
        if count != len {
            return Err(bad(format!(
                "{label}: {name} ended after {count} of {len} bytes"
            )));
        }
        let end = self.out.stream_position().map_err(self.io())?;
        self.out
            .seek(SeekFrom::Start(offset + 14))
            .map_err(self.io())?;
        self.out.write_all(&crc.to_le_bytes()).map_err(self.io())?;
        self.out.seek(SeekFrom::Start(end)).map_err(self.io())?;
        self.entries.push(Written {
            name,
            method: 0,
            crc,
            compressed: len,
            size: len,
            offset,
        });
        Ok(())
    }

    /// Add the file at `path` as `name`, stored.
    pub fn add_file(&mut self, name: &str, path: &Path) -> Result<()> {
        let io = |e| InterchangeError::io(path.display(), e);
        let mut f = std::fs::File::open(path).map_err(io)?;
        let len = f.metadata().map_err(io)?.len();
        self.add_stored(name, &mut f, len)
    }

    /// Write the central directory; returns the output.
    pub fn finish(mut self) -> Result<W> {
        let cd_offset = self.out.stream_position().map_err(self.io())?;
        let mut cd = Vec::new();
        let mut any64 = false;
        for e in &self.entries {
            let big = [e.size >= MAX32, e.compressed >= MAX32, e.offset >= MAX32];
            let zip64 = big.iter().any(|b| *b);
            any64 |= zip64;
            let mut extra = Vec::new();
            if zip64 {
                let mut body = Vec::new();
                if big[0] {
                    body.extend_from_slice(&e.size.to_le_bytes());
                }
                if big[1] {
                    body.extend_from_slice(&e.compressed.to_le_bytes());
                }
                if big[2] {
                    body.extend_from_slice(&e.offset.to_le_bytes());
                }
                extra.extend_from_slice(&1u16.to_le_bytes());
                extra.extend_from_slice(&(body.len() as u16).to_le_bytes());
                extra.extend_from_slice(&body);
            }
            let clip = |v: u64, b: bool| if b { MAX32 as u32 } else { v as u32 };
            cd.extend_from_slice(&CENTRAL.to_le_bytes());
            // Made by: Unix, 4.5 (so permissions below count).
            cd.extend_from_slice(&0x032Du16.to_le_bytes());
            cd.extend_from_slice(&(if zip64 { 45u16 } else { 20 }).to_le_bytes());
            cd.extend_from_slice(&UTF8.to_le_bytes());
            cd.extend_from_slice(&e.method.to_le_bytes());
            cd.extend_from_slice(&self.time.0.to_le_bytes());
            cd.extend_from_slice(&self.time.1.to_le_bytes());
            cd.extend_from_slice(&e.crc.to_le_bytes());
            cd.extend_from_slice(&clip(e.compressed, big[1]).to_le_bytes());
            cd.extend_from_slice(&clip(e.size, big[0]).to_le_bytes());
            cd.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment
            cd.extend_from_slice(&0u16.to_le_bytes()); // disk
            cd.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            cd.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
            cd.extend_from_slice(&clip(e.offset, big[2]).to_le_bytes());
            cd.extend_from_slice(e.name.as_bytes());
            cd.extend_from_slice(&extra);
        }
        self.out.write_all(&cd).map_err(self.io())?;
        let cd_size = cd.len() as u64;
        let count = self.entries.len() as u64;
        let zip64 = any64 || count > 0xFFFE || cd_offset >= MAX32 || cd_size >= MAX32;
        let mut end = Vec::new();
        if zip64 {
            let end64_at = cd_offset + cd_size;
            end.extend_from_slice(&END64.to_le_bytes());
            end.extend_from_slice(&44u64.to_le_bytes());
            end.extend_from_slice(&0x032Du16.to_le_bytes());
            end.extend_from_slice(&45u16.to_le_bytes());
            end.extend_from_slice(&0u32.to_le_bytes());
            end.extend_from_slice(&0u32.to_le_bytes());
            end.extend_from_slice(&count.to_le_bytes());
            end.extend_from_slice(&count.to_le_bytes());
            end.extend_from_slice(&cd_size.to_le_bytes());
            end.extend_from_slice(&cd_offset.to_le_bytes());
            end.extend_from_slice(&LOCATOR64.to_le_bytes());
            end.extend_from_slice(&0u32.to_le_bytes());
            end.extend_from_slice(&end64_at.to_le_bytes());
            end.extend_from_slice(&1u32.to_le_bytes());
        }
        let n16 = if zip64 { 0xFFFF } else { count as u16 };
        end.extend_from_slice(&END.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&n16.to_le_bytes());
        end.extend_from_slice(&n16.to_le_bytes());
        end.extend_from_slice(&(if zip64 { MAX32 as u32 } else { cd_size as u32 }).to_le_bytes());
        end.extend_from_slice(
            &(if zip64 {
                MAX32 as u32
            } else {
                cd_offset as u32
            })
            .to_le_bytes(),
        );
        end.extend_from_slice(&0u16.to_le_bytes());
        self.out.write_all(&end).map_err(self.io())?;
        self.out.flush().map_err(self.io())?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn entries_round_trip_stored_and_deflated() {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()), "test");
        let text = "<Project>".repeat(500);
        w.add("project.xml", text.as_bytes(), true).unwrap();
        w.add("tiny.txt", b"x", true).unwrap();
        let audio: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        w.add_stored("audio/a b.wav", &mut audio.as_slice(), audio.len() as u64)
            .unwrap();
        assert!(w.add("project.xml", b"again", false).is_err());
        let bytes = w.finish().unwrap().into_inner();
        let mut r = ZipReader::new(Cursor::new(bytes.clone()), "test").unwrap();
        assert_eq!(r.entries().len(), 3);
        assert_eq!(r.find("project.xml").unwrap().method, Method::Deflated);
        assert_eq!(
            r.find("tiny.txt").unwrap().method,
            Method::Stored,
            "deflate grew it"
        );
        assert_eq!(r.read("project.xml").unwrap(), text.as_bytes());
        assert_eq!(
            r.read("./audio\\A B.WAV").unwrap(),
            audio,
            "names are forgiven"
        );
        let mut out = Vec::new();
        assert_eq!(r.copy_to("tiny.txt", &mut out).unwrap(), 1);
        // A stub in front of it (a self-extracting archive) moves nothing.
        let mut stubbed = b"#!/bin/sh\nexit 0\n".to_vec();
        stubbed.extend_from_slice(&bytes);
        let mut r = ZipReader::new(Cursor::new(stubbed), "stub").unwrap();
        assert_eq!(r.read("tiny.txt").unwrap(), b"x");
        // A damaged byte is caught by the CRC.
        let mut broken = bytes.clone();
        let at = bytes.windows(4).position(|w| w == [7, 14, 21, 28]).unwrap();
        broken[at] ^= 0xFF;
        let mut r = ZipReader::new(Cursor::new(broken), "broken").unwrap();
        assert!(r.read("audio/a b.wav").is_err());
        assert!(ZipReader::new(Cursor::new(b"not a zip".to_vec()), "x").is_err());
    }

    #[test]
    fn the_archive_unzips_with_other_tools() {
        let Ok(out) = std::process::Command::new("unzip").arg("-v").output() else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ff-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.zip");
        let mut w = ZipWriter::create(&path).unwrap();
        w.add("a/b.txt", b"hello hello hello hello", true).unwrap();
        w.add_stored("c.bin", &mut &[1u8, 2, 3][..], 3).unwrap();
        w.finish().unwrap();
        let test = std::process::Command::new("unzip")
            .arg("-t")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            test.status.success(),
            "{}",
            String::from_utf8_lossy(&test.stdout)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip64_records_are_written_and_read() {
        // Many entries force ZIP64's end record.
        let mut w = ZipWriter::new(Cursor::new(Vec::new()), "many");
        for i in 0..70_000u32 {
            w.add(&format!("{i}"), &i.to_le_bytes(), false).unwrap();
        }
        let bytes = w.finish().unwrap().into_inner();
        assert!(bytes.windows(4).any(|w| w == END64.to_le_bytes()));
        let mut r = ZipReader::new(Cursor::new(bytes), "many").unwrap();
        assert_eq!(r.entries().len(), 70_000);
        assert_eq!(r.read("69999").unwrap(), 69_999u32.to_le_bytes());
    }

    #[test]
    fn dos_dates_count_from_1980() {
        // 2026-11-03 21:30:10 UTC.
        let (time, date) = dos_time(1_793_741_410);
        assert_eq!(date >> 9, 46);
        assert_eq!((date >> 5) & 15, 11);
        assert_eq!(date & 31, 3);
        assert_eq!(time >> 11, 21);
        assert_eq!((time >> 5) & 63, 30);
        assert_eq!(time & 31, 5);
        assert_eq!(dos_time(0), (0, 33));
    }
}

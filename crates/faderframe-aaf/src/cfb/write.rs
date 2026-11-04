//! Writing a compound file: streams go to disk as they are added (small
//! ones into the mini stream, kept until the end), the mini stream, its
//! FAT, the directory, the FAT and the DIFAT after them, the header last.

use super::{
    CfbError, DIFSECT, DIR_ENTRY, ENDOFCHAIN, FATSECT, FREESECT, HEADER_DIFAT, Kind, MINI_CUTOFF,
    MINI_SECTOR, NOSTREAM, SIGNATURE, SectorSize, compare_names,
};
use std::io::{self, Seek, SeekFrom, Write};

/// A storage or stream of the file being written.
pub type EntryId = usize;

struct Entry {
    name: String,
    kind: Kind,
    clsid: [u8; 16],
    children: Vec<EntryId>,
    start: u32,
    size: u64,
}

/// A compound file being written.
pub struct CfbWriter<W: Write + Seek> {
    out: W,
    shift: u32,
    /// The next sector to write (the file's end is just before it).
    next: u32,
    fat: Vec<u32>,
    entries: Vec<Entry>,
    mini: Vec<u8>,
    minifat: Vec<u32>,
}

impl<W: Write + Seek> CfbWriter<W> {
    /// Start a file in `out` (the header is written at the end).
    pub fn new(mut out: W, size: SectorSize) -> Result<Self, CfbError> {
        let shift = size.shift();
        out.seek(SeekFrom::Start(0))?;
        // The header's sector, written for real in `finish`.
        out.write_all(&vec![0u8; 1 << shift])?;
        Ok(CfbWriter {
            out,
            shift,
            next: 0,
            fat: Vec::new(),
            entries: vec![Entry {
                name: "Root Entry".into(),
                kind: Kind::Root,
                clsid: [0; 16],
                children: Vec::new(),
                start: ENDOFCHAIN,
                size: 0,
            }],
            mini: Vec::new(),
            minifat: Vec::new(),
        })
    }

    pub fn root(&self) -> EntryId {
        0
    }

    fn sector_size(&self) -> usize {
        1 << self.shift
    }

    /// The class id of a storage (or the root).
    pub fn set_clsid(&mut self, entry: EntryId, clsid: [u8; 16]) {
        if let Some(e) = self.entries.get_mut(entry) {
            e.clsid = clsid;
        }
    }

    fn add(&mut self, parent: EntryId, name: &str, kind: Kind) -> Result<EntryId, CfbError> {
        if name.is_empty()
            || name.encode_utf16().count() > 31
            || name.contains(['/', '\\', ':', '!'])
        {
            return Err(CfbError::BadName(name.into()));
        }
        let Some(p) = self.entries.get(parent) else {
            return Err(CfbError::BadName(name.into()));
        };
        if p.kind == Kind::Stream {
            return Err(CfbError::BadName(name.into()));
        }
        if p.children
            .iter()
            .any(|&c| compare_names(&self.entries[c].name, name) == std::cmp::Ordering::Equal)
        {
            return Err(CfbError::Duplicate(name.into()));
        }
        let id = self.entries.len();
        self.entries.push(Entry {
            name: name.into(),
            kind,
            clsid: [0; 16],
            children: Vec::new(),
            start: ENDOFCHAIN,
            size: 0,
        });
        self.entries[parent].children.push(id);
        Ok(id)
    }

    /// A storage (directory) under `parent`.
    pub fn storage(
        &mut self,
        parent: EntryId,
        name: &str,
        clsid: [u8; 16],
    ) -> Result<EntryId, CfbError> {
        let id = self.add(parent, name, Kind::Storage)?;
        self.entries[id].clsid = clsid;
        Ok(id)
    }

    /// A stream under `parent` holding `data`.
    pub fn stream(
        &mut self,
        parent: EntryId,
        name: &str,
        data: &[u8],
    ) -> Result<EntryId, CfbError> {
        self.stream_with(parent, name, data.len() as u64, &mut |w| w.write_all(data))
    }

    /// A stream of `len` bytes that `fill` writes. Streams of 4096 bytes and
    /// more go straight to the file.
    pub fn stream_with(
        &mut self,
        parent: EntryId,
        name: &str,
        len: u64,
        fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
    ) -> Result<EntryId, CfbError> {
        let id = self.add(parent, name, Kind::Stream)?;
        if len < MINI_CUTOFF {
            let mut data = Vec::with_capacity(len as usize);
            fill(&mut data)?;
            if data.len() as u64 != len {
                return Err(CfbError::WrongLength {
                    expected: len,
                    given: data.len() as u64,
                });
            }
            let e = &mut self.entries[id];
            e.size = len;
            if len == 0 {
                e.start = ENDOFCHAIN;
                return Ok(id);
            }
            let first = (self.mini.len() / MINI_SECTOR) as u32;
            let count = (data.len()).div_ceil(MINI_SECTOR) as u32;
            self.mini.extend_from_slice(&data);
            self.mini.resize((first + count) as usize * MINI_SECTOR, 0);
            for k in 0..count {
                self.minifat.push(if k + 1 == count {
                    ENDOFCHAIN
                } else {
                    first + k + 1
                });
            }
            e.start = first;
            return Ok(id);
        }
        let start = self.next;
        let mut counting = Counting {
            out: &mut self.out,
            written: 0,
        };
        fill(&mut counting)?;
        let written = counting.written;
        if written != len {
            return Err(CfbError::WrongLength {
                expected: len,
                given: written,
            });
        }
        let count = self.pad_and_chain(len)?;
        let e = &mut self.entries[id];
        e.start = if count == 0 { ENDOFCHAIN } else { start };
        e.size = len;
        Ok(id)
    }

    /// Pad the bytes just written to whole sectors and chain them in the
    /// FAT; how many sectors they took.
    fn pad_and_chain(&mut self, len: u64) -> Result<u32, CfbError> {
        let size = self.sector_size() as u64;
        let count = len.div_ceil(size);
        let pad = count * size - len;
        if pad > 0 {
            self.out.write_all(&vec![0u8; pad as usize])?;
        }
        let count = u32::try_from(count).map_err(|_| CfbError::Damaged("the file is too large"))?;
        let start = self.next;
        for k in 0..count {
            self.fat.push(if k + 1 == count {
                ENDOFCHAIN
            } else {
                start + k + 1
            });
        }
        self.next += count;
        Ok(count)
    }

    /// Write bytes as whole new sectors chained together; their first
    /// sector (ENDOFCHAIN when there are none).
    fn write_chain(&mut self, data: &[u8]) -> Result<u32, CfbError> {
        if data.is_empty() {
            return Ok(ENDOFCHAIN);
        }
        let start = self.next;
        self.out.write_all(data)?;
        self.pad_and_chain(data.len() as u64)?;
        Ok(start)
    }

    /// The red-black tree of `parent`'s children: their left/right links
    /// and colours, and the root of the tree.
    fn tree(&self, parent: EntryId, links: &mut [(u32, u32, bool)]) -> u32 {
        let mut kids = self.entries[parent].children.clone();
        kids.sort_by(|&a, &b| compare_names(&self.entries[a].name, &self.entries[b].name));
        // A balanced tree from the sorted names: every path to a leaf is
        // as long or one longer, so the deepest level red and the rest
        // black is a valid red-black tree.
        fn build(
            kids: &[EntryId],
            depth: u32,
            links: &mut [(u32, u32, bool)],
            depths: &mut Vec<(EntryId, u32)>,
        ) -> u32 {
            if kids.is_empty() {
                return super::NOSTREAM;
            }
            let mid = kids.len() / 2;
            let left = build(&kids[..mid], depth + 1, links, depths);
            let right = build(&kids[mid + 1..], depth + 1, links, depths);
            links[kids[mid]].0 = left;
            links[kids[mid]].1 = right;
            depths.push((kids[mid], depth));
            kids[mid] as u32
        }
        let mut depths = Vec::new();
        let root = build(&kids, 0, links, &mut depths);
        let deepest = depths.iter().map(|d| d.1).max().unwrap_or(0);
        for (e, d) in depths {
            // true = black
            links[e].2 = !(d == deepest && deepest > 0);
        }
        root
    }

    /// Write everything that is left and the header; the output back.
    pub fn finish(mut self) -> Result<W, CfbError> {
        let size = self.sector_size();
        let per = size / 4;
        // The mini stream, held by the root entry.
        let mini = std::mem::take(&mut self.mini);
        let mini_start = self.write_chain(&mini)?;
        self.entries[0].start = mini_start;
        self.entries[0].size = mini.len() as u64;
        // Its FAT.
        let mut minifat_bytes = Vec::with_capacity(self.minifat.len() * 4);
        for v in &self.minifat {
            minifat_bytes.extend_from_slice(&v.to_le_bytes());
        }
        while !minifat_bytes.is_empty() && minifat_bytes.len() % size != 0 {
            minifat_bytes.extend_from_slice(&FREESECT.to_le_bytes());
        }
        let minifat_sectors = (minifat_bytes.len() / size) as u32;
        let minifat_start = self.write_chain(&minifat_bytes)?;
        // The directory.
        let mut links = vec![(NOSTREAM, NOSTREAM, true); self.entries.len()];
        let mut child = vec![NOSTREAM; self.entries.len()];
        let storages: Vec<EntryId> = (0..self.entries.len())
            .filter(|&e| self.entries[e].kind != Kind::Stream)
            .collect();
        for e in storages {
            child[e] = self.tree(e, &mut links);
        }
        let mut dir = Vec::with_capacity(self.entries.len() * DIR_ENTRY);
        for (i, e) in self.entries.iter().enumerate() {
            let mut b = [0u8; DIR_ENTRY];
            let name: Vec<u16> = e.name.encode_utf16().collect();
            for (k, c) in name.iter().enumerate() {
                b[2 * k..2 * k + 2].copy_from_slice(&c.to_le_bytes());
            }
            b[64..66].copy_from_slice(&(((name.len() + 1) * 2) as u16).to_le_bytes());
            b[66] = e.kind.code();
            b[67] = if i == 0 || links[i].2 { 1 } else { 0 };
            b[68..72].copy_from_slice(&links[i].0.to_le_bytes());
            b[72..76].copy_from_slice(&links[i].1.to_le_bytes());
            b[76..80].copy_from_slice(&child[i].to_le_bytes());
            b[80..96].copy_from_slice(&e.clsid);
            let start = if e.kind != Kind::Storage { e.start } else { 0 };
            b[116..120].copy_from_slice(&start.to_le_bytes());
            let size = if e.kind != Kind::Storage { e.size } else { 0 };
            b[120..128].copy_from_slice(&size.to_le_bytes());
            dir.extend_from_slice(&b);
        }
        while dir.len() % size != 0 {
            let mut b = [0u8; DIR_ENTRY];
            b[68..80].copy_from_slice(&[0xff; 12]);
            dir.extend_from_slice(&b);
        }
        let dir_sectors = (dir.len() / size) as u32;
        let dir_start = self.write_chain(&dir)?;
        // The FAT, counting its own sectors and the DIFAT's.
        let used = self.next as usize;
        let mut fat_count = used.div_ceil(per).max(1);
        let difat_count = loop {
            let difat = if fat_count > HEADER_DIFAT {
                (fat_count - HEADER_DIFAT).div_ceil(per - 1)
            } else {
                0
            };
            if fat_count * per >= used + fat_count + difat {
                break difat;
            }
            fat_count += 1;
        };
        let fat_first = self.next;
        for _ in 0..fat_count {
            self.fat.push(FATSECT);
        }
        let difat_first = fat_first + fat_count as u32;
        for _ in 0..difat_count {
            self.fat.push(DIFSECT);
        }
        self.fat.resize(fat_count * per, FREESECT);
        let mut bytes = Vec::with_capacity(fat_count * size);
        for v in &self.fat {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.out.write_all(&bytes)?;
        let fat_sectors: Vec<u32> = (0..fat_count as u32).map(|k| fat_first + k).collect();
        // The DIFAT beyond the header's 109 places.
        let rest = fat_sectors.get(HEADER_DIFAT..).unwrap_or(&[]);
        for k in 0..difat_count {
            let mut words = vec![FREESECT; per];
            for (j, s) in rest.iter().skip(k * (per - 1)).take(per - 1).enumerate() {
                words[j] = *s;
            }
            words[per - 1] = if k + 1 == difat_count {
                ENDOFCHAIN
            } else {
                difat_first + k as u32 + 1
            };
            let mut b = Vec::with_capacity(size);
            for w in words {
                b.extend_from_slice(&w.to_le_bytes());
            }
            self.out.write_all(&b)?;
        }
        // The header.
        let mut h = vec![0u8; size];
        h[..8].copy_from_slice(&SIGNATURE);
        h[24..26].copy_from_slice(&0x003eu16.to_le_bytes());
        let major: u16 = if self.shift == 12 { 4 } else { 3 };
        h[26..28].copy_from_slice(&major.to_le_bytes());
        h[28..30].copy_from_slice(&0xfffeu16.to_le_bytes());
        h[30..32].copy_from_slice(&(self.shift as u16).to_le_bytes());
        h[32..34].copy_from_slice(&6u16.to_le_bytes());
        let dir_count = if major == 4 { dir_sectors } else { 0 };
        h[40..44].copy_from_slice(&dir_count.to_le_bytes());
        h[44..48].copy_from_slice(&(fat_count as u32).to_le_bytes());
        h[48..52].copy_from_slice(&dir_start.to_le_bytes());
        h[56..60].copy_from_slice(&(MINI_CUTOFF as u32).to_le_bytes());
        h[60..64].copy_from_slice(&minifat_start.to_le_bytes());
        h[64..68].copy_from_slice(&minifat_sectors.to_le_bytes());
        let difat_start = if difat_count > 0 {
            difat_first
        } else {
            ENDOFCHAIN
        };
        h[68..72].copy_from_slice(&difat_start.to_le_bytes());
        h[72..76].copy_from_slice(&(difat_count as u32).to_le_bytes());
        for k in 0..HEADER_DIFAT {
            let v = fat_sectors.get(k).copied().unwrap_or(FREESECT);
            h[76 + 4 * k..80 + 4 * k].copy_from_slice(&v.to_le_bytes());
        }
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&h)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Counts what passes through to the file.
struct Counting<'a, W: Write> {
    out: &'a mut W,
    written: u64,
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

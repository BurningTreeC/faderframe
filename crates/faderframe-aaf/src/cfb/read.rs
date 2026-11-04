//! Reading a compound file: the header, the FAT through the DIFAT, the
//! directory as a tree, small streams from the mini stream, large ones
//! sector run by sector run.

use super::{
    CfbError, DIR_ENTRY, ENDOFCHAIN, FREESECT, HEADER_DIFAT, Kind, MAXREGSECT, MINI_SECTOR,
    NOSTREAM, SIGNATURE,
};
use std::io::{Read, Seek, SeekFrom, Write};

/// A storage or stream of a file being read.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub clsid: [u8; 16],
    pub start: u32,
    pub size: u64,
    /// Children of a storage, in the order the tree keeps them.
    pub children: Vec<usize>,
}

/// An open compound file.
pub struct Cfb<R: Read + Seek> {
    file: R,
    shift: u32,
    cutoff: u64,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    /// The mini stream (small streams' 64-byte sectors), read whole.
    mini: Vec<u8>,
    entries: Vec<Entry>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl<R: Read + Seek> Cfb<R> {
    pub fn open(mut file: R) -> Result<Self, CfbError> {
        let mut h = [0u8; 512];
        file.seek(SeekFrom::Start(0))?;
        if file.read_exact(&mut h).is_err() || h[..8] != SIGNATURE {
            return Err(CfbError::NotCompound);
        }
        let major = u16_at(&h, 26);
        let shift = u32::from(u16_at(&h, 30));
        if !matches!((major, shift), (3, 9) | (4, 12)) || u16_at(&h, 32) != 6 {
            return Err(CfbError::Damaged("an unknown version or sector size"));
        }
        let fat_count = u32_at(&h, 44) as usize;
        let dir_start = u32_at(&h, 48);
        let cutoff = u64::from(u32_at(&h, 56));
        let minifat_start = u32_at(&h, 60);
        let mut difat_next = u32_at(&h, 68);
        let difat_count = u32_at(&h, 72) as usize;
        let mut cfb = Cfb {
            file,
            shift,
            cutoff,
            fat: Vec::new(),
            minifat: Vec::new(),
            mini: Vec::new(),
            entries: Vec::new(),
        };
        let per = cfb.sector_size() / 4;
        // FAT sector locations: 109 in the header, the rest in the DIFAT.
        let mut fat_sectors: Vec<u32> = (0..HEADER_DIFAT.min(fat_count))
            .map(|k| u32_at(&h, 76 + 4 * k))
            .collect();
        let mut seen = 0;
        while fat_sectors.len() < fat_count && difat_next <= MAXREGSECT {
            seen += 1;
            if seen > difat_count + 1 {
                return Err(CfbError::Damaged("the DIFAT loops"));
            }
            let b = cfb.read_sector(difat_next)?;
            for k in 0..per - 1 {
                if fat_sectors.len() < fat_count {
                    fat_sectors.push(u32_at(&b, 4 * k));
                }
            }
            difat_next = u32_at(&b, 4 * (per - 1));
        }
        if fat_sectors.len() < fat_count {
            return Err(CfbError::Damaged("FAT sectors are missing"));
        }
        let mut fat = Vec::with_capacity(fat_count * per);
        for s in fat_sectors {
            let b = cfb.read_sector(s)?;
            fat.extend((0..per).map(|k| u32_at(&b, 4 * k)));
        }
        cfb.fat = fat;
        // The directory.
        let dir = cfb.read_chain_bytes(dir_start, None)?;
        let mut raw = Vec::new();
        for b in dir.as_chunks::<DIR_ENTRY>().0 {
            let len = (u16_at(b, 64) as usize).min(64);
            let units: Vec<u16> = (0..len / 2)
                .map(|k| u16_at(b, 2 * k))
                .take_while(|&c| c != 0)
                .collect();
            let kind = Kind::of(b[66]);
            let mut size = u64::from_le_bytes(b[120..128].try_into().unwrap_or([0; 8]));
            if major == 3 {
                size &= 0xffff_ffff;
            }
            raw.push((
                Entry {
                    name: String::from_utf16_lossy(&units),
                    kind: kind.unwrap_or(Kind::Stream),
                    clsid: b[80..96].try_into().unwrap_or([0; 16]),
                    start: u32_at(b, 116),
                    size,
                    children: Vec::new(),
                },
                kind.is_some(),
                u32_at(b, 68),
                u32_at(b, 72),
                u32_at(b, 76),
            ));
        }
        if raw.first().is_none_or(|r| r.0.kind != Kind::Root) {
            return Err(CfbError::Damaged("no root entry"));
        }
        // Walk each storage's tree in order.
        let n = raw.len();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut visited = vec![false; n];
        visited[0] = true;
        let mut storages = vec![0usize];
        while let Some(s) = storages.pop() {
            let mut stack = Vec::new();
            let mut node = raw[s].4;
            let mut list = Vec::new();
            loop {
                while node != NOSTREAM && (node as usize) < n {
                    let i = node as usize;
                    if visited[i] {
                        return Err(CfbError::Damaged("the directory loops"));
                    }
                    visited[i] = true;
                    stack.push(i);
                    node = raw[i].2;
                }
                let Some(i) = stack.pop() else { break };
                if raw[i].1 {
                    list.push(i);
                    if raw[i].0.kind != Kind::Stream {
                        storages.push(i);
                    }
                }
                node = raw[i].3;
            }
            children[s] = list;
        }
        cfb.entries = raw
            .into_iter()
            .zip(children)
            .map(|(r, c)| Entry { children: c, ..r.0 })
            .collect();
        // The mini stream and its FAT.
        let root = cfb.entries[0].clone();
        if root.size > 0 {
            cfb.mini = cfb.read_chain_bytes(root.start, Some(root.size))?;
        }
        if minifat_start <= MAXREGSECT {
            let b = cfb.read_chain_bytes(minifat_start, None)?;
            cfb.minifat = b
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect();
        }
        Ok(cfb)
    }

    fn sector_size(&self) -> usize {
        1 << self.shift
    }

    fn read_sector(&mut self, s: u32) -> Result<Vec<u8>, CfbError> {
        if s > MAXREGSECT {
            return Err(CfbError::Damaged("a chain points nowhere"));
        }
        let mut b = vec![0u8; self.sector_size()];
        self.file
            .seek(SeekFrom::Start((u64::from(s) + 1) << self.shift))?;
        self.file.read_exact(&mut b)?;
        Ok(b)
    }

    /// The sectors of the chain from `start`.
    fn chain(&self, start: u32) -> Result<Vec<u32>, CfbError> {
        let mut out = Vec::new();
        let mut s = start;
        while s != ENDOFCHAIN && s != FREESECT {
            if s > MAXREGSECT || out.len() > self.fat.len() {
                return Err(CfbError::Damaged("a broken sector chain"));
            }
            out.push(s);
            s = *self
                .fat
                .get(s as usize)
                .ok_or(CfbError::Damaged("a chain runs past the FAT"))?;
        }
        Ok(out)
    }

    fn read_chain_bytes(&mut self, start: u32, len: Option<u64>) -> Result<Vec<u8>, CfbError> {
        let mut out = Vec::new();
        self.copy_chain(start, len, &mut out, &mut |_| {})?;
        Ok(out)
    }

    /// Copy the chain from `start` (`len` bytes of it, else all) into
    /// `to`, run of consecutive sectors by run.
    fn copy_chain(
        &mut self,
        start: u32,
        len: Option<u64>,
        to: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), CfbError> {
        let chain = self.chain(start)?;
        let size = self.sector_size() as u64;
        let mut left = len.unwrap_or(chain.len() as u64 * size);
        if left > chain.len() as u64 * size {
            return Err(CfbError::Damaged("a stream is longer than its sectors"));
        }
        let mut i = 0;
        let mut buf = Vec::new();
        let mut done = 0u64;
        while left > 0 && i < chain.len() {
            let mut run = 1;
            while i + run < chain.len() && chain[i + run] == chain[i] + run as u32 && run < 256 {
                run += 1;
            }
            let bytes = (run as u64 * size).min(left);
            buf.resize(bytes as usize, 0);
            self.file
                .seek(SeekFrom::Start((u64::from(chain[i]) + 1) << self.shift))?;
            self.file.read_exact(&mut buf)?;
            to.write_all(&buf)?;
            left -= bytes;
            done += bytes;
            progress(done);
            i += run;
        }
        Ok(())
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, id: usize) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// The child of `parent` called `name`.
    pub fn child(&self, parent: usize, name: &str) -> Option<usize> {
        self.entries
            .get(parent)?
            .children
            .iter()
            .copied()
            .find(|&c| {
                super::compare_names(&self.entries[c].name, name) == std::cmp::Ordering::Equal
            })
    }

    /// A stream's bytes (for the small streams of an AAF file's objects).
    pub fn read(&mut self, id: usize) -> Result<Vec<u8>, CfbError> {
        let mut out = Vec::new();
        self.copy_to(id, &mut out, &mut |_| {})?;
        Ok(out)
    }

    /// Copy a stream into `to`; `progress` hears the bytes copied so far.
    pub fn copy_to(
        &mut self,
        id: usize,
        to: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), CfbError> {
        let e = self
            .entries
            .get(id)
            .ok_or(CfbError::Damaged("no such entry"))?
            .clone();
        if e.kind != Kind::Stream || e.size == 0 {
            return Ok(());
        }
        if e.size >= self.cutoff {
            return self.copy_chain(e.start, Some(e.size), to, progress);
        }
        // From the mini stream.
        let mut left = e.size as usize;
        let mut s = e.start;
        let mut steps = 0;
        while left > 0 {
            if s > MAXREGSECT || steps > self.minifat.len() {
                return Err(CfbError::Damaged("a broken mini stream chain"));
            }
            steps += 1;
            let at = s as usize * MINI_SECTOR;
            let n = left.min(MINI_SECTOR);
            let b = self
                .mini
                .get(at..at + n)
                .ok_or(CfbError::Damaged("the mini stream is short"))?;
            to.write_all(b)?;
            left -= n;
            progress((e.size as usize - left) as u64);
            s = *self
                .minifat
                .get(s as usize)
                .ok_or(CfbError::Damaged("a mini chain runs past its FAT"))?;
        }
        Ok(())
    }
}

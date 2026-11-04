//! Microsoft Compound File Binary ([MS-CFB]): the "structured storage"
//! container AAF files are. A file of fixed-size sectors chained by a FAT,
//! holding a tree of storages (directories, each with a class id) and
//! streams; streams under 4096 bytes live in 64-byte sectors of the mini
//! stream.
//!
//! [`CfbWriter`] writes version 4 (4096-byte sectors, as AAF's "4K"
//! files and pyaaf2 do) or version 3 (512) and streams large streams
//! straight to disk as they are filled, so an export holding gigabytes of
//! embedded audio never holds more than a buffer of it. [`Cfb`] reads
//! both versions, large streams through [`Cfb::copy_to`].

mod read;
mod write;

pub use read::{Cfb, Entry};
pub use write::{CfbWriter, EntryId};

pub(crate) const SIGNATURE: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
pub(crate) const FREESECT: u32 = 0xffff_ffff;
pub(crate) const ENDOFCHAIN: u32 = 0xffff_fffe;
pub(crate) const FATSECT: u32 = 0xffff_fffd;
pub(crate) const DIFSECT: u32 = 0xffff_fffc;
pub(crate) const MAXREGSECT: u32 = 0xffff_fffa;
pub(crate) const NOSTREAM: u32 = 0xffff_ffff;
pub(crate) const MINI_CUTOFF: u64 = 4096;
pub(crate) const MINI_SECTOR: usize = 64;
pub(crate) const DIR_ENTRY: usize = 128;
/// FAT sector locations the header holds itself.
pub(crate) const HEADER_DIFAT: usize = 109;

/// What a directory entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Storage,
    Stream,
    Root,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Kind::Storage => 1,
            Kind::Stream => 2,
            Kind::Root => 5,
        }
    }

    fn of(code: u8) -> Option<Kind> {
        match code {
            1 => Some(Kind::Storage),
            2 => Some(Kind::Stream),
            5 => Some(Kind::Root),
            _ => None,
        }
    }
}

/// The sector size a file is written with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SectorSize {
    /// Version 3: 512-byte sectors (files up to 2 GiB for some readers).
    Small,
    /// Version 4: 4096-byte sectors.
    #[default]
    Large,
}

impl SectorSize {
    pub(crate) fn shift(self) -> u32 {
        match self {
            SectorSize::Small => 9,
            SectorSize::Large => 12,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CfbError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("not a compound file")]
    NotCompound,
    #[error("the compound file is damaged: {0}")]
    Damaged(&'static str),
    #[error("a stream or storage cannot be named {0:?}")]
    BadName(String),
    #[error("{0:?} is there already")]
    Duplicate(String),
    #[error("a stream was given {given} bytes where {expected} were announced")]
    WrongLength { expected: u64, given: u64 },
}

/// The order siblings are kept in: shorter names first, then by the
/// upper-case UTF-16 code units ([MS-CFB] 2.6.4).
pub(crate) fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    let ua: Vec<u16> = a.encode_utf16().collect();
    let ub: Vec<u16> = b.encode_utf16().collect();
    ua.len().cmp(&ub.len()).then_with(|| {
        let up = |c: u16| -> u16 {
            if (u16::from(b'a')..=u16::from(b'z')).contains(&c) {
                c - 32
            } else if c >= 0x80 {
                char::from_u32(u32::from(c))
                    .and_then(|ch| ch.to_uppercase().next())
                    .and_then(|ch| u16::try_from(u32::from(ch)).ok())
                    .unwrap_or(c)
            } else {
                c
            }
        };
        ua.iter().map(|&c| up(c)).cmp(ub.iter().map(|&c| up(c)))
    })
}

#[cfg(test)]
mod tests;

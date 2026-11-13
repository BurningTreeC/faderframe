//! Reading a PE image as it is mapped (by RVA, not by file offset): the
//! headers a module is named by, its exports (the nearest name below an
//! address) and its CodeView record (the PDB a symbol server keeps).
//!
//! Everything is read through [`Image`], which says whether bytes are
//! there: a crash report reads the images of a process that just crashed,
//! and nothing here trusts what it reads to point anywhere sensible.

/// The bytes of an image by RVA.
pub trait Image {
    /// `len` bytes at `rva`, if they are mapped and readable.
    fn read(&self, rva: u32, len: u32) -> Option<&[u8]>;

    /// Up to `max` bytes at `rva`: as many as can be read, at least one.
    fn read_upto(&self, rva: u32, max: u32) -> Option<&[u8]> {
        let mut len = max;
        while len > 0 {
            if let Some(b) = self.read(rva, len) {
                return Some(b);
            }
            len /= 2;
        }
        None
    }
}

impl Image for [u8] {
    fn read(&self, rva: u32, len: u32) -> Option<&[u8]> {
        let start = rva as usize;
        self.get(start..start.checked_add(len as usize)?)
    }
}

/// The longest export table read (a corrupt count reads no further).
const MAX_EXPORTS: u32 = 1 << 16;
/// The longest name read.
const MAX_NAME: u32 = 512;
/// The debug directory's entries looked at.
const MAX_DEBUG_ENTRIES: u32 = 16;

fn u16_of(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_of(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u16_at(img: &(impl Image + ?Sized), rva: u32) -> Option<u16> {
    img.read(rva, 2).map(|b| u16_of(b, 0))
}

fn u32_at(img: &(impl Image + ?Sized), rva: u32) -> Option<u32> {
    img.read(rva, 4).map(|b| u32_of(b, 0))
}

fn u64_at(img: &(impl Image + ?Sized), rva: u32) -> Option<u64> {
    img.read(rva, 8)
        .map(|b| u64::from(u32_of(b, 0)) | u64::from(u32_of(b, 4)) << 32)
}

/// A NUL-terminated name at `rva`.
fn name_at(img: &(impl Image + ?Sized), rva: u32) -> Option<String> {
    let b = img.read_upto(rva, MAX_NAME)?;
    let end = b.iter().position(|&c| c == 0)?;
    Some(String::from_utf8_lossy(&b[..end]).into_owned())
}

/// A data directory: where a table is and how long.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Directory {
    rva: u32,
    size: u32,
}

const EXPORT_DIRECTORY: u32 = 0;
const DEBUG_DIRECTORY: u32 = 6;
const CODEVIEW: u32 = 2;

/// What an image's headers say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Headers {
    /// `IMAGE_FILE_MACHINE_*` (0x8664 x86-64, 0xaa64 ARM64, 0x14c x86).
    pub machine: u16,
    /// The link time (`TimeDateStamp`). With [`size_of_image`](Self::size_of_image)
    /// the key a symbol server keeps the binary under.
    pub time_stamp: u32,
    pub size_of_image: u32,
    /// The base it was linked for (`ImageBase`): a symbols file's
    /// addresses are this plus an offset into the module.
    pub image_base: u64,
    export: Directory,
    debug: Directory,
}

/// The headers of the image, if it is a PE image.
pub fn headers(img: &(impl Image + ?Sized)) -> Option<Headers> {
    if img.read(0, 2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(img, 0x3c)?;
    if img.read(pe, 4)? != b"PE\0\0" {
        return None;
    }
    let file = pe.checked_add(4)?;
    let machine = u16_at(img, file)?;
    let time_stamp = u32_at(img, file + 4)?;
    let optional_size = u32::from(u16_at(img, file + 16)?);
    let optional = file + 20;
    let (image_base, count_at, directories) = match u16_at(img, optional)? {
        // PE32+
        0x20b => (u64_at(img, optional + 24)?, optional + 108, optional + 112),
        // PE32
        0x10b => (
            u64::from(u32_at(img, optional + 28)?),
            optional + 92,
            optional + 96,
        ),
        _ => return None,
    };
    let size_of_image = u32_at(img, optional + 56)?;
    let count = u32_at(img, count_at)?;
    let directory = |i: u32| {
        let at = directories + 8 * i;
        if i >= count || at + 8 > optional + optional_size {
            return Directory::default();
        }
        Directory {
            rva: u32_at(img, at).unwrap_or(0),
            size: u32_at(img, at + 4).unwrap_or(0),
        }
    };
    Some(Headers {
        machine,
        time_stamp,
        size_of_image,
        image_base,
        export: directory(EXPORT_DIRECTORY),
        debug: directory(DEBUG_DIRECTORY),
    })
}

/// The export nearest below `rva` — its name (`#<ordinal>` when it has
/// none) and how far `rva` is past it — if the image exports anything
/// there. Forwarded exports (names of another module's functions) are
/// left out.
pub fn export_below(img: &(impl Image + ?Sized), h: &Headers, rva: u32) -> Option<(String, u32)> {
    let d = h.export;
    if d.rva == 0 || d.size < 40 {
        return None;
    }
    let table = img.read(d.rva, 40)?;
    let ordinal_base = u32_of(table, 16);
    let functions = u32_of(table, 20).min(MAX_EXPORTS);
    let names = u32_of(table, 24).min(MAX_EXPORTS);
    let (at_functions, at_names, at_ordinals) =
        (u32_of(table, 28), u32_of(table, 32), u32_of(table, 36));
    let forwarded = |f: u32| f >= d.rva && f - d.rva < d.size;
    let addresses = img.read(at_functions, functions.checked_mul(4)?)?;
    let mut best: Option<(u32, u32)> = None;
    for i in 0..functions {
        let f = u32_of(addresses, 4 * i as usize);
        if f == 0 || f > rva || forwarded(f) {
            continue;
        }
        if best.is_none_or(|(b, _)| f > b) {
            best = Some((f, i));
        }
    }
    let (f, index) = best?;
    let mut name = None;
    if names > 0
        && let (Some(ordinals), Some(name_rvas)) = (
            img.read(at_ordinals, names * 2),
            img.read(at_names, names * 4),
        )
    {
        name = (0..names as usize)
            .find(|&j| u32::from(u16_of(ordinals, 2 * j)) == index)
            .and_then(|j| name_at(img, u32_of(name_rvas, 4 * j)));
    }
    let name = name.unwrap_or_else(|| format!("#{}", ordinal_base.wrapping_add(index)));
    Some((name, rva - f))
}

/// The PDB a module was linked with (a CodeView `RSDS` record).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeView {
    pub guid: [u8; 16],
    pub age: u32,
    /// The PDB's path as the linker wrote it.
    pub pdb: String,
}

impl CodeView {
    /// The id a symbol server keeps the PDB under (GUID then age, hex).
    pub fn id(&self) -> String {
        let g = &self.guid;
        let mut s = format!(
            "{:08X}{:04X}{:04X}",
            u32_of(g, 0),
            u16_of(g, 4),
            u16_of(g, 6)
        );
        for b in &g[8..] {
            s.push_str(&format!("{b:02X}"));
        }
        s.push_str(&format!("{:X}", self.age));
        s
    }

    /// The PDB's file name (the last part of its path).
    pub fn file_name(&self) -> &str {
        self.pdb.rsplit(['\\', '/']).next().unwrap_or(&self.pdb)
    }
}

/// The image's CodeView record, if its debug directory has one in memory.
pub fn codeview(img: &(impl Image + ?Sized), h: &Headers) -> Option<CodeView> {
    let d = h.debug;
    if d.rva == 0 {
        return None;
    }
    let count = (d.size / 28).min(MAX_DEBUG_ENTRIES);
    let entries = img.read(d.rva, count * 28)?;
    entries.as_chunks::<28>().0.iter().find_map(|e| {
        let (kind, size, rva) = (u32_of(e, 12), u32_of(e, 16), u32_of(e, 20));
        if kind != CODEVIEW || rva == 0 || size < 25 {
            return None;
        }
        let record = img.read(rva, 24)?;
        if &record[..4] != b"RSDS" {
            return None;
        }
        let mut guid = [0; 16];
        guid.copy_from_slice(&record[4..20]);
        Some(CodeView {
            guid,
            age: u32_of(record, 20),
            pdb: name_at(img, rva + 24).unwrap_or_default(),
        })
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small PE32+ image as it would be mapped: headers, an export
    /// table (two named functions, one by ordinal only, one forwarded) and
    /// a CodeView record.
    pub(crate) fn sample() -> Vec<u8> {
        let mut img = vec![0u8; 0x3000];
        let put16 = |img: &mut Vec<u8>, at: usize, v: u16| {
            img[at..at + 2].copy_from_slice(&v.to_le_bytes())
        };
        let put32 = |img: &mut Vec<u8>, at: usize, v: u32| {
            img[at..at + 4].copy_from_slice(&v.to_le_bytes())
        };
        img[..2].copy_from_slice(b"MZ");
        put32(&mut img, 0x3c, 0x80);
        img[0x80..0x84].copy_from_slice(b"PE\0\0");
        let file = 0x84;
        put16(&mut img, file, 0x8664);
        put32(&mut img, file + 4, 0x6730_f1a2);
        put16(&mut img, file + 16, 240);
        let opt = file + 20;
        put16(&mut img, opt, 0x20b);
        img[opt + 24..opt + 32].copy_from_slice(&0x1_8000_0000u64.to_le_bytes());
        put32(&mut img, opt + 56, 0x3000);
        put32(&mut img, opt + 108, 16);
        // Exports at 0x1000, 0x100 long.
        put32(&mut img, opt + 112, 0x1000);
        put32(&mut img, opt + 116, 0x100);
        // Debug directory at 0x2000, one entry.
        put32(&mut img, opt + 112 + 48, 0x2000);
        put32(&mut img, opt + 112 + 52, 28);

        let e = 0x1000;
        put32(&mut img, e + 16, 1); // ordinal base
        put32(&mut img, e + 20, 4); // functions
        put32(&mut img, e + 24, 2); // names
        put32(&mut img, e + 28, 0x1040);
        put32(&mut img, e + 32, 0x1060);
        put32(&mut img, e + 36, 0x1070);
        // Functions: 0x1800 "second", 0x1400 "first", 0x1c00 unnamed,
        // and one forwarded (inside the export table).
        put32(&mut img, 0x1040, 0x1800);
        put32(&mut img, 0x1044, 0x1400);
        put32(&mut img, 0x1048, 0x1c00);
        put32(&mut img, 0x104c, 0x10f0);
        put32(&mut img, 0x1060, 0x1080);
        put32(&mut img, 0x1064, 0x1090);
        put16(&mut img, 0x1070, 1);
        put16(&mut img, 0x1072, 0);
        img[0x1080..0x1086].copy_from_slice(b"first\0");
        img[0x1090..0x1097].copy_from_slice(b"second\0");

        let d = 0x2000;
        put32(&mut img, d + 12, CODEVIEW);
        put32(&mut img, d + 16, 24 + 9);
        put32(&mut img, d + 20, 0x2100);
        img[0x2100..0x2104].copy_from_slice(b"RSDS");
        let guid = [
            0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8,
        ];
        img[0x2104..0x2114].copy_from_slice(&guid);
        put32(&mut img, 0x2114, 3);
        img[0x2118..0x2121].copy_from_slice(b"x\\a.pdb\0\0");
        img
    }

    #[test]
    fn the_headers_are_read() {
        let img = sample();
        let h = headers(img.as_slice()).expect("headers");
        assert_eq!(h.machine, 0x8664);
        assert_eq!(h.time_stamp, 0x6730_f1a2);
        assert_eq!(h.size_of_image, 0x3000);
        assert_eq!(h.image_base, 0x1_8000_0000);
        assert!(headers(&img[..0x90]).is_none(), "cut short");
        assert!(headers(&[0u8; 64][..]).is_none(), "not an image");
    }

    #[test]
    fn the_nearest_export_below_names_an_address() {
        let img = sample();
        let h = headers(img.as_slice()).expect("headers");
        let at = |rva| export_below(img.as_slice(), &h, rva);
        assert_eq!(at(0x1300), None, "below every export");
        assert_eq!(at(0x1400), Some(("first".into(), 0)));
        assert_eq!(at(0x17ff), Some(("first".into(), 0x3ff)));
        assert_eq!(at(0x1850), Some(("second".into(), 0x50)));
        assert_eq!(at(0x1c10), Some(("#3".into(), 0x10)), "by ordinal");
    }

    #[test]
    fn the_codeview_record_names_the_pdb() {
        let img = sample();
        let h = headers(img.as_slice()).expect("headers");
        let cv = codeview(img.as_slice(), &h).expect("codeview");
        assert_eq!(cv.pdb, "x\\a.pdb");
        assert_eq!(cv.file_name(), "a.pdb");
        assert_eq!(cv.id(), "123456789ABCDEF001020304050607083");
    }

    #[test]
    fn nonsense_reads_nothing() {
        let mut img = sample();
        // An export count past the end of the image.
        img[0x1014..0x1018].copy_from_slice(&u32::MAX.to_le_bytes());
        let h = headers(img.as_slice()).expect("headers");
        assert_eq!(export_below(img.as_slice(), &h, 0x1850), None);
    }
}

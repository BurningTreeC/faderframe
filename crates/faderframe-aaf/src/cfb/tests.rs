use super::*;
use std::io::Cursor;

fn bytes(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[1] ^ seed)
        .collect()
}

fn round_trip(size: SectorSize, big: usize) {
    let mut w = CfbWriter::new(Cursor::new(Vec::new()), size).unwrap();
    let root = w.root();
    w.set_clsid(root, [7; 16]);
    let a = w.storage(root, "Header-2", [1; 16]).unwrap();
    let b = w.storage(a, "Mobs-1901{0}", [2; 16]).unwrap();
    w.stream(a, "properties", &bytes(100, 1)).unwrap();
    w.stream(b, "properties", &bytes(4095, 2)).unwrap();
    w.stream(b, "empty", &[]).unwrap();
    w.stream(root, "four k", &bytes(4096, 3)).unwrap();
    let data = bytes(big, 4);
    w.stream_with(root, "Data-2702", big as u64, &mut |o| {
        for c in data.chunks(1000) {
            o.write_all(c)?;
        }
        Ok(())
    })
    .unwrap();
    // Many siblings: a tree several levels deep.
    for k in 0..40 {
        w.stream(b, &format!("s{k}"), &bytes(k * 37, k as u8))
            .unwrap();
    }
    let file = w.finish().unwrap().into_inner();
    assert_eq!(file.len() % (1 << size.shift()), 0);

    let mut r = Cfb::open(Cursor::new(file)).unwrap();
    assert_eq!(r.entry(0).unwrap().clsid, [7; 16]);
    let a = r.child(0, "header-2").unwrap();
    assert_eq!(r.entry(a).unwrap().clsid, [1; 16]);
    let b = r.child(a, "Mobs-1901{0}").unwrap();
    let p = r.child(a, "properties").unwrap();
    assert_eq!(r.read(p).unwrap(), bytes(100, 1));
    let p = r.child(b, "properties").unwrap();
    assert_eq!(r.read(p).unwrap(), bytes(4095, 2));
    assert_eq!(
        r.read(r.child(b, "empty").unwrap()).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        r.read(r.child(0, "four k").unwrap()).unwrap(),
        bytes(4096, 3)
    );
    let d = r.child(0, "Data-2702").unwrap();
    let mut got = Vec::new();
    let mut last = 0;
    r.copy_to(d, &mut got, &mut |n| last = n).unwrap();
    assert_eq!(got.len(), big);
    assert!(got == data);
    assert_eq!(last, big as u64);
    for k in 0..40 {
        let s = r.child(b, &format!("s{k}")).unwrap();
        assert_eq!(r.read(s).unwrap(), bytes(k * 37, k as u8));
    }
    // The siblings come out in the tree's order: shorter names first.
    let names: Vec<&str> = r
        .entry(b)
        .unwrap()
        .children
        .iter()
        .map(|&c| r.entry(c).unwrap().name.as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_by(|x, y| compare_names(x, y));
    assert_eq!(names, sorted);
}

#[test]
fn large_sectors_round_trip() {
    round_trip(SectorSize::Large, 300_000);
}

#[test]
fn small_sectors_round_trip() {
    round_trip(SectorSize::Small, 300_000);
}

#[test]
fn files_past_109_fat_sectors_chain_a_difat() {
    // 512-byte sectors: 109 FAT sectors cover 109 × 128 sectors (7 MB).
    round_trip(SectorSize::Small, 8 << 20);
}

#[test]
fn names_sort_by_length_then_letters() {
    use std::cmp::Ordering::*;
    assert_eq!(compare_names("b", "AA"), Less);
    assert_eq!(compare_names("abc", "ABD"), Less);
    assert_eq!(compare_names("Header-2", "HEADER-2"), Equal);
}

#[test]
fn names_are_checked() {
    let mut w = CfbWriter::new(Cursor::new(Vec::new()), SectorSize::Large).unwrap();
    let root = w.root();
    assert!(w.stream(root, "a/b", b"x").is_err());
    assert!(w.stream(root, &"x".repeat(32), b"x").is_err());
    w.stream(root, "same", b"x").unwrap();
    assert!(matches!(
        w.stream(root, "SAME", b"y"),
        Err(CfbError::Duplicate(_))
    ));
    assert!(matches!(
        w.stream_with(root, "short", 10, &mut |o| o.write_all(b"abc")),
        Err(CfbError::WrongLength { .. })
    ));
}

#[test]
fn other_files_are_refused() {
    assert!(matches!(
        Cfb::open(Cursor::new(b"RIFF\0\0\0\0WAVE".to_vec())),
        Err(CfbError::NotCompound)
    ));
}

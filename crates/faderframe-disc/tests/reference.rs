//! The DDP writer against filesets the DDP Mastering Tools' cue2ddp wrote
//! (see `tests/reference/README.md`): every descriptor file byte for byte,
//! the audio image by its checksum, and each fileset read back.
#![allow(clippy::unwrap_used)]

use faderframe_disc::ddp::{DdpNames, DdpWriter, read};
use faderframe_disc::{CdText, Disc, SECTOR_FRAMES, Track, TrackFlags, normalize_upc};
use std::path::{Path, PathBuf};

fn reference(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/reference")
        .join(name)
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

fn msf(s: &str) -> u32 {
    let v: Vec<u32> = s.split(':').map(|p| p.parse().unwrap()).collect();
    (v[0] * 60 + v[1]) * 75 + v[2]
}

fn set_text(t: &mut CdText, cmd: &str, value: String) -> bool {
    let f = match cmd {
        "TITLE" => &mut t.title,
        "PERFORMER" => &mut t.performer,
        "SONGWRITER" => &mut t.songwriter,
        "COMPOSER" => &mut t.composer,
        "ARRANGER" => &mut t.arranger,
        "MESSAGE" => &mut t.message,
        _ => return false,
    };
    *f = value;
    true
}

/// A disc from a cue sheet as cue2ddp reads it (times relative to the
/// audio; 150 sectors of silence prepended when track 1 has no index 00),
/// and those sectors.
fn disc_from_cue(cue: &str, text: bool) -> (Disc, u32) {
    let mut disc = Disc::default();
    for line in cue.lines() {
        let line = line.trim();
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "CATALOG" => disc.upc = Some(normalize_upc(rest).unwrap()),
            "TRACK" => disc.tracks.push(Track::default()),
            "ISRC" => disc.tracks.last_mut().unwrap().isrc = Some(rest.to_string()),
            "FLAGS" => {
                let f = &mut disc.tracks.last_mut().unwrap().flags;
                for flag in rest.split_whitespace() {
                    match flag {
                        "PRE" => f.pre_emphasis = true,
                        "DCP" => f.copy_permitted = true,
                        "4CH" => f.four_channel = true,
                        "SCMS" => f.scms = true,
                        _ => {}
                    }
                }
            }
            "INDEX" => {
                let (n, at) = rest.split_once(' ').unwrap();
                let t = disc.tracks.last_mut().unwrap();
                if n == "00" {
                    t.pregap = Some(msf(at));
                } else {
                    t.indexes.push(msf(at));
                }
            }
            other => {
                let value = unquote(rest);
                if text {
                    let target = match disc.tracks.last_mut() {
                        Some(t) => &mut t.text,
                        None => &mut disc.text,
                    };
                    set_text(target, other, value);
                }
            }
        }
    }
    let shift = if disc.tracks[0].pregap.is_none() {
        150
    } else {
        0
    };
    if shift > 0 {
        for t in &mut disc.tracks {
            t.pregap = t.pregap.map(|p| p + shift);
            for i in &mut t.indexes {
                *i += shift;
            }
        }
        disc.tracks[0].pregap = Some(0);
    }
    let _ = TrackFlags::default();
    (disc, shift)
}

/// Write the experiment `name`'s fileset into a temporary folder; the
/// expected files are in `expected` (another experiment's for 091).
fn check(name: &str, expected: &str) {
    let args = std::fs::read_to_string(reference(name).join("run.args")).unwrap();
    let mut words = args.split_whitespace();
    let frames: u64 = words.next().unwrap().parse().unwrap();
    let rest: Vec<&str> = words.collect();
    let text = rest.contains(&"-t");
    let cue = std::fs::read_to_string(reference(name).join("input.cue")).unwrap();
    let (mut disc, shift) = disc_from_cue(&cue, text);
    if let Some(i) = rest.iter().position(|a| *a == "-m") {
        disc.master_id = rest[i + 1].to_string();
    }
    let dir = std::env::temp_dir().join(format!("faderframe-ddp-{}-{name}", std::process::id()));
    let names = DdpNames {
        pq: "SD".into(),
        ..DdpNames::default()
    };
    let mut w = DdpWriter::create(&dir, names).unwrap();
    w.write_silence(u64::from(shift) * SECTOR_FRAMES).unwrap();
    // The position pattern, in blocks.
    let total = frames * SECTOR_FRAMES;
    let mut i = 0u64;
    while i < total {
        let n = (total - i).min(65_536);
        let block: Vec<i16> = (i..i + n)
            .flat_map(|k| [(k & 0xffff) as u16 as i16, (k >> 16) as u16 as i16])
            .collect();
        w.write_interleaved(&block).unwrap();
        i += n;
    }
    let fileset = w.finish(&disc).unwrap();
    for file in [
        "DDPID",
        "DDPMS",
        "SD",
        "CDTEXT.BIN",
        "CHECKSUM.MD5",
        "CHECKSUM.TXT",
    ] {
        let want = reference(expected).join(file);
        let got = dir.join(file);
        match std::fs::read(&want) {
            Ok(want) => assert_eq!(
                String::from_utf8_lossy(&std::fs::read(&got).unwrap()),
                String::from_utf8_lossy(&want),
                "{name}: {file}"
            ),
            Err(_) => assert!(!got.exists(), "{name}: {file} should not be written"),
        }
    }
    // Read back: the same disc, checksums matching.
    let back = read(&dir).unwrap();
    assert_eq!(back.checksums, Some(true), "{name}");
    assert_eq!(back.disc, fileset.disc, "{name}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn baseline() {
    check("001-baseline", "001-baseline");
}

#[test]
fn track_2_pregap() {
    check("003-track2-pregap", "003-track2-pregap");
}

#[test]
fn isrc_and_upc() {
    check("005-isrc-upc", "005-isrc-upc");
}

#[test]
fn flags() {
    check("006-flags", "006-flags");
}

#[test]
fn master_id() {
    check("007-master-id", "007-master-id");
}

#[test]
fn cd_text() {
    check("011-cdtext-cue", "011-cdtext-cue");
}

#[test]
fn three_tracks_with_index_02() {
    check("014-three-tracks", "014-three-tracks");
}

#[test]
fn cd_text_of_every_type() {
    check("091-cdtext-extended", "092-ref-cdtext-extended");
}

//! Red Book audio CD masters: what goes on the disc (tracks, indexes,
//! ISRC, UPC/EAN, flags, CD-Text) and the files that carry it to a
//! replication plant.
//!
//! * [`Disc`] describes a master in CD sectors (1/75 s, 588 stereo
//!   frames at 44.1 kHz); [`Disc::validate`] checks the Red Book and
//!   plant rules (track 1 pregap of at least 2 s, tracks of at least 4 s,
//!   1-99 tracks, codes well formed, at most 99:59:74).
//! * [`ddp`] writes (and reads back) a DDP 2.00 fileset: `DDPID`,
//!   `DDPMS`, the PQ descriptor, `CDTEXT.BIN`, `IMAGE.DAT` and the
//!   `CHECKSUM.MD5`/`CHECKSUM.TXT` files plants check.
//! * [`cdtext`] encodes CD-Text lead-in packs, [`cue`] writes cue sheets.
//!
//! The DDP layout follows the open, reverse-engineered description of the
//! format by the ddp-reverse-eng project (MIT licence), and this crate's
//! tests compare its output byte for byte with the reference filesets
//! recorded there.

#![forbid(unsafe_code)]

pub mod cdtext;
pub mod cue;
pub mod ddp;
pub mod hash;

/// Stereo sample frames per sector (44.1 kHz).
pub const SECTOR_FRAMES: u64 = 588;
/// Bytes per audio sector (588 frames × 2 channels × 2 bytes).
pub const SECTOR_BYTES: usize = 2352;
pub const SECTORS_PER_SECOND: u32 = 75;
/// The CD sample rate.
pub const CD_RATE: u32 = 44_100;
/// Track 1's minimum pregap (2 s).
pub const MIN_PREGAP: u32 = 150;
/// The shortest track (4 s, index 01 to the next index 01).
pub const MIN_TRACK: u32 = 300;
/// The longest program: 99:59:74.
pub const MAX_SECTORS: u32 = (99 * 60 + 59) * 75 + 74;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscError {
    #[error("a CD has 1 to 99 tracks, not {0}")]
    TrackCount(usize),
    #[error("track {0}: it has no start (index 01)")]
    NoStart(usize),
    #[error("track 1 must begin with a pregap of at least 2 s at 00:00:00")]
    FirstPregap,
    #[error("track {track}: {sectors} sectors (1/75 s) long, a CD track needs at least 300 (4 s)")]
    TooShort { track: usize, sectors: u32 },
    #[error("track {0}: its indexes are out of order")]
    IndexOrder(usize),
    #[error("the program is longer than 99:59:74")]
    TooLong,
    #[error(
        "ISRC '{0}': 12 characters — 2 letters (country), 3 letters or digits (registrant), 2 digits (year), 5 digits"
    )]
    Isrc(String),
    #[error("UPC/EAN '{0}': 12 (UPC-A) or 13 (EAN-13) digits")]
    UpcFormat(String),
    #[error("UPC/EAN '{code}': the check digit should be {expected}")]
    UpcCheck { code: String, expected: char },
    #[error("track {0}: digital copy permitted and SCMS exclude each other")]
    CopyFlags(usize),
    #[error("the master id is longer than 48 characters")]
    MasterId,
    #[error("CD-Text needs {0} packs, a disc holds 256")]
    CdTextTooLong(usize),
}

/// CD-Text of the disc or a track (empty: none). Characters outside ISO
/// 8859-1 become `?`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CdText {
    pub title: String,
    pub performer: String,
    pub songwriter: String,
    pub composer: String,
    pub arranger: String,
    pub message: String,
}

impl CdText {
    pub fn is_empty(&self) -> bool {
        self.fields().iter().all(|f| f.is_empty())
    }

    /// The text types 0x80-0x85 in order.
    pub fn fields(&self) -> [&str; 6] {
        [
            &self.title,
            &self.performer,
            &self.songwriter,
            &self.composer,
            &self.arranger,
            &self.message,
        ]
    }
}

/// The Q-channel control flags of a track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackFlags {
    pub pre_emphasis: bool,
    /// Digital copy permitted (DCP).
    pub copy_permitted: bool,
    pub four_channel: bool,
    /// Serial copy management: one generation of copies.
    pub scms: bool,
}

impl TrackFlags {
    /// The PQ descriptor's C1 field: control nibble in hex, then ADR (`1`)
    /// or `S` under SCMS.
    pub(crate) fn c1(self) -> [u8; 2] {
        let nibble = u8::from(self.pre_emphasis)
            | (u8::from(self.copy_permitted) << 1)
            | (u8::from(self.four_channel) << 3);
        [
            b"0123456789ABCDEF"[nibble as usize],
            if self.scms { b'S' } else { b'1' },
        ]
    }
}

/// A track: where it starts (index 01) and its further indexes, in
/// sectors from the start of the image (time 0).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    /// Index 00: the pregap's start, when the track has one.
    pub pregap: Option<u32>,
    /// Index 01, 02, …
    pub indexes: Vec<u32>,
    /// Normalised (see [`normalize_isrc`]).
    pub isrc: Option<String>,
    pub flags: TrackFlags,
    pub text: CdText,
}

impl Track {
    /// Where the track starts (index 01).
    pub fn start(&self) -> Option<u32> {
        self.indexes.first().copied()
    }
}

/// An audio CD master.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disc {
    /// The media catalog number: 13 digits (see [`normalize_upc`]).
    pub upc: Option<String>,
    /// Free text identifying the master (DDPID; not on the disc).
    pub master_id: String,
    pub text: CdText,
    pub tracks: Vec<Track>,
    /// The program's length in sectors: the lead-out's address.
    pub sectors: u32,
}

impl Disc {
    /// Check the Red Book and plant rules.
    pub fn validate(&self) -> Result<(), DiscError> {
        let n = self.tracks.len();
        if !(1..=99).contains(&n) {
            return Err(DiscError::TrackCount(n));
        }
        if self.sectors > MAX_SECTORS {
            return Err(DiscError::TooLong);
        }
        if self.master_id.chars().count() > 48 {
            return Err(DiscError::MasterId);
        }
        if let Some(upc) = &self.upc {
            normalize_upc(upc)?;
        }
        let first = &self.tracks[0];
        if first.pregap != Some(0) || first.start().is_none_or(|s| s < MIN_PREGAP) {
            return Err(DiscError::FirstPregap);
        }
        let mut last = 0u32;
        for (i, t) in self.tracks.iter().enumerate() {
            let number = i + 1;
            let start = t.start().ok_or(DiscError::NoStart(number))?;
            let mut at = t.pregap.unwrap_or(start);
            if i > 0 && at < last {
                return Err(DiscError::IndexOrder(number));
            }
            for &x in t.pregap.iter().chain(&t.indexes) {
                if x < at || x >= self.sectors {
                    return Err(DiscError::IndexOrder(number));
                }
                at = x;
            }
            if t.indexes.windows(2).any(|w| w[1] <= w[0]) || t.pregap.is_some_and(|p| p >= start) {
                return Err(DiscError::IndexOrder(number));
            }
            let end = self
                .tracks
                .get(i + 1)
                .and_then(Track::start)
                .unwrap_or(self.sectors);
            if end.saturating_sub(start) < MIN_TRACK {
                return Err(DiscError::TooShort {
                    track: number,
                    sectors: end.saturating_sub(start),
                });
            }
            if let Some(isrc) = &t.isrc {
                normalize_isrc(isrc)?;
            }
            if t.flags.copy_permitted && t.flags.scms {
                return Err(DiscError::CopyFlags(number));
            }
            last = *t.indexes.last().unwrap_or(&start);
        }
        let packs = cdtext::packs(self).len();
        if packs > 256 {
            return Err(DiscError::CdTextTooLong(packs));
        }
        Ok(())
    }
}

/// An ISRC as the disc carries it: 12 characters, uppercase, no dashes.
pub fn normalize_isrc(isrc: &str) -> Result<String, DiscError> {
    let s: String = isrc
        .trim()
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect::<String>()
        .to_ascii_uppercase();
    let b = s.as_bytes();
    let ok = b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..5].iter().all(u8::is_ascii_alphanumeric)
        && b[5..].iter().all(u8::is_ascii_digit);
    if ok {
        Ok(s)
    } else {
        Err(DiscError::Isrc(isrc.to_string()))
    }
}

/// A UPC-A or EAN-13 as the disc carries it: 13 digits (a UPC-A gets a
/// leading 0), with a correct check digit.
pub fn normalize_upc(code: &str) -> Result<String, DiscError> {
    let digits: String = code
        .trim()
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) || !matches!(digits.len(), 12 | 13) {
        return Err(DiscError::UpcFormat(code.to_string()));
    }
    let ean = if digits.len() == 12 {
        format!("0{digits}")
    } else {
        digits
    };
    let d: Vec<u32> = ean.bytes().map(|b| u32::from(b - b'0')).collect();
    let sum: u32 = d[..12]
        .iter()
        .enumerate()
        .map(|(i, v)| if i % 2 == 0 { *v } else { v * 3 })
        .sum();
    let check = (10 - sum % 10) % 10;
    if d[12] != check {
        return Err(DiscError::UpcCheck {
            code: code.to_string(),
            expected: char::from(b'0' + check as u8),
        });
    }
    Ok(ean)
}

/// `MMSSFF` text of a sector address.
pub(crate) fn msf(sectors: u32) -> String {
    format!(
        "{:02}{:02}{:02}",
        sectors / 75 / 60,
        sectors / 75 % 60,
        sectors % 75
    )
}

/// `MM:SS:FF` of a sector address (cue sheets).
pub fn msf_colon(sectors: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        sectors / 75 / 60,
        sectors / 75 % 60,
        sectors % 75
    )
}

/// ISO 8859-1 bytes of `s` (other characters become `?`).
pub(crate) fn latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(lengths: &[u32]) -> Disc {
        let mut tracks = Vec::new();
        let mut at = MIN_PREGAP;
        for (i, &l) in lengths.iter().enumerate() {
            tracks.push(Track {
                pregap: (i == 0).then_some(0),
                indexes: vec![at],
                ..Track::default()
            });
            at += l;
        }
        Disc {
            tracks,
            sectors: at,
            ..Disc::default()
        }
    }

    #[test]
    fn codes_are_normalised_and_checked() {
        assert_eq!(normalize_isrc("us-abc-12-34567").unwrap(), "USABC1234567");
        assert!(normalize_isrc("US-ABC-12-3456").is_err());
        assert!(normalize_isrc("1SABC1234567").is_err());
        assert_eq!(normalize_upc("0123456789012").unwrap(), "0123456789012");
        // A UPC-A is the EAN-13 without its leading 0.
        assert_eq!(normalize_upc("036000291452").unwrap(), "0036000291452");
        assert_eq!(
            normalize_upc("036000291453"),
            Err(DiscError::UpcCheck {
                code: "036000291453".into(),
                expected: '2'
            })
        );
        assert!(normalize_upc("12345").is_err());
    }

    #[test]
    fn red_book_rules() {
        assert_eq!(disc(&[300, 300]).validate(), Ok(()));
        assert_eq!(
            disc(&[300, 299]).validate(),
            Err(DiscError::TooShort {
                track: 2,
                sectors: 299
            })
        );
        let mut d = disc(&[300]);
        d.tracks[0].indexes[0] = 149;
        assert_eq!(d.validate(), Err(DiscError::FirstPregap));
        assert_eq!(Disc::default().validate(), Err(DiscError::TrackCount(0)));
        let mut d = disc(&[300]);
        d.tracks[0].flags.copy_permitted = true;
        d.tracks[0].flags.scms = true;
        assert_eq!(d.validate(), Err(DiscError::CopyFlags(1)));
        assert_eq!(msf(150), "000200");
        assert_eq!(msf_colon(MAX_SECTORS), "99:59:74");
    }
}

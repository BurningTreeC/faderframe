//! CD-Text: the lead-in packs of 18 bytes that carry the disc's and the
//! tracks' titles, performers, songwriters, composers, arrangers and
//! messages, in up to eight languages (blocks).
//!
//! Per block and text type, the disc's string and then each track's
//! follow each other NUL-terminated (a track without that text has an
//! empty string), cut into 12-byte payloads; each pack names the string its
//! first payload byte belongs to and how many characters of that string
//! earlier packs held. Three size packs (type 0x8F) close each block: its
//! character code, first and last track, pack counts per type, and — the
//! same in every block — every block's last sequence number and language.
//! Sequence numbers count each block's packs from 0; block numbers are bits
//! 4-6 of the fourth header byte, bit 7 marks double-byte text (MS-JIS:
//! Japanese, two bytes a character, two NULs end a string). Every pack ends
//! with its inverted CRC-16/CCITT, big-endian. (After libburn's
//! `doc/cdtext.txt`, from MMC-3 Annex J and Sony's CD-Text description.)

use crate::hash::crc16;
use crate::{CdText, Charset, Disc, Language, TextBlock};

/// One pack.
pub type Pack = [u8; 18];

const TEXT_TYPES: std::ops::RangeInclusive<u8> = 0x80..=0x85;
const SIZE_INFO: u8 = 0x8F;

fn seal(p: &mut Pack) {
    let crc = !crc16(&p[..16]);
    p[16..].copy_from_slice(&crc.to_be_bytes());
}

/// The CD-Text blocks of `disc`: its own text (in `text_language`) and its
/// further languages, empty ones left out.
pub fn blocks(disc: &Disc) -> Vec<TextBlock> {
    let first = TextBlock {
        language: disc.text_language,
        disc: disc.text.clone(),
        tracks: disc.tracks.iter().map(|t| t.text.clone()).collect(),
    };
    std::iter::once(first)
        .chain(disc.more_text.iter().cloned())
        .filter(|b| !b.is_empty())
        .collect()
}

/// `s` as a block of `charset` stores it: double-byte text two bytes a
/// character (ASCII in its full-width form).
fn encode(charset: Charset, s: &str) -> Vec<u8> {
    if !charset.double_byte() {
        return charset.encode(s);
    }
    let mut out = Vec::with_capacity(s.len() * 2);
    for c in s.chars() {
        let wide = match c {
            ' ' => '\u{3000}',
            '!'..='~' => char::from_u32(u32::from(c) + 0xFEE0).unwrap_or('?'),
            _ => c,
        };
        let bytes = charset.encode(wide.encode_utf8(&mut [0; 4]));
        if bytes.len() == 2 {
            out.extend_from_slice(&bytes);
        } else {
            // One byte (half-width kana): a full-width question mark.
            out.extend_from_slice(&[0x81, 0x48]);
        }
    }
    out
}

/// A block's text packs (sequence numbers from 0) and their counts per type.
fn text_packs(block: &TextBlock, number: u8, tracks: usize) -> (Vec<Pack>, [u8; 16]) {
    let charset = block.language.charset();
    let double = charset.double_byte();
    let unit = if double { 2 } else { 1 };
    let mut out: Vec<Pack> = Vec::new();
    let mut counts = [0u8; 16];
    let empty = CdText::default();
    for (k, ty) in TEXT_TYPES.enumerate() {
        let strings: Vec<Vec<u8>> = std::iter::once(&block.disc)
            .chain((0..tracks).map(|i| block.tracks.get(i).unwrap_or(&empty)))
            .map(|t| encode(charset, t.fields()[k]))
            .collect();
        if strings.iter().all(Vec::is_empty) {
            continue;
        }
        // Every byte with the string (0 disc, n track) and position it has.
        let mut bytes: Vec<(u8, u8, usize)> = Vec::new();
        for (owner, s) in strings.iter().enumerate() {
            let terminator: &[u8] = if double { &[0, 0] } else { &[0] };
            for (pos, &b) in s.iter().chain(terminator).enumerate() {
                bytes.push((b, owner as u8, pos));
            }
        }
        for chunk in bytes.chunks(12) {
            let mut p: Pack = [0; 18];
            p[0] = ty;
            p[1] = chunk[0].1;
            p[2] = out.len() as u8;
            let chars = (chunk[0].2 / unit).min(15) as u8;
            p[3] = (u8::from(double) << 7) | (number << 4) | chars;
            for (i, (b, _, _)) in chunk.iter().enumerate() {
                p[4 + i] = *b;
            }
            seal(&mut p);
            out.push(p);
            counts[k] = counts[k].saturating_add(1);
        }
    }
    (out, counts)
}

/// Each block's language and pack count, size packs included.
pub(crate) fn block_sizes(disc: &Disc) -> Vec<(Language, usize)> {
    blocks(disc)
        .iter()
        .enumerate()
        .map(|(n, b)| {
            (
                b.language,
                text_packs(b, n.min(7) as u8, disc.tracks.len()).0.len() + 3,
            )
        })
        .collect()
}

/// The CD-Text packs of `disc` (none when it has no text), at most eight
/// blocks.
pub fn packs(disc: &Disc) -> Vec<Pack> {
    let blocks: Vec<TextBlock> = blocks(disc).into_iter().take(8).collect();
    let tracks = disc.tracks.len();
    let built: Vec<(Vec<Pack>, [u8; 16])> = blocks
        .iter()
        .enumerate()
        .map(|(n, b)| text_packs(b, n as u8, tracks))
        .collect();
    let mut out = Vec::new();
    for (n, (block, (text, counts))) in blocks.iter().zip(&built).enumerate() {
        let mut counts = *counts;
        counts[15] = 3;
        let mut info = [0u8; 36];
        info[0] = block.language.charset().code();
        info[1] = 1;
        info[2] = tracks as u8;
        info[4..20].copy_from_slice(&counts);
        for (k, (b, (t, _))) in blocks.iter().zip(&built).enumerate() {
            // The block's last sequence number, size packs included.
            info[20 + k] = (t.len() + 2).min(255) as u8;
            info[28 + k] = b.language.0;
        }
        out.extend_from_slice(text);
        for (i, payload) in info.chunks(12).enumerate() {
            let mut p: Pack = [0; 18];
            p[0] = SIZE_INFO;
            p[1] = i as u8;
            p[2] = (text.len() + i).min(255) as u8;
            p[3] = (n as u8) << 4;
            p[4..16].copy_from_slice(payload);
            seal(&mut p);
            out.push(p);
        }
    }
    out
}

/// The text of the disc and of `tracks` tracks from packs, the first
/// block's (pack CRCs are checked; `Err` names the first bad pack).
pub fn decode(data: &[u8], tracks: usize) -> Result<(CdText, Vec<CdText>), usize> {
    let mut blocks = decode_blocks(data, tracks)?;
    if blocks.is_empty() {
        return Ok((CdText::default(), vec![CdText::default(); tracks]));
    }
    let first = blocks.swap_remove(0);
    Ok((first.disc, first.tracks))
}

/// Every block of packs, in block order, with its language (pack CRCs are
/// checked; `Err` names the first bad pack).
pub fn decode_blocks(data: &[u8], tracks: usize) -> Result<Vec<TextBlock>, usize> {
    // Per block: the text streams per type, the size information.
    let mut streams: Vec<[Vec<u8>; 6]> = vec![Default::default(); 8];
    let mut info: Vec<[u8; 36]> = vec![[0; 36]; 8];
    let mut seen = [false; 8];
    for (i, p) in data.as_chunks::<18>().0.iter().enumerate() {
        let crc = u16::from_be_bytes([p[16], p[17]]);
        if crc != !crc16(&p[..16]) {
            return Err(i);
        }
        let block = usize::from((p[3] >> 4) & 7);
        seen[block] = true;
        if TEXT_TYPES.contains(&p[0]) {
            streams[block][usize::from(p[0] - 0x80)].extend_from_slice(&p[4..16]);
        } else if p[0] == SIZE_INFO && p[1] < 3 {
            let at = usize::from(p[1]) * 12;
            info[block][at..at + 12].copy_from_slice(&p[4..16]);
        }
    }
    let mut out = Vec::new();
    for block in (0..8).filter(|&b| seen[b]) {
        let charset = Charset::from_code(info[block][0]).unwrap_or_default();
        let language = match info[block][28 + block] {
            // Older writers leave it out: English.
            0 => Language::ENGLISH,
            code => Language(code),
        };
        let mut text = TextBlock {
            language,
            disc: CdText::default(),
            tracks: vec![CdText::default(); tracks],
        };
        for (k, stream) in streams[block].iter().enumerate() {
            if stream.is_empty() {
                continue;
            }
            let strings = split(stream, charset.double_byte());
            let mut previous = String::new();
            for (owner, raw) in strings.into_iter().take(tracks + 1).enumerate() {
                // A TAB (two in double-byte text) repeats the previous
                // track's string.
                let tab: &[u8] = if charset.double_byte() {
                    b"\t\t"
                } else {
                    b"\t"
                };
                let s = if raw == tab {
                    previous.clone()
                } else {
                    narrow(&charset.decode(raw))
                };
                previous.clone_from(&s);
                let target = if owner == 0 {
                    &mut text.disc
                } else {
                    &mut text.tracks[owner - 1]
                };
                let field = match k {
                    0 => &mut target.title,
                    1 => &mut target.performer,
                    2 => &mut target.songwriter,
                    3 => &mut target.composer,
                    4 => &mut target.arranger,
                    _ => &mut target.message,
                };
                *field = s;
            }
        }
        out.push(text);
    }
    Ok(out)
}

/// The NUL-terminated strings of a stream (two NULs, on character
/// boundaries, in double-byte text).
fn split(stream: &[u8], double: bool) -> Vec<&[u8]> {
    if !double {
        return stream.split(|b| *b == 0).collect();
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + 1 < stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 {
            out.push(&stream[start..i]);
            start = i + 2;
        }
        i += 2;
    }
    out.push(&stream[start.min(stream.len())..]);
    out
}

/// Full-width ASCII (as double-byte text stores it) back to ASCII.
fn narrow(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Track;

    fn disc() -> Disc {
        let track = |title: &str, at: u32| Track {
            pregap: (at == 150).then_some(0),
            indexes: vec![at],
            text: CdText {
                title: title.into(),
                performer: "The Band".into(),
                ..CdText::default()
            },
            ..Track::default()
        };
        Disc {
            text: CdText {
                title: "Album".into(),
                performer: "The Band".into(),
                ..CdText::default()
            },
            tracks: vec![track("One", 150), track("Two", 600)],
            sectors: 1000,
            ..Disc::default()
        }
    }

    #[test]
    fn one_language_is_block_0_in_english() {
        let p = packs(&disc());
        assert!(p.iter().all(|p| p[3] & 0xF0 == 0), "block 0, single byte");
        let size: Vec<&Pack> = p.iter().filter(|p| p[0] == SIZE_INFO).collect();
        assert_eq!(size.len(), 3);
        assert_eq!(size[2][4 + 28 - 24], 0x09, "English");
        assert_eq!(
            usize::from(size[1][4 + 20 - 12]),
            p.len() - 1,
            "last sequence"
        );
        let (d, t) = decode(&p.concat(), 2).unwrap();
        assert_eq!(d.title, "Album");
        assert_eq!(t[1].title, "Two");
    }

    #[test]
    fn languages_are_blocks_with_their_own_sequences_and_shared_tables() {
        let mut d = disc();
        d.more_text = vec![
            TextBlock {
                language: Language(0x08),
                disc: CdText {
                    title: "Größtes Album".into(),
                    ..CdText::default()
                },
                tracks: vec![
                    CdText {
                        title: "Eins".into(),
                        ..CdText::default()
                    },
                    CdText {
                        title: "Zwei".into(),
                        ..CdText::default()
                    },
                ],
            },
            TextBlock {
                language: Language::JAPANESE,
                disc: CdText {
                    title: "アルバム 1".into(),
                    ..CdText::default()
                },
                tracks: vec![CdText {
                    title: "一".into(),
                    ..CdText::default()
                }],
            },
        ];
        assert_eq!(d.validate(), Ok(()));
        let p = packs(&d);
        let in_block =
            |b: u8| -> Vec<&Pack> { p.iter().filter(|x| (x[3] >> 4) & 7 == b).collect() };
        for b in 0..3u8 {
            let packs = in_block(b);
            // Sequence numbers from 0 in each block, in order.
            for (i, x) in packs.iter().enumerate() {
                assert_eq!(usize::from(x[2]), i, "block {b}");
            }
            let size: Vec<_> = packs.iter().filter(|x| x[0] == SIZE_INFO).collect();
            assert_eq!(size.len(), 3);
            let mut info = Vec::new();
            for s in &size {
                info.extend_from_slice(&s[4..16]);
            }
            assert_eq!(
                info[0],
                [0x00, 0x00, 0x80][usize::from(b)],
                "character code"
            );
            assert_eq!(&info[28..31], &[0x09, 0x08, 0x69], "languages");
            for k in 0..3 {
                assert_eq!(usize::from(info[20 + k]), in_block(k as u8).len() - 1);
            }
        }
        // Japanese text is double-byte.
        assert!(
            in_block(2)
                .iter()
                .filter(|x| x[0] != SIZE_INFO)
                .all(|x| x[3] & 0x80 != 0)
        );
        let blocks = decode_blocks(&p.concat(), 2).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1].language, Language(0x08));
        assert_eq!(blocks[1].disc.title, "Größtes Album");
        assert_eq!(blocks[1].tracks[1].title, "Zwei");
        assert_eq!(blocks[2].language, Language::JAPANESE);
        assert_eq!(blocks[2].disc.title, "アルバム 1");
        assert_eq!(blocks[2].tracks[0].title, "一");
        assert_eq!(blocks[2].tracks[1].title, "");
    }

    #[test]
    fn limits_are_checked_per_language() {
        let mut d = disc();
        d.more_text = (0..8)
            .map(|i| TextBlock {
                language: Language(0x01 + i),
                disc: CdText {
                    title: "x".into(),
                    ..CdText::default()
                },
                tracks: Vec::new(),
            })
            .collect();
        assert_eq!(d.validate(), Err(crate::DiscError::TooManyLanguages(9)));
        d.more_text.truncate(1);
        d.more_text[0].language = Language::ENGLISH;
        assert_eq!(
            d.validate(),
            Err(crate::DiscError::LanguageTwice("English"))
        );
        d.more_text[0].language = Language(0x08);
        d.more_text[0].disc.message = "y".repeat(3200);
        assert!(matches!(
            d.validate(),
            Err(crate::DiscError::CdTextTooLong {
                language: "German",
                ..
            })
        ));
    }
}

#[cfg(test)]
mod dump {
    use super::*;

    /// Writes a three-language disc's packs to `$CDTEXT_OUT` (to check with
    /// another decoder).
    #[test]
    #[ignore]
    fn dump_three_languages() {
        let Ok(path) = std::env::var("CDTEXT_OUT") else {
            return;
        };
        let mut d = tests_disc();
        d.more_text = vec![
            TextBlock {
                language: Language(0x08),
                disc: CdText {
                    title: "Größtes Album".into(),
                    performer: "Die Band".into(),
                    ..CdText::default()
                },
                tracks: vec![
                    CdText {
                        title: "Eins".into(),
                        ..CdText::default()
                    },
                    CdText {
                        title: "Zwei".into(),
                        ..CdText::default()
                    },
                ],
            },
            TextBlock {
                language: Language::JAPANESE,
                disc: CdText {
                    title: "アルバム".into(),
                    performer: "バンド".into(),
                    ..CdText::default()
                },
                tracks: vec![
                    CdText {
                        title: "一番".into(),
                        ..CdText::default()
                    },
                    CdText {
                        title: "二番".into(),
                        ..CdText::default()
                    },
                ],
            },
        ];
        if std::env::var_os("CDTEXT_LATIN_ONLY").is_some() {
            d.more_text.truncate(1);
        }
        std::fs::write(path, packs(&d).concat()).unwrap();
    }

    fn tests_disc() -> Disc {
        let track = |title: &str, at: u32| crate::Track {
            pregap: (at == 150).then_some(0),
            indexes: vec![at],
            text: CdText {
                title: title.into(),
                performer: "The Band".into(),
                ..CdText::default()
            },
            ..crate::Track::default()
        };
        Disc {
            text: CdText {
                title: "Album".into(),
                performer: "The Band".into(),
                ..CdText::default()
            },
            tracks: vec![track("One", 150), track("Two", 600)],
            sectors: 1000,
            ..Disc::default()
        }
    }
}

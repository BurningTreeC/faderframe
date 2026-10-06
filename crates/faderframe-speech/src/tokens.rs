//! Whisper's tokens back to text: GPT-2's byte-level vocabulary
//! (`vocab.json`: token strings in its byte-to-character mapping).

use std::collections::HashMap;

/// Tokens to text.
pub struct Detokenizer {
    pieces: Vec<Vec<u8>>,
}

/// GPT-2's mapping of bytes to printable characters, reversed.
fn byte_of_char() -> HashMap<char, u8> {
    let mut bs: Vec<u32> = (u32::from(b'!')..=u32::from(b'~'))
        .chain(0xa1..=0xac)
        .chain(0xae..=0xff)
        .collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    bs.into_iter()
        .zip(cs)
        .filter_map(|(b, c)| char::from_u32(c).map(|c| (c, b as u8)))
        .collect()
}

impl Detokenizer {
    /// From `vocab.json` (token string → id).
    pub fn new(vocab: &serde_json::Value) -> Option<Self> {
        let map = vocab.as_object()?;
        let bytes = byte_of_char();
        let size = map.values().filter_map(|v| v.as_u64()).max()? as usize + 1;
        let mut pieces = vec![Vec::new(); size];
        for (token, id) in map {
            let id = id.as_u64()? as usize;
            pieces[id] = token
                .chars()
                .map(|c| bytes.get(&c).copied().unwrap_or(b'?'))
                .collect();
        }
        Some(Self { pieces })
    }

    /// The text of `tokens` (special tokens, beyond the vocabulary, left
    /// out).
    pub fn decode(&self, tokens: &[u32]) -> String {
        let bytes: Vec<u8> = tokens
            .iter()
            .filter_map(|t| self.pieces.get(*t as usize))
            .flatten()
            .copied()
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

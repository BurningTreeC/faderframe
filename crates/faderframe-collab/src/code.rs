//! Invite codes: the secret a host gives the people it invites.
//!
//! A code is 15 random bytes from the operating system (120 bits), written
//! as 24 characters of Crockford's base 32 in six groups of four
//! (`K7QF-M2ZP-…`). Reading one forgives what people get wrong when they
//! copy it by hand: case, spaces, missing or extra dashes, `I`/`L` for
//! `1`, `O` for `0`.
//!
//! Nobody who lacks the code gets past the handshake: the code is the
//! pre-shared key of the Noise handshake ([`crate::secure`]), hashed with
//! a label of this protocol first. 120 bits are far beyond guessing, also
//! by trying codes offline against a recorded handshake.

use sha2::{Digest, Sha256};
use std::fmt;

/// Bytes of randomness in a code.
pub const CODE_BYTES: usize = 15;
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Characters per group as written.
const GROUP: usize = 4;

#[derive(Clone, PartialEq, Eq)]
pub struct InviteCode([u8; CODE_BYTES]);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodeError {
    #[error("an invite code has 24 letters and digits (it has {0})")]
    Length(usize),
    #[error("‘{0}’ is not in an invite code")]
    Character(char),
    #[error("the operating system gave no random numbers: {0}")]
    Random(String),
}

impl InviteCode {
    /// A new random code.
    pub fn new() -> Result<Self, CodeError> {
        let mut b = [0u8; CODE_BYTES];
        getrandom::fill(&mut b).map_err(|e| CodeError::Random(e.to_string()))?;
        Ok(Self(b))
    }

    pub fn from_bytes(b: [u8; CODE_BYTES]) -> Self {
        Self(b)
    }

    /// Read a code as people type it.
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let mut digits = Vec::with_capacity(24);
        for c in text.chars() {
            let d = match c.to_ascii_uppercase() {
                '-' | ' ' | '_' | '\t' => continue,
                'O' => 0,
                'I' | 'L' => 1,
                c => match ALPHABET.iter().position(|&a| a as char == c) {
                    Some(d) => d as u8,
                    None => return Err(CodeError::Character(c)),
                },
            };
            digits.push(d);
        }
        if digits.len() != CODE_BYTES * 8 / 5 {
            return Err(CodeError::Length(digits.len()));
        }
        let mut out = [0u8; CODE_BYTES];
        let (mut acc, mut bits, mut i) = (0u32, 0, 0);
        for d in digits {
            acc = (acc << 5) | u32::from(d);
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out[i] = (acc >> bits) as u8;
                i += 1;
                acc &= (1 << bits) - 1;
            }
        }
        Ok(Self(out))
    }

    /// The Noise handshake's pre-shared key.
    pub fn psk(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"FaderFrame collaboration invite v1");
        h.update(self.0);
        h.finalize().into()
    }
}

impl fmt::Display for InviteCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (mut acc, mut bits) = (0u32, 0);
        let mut n = 0;
        for &b in &self.0 {
            acc = (acc << 8) | u32::from(b);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                if n > 0 && n % GROUP == 0 {
                    f.write_str("-")?;
                }
                let d = (acc >> bits) & 31;
                write!(f, "{}", ALPHABET[d as usize] as char)?;
                n += 1;
                acc &= (1 << bits) - 1;
            }
        }
        Ok(())
    }
}

/// Codes are secrets: never in logs.
impl fmt::Debug for InviteCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InviteCode(…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_read_back_as_written_and_forgive_typing() {
        let code = InviteCode::from_bytes(*b"FaderFrame live");
        let text = code.to_string();
        assert_eq!(text.len(), 24 + 5);
        assert_eq!(text.split('-').count(), 6);
        assert_eq!(InviteCode::parse(&text), Ok(code.clone()));
        let sloppy = text.to_lowercase().replace('-', " ").replace('1', "l");
        assert_eq!(InviteCode::parse(&sloppy), Ok(code.clone()));
        assert_eq!(InviteCode::parse("ABCD-EFGH"), Err(CodeError::Length(8)));
        assert_eq!(
            InviteCode::parse(&text.replacen(|c: char| c.is_ascii_alphanumeric(), "U", 1)),
            Err(CodeError::Character('U'))
        );
        assert_eq!(format!("{code:?}"), "InviteCode(…)");
    }

    #[test]
    fn new_codes_differ_and_so_do_their_keys() {
        let a = InviteCode::new().unwrap();
        let b = InviteCode::new().unwrap();
        assert_ne!(a, b);
        assert_ne!(a.psk(), b.psk());
        assert_eq!(a.psk(), InviteCode::parse(&a.to_string()).unwrap().psk());
    }
}

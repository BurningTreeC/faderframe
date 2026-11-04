//! Secrets from the operating system's random source: pairing codes,
//! devices' tokens and ids, and the hash kept of a token.

use crate::hash;
use std::io;

fn fill(buf: &mut [u8]) -> io::Result<()> {
    getrandom::fill(buf).map_err(|e| io::Error::other(format!("no random numbers: {e}")))
}

/// A random 64-bit number (device ids).
pub fn random_u64() -> io::Result<u64> {
    let mut b = [0u8; 8];
    fill(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Digits of a pairing code.
pub const CODE_DIGITS: usize = 6;

/// A pairing code: six decimal digits, every one of the million alike
/// likely.
pub fn pairing_code() -> io::Result<String> {
    const RANGE: u32 = 1_000_000;
    // The largest multiple of the range below 2³²: draws above it are
    // drawn again, so no code is likelier than another.
    const LIMIT: u32 = u32::MAX - u32::MAX % RANGE;
    loop {
        let mut b = [0u8; 4];
        fill(&mut b)?;
        let v = u32::from_le_bytes(b);
        if v < LIMIT {
            return Ok(format!("{:0width$}", v % RANGE, width = CODE_DIGITS));
        }
    }
}

/// A device's token: 32 random bytes as hexadecimal (what the device
/// keeps and shows on every connection).
pub fn token() -> io::Result<String> {
    let mut b = [0u8; 32];
    fill(&mut b)?;
    Ok(hash::hex(&b))
}

/// What is kept of a token (never the token itself): its SHA-256.
pub fn token_hash(token: &str) -> String {
    hash::hex(&hash::sha256(token.as_bytes()))
}

/// `a == b` in a time that does not depend on where they differ.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_tokens_look_as_they_should() {
        let code = pairing_code().unwrap();
        assert_eq!(code.len(), CODE_DIGITS);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        let a = token().unwrap();
        let b = token().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_eq!(token_hash(&a), token_hash(&a));
        assert_ne!(token_hash(&a), token_hash(&b));
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
    }
}

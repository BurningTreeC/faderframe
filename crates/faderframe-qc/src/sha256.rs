//! SHA-256 (FIPS 180-4), streaming: the receipt of every delivered file.

use std::io::Read;
use std::path::Path;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// A running SHA-256.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: H0,
            buffer: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    fn block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.filled > 0 {
            let n = (64 - self.filled).min(data.len());
            self.buffer[self.filled..self.filled + n].copy_from_slice(&data[..n]);
            self.filled += n;
            data = &data[n..];
            if self.filled < 64 {
                return;
            }
            let block = self.buffer;
            self.block(&block);
            self.filled = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            self.block(block);
        }
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        let len = (self.filled + 1) % 64;
        let zeros = if len <= 56 { 56 - len } else { 120 - len };
        pad.extend(std::iter::repeat_n(0u8, zeros));
        pad.extend(bits.to_be_bytes());
        // The padding is not message: keep the length as it was.
        let length = self.length;
        self.update(&pad);
        self.length = length;
        let mut out = [0u8; 32];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            o.copy_from_slice(&s.to_be_bytes());
        }
        out
    }

    /// The digest in lowercase hexadecimal (as `sha256sum` prints it).
    pub fn hex(self) -> String {
        self.finish().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The SHA-256 of a file's bytes (hex); `stop` is asked between pieces
/// and ends the reading early (an error).
pub fn file(path: &Path, stop: &dyn Fn() -> bool) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if stop() {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.hex()
    }

    #[test]
    fn the_standard_vectors() {
        // FIPS 180-4 / NIST examples.
        assert_eq!(
            hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // A million 'a's, fed in uneven pieces.
        let mut h = Sha256::new();
        let a = vec![b'a'; 1_000_000];
        let mut at = 0;
        for n in [1, 63, 64, 65, 1000, 4095].iter().cycle() {
            if at >= a.len() {
                break;
            }
            let e = (at + n).min(a.len());
            h.update(&a[at..e]);
            at = e;
        }
        assert_eq!(
            h.hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        // Lengths round the padding boundary (as `sha256sum` hashes them).
        for (n, want) in [
            (
                55,
                "d5e285683cd4efc02d021a5c62014694958901005d6f71e89e0989fac77e4072",
            ),
            (
                56,
                "04c26261370ee7541549d16dee320c723e3fd14671e66a099afe0a377c16888e",
            ),
            (
                63,
                "75220b47218278e656f2013bb8f0c455a25eaf01e86c64924e9d48d89776d6f2",
            ),
            (
                64,
                "7ce100971f64e7001e8fe5a51973ecdfe1ced42befe7ee8d5fd6219506b5393c",
            ),
            (
                119,
                "000b48d4edf0fa7bee3c6236ecd2785baa5db4eeb8bb54341b029e0d9fa5fb0c",
            ),
        ] {
            assert_eq!(hex(&vec![b'x'; n]), want, "{n} bytes");
        }
    }

    #[test]
    fn a_file_hashes_as_its_bytes() {
        let path = std::env::temp_dir().join(format!("ff-qc-sha-{}", std::process::id()));
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        assert_eq!(file(&path, &|| false).unwrap(), hex(&data));
        assert!(file(&path, &|| true).is_err());
        std::fs::remove_file(&path).unwrap();
    }
}

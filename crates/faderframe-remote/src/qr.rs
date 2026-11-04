//! QR codes (ISO/IEC 18004, Model 2), byte mode: what Preferences shows so
//! a phone's camera opens the remote and pairs in one go. The smallest
//! version (1–40) that holds the data at the error-correction level asked
//! for; Reed–Solomon codewords per block, interleaved; the function
//! patterns, the data in the zigzag, then the mask of least penalty
//! (or the one asked for) with its format and version bits.

/// How much of the code may be damaged and still read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ecl {
    /// About 7 %.
    Low,
    /// About 15 %.
    Medium,
    /// About 25 %.
    Quartile,
    /// About 30 %.
    High,
}

impl Ecl {
    fn index(self) -> usize {
        match self {
            Ecl::Low => 0,
            Ecl::Medium => 1,
            Ecl::Quartile => 2,
            Ecl::High => 3,
        }
    }

    /// The two bits the format information carries.
    fn format_bits(self) -> u32 {
        match self {
            Ecl::Low => 1,
            Ecl::Medium => 0,
            Ecl::Quartile => 3,
            Ecl::High => 2,
        }
    }
}

/// Error-correction codewords per block, by level and version (0 unused).
const ECC_PER_BLOCK: [[u8; 41]; 4] = [
    [
        0, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28,
        30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    [
        0, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28,
        28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    ],
    [
        0, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30,
        30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    [
        0, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24,
        30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
];

/// Error-correction blocks, by level and version (0 unused).
const BLOCKS: [[u8; 41]; 4] = [
    [
        0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13,
        14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25,
    ],
    [
        0, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21,
        23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
    ],
    [
        0, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29,
        34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68,
    ],
    [
        0, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32,
        35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81,
    ],
];

/// A QR code's modules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrCode {
    version: u8,
    size: usize,
    /// Row by row, dark = true.
    modules: Vec<bool>,
}

/// Modules not covered by function patterns (version `v`).
fn raw_data_modules(v: usize) -> usize {
    let mut n = (16 * v + 128) * v + 64;
    if v >= 2 {
        let align = v / 7 + 2;
        n -= (25 * align - 10) * align - 55;
        if v >= 7 {
            n -= 36;
        }
    }
    n
}

/// Data codewords of version `v` at `ecl`.
fn data_codewords(v: usize, ecl: Ecl) -> usize {
    let e = ecl.index();
    raw_data_modules(v) / 8 - usize::from(ECC_PER_BLOCK[e][v]) * usize::from(BLOCKS[e][v])
}

/// Bits the byte mode's character count takes.
fn count_bits(v: usize) -> usize {
    if v <= 9 { 8 } else { 16 }
}

/// GF(2⁸) product, modulo x⁸ + x⁴ + x³ + x² + 1.
fn gf_mul(x: u8, y: u8) -> u8 {
    let mut z: u8 = 0;
    for i in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x1D);
        z ^= ((y >> i) & 1) * x;
    }
    z
}

/// The Reed–Solomon generator of `degree` (highest term left out,
/// coefficients from the next highest).
fn rs_divisor(degree: usize) -> Vec<u8> {
    let mut result = vec![0u8; degree];
    result[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for j in 0..degree {
            result[j] = gf_mul(result[j], root);
            if j + 1 < degree {
                result[j] ^= result[j + 1];
            }
        }
        root = gf_mul(root, 0x02);
    }
    result
}

/// The error-correction codewords of `data`.
fn rs_remainder(data: &[u8], divisor: &[u8]) -> Vec<u8> {
    let mut result = vec![0u8; divisor.len()];
    for b in data {
        let factor = b ^ result[0];
        result.remove(0);
        result.push(0);
        for (x, y) in result.iter_mut().zip(divisor) {
            *x ^= gf_mul(*y, factor);
        }
    }
    result
}

impl QrCode {
    /// `data` at error-correction level Medium (the smallest version that
    /// holds it); `None` when it is too long for any version.
    pub fn encode(data: &[u8]) -> Option<QrCode> {
        Self::encode_with(data, Ecl::Medium, None)
    }

    /// `data` at `ecl`, with the mask given (0–7) or the one of least
    /// penalty.
    pub fn encode_with(data: &[u8], ecl: Ecl, mask: Option<u8>) -> Option<QrCode> {
        let version = (1..=40usize).find(|&v| {
            data.len() < (1 << count_bits(v))
                && 4 + count_bits(v) + 8 * data.len() <= data_codewords(v, ecl) * 8
        })?;
        let capacity = data_codewords(version, ecl) * 8;
        let mut bits: Vec<bool> = Vec::with_capacity(capacity);
        let mut push = |value: usize, n: usize| {
            for i in (0..n).rev() {
                bits.push((value >> i) & 1 == 1);
            }
        };
        push(0b0100, 4);
        push(data.len(), count_bits(version));
        for b in data {
            push(usize::from(*b), 8);
        }
        let terminator = (capacity - bits.len()).min(4);
        bits.extend(std::iter::repeat_n(false, terminator));
        while !bits.len().is_multiple_of(8) {
            bits.push(false);
        }
        let mut codewords: Vec<u8> = bits
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |acc, b| (acc << 1) | u8::from(*b)))
            .collect();
        for pad in [0xEC, 0x11].into_iter().cycle() {
            if codewords.len() >= capacity / 8 {
                break;
            }
            codewords.push(pad);
        }
        let all = interleave(&codewords, version, ecl);
        let mut qr = QrCode {
            version: version as u8,
            size: version * 4 + 17,
            modules: vec![false; (version * 4 + 17).pow(2)],
        };
        let mut function = vec![false; qr.modules.len()];
        qr.draw_function_patterns(&mut function, ecl);
        qr.draw_codewords(&all, &function);
        let mask = match mask {
            Some(m) => m.min(7),
            None => (0..8u8)
                .min_by_key(|&m| {
                    let mut trial = qr.clone();
                    trial.apply_mask(m, &function);
                    trial.draw_format_bits(ecl, m, &mut function.clone());
                    trial.penalty()
                })
                .unwrap_or(0),
        };
        qr.apply_mask(mask, &function);
        qr.draw_format_bits(ecl, mask, &mut function);
        Some(qr)
    }

    pub fn version(&self) -> u8 {
        self.version
    }

    /// Modules a side (without the quiet zone).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Whether the module at column `x`, row `y` is dark (outside: light).
    pub fn dark(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.modules[y * self.size + x]
    }

    /// The code as text, two characters a module, with a quiet zone of
    /// `quiet` modules (for logs and terminals: dark on light).
    pub fn to_text(&self, quiet: usize) -> String {
        let n = self.size + 2 * quiet;
        let mut s = String::with_capacity(n * (n * 2 + 1));
        for y in 0..n {
            for x in 0..n {
                let dark = x >= quiet && y >= quiet && self.dark(x - quiet, y - quiet);
                s.push_str(if dark { "##" } else { "  " });
            }
            s.push('\n');
        }
        s
    }

    fn set(&mut self, x: usize, y: usize, dark: bool, function: &mut [bool]) {
        let i = y * self.size + x;
        self.modules[i] = dark;
        function[i] = true;
    }

    fn draw_function_patterns(&mut self, function: &mut [bool], ecl: Ecl) {
        let size = self.size;
        for i in 0..size {
            self.set(6, i, i % 2 == 0, function);
            self.set(i, 6, i % 2 == 0, function);
        }
        self.finder(3, 3, function);
        self.finder(size - 4, 3, function);
        self.finder(3, size - 4, function);
        let places = self.alignment_places();
        let n = places.len();
        for (i, &x) in places.iter().enumerate() {
            for (j, &y) in places.iter().enumerate() {
                let corner = (i == 0 && j == 0) || (i == 0 && j == n - 1) || (i == n - 1 && j == 0);
                if !corner {
                    for dy in -2i32..=2 {
                        for dx in -2i32..=2 {
                            let dark = dx.abs().max(dy.abs()) != 1;
                            self.set(
                                (x as i32 + dx) as usize,
                                (y as i32 + dy) as usize,
                                dark,
                                function,
                            );
                        }
                    }
                }
            }
        }
        // Reserved for now; drawn again once the mask is chosen.
        self.draw_format_bits(ecl, 0, function);
        self.draw_version(function);
    }

    /// A finder pattern with its separator, centred on (`cx`, `cy`).
    fn finder(&mut self, cx: usize, cy: usize, function: &mut [bool]) {
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                if (0..self.size as i32).contains(&x) && (0..self.size as i32).contains(&y) {
                    let d = dx.abs().max(dy.abs());
                    self.set(x as usize, y as usize, d != 2 && d != 4, function);
                }
            }
        }
    }

    /// The alignment patterns' centres along an axis.
    fn alignment_places(&self) -> Vec<usize> {
        let v = usize::from(self.version);
        if v == 1 {
            return Vec::new();
        }
        let n = v / 7 + 2;
        let step = (v * 8 + n * 3 + 5) / (n * 4 - 4) * 2;
        let mut places: Vec<usize> = (0..n - 1).map(|i| self.size - 7 - i * step).collect();
        places.push(6);
        places.reverse();
        places
    }

    fn draw_format_bits(&mut self, ecl: Ecl, mask: u8, function: &mut [bool]) {
        let bits = format_bits(ecl, mask);
        let bit = |i: u32| (bits >> i) & 1 == 1;
        let size = self.size;
        for i in 0..6 {
            self.set(8, i as usize, bit(i), function);
        }
        self.set(8, 7, bit(6), function);
        self.set(8, 8, bit(7), function);
        self.set(7, 8, bit(8), function);
        for i in 9..15 {
            self.set(14 - i as usize, 8, bit(i), function);
        }
        for i in 0..8 {
            self.set(size - 1 - i as usize, 8, bit(i), function);
        }
        for i in 8..15 {
            self.set(8, size - 15 + i as usize, bit(i), function);
        }
        // The dark module.
        self.set(8, size - 8, true, function);
    }

    fn draw_version(&mut self, function: &mut [bool]) {
        if self.version < 7 {
            return;
        }
        let bits = version_bits(self.version);
        for i in 0..18 {
            let dark = (bits >> i) & 1 == 1;
            let a = self.size - 11 + i % 3;
            let b = i / 3;
            self.set(a, b, dark, function);
            self.set(b, a, dark, function);
        }
    }

    /// The codewords in the zigzag from the bottom right, two columns at a
    /// time, round the vertical timing pattern.
    fn draw_codewords(&mut self, data: &[u8], function: &[bool]) {
        let size = self.size as i32;
        let total = data.len() * 8;
        let mut i = 0;
        let mut right = size - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            for vert in 0..size {
                for j in 0..2 {
                    let x = (right - j) as usize;
                    let upward = (right + 1) & 2 == 0;
                    let y = if upward { size - 1 - vert } else { vert } as usize;
                    let at = y * self.size + x;
                    if !function[at] && i < total {
                        self.modules[at] = (data[i >> 3] >> (7 - (i & 7))) & 1 == 1;
                        i += 1;
                    }
                }
            }
            right -= 2;
        }
    }

    fn apply_mask(&mut self, mask: u8, function: &[bool]) {
        for y in 0..self.size {
            for x in 0..self.size {
                let flip = match mask {
                    0 => (x + y) % 2 == 0,
                    1 => y % 2 == 0,
                    2 => x % 3 == 0,
                    3 => (x + y) % 3 == 0,
                    4 => (x / 3 + y / 2) % 2 == 0,
                    5 => x * y % 2 + x * y % 3 == 0,
                    6 => (x * y % 2 + x * y % 3) % 2 == 0,
                    _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
                };
                let i = y * self.size + x;
                if flip && !function[i] {
                    self.modules[i] = !self.modules[i];
                }
            }
        }
    }

    /// The standard's penalty (runs, blocks, finder-like patterns,
    /// balance).
    fn penalty(&self) -> usize {
        const N1: usize = 3;
        const N2: usize = 3;
        const N3: usize = 40;
        const N4: usize = 10;
        let size = self.size;
        let mut result = 0;
        for transpose in [false, true] {
            for a in 0..size {
                let mut run_color = false;
                let mut run = 0usize;
                let mut history = [0usize; 7];
                for b in 0..size {
                    let dark = if transpose {
                        self.dark(a, b)
                    } else {
                        self.dark(b, a)
                    };
                    if dark == run_color {
                        run += 1;
                        if run == 5 {
                            result += N1;
                        } else if run > 5 {
                            result += 1;
                        }
                    } else {
                        self.add_history(run, &mut history);
                        if !run_color {
                            result += count_finders(&history) * N3;
                        }
                        run_color = dark;
                        run = 1;
                    }
                }
                // The end of the line: a light border follows.
                if run_color {
                    self.add_history(run, &mut history);
                    run = 0;
                }
                self.add_history(run + size, &mut history);
                result += count_finders(&history) * N3;
            }
        }
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let c = self.dark(x, y);
                if c == self.dark(x + 1, y)
                    && c == self.dark(x, y + 1)
                    && c == self.dark(x + 1, y + 1)
                {
                    result += N2;
                }
            }
        }
        let dark = self.modules.iter().filter(|m| **m).count() as i64;
        let total = (size * size) as i64;
        let k = ((dark * 20 - total * 10).abs() + total - 1) / total - 1;
        result + k.max(0) as usize * N4
    }

    fn add_history(&self, mut run: usize, history: &mut [usize; 7]) {
        // The first run of a line has the light border before it.
        if history[0] == 0 {
            run += self.size;
        }
        history.copy_within(0..6, 1);
        history[0] = run;
    }
}

/// 1:1:3:1:1 runs with four light modules on one side or the other.
fn count_finders(h: &[usize; 7]) -> usize {
    let n = h[1];
    let core = n > 0 && h[2] == n && h[3] == n * 3 && h[4] == n && h[5] == n;
    usize::from(core && h[0] >= n * 4 && h[6] >= n)
        + usize::from(core && h[6] >= n * 4 && h[0] >= n)
}

/// The 15 format bits (BCH(15, 5), masked) for a level and mask.
fn format_bits(ecl: Ecl, mask: u8) -> u32 {
    let data = (ecl.format_bits() << 3) | u32::from(mask);
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    ((data << 10) | rem) ^ 0x5412
}

/// The 18 version bits (BCH(18, 6)).
fn version_bits(version: u8) -> u32 {
    let v = u32::from(version);
    let mut rem = v;
    for _ in 0..12 {
        rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
    }
    (v << 12) | rem
}

/// Data codewords split into blocks, each with its error correction, then
/// interleaved: the first codeword of every block, the second, …
fn interleave(data: &[u8], version: usize, ecl: Ecl) -> Vec<u8> {
    let e = ecl.index();
    let blocks = usize::from(BLOCKS[e][version]);
    let ecc_len = usize::from(ECC_PER_BLOCK[e][version]);
    let raw = raw_data_modules(version) / 8;
    let short_blocks = blocks - raw % blocks;
    let short_len = raw / blocks;
    let divisor = rs_divisor(ecc_len);
    let mut made: Vec<Vec<u8>> = Vec::with_capacity(blocks);
    let mut k = 0;
    for i in 0..blocks {
        let len = short_len - ecc_len + usize::from(i >= short_blocks);
        let mut block = data[k..k + len].to_vec();
        k += len;
        let ecc = rs_remainder(&block, &divisor);
        if i < short_blocks {
            block.push(0);
        }
        block.extend(ecc);
        made.push(block);
    }
    let mut out = Vec::with_capacity(raw);
    for i in 0..made[0].len() {
        for (j, block) in made.iter().enumerate() {
            // Short blocks' padding place is skipped.
            if i != short_len - ecc_len || j >= short_blocks {
                out.push(block[i]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_add_up() {
        // Every version's codewords fill its data modules exactly.
        for v in 1..=40 {
            for ecl in [Ecl::Low, Ecl::Medium, Ecl::Quartile, Ecl::High] {
                let e = ecl.index();
                let total = data_codewords(v, ecl)
                    + usize::from(ECC_PER_BLOCK[e][v]) * usize::from(BLOCKS[e][v]);
                assert_eq!(total, raw_data_modules(v) / 8, "v{v} {ecl:?}");
            }
        }
        // Known capacities (data codewords).
        assert_eq!(data_codewords(1, Ecl::Medium), 16);
        assert_eq!(data_codewords(4, Ecl::Medium), 64);
        assert_eq!(data_codewords(10, Ecl::Medium), 216);
        assert_eq!(data_codewords(40, Ecl::Low), 2956);
        assert_eq!(raw_data_modules(1), 208);
        assert_eq!(raw_data_modules(7), 1568);
    }

    #[test]
    fn reed_solomon_matches_the_worked_example() {
        // Version 1-M, "HELLO WORLD" in alphanumeric mode (the standard's
        // and Thonky's worked example).
        let data = [
            32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17,
        ];
        let ecc = rs_remainder(&data, &rs_divisor(10));
        assert_eq!(ecc, [196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
    }

    #[test]
    fn format_and_version_bits_are_the_standards() {
        assert_eq!(format_bits(Ecl::Medium, 0), 0b101010000010010);
        assert_eq!(format_bits(Ecl::Low, 0), 0b111011111000100);
        assert_eq!(format_bits(Ecl::High, 7), 0b000100000111011);
        assert_eq!(version_bits(7), 0x07C94);
        assert_eq!(version_bits(40), 0x28C69);
    }

    #[test]
    fn a_url_fits_a_small_code_with_its_patterns() {
        let url = b"http://192.168.178.20:8642/#pair=123456";
        let qr = QrCode::encode(url).unwrap();
        assert_eq!(qr.version(), 3);
        assert_eq!(qr.size(), 29);
        // Finder patterns in three corners, the timing pattern between.
        for (cx, cy) in [(3, 3), (qr.size() - 4, 3), (3, qr.size() - 4)] {
            assert!(qr.dark(cx, cy));
            assert!(!qr.dark(cx + 2, cy));
            assert!(qr.dark(cx + 3, cy));
        }
        for i in 8..qr.size() - 8 {
            assert_eq!(qr.dark(i, 6), i % 2 == 0);
            assert_eq!(qr.dark(6, i), i % 2 == 0);
        }
        assert!(qr.dark(8, qr.size() - 8));
        assert!(QrCode::encode(&vec![b'a'; 3000]).is_none());
        let big = QrCode::encode(&vec![b'a'; 300]).unwrap();
        assert!(big.version() >= 7);
        assert!(qr.to_text(4).lines().count() == 37);
    }

    /// Codes a real reader reads: `zbarimg` (ZBar) on a picture of each.
    /// Opt-in: `cargo test -p faderframe-remote --lib -- --ignored
    /// codes_read_back`.
    #[test]
    #[ignore = "needs zbarimg"]
    fn codes_read_back() {
        let dir = std::env::temp_dir().join(format!("ff-qr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut texts: Vec<String> = vec!["http://10.0.0.2:8642/#pair=000001".into()];
        for n in [1usize, 17, 40, 90, 150, 260, 400, 700, 1100, 1700, 2300] {
            texts.push(
                (0..n)
                    .map(|i| char::from(b'a' + (i * 7 % 26) as u8))
                    .collect(),
            );
        }
        for (t, text) in texts.iter().enumerate() {
            for ecl in [Ecl::Low, Ecl::Medium, Ecl::Quartile, Ecl::High] {
                for mask in [None, Some((t % 8) as u8)] {
                    let Some(qr) = QrCode::encode_with(text.as_bytes(), ecl, mask) else {
                        continue;
                    };
                    let scale = 4;
                    let n = (qr.size() + 8) * scale;
                    let mut pgm = format!("P5 {n} {n} 255\n").into_bytes();
                    for y in 0..n {
                        for x in 0..n {
                            let (mx, my) = (x / scale, y / scale);
                            let dark = mx >= 4 && my >= 4 && qr.dark(mx - 4, my - 4);
                            pgm.push(if dark { 0 } else { 255 });
                        }
                    }
                    let path = dir.join("code.pgm");
                    std::fs::write(&path, pgm).unwrap();
                    let out = std::process::Command::new("zbarimg")
                        .args(["--quiet", "--raw", "-Sdisable", "-Sqrcode.enable"])
                        .arg(&path)
                        .output()
                        .unwrap();
                    let read = String::from_utf8_lossy(&out.stdout);
                    assert_eq!(
                        read.trim_end_matches('\n'),
                        text,
                        "v{} {ecl:?} mask {mask:?}",
                        qr.version()
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

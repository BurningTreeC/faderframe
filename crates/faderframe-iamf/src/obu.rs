//! The bitstream: bit fields (most significant bit first), leb128 and OBU
//! headers (IAMF § 3.2, § 8.1.1).

/// Bit fields, most significant bit first, into bytes.
#[derive(Default)]
pub(crate) struct Bits {
    pub out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    pub fn new() -> Self {
        Self::default()
    }

    /// `width` bits of `v` (at most 32).
    pub fn put(&mut self, v: u32, width: u32) {
        for i in (0..width).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    pub fn u8(&mut self, v: u8) {
        self.put(u32::from(v), 8);
    }

    pub fn u16(&mut self, v: u16) {
        self.put(u32::from(v), 16);
    }

    pub fn i16(&mut self, v: i16) {
        self.put(u32::from(v as u16), 16);
    }

    pub fn u32(&mut self, v: u32) {
        self.put(v, 32);
    }

    pub fn bytes(&mut self, b: &[u8]) {
        debug_assert_eq!(self.n, 0, "byte aligned");
        self.out.extend_from_slice(b);
    }

    pub fn leb128(&mut self, v: u32) {
        debug_assert_eq!(self.n, 0, "byte aligned");
        leb128(&mut self.out, v);
    }

    /// A null-terminated UTF-8 string of at most 128 bytes (cut at a
    /// character boundary).
    pub fn string(&mut self, s: &str) {
        let mut end = s.len().min(127);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.bytes(&s.as_bytes()[..end]);
        self.out.push(0);
    }

    pub fn finish(self) -> Vec<u8> {
        debug_assert_eq!(self.n, 0, "byte aligned");
        self.out
    }
}

pub(crate) fn leb128(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// OBU types (§ 3.2).
pub mod kind {
    pub const CODEC_CONFIG: u8 = 0;
    pub const AUDIO_ELEMENT: u8 = 1;
    pub const MIX_PRESENTATION: u8 = 2;
    pub const TEMPORAL_DELIMITER: u8 = 4;
    /// Audio frames of substreams 0 to 17 (`AUDIO_FRAME_ID0 + id`).
    pub const AUDIO_FRAME_ID0: u8 = 6;
    pub const SEQUENCE_HEADER: u8 = 31;
}

/// An OBU: header (type, no redundancy, trimming when given, no
/// extension) and payload.
pub(crate) fn obu(out: &mut Vec<u8>, kind: u8, payload: &[u8], trim: Option<(u32, u32)>) {
    let mut fields = Vec::new();
    if let Some((start, end)) = trim {
        // At the end first, then at the start.
        leb128(&mut fields, end);
        leb128(&mut fields, start);
    }
    out.push((kind << 3) | (u8::from(trim.is_some()) << 1));
    leb128(out, (fields.len() + payload.len()) as u32);
    out.extend_from_slice(&fields);
    out.extend_from_slice(payload);
}

/// Q7.8 fixed point (dB values), clamped to its range.
pub(crate) fn q7_8(db: f64) -> i16 {
    if db.is_nan() {
        return 0;
    }
    (db * 256.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_and_bits_are_as_the_spec_writes_them() {
        let mut v = Vec::new();
        leb128(&mut v, 0);
        leb128(&mut v, 127);
        leb128(&mut v, 128);
        leb128(&mut v, 624_485);
        assert_eq!(v, [0x00, 0x7F, 0x80, 0x01, 0xE5, 0x8E, 0x26]);
        let mut b = Bits::new();
        b.put(1, 3);
        b.put(0, 5);
        b.put(0b1010, 4);
        b.put(0b0110, 4);
        assert_eq!(b.finish(), [0x20, 0xA6]);
        assert_eq!(q7_8(-23.0), -23 * 256);
        assert_eq!(q7_8(f64::NEG_INFINITY), i16::MIN);
        // The sequence header OBU's header (§ 3.4: 0xF8 then its size).
        let mut o = Vec::new();
        obu(&mut o, kind::SEQUENCE_HEADER, b"iamf\x00\x00", None);
        assert_eq!(&o[..2], &[0xF8, 0x06]);
        // Trimming: the flag, the size counting both fields, end first.
        let mut t = Vec::new();
        obu(&mut t, kind::AUDIO_FRAME_ID0, &[1, 2], Some((3, 5)));
        assert_eq!(t, [0x32, 0x04, 0x05, 0x03, 1, 2]);
    }
}

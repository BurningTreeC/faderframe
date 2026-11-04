//! What each key sounds: the table instruments read on the audio thread.

/// A key's place in its scale (for labels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyDegree {
    /// 0 = the tonic.
    pub degree: usize,
    /// Periods above (or below, negative) the middle key's.
    pub period: i32,
    /// The degree's cents within a period.
    pub cents: f64,
}

/// What each of the 128 keys sounds, as a fractional MIDI note number in
/// equal temperament at A4 = 440 Hz (69.0 = A4); NaN = unmapped (the key
/// plays nothing). `Copy` and allocation-free: processors keep their own.
#[derive(Clone, Copy, Debug)]
pub struct TuningTable {
    pitch: [f64; 128],
}

impl PartialEq for TuningTable {
    fn eq(&self, other: &Self) -> bool {
        self.pitch
            .iter()
            .zip(&other.pitch)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl Default for TuningTable {
    fn default() -> Self {
        Self::STANDARD
    }
}

impl TuningTable {
    /// Every key at its own equal-tempered pitch at A4 = 440 Hz.
    pub const STANDARD: TuningTable = {
        let mut pitch = [0.0; 128];
        let mut k = 0;
        while k < 128 {
            pitch[k] = k as f64;
            k += 1;
        }
        TuningTable { pitch }
    };

    /// From pitches per key (NaN: unmapped).
    pub fn from_pitches(pitch: [f64; 128]) -> Self {
        Self { pitch }
    }

    /// The key's pitch (a fractional MIDI note number), `None` when it is
    /// unmapped. Realtime-safe.
    #[inline]
    pub fn pitch(&self, key: u8) -> Option<f64> {
        let p = self.pitch[usize::from(key & 0x7F)];
        (!p.is_nan()).then_some(p)
    }

    /// The key's pitch, an unmapped key's own (for what cannot leave a key
    /// silent: MIDI Tuning Standard messages).
    #[inline]
    pub fn pitch_or_key(&self, key: u8) -> f64 {
        self.pitch(key).unwrap_or(f64::from(key & 0x7F))
    }

    /// Semitones from the key's equal-tempered pitch at 440 Hz (0 for an
    /// unmapped key). Realtime-safe.
    #[inline]
    pub fn offset(&self, key: u8) -> f64 {
        self.pitch(key).map_or(0.0, |p| p - f64::from(key & 0x7F))
    }

    /// Does the key play?
    #[inline]
    pub fn is_mapped(&self, key: u8) -> bool {
        self.pitch(key).is_some()
    }

    /// The key's frequency in Hz.
    #[inline]
    pub fn frequency(&self, key: u8) -> Option<f64> {
        self.pitch(key)
            .map(|p| super::A4_HZ * ((p - 69.0) / 12.0).exp2())
    }

    /// Every key mapped and within a thousandth of a cent of its own pitch.
    pub fn is_standard(&self) -> bool {
        self.pitch
            .iter()
            .enumerate()
            .all(|(k, p)| (p - k as f64).abs() < 1e-5)
    }

    /// A hash of the pitches (equal tables, equal fingerprints; never 0).
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for p in &self.pitch {
            for b in p.to_bits().to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        h.max(1)
    }

    /// The pitches, by key.
    pub fn pitches(&self) -> &[f64; 128] {
        &self.pitch
    }
}

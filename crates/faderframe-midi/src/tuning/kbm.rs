//! Scala keyboard mappings (`.kbm`). After comment lines (`!`), in order:
//! the size of the repeating pattern (0: linear — each key the next
//! degree), the first and last key to retune, the middle key (where the
//! pattern's first entry, degree 0, sits), the reference key and the
//! frequency it sounds at, the degree the pattern repeats at (its "formal
//! octave"), then one entry per key of the pattern: a degree, or `x` for
//! a key that plays nothing. Entries left out are unmapped.

use super::TuningError;
use serde::{Deserialize, Serialize};

/// A keyboard mapping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyboardMap {
    /// Keys in the repeating pattern (0: linear).
    pub size: usize,
    /// Keys outside `first..=last` are left in equal temperament.
    pub first: u8,
    pub last: u8,
    /// The key of the pattern's first entry (degree 0 for a linear map).
    pub middle: u8,
    /// The key that sounds at `reference_hz` (at A4 = 440 Hz).
    pub reference_key: u8,
    pub reference_hz: f64,
    /// The degree one pattern further up starts at (its formal octave).
    pub octave_degree: usize,
    /// Per key of the pattern: its degree, or `None` (it plays nothing).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub map: Vec<Option<i32>>,
}

/// Middle C's frequency in equal temperament at A4 = 440 Hz.
pub(crate) const MIDDLE_C_HZ: f64 = 261.625_565_300_598_6;

impl KeyboardMap {
    /// The standard mapping of a scale of `notes` notes: linear, degree 0
    /// on middle C at its equal-tempered frequency.
    pub fn standard(notes: usize) -> Self {
        Self {
            size: 0,
            first: 0,
            last: 127,
            middle: 60,
            reference_key: 60,
            reference_hz: MIDDLE_C_HZ,
            octave_degree: notes,
            map: Vec::new(),
        }
    }

    /// Read a `.kbm` file's text.
    pub fn parse(text: &str) -> Result<Self, TuningError> {
        let mut lines = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with('!'))
            .filter_map(|(i, l)| l.split_whitespace().next().map(|t| (i, t)));
        const FIELDS: [&str; 7] = [
            "the size of the map",
            "the first key",
            "the last key",
            "the middle key",
            "the reference key",
            "the reference frequency",
            "the octave degree",
        ];
        let mut header = [0.0f64; 7];
        for (field, slot) in FIELDS.iter().zip(header.iter_mut()) {
            let Some((i, token)) = lines.next() else {
                return Err(TuningError::new(format!("{field} is missing")));
            };
            *slot = token
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| TuningError::at(i, format!("{field} is not a number")))?;
        }
        let key = |v: f64, what: &str| -> Result<u8, TuningError> {
            if v.fract() == 0.0 && (0.0..=127.0).contains(&v) {
                Ok(v as u8)
            } else {
                Err(TuningError::new(format!(
                    "{what} must be a key from 0 to 127"
                )))
            }
        };
        let whole = |v: f64, what: &str| -> Result<usize, TuningError> {
            if v.fract() == 0.0 && (0.0..=1024.0).contains(&v) {
                Ok(v as usize)
            } else {
                Err(TuningError::new(format!(
                    "{what} must be a whole number from 0 to 1024"
                )))
            }
        };
        let size = whole(header[0], "the size of the map")?;
        let first = key(header[1], "the first key")?;
        let last = key(header[2], "the last key")?;
        let middle = key(header[3], "the middle key")?;
        let reference_key = key(header[4], "the reference key")?;
        let reference_hz = header[5];
        if reference_hz <= 0.0 {
            return Err(TuningError::new("the reference frequency must be above 0"));
        }
        let octave_degree = whole(header[6], "the octave degree")?;
        let mut map = Vec::with_capacity(size);
        for (i, token) in lines.take(size) {
            if token.eq_ignore_ascii_case("x") {
                map.push(None);
            } else {
                let degree = token
                    .parse::<i32>()
                    .ok()
                    .filter(|d| d.unsigned_abs() <= 1 << 20)
                    .ok_or_else(|| {
                        TuningError::at(i, format!("'{token}' is neither a degree nor x"))
                    })?;
                map.push(Some(degree));
            }
        }
        map.resize(size, None);
        Ok(Self {
            size,
            first,
            last,
            middle,
            reference_key,
            reference_hz,
            octave_degree,
            map,
        })
    }

    /// The scale step key `key` plays (0 = degree 0 of the middle key's
    /// period; `None`: unmapped). The range is not checked here.
    pub fn step_of(&self, key: u8) -> Option<i64> {
        let d = i64::from(key) - i64::from(self.middle);
        if self.size == 0 {
            return Some(d);
        }
        let size = self.size as i64;
        let (pattern, i) = (d.div_euclid(size), d.rem_euclid(size));
        let entry = self.map.get(i as usize).copied().flatten()?;
        Some(i64::from(entry) + pattern * self.octave_degree as i64)
    }

    /// The mapping as a `.kbm` file.
    pub fn to_kbm(&self) -> String {
        let mut out = format!(
            "! Keyboard mapping\n{}\n{}\n{}\n{}\n{}\n{:.6}\n{}\n",
            self.size,
            self.first,
            self.last,
            self.middle,
            self.reference_key,
            self.reference_hz,
            self.octave_degree
        );
        for e in &self.map {
            match e {
                Some(d) => out.push_str(&format!("{d}\n")),
                None => out.push_str("x\n"),
            }
        }
        out
    }
}

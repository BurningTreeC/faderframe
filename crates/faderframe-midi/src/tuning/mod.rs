//! Microtonal tuning: scales, keyboard mappings and what every MIDI key
//! sounds.
//!
//! * [`ScalaScale`] — a Scala scale file (`.scl`): the degrees above the
//!   tonic in cents or as ratios, the last one the period (usually the
//!   octave, 2/1). [`ScalaScale::equal`] makes an equal temperament.
//! * [`KeyboardMap`] — a Scala keyboard mapping (`.kbm`): which key plays
//!   which degree, which key sounds at which frequency, unmapped keys.
//! * [`Tuning`] — a scale with an optional mapping, as a project stores it.
//! * [`TuningTable`] — what each of the 128 keys sounds, as a fractional
//!   MIDI note number in equal temperament at A4 = 440 Hz (69.0 = A4, 69.5
//!   a quarter tone above it; unmapped keys have none). Instruments read
//!   it on the audio thread; [`mts`] encodes it as MIDI Tuning Standard
//!   SysEx for plugins and devices that take it.
//!
//! Without a mapping, degree 0 of the scale sits on middle C (key 60) and
//! sounds at middle C's equal-tempered frequency at the reference pitch,
//! the following keys play the following degrees (the convention of Scala
//! and most microtonal software). A mapping's reference frequency is taken
//! as given at A4 = 440 Hz and moves with the reference pitch, so a
//! reference of 432 Hz lowers every tuning alike.

mod kbm;
pub mod mts;
mod scala;
mod table;

#[cfg(test)]
mod tests;

pub use kbm::KeyboardMap;
pub use scala::{Degree, ScalaScale};
pub use table::{KeyDegree, TuningTable};

use serde::{Deserialize, Serialize};

/// The standard reference pitch (A4) in Hz.
pub const A4_HZ: f64 = 440.0;
/// Lowest and highest reference pitch a project accepts (Hz).
pub const REFERENCE_RANGE: (f64, f64) = (200.0, 1000.0);
/// Most notes a scale may have per period.
pub const MAX_DEGREES: usize = 1024;

/// What went wrong reading a scale or mapping (the 1-based line, when the
/// problem is on one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TuningError {
    pub line: Option<usize>,
    pub message: String,
}

impl TuningError {
    pub(crate) fn at(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line + 1),
            message: message.into(),
        }
    }

    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TuningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for TuningError {}

/// Where a tuning's degrees come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScaleSource {
    /// `steps` equal steps to the octave (12: the standard tuning).
    Equal { steps: u32 },
    /// A Scala scale.
    Scala(ScalaScale),
}

/// A tuning as a project stores it: the scale, and how the keyboard plays
/// it (`None`: the standard mapping, see the module documentation).
/// Files are read into it once; the project keeps the degrees, never a
/// path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tuning {
    /// What menus and the piano roll call it ("19-EDO", the file's name).
    pub name: String,
    pub scale: ScaleSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<KeyboardMap>,
}

impl Tuning {
    /// `steps` equal steps to the octave ("12-TET" for 12, else "n-EDO").
    pub fn equal(steps: u32) -> Self {
        let steps = steps.clamp(1, MAX_DEGREES as u32);
        Self {
            name: if steps == 12 {
                "12-TET".to_string()
            } else {
                format!("{steps}-EDO")
            },
            scale: ScaleSource::Equal { steps },
            mapping: None,
        }
    }

    /// The standard tuning.
    pub fn twelve_tet() -> Self {
        Self::equal(12)
    }

    /// A Scala scale with an optional mapping; checks that they fit.
    pub fn scala(
        name: impl Into<String>,
        scale: ScalaScale,
        mapping: Option<KeyboardMap>,
    ) -> Result<Self, TuningError> {
        let t = Self {
            name: name.into(),
            scale: ScaleSource::Scala(scale),
            mapping,
        };
        t.check()?;
        Ok(t)
    }

    /// The same scale with another mapping (`None`: the standard one).
    pub fn with_mapping(&self, mapping: Option<KeyboardMap>) -> Result<Self, TuningError> {
        let t = Self {
            mapping,
            ..self.clone()
        };
        t.check()?;
        Ok(t)
    }

    /// Twelve equal steps on the standard mapping (what every instrument
    /// plays without a tuning).
    pub fn is_twelve_tet(&self) -> bool {
        self.mapping.is_none() && self.cents_per_degree() == Some(100.0) && self.notes() == 12
    }

    /// Notes per period.
    pub fn notes(&self) -> usize {
        match &self.scale {
            ScaleSource::Equal { steps } => (*steps).max(1) as usize,
            ScaleSource::Scala(s) => s.len(),
        }
    }

    /// The period in cents (1200 for an octave).
    pub fn period_cents(&self) -> f64 {
        match &self.scale {
            ScaleSource::Equal { .. } => 1200.0,
            ScaleSource::Scala(s) => s.period_cents(),
        }
    }

    /// The step of an equal temperament (its cents per degree).
    fn cents_per_degree(&self) -> Option<f64> {
        match &self.scale {
            ScaleSource::Equal { steps } => Some(1200.0 / f64::from((*steps).max(1))),
            ScaleSource::Scala(s) => {
                let n = s.len() as f64;
                let step = s.period_cents() / n;
                s.degrees
                    .iter()
                    .enumerate()
                    .all(|(i, d)| (d.cents - step * (i + 1) as f64).abs() < 1e-6)
                    .then_some(step)
            }
        }
    }

    /// Cents of degree `degree` (0 = the tonic) within a period.
    pub fn degree_cents(&self, degree: usize) -> f64 {
        if degree == 0 {
            return 0.0;
        }
        match &self.scale {
            ScaleSource::Equal { steps } => 1200.0 * degree as f64 / f64::from((*steps).max(1)),
            ScaleSource::Scala(s) => s.degrees.get(degree - 1).map_or(0.0, |d| d.cents),
        }
    }

    /// Cents of scale step `step` (degree 0 of period 0 = 0; may be
    /// negative or beyond a period).
    pub fn step_cents(&self, step: i64) -> f64 {
        let n = self.notes() as i64;
        let (period, degree) = (step.div_euclid(n), step.rem_euclid(n));
        period as f64 * self.period_cents() + self.degree_cents(degree as usize)
    }

    /// The scale's own description (a Scala file's first line).
    pub fn description(&self) -> &str {
        match &self.scale {
            ScaleSource::Equal { .. } => "",
            ScaleSource::Scala(s) => &s.description,
        }
    }

    /// The mapping played (the given one, else the standard one).
    pub fn keyboard(&self) -> KeyboardMap {
        self.keys().into_owned()
    }

    /// The mapping played, borrowed (the standard one costs nothing).
    fn keys(&self) -> std::borrow::Cow<'_, KeyboardMap> {
        match &self.mapping {
            Some(m) => std::borrow::Cow::Borrowed(m),
            None => std::borrow::Cow::Owned(KeyboardMap::standard(self.notes())),
        }
    }

    /// Does the mapping fit the scale (its reference key mapped)?
    pub fn check(&self) -> Result<(), TuningError> {
        if self.notes() == 0 || self.notes() > MAX_DEGREES {
            return Err(TuningError::new(format!(
                "a scale has 1 to {MAX_DEGREES} notes"
            )));
        }
        if self.period_cents() <= 0.0 || !self.period_cents().is_finite() {
            return Err(TuningError::new("the scale's period must rise"));
        }
        let k = self.keys();
        if k.step_of(k.reference_key).is_none() {
            return Err(TuningError::new(format!(
                "the reference key {} is not mapped",
                k.reference_key
            )));
        }
        Ok(())
    }

    /// The scale step key `key` plays (`None`: unmapped or outside the
    /// mapping's range).
    pub fn step_of(&self, key: u8) -> Option<i64> {
        let k = self.keys();
        if key < k.first || key > k.last {
            return None;
        }
        k.step_of(key)
    }

    /// The degree and period of the key (for labels; `None`: unmapped or
    /// left at equal temperament).
    pub fn key_degree(&self, key: u8) -> Option<KeyDegree> {
        let step = self.step_of(key)?;
        let n = self.notes() as i64;
        let degree = step.rem_euclid(n) as usize;
        Some(KeyDegree {
            degree,
            period: step.div_euclid(n) as i32,
            cents: self.degree_cents(degree),
        })
    }

    /// What each key sounds at reference pitch `a4_hz` (A4, Hz).
    pub fn table(&self, a4_hz: f64) -> TuningTable {
        let a4 = if a4_hz.is_finite() && a4_hz > 0.0 {
            a4_hz
        } else {
            A4_HZ
        };
        let shift = 12.0 * (a4 / A4_HZ).log2();
        let k = self.keys();
        // The reference key: its pitch is fixed, the rest are relative to
        // it. An unmapped one (a crafted file) falls back to the middle key.
        let (ref_key, ref_hz) = match k.step_of(k.reference_key) {
            Some(_) => (k.reference_key, k.reference_hz),
            None => (k.middle, k.reference_hz),
        };
        let ref_cents = self.step_cents(k.step_of(ref_key).unwrap_or(0));
        let ref_pitch = 69.0 + 12.0 * (ref_hz.max(1e-6) / A4_HZ).log2() + shift;
        let mut pitch = [f64::NAN; 128];
        for (key, p) in pitch.iter_mut().enumerate() {
            let key = key as u8;
            *p = if key < k.first || key > k.last {
                // Outside the mapping's range: left in equal temperament.
                f64::from(key) + shift
            } else {
                match k.step_of(key) {
                    Some(step) => ref_pitch + (self.step_cents(step) - ref_cents) / 100.0,
                    None => f64::NAN,
                }
            };
        }
        TuningTable::from_pitches(pitch)
    }
}

/// A reference pitch within [`REFERENCE_RANGE`] (others fall back to
/// 440 Hz).
pub fn reference_or_standard(hz: f64) -> f64 {
    if hz.is_finite() && (REFERENCE_RANGE.0..=REFERENCE_RANGE.1).contains(&hz) {
        hz
    } else {
        A4_HZ
    }
}

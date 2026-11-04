//! Scala scale files (`.scl`), as the Scala documentation describes them:
//! comment lines start with `!`; the first other line describes the scale
//! (it may be empty), the next gives the number of notes, then one pitch
//! per line — cents when it has a period (`701.955`), else a ratio (`3/2`,
//! or a whole number `2`). Text after a pitch is ignored. The tonic (1/1)
//! is implicit; the last pitch is the period.

use super::{MAX_DEGREES, TuningError};
use serde::{Deserialize, Serialize};

/// One degree of a scale: its pitch above the tonic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Degree {
    /// Cents above the tonic.
    pub cents: f64,
    /// As the file wrote it ("3/2", "701.955"; empty when computed).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// A scale: the degrees above the tonic, the last one the period.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScalaScale {
    /// The file's description line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub degrees: Vec<Degree>,
}

impl ScalaScale {
    /// Read a `.scl` file's text.
    pub fn parse(text: &str) -> Result<Self, TuningError> {
        let mut lines = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with('!'));
        let Some((_, description)) = lines.next() else {
            return Err(TuningError::new("no description line"));
        };
        let Some((count_line, count)) = lines.next() else {
            return Err(TuningError::new("no number of notes"));
        };
        let count: usize = count
            .split_whitespace()
            .next()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| TuningError::at(count_line, "the number of notes is not a number"))?;
        if count == 0 || count > MAX_DEGREES {
            return Err(TuningError::at(
                count_line,
                format!("a scale has 1 to {MAX_DEGREES} notes"),
            ));
        }
        let mut degrees = Vec::with_capacity(count);
        for (i, line) in lines {
            if degrees.len() == count {
                break;
            }
            let Some(token) = line.split_whitespace().next() else {
                // A blank line among the pitches says nothing.
                continue;
            };
            let cents = pitch_cents(token).map_err(|m| TuningError::at(i, m))?;
            degrees.push(Degree {
                cents,
                text: token.to_string(),
            });
        }
        if degrees.len() < count {
            return Err(TuningError::new(format!(
                "{count} notes announced, {} found",
                degrees.len()
            )));
        }
        let scale = Self {
            description: description.trim().to_string(),
            degrees,
        };
        if scale.period_cents() <= 0.0 {
            return Err(TuningError::new(
                "the last pitch (the period) must be above the tonic",
            ));
        }
        Ok(scale)
    }

    /// `steps` equal steps to the octave.
    pub fn equal(steps: u32) -> Self {
        let steps = steps.clamp(1, MAX_DEGREES as u32);
        Self {
            description: format!("{steps} equal steps to the octave"),
            degrees: (1..=steps)
                .map(|i| Degree {
                    cents: 1200.0 * f64::from(i) / f64::from(steps),
                    text: String::new(),
                })
                .collect(),
        }
    }

    /// Notes per period.
    pub fn len(&self) -> usize {
        self.degrees.len()
    }

    pub fn is_empty(&self) -> bool {
        self.degrees.is_empty()
    }

    /// The period in cents (the last degree).
    pub fn period_cents(&self) -> f64 {
        self.degrees.last().map_or(0.0, |d| d.cents)
    }

    /// The scale as a `.scl` file.
    pub fn to_scl(&self, name: &str) -> String {
        let mut out = format!(
            "! {name}.scl\n!\n{}\n {}\n!\n",
            self.description,
            self.len()
        );
        for d in &self.degrees {
            if d.text.is_empty() {
                out.push_str(&format!(" {:.6}\n", d.cents));
            } else {
                out.push_str(&format!(" {}\n", d.text));
            }
        }
        out
    }
}

/// A pitch's cents above the tonic: `701.955` (cents), `3/2` or `2`
/// (ratios).
fn pitch_cents(token: &str) -> Result<f64, String> {
    if token.contains('.') {
        return token
            .parse::<f64>()
            .ok()
            .filter(|c| c.is_finite())
            .ok_or_else(|| format!("'{token}' is not a number of cents"));
    }
    let (n, d) = token.split_once('/').unwrap_or((token, "1"));
    let whole = |s: &str| -> Option<f64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<f64>().ok())
            .flatten()
    };
    match (whole(n), whole(d)) {
        (Some(n), Some(d)) if n > 0.0 && d > 0.0 => Ok(1200.0 * (n / d).log2()),
        _ => Err(format!("'{token}' is not a ratio of whole numbers above 0")),
    }
}

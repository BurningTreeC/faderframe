//! Pan laws.
//!
//! `pan` is always in `-1.0 ..= 1.0` (hard left .. hard right).
//!
//! * Mono sources feeding a stereo destination are positioned with a
//!   [`PanLaw`]. The default is [`PanLaw::ConstantPower3dB`]: sin/cos law,
//!   -3 dB per side at centre, so perceived loudness stays constant while
//!   panning.
//! * Stereo sources feeding a stereo destination use a *balance* control
//!   ([`stereo_balance`]): centre is 0 dB on both sides and moving the
//!   control attenuates the opposite side linearly.

use serde::{Deserialize, Serialize};
use std::f32::consts::FRAC_PI_2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanLaw {
    /// sin/cos law, -3 dB at centre (default).
    #[default]
    ConstantPower3dB,
    /// Compromise law, -4.5 dB at centre.
    Compromise4_5dB,
    /// Linear law, -6 dB at centre (sums to unity when mono'd).
    Linear6dB,
    /// 0 dB at centre; only the opposite side is attenuated.
    Balance0dB,
}

impl PanLaw {
    /// Left/right gains for a mono source at `pan`.
    #[inline]
    pub fn mono_gains(self, pan: f32) -> (f32, f32) {
        let pan = pan.clamp(-1.0, 1.0);
        match self {
            PanLaw::ConstantPower3dB => constant_power(pan),
            PanLaw::Linear6dB => linear(pan),
            PanLaw::Compromise4_5dB => {
                let (cl, cr) = constant_power(pan);
                let (ll, lr) = linear(pan);
                ((cl * ll).sqrt(), (cr * lr).sqrt())
            }
            PanLaw::Balance0dB => stereo_balance(pan),
        }
    }

    /// Attenuation at centre in dB (for documentation/UI).
    pub fn centre_db(self) -> f32 {
        let (l, _) = self.mono_gains(0.0);
        crate::gain::gain_to_db(l)
    }
}

#[inline]
fn constant_power(pan: f32) -> (f32, f32) {
    let angle = (pan + 1.0) * 0.5 * FRAC_PI_2;
    (angle.cos(), angle.sin())
}

#[inline]
fn linear(pan: f32) -> (f32, f32) {
    ((1.0 - pan) * 0.5, (1.0 + pan) * 0.5)
}

/// Balance gains for a stereo source: 0 dB at centre, opposite side fades out.
#[inline]
pub fn stereo_balance(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// Format a pan value for display ("C", "L42", "R100").
pub fn format_pan(pan: f32) -> String {
    let pct = (pan.clamp(-1.0, 1.0) * 100.0).round() as i32;
    match pct {
        0 => "C".to_string(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}

/// Parse a typed pan: "C", "L42", "42L", "R100", "-42" (left) or "42"
/// (right), in percent. Returns −1…1.
pub fn parse_pan(text: &str) -> Option<f32> {
    let t = text.trim().to_ascii_uppercase();
    let t = t.trim_end_matches('%').trim();
    if t.is_empty() || t == "C" || t == "CENTER" || t == "CENTRE" || t == "MID" {
        return Some(0.0);
    }
    let number = |s: &str| s.trim().parse::<f32>().ok().filter(|v| v.is_finite());
    let pct = if let Some(rest) = t.strip_prefix('L') {
        -number(rest)?
    } else if let Some(rest) = t.strip_suffix('L') {
        -number(rest)?
    } else if let Some(rest) = t.strip_prefix('R') {
        number(rest)?
    } else if let Some(rest) = t.strip_suffix('R') {
        number(rest)?
    } else {
        number(t)?
    };
    Some((pct / 100.0).clamp(-1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_pans() {
        assert_eq!(parse_pan("c"), Some(0.0));
        assert_eq!(parse_pan("L42"), Some(-0.42));
        assert_eq!(parse_pan(" 30 l"), Some(-0.3));
        assert_eq!(parse_pan("R100"), Some(1.0));
        assert_eq!(parse_pan("-25"), Some(-0.25));
        assert_eq!(parse_pan("250"), Some(1.0));
        assert_eq!(parse_pan("left"), None);
        assert_eq!(parse_pan(&format_pan(-0.7)), Some(-0.7));
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn constant_power_law() {
        let law = PanLaw::ConstantPower3dB;
        let (l, r) = law.mono_gains(0.0);
        assert!(approx(l, r));
        assert!(approx(l * l + r * r, 1.0));
        assert!((law.centre_db() + 3.0103).abs() < 1e-3);
        assert!(approx(law.mono_gains(-1.0).0, 1.0));
        assert!(approx(law.mono_gains(-1.0).1, 0.0));
        assert!(approx(law.mono_gains(1.0).1, 1.0));
        // Power is constant across the whole range.
        for i in -10..=10 {
            let (l, r) = law.mono_gains(i as f32 / 10.0);
            assert!(approx(l * l + r * r, 1.0));
        }
    }

    #[test]
    fn linear_and_compromise_laws() {
        assert!((PanLaw::Linear6dB.centre_db() + 6.0206).abs() < 1e-3);
        assert!((PanLaw::Compromise4_5dB.centre_db() + 4.515).abs() < 1e-2);
        assert!(approx(PanLaw::Balance0dB.centre_db(), 0.0));
    }

    #[test]
    fn balance_attenuates_opposite_side() {
        assert_eq!(stereo_balance(0.0), (1.0, 1.0));
        assert_eq!(stereo_balance(0.5), (0.5, 1.0));
        assert_eq!(stereo_balance(-1.0), (1.0, 0.0));
    }

    #[test]
    fn pan_formatting() {
        assert_eq!(format_pan(0.0), "C");
        assert_eq!(format_pan(-0.42), "L42");
        assert_eq!(format_pan(1.0), "R100");
    }
}

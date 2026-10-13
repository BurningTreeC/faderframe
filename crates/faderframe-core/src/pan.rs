//! Pan laws and stereo placement.
//!
//! Positions are always in `-1.0 ..= 1.0` (hard left .. hard right, `0.0`
//! centre). Every function here clamps them and reads NaN as centre
//! ([`position`]), so a bad value can never reach the audio.
//!
//! A channel strip places its input into a stereo destination in one of
//! three ways, each its own operation:
//!
//! * **Mono pan** — a mono source by the project's [`PanLaw`]
//!   ([`PanLaw::mono_gains`], [`mono_pan`]). The default is
//!   [`PanLaw::ConstantPower3dB`].
//! * **Balance** — a stereo source in [`PanMode::Balance`] (the default):
//!   [`stereo_balance`], 0 dB on both sides at centre, the opposite side
//!   attenuated linearly. Left stays left and right stays right: no
//!   crossfeed, nothing summed to mono, and the pan law does not apply.
//! * **Dual pan** — a stereo source in [`PanMode::Dual`]: each input channel
//!   is a mono source placed at its own position by the project's pan law
//!   ([`dual_pan`]). At the default positions ([`DualPan::ORIGINAL`],
//!   left −1 and right +1) it is the identity.
//!
//! Gains are given as a [`PanMatrix`]: `m[source][destination]`, the
//! gain from each input channel to each output channel.

use serde::{Deserialize, Serialize};
use std::f32::consts::FRAC_PI_4;

/// Gains from each source channel to each destination channel:
/// `m[source][destination]` (a mono source uses row 0 only).
pub type PanMatrix = [[f32; 2]; 2];

/// No gain anywhere.
const SILENT: PanMatrix = [[0.0; 2]; 2];

/// A position made safe: clamped to `-1.0 ..= 1.0`, NaN read as centre
/// (infinities clamp to the side they point at).
#[inline]
pub fn position(pan: f32) -> f32 {
    if pan.is_nan() {
        0.0
    } else {
        pan.clamp(-1.0, 1.0)
    }
}

/// How a mono source is spread over a stereo destination: the gain curve
/// of a pan control, named by its attenuation at centre. Project-wide
/// (`Project::pan_law`); it applies to mono pan and to each channel of
/// [`dual_pan`], never to [`stereo_balance`].
///
/// All laws give exactly `(1, 0)` hard left and `(0, 1)` hard right and
/// are mirror images (`gains(-p) == swap(gains(p))`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanLaw {
    /// −3 dB at centre (default): sin/cos, constant power.
    /// `θ = π (p + 1) / 4`, `L = cos θ`, `R = sin θ`, so `L² + R² = 1`
    /// everywhere; centre `L = R = 1/√2` (−3.0103 dB).
    #[default]
    #[serde(rename = "constant_power_3db")]
    ConstantPower3dB,
    /// −4.5 dB at centre: the geometric mean of the −3 dB and −6 dB laws,
    /// `L = √(cos θ · (1 − p) / 2)`, `R = √(sin θ · (1 + p) / 2)`; centre
    /// `2^(−3/4)` = 0.5946 (−4.515 dB). Power dips 1.5 dB at centre, the
    /// sum of the two gains rises 1.5 dB.
    #[serde(rename = "compromise_4_5db")]
    Compromise4_5dB,
    /// −6 dB at centre: linear, constant amplitude.
    /// `L = (1 − p) / 2`, `R = (1 + p) / 2`, so `L + R = 1` everywhere (the
    /// mono sum of the output never changes); centre 0.5 (−6.0206 dB).
    #[serde(rename = "linear_6db")]
    Linear6dB,
    /// 0 dB at centre: `L = min(1, 1 − p)`, `R = min(1, 1 + p)`; only the
    /// far side is attenuated. A centred source is 3 dB louder (power)
    /// than one panned hard to a side.
    #[serde(rename = "centre_0db")]
    Centre0dB,
}

impl PanLaw {
    /// Every law, in menu order.
    pub const ALL: [PanLaw; 4] = [
        PanLaw::ConstantPower3dB,
        PanLaw::Compromise4_5dB,
        PanLaw::Linear6dB,
        PanLaw::Centre0dB,
    ];

    /// Left/right gains for a mono source at `pan`.
    #[inline]
    pub fn mono_gains(self, pan: f32) -> (f32, f32) {
        let pan = position(pan);
        match self {
            PanLaw::ConstantPower3dB => constant_power(pan),
            PanLaw::Linear6dB => linear(pan),
            PanLaw::Compromise4_5dB => {
                let (cl, cr) = constant_power(pan);
                let (ll, lr) = linear(pan);
                ((cl * ll).sqrt(), (cr * lr).sqrt())
            }
            PanLaw::Centre0dB => stereo_balance(pan),
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Attenuation at centre in dB (for documentation/UI).
    pub fn centre_db(self) -> f32 {
        let (l, _) = self.mono_gains(0.0);
        crate::gain::gain_to_db(l)
    }

    /// For menus: the centre attenuation and the curve.
    pub fn label(self) -> &'static str {
        match self {
            PanLaw::ConstantPower3dB => "−3 dB (constant power)",
            PanLaw::Compromise4_5dB => "−4.5 dB (compromise)",
            PanLaw::Linear6dB => "−6 dB (linear)",
            PanLaw::Centre0dB => "0 dB (centre at unity)",
        }
    }

    /// Its place in [`Self::ALL`] (how the engine's parameter slot carries
    /// it).
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|l| *l == self).unwrap_or(0)
    }

    /// The law at `index` in [`Self::ALL`] (rounded; anything else, NaN
    /// included, is the default).
    pub fn from_index(index: f32) -> Self {
        let i = index.round();
        if i >= 0.0 && (i as usize) < Self::ALL.len() {
            Self::ALL[i as usize]
        } else {
            Self::default()
        }
    }
}

/// `sin` of both complementary angles, so the endpoints are exact
/// (`sin 0 = 0`, `sin π/2 = 1`) and the law is exactly symmetric.
#[inline]
fn constant_power(pan: f32) -> (f32, f32) {
    (
        (FRAC_PI_4 * (1.0 - pan)).sin(),
        (FRAC_PI_4 * (1.0 + pan)).sin(),
    )
}

#[inline]
fn linear(pan: f32) -> (f32, f32) {
    ((1.0 - pan) * 0.5, (1.0 + pan) * 0.5)
}

/// Balance gains for a stereo source: 0 dB at centre, opposite side fades
/// out linearly (`L = min(1, 1 − p)`, `R = min(1, 1 + p)`).
#[inline]
pub fn stereo_balance(pan: f32) -> (f32, f32) {
    let pan = position(pan);
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// How a stereo source's pan control works (only where a stereo source
/// feeds a stereo destination; mono sources always use mono pan).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanMode {
    /// One control attenuating the opposite side ([`stereo_balance`]).
    #[default]
    Balance,
    /// Each input channel placed on its own ([`dual_pan`]).
    Dual,
}

impl PanMode {
    pub const ALL: [PanMode; 2] = [PanMode::Balance, PanMode::Dual];

    pub fn label(self) -> &'static str {
        match self {
            PanMode::Balance => "Balance",
            PanMode::Dual => "Dual Pan",
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Where dual pan places a stereo source's two channels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DualPan {
    /// The left input channel's position.
    pub left: f32,
    /// The right input channel's position.
    pub right: f32,
}

impl Default for DualPan {
    fn default() -> Self {
        Self::ORIGINAL
    }
}

impl DualPan {
    /// Left hard left, right hard right: the source as it is.
    pub const ORIGINAL: DualPan = DualPan {
        left: -1.0,
        right: 1.0,
    };

    /// Both positions in range; a NaN goes back to its original side.
    pub fn clamped(self) -> Self {
        let side = |v: f32, original: f32| {
            if v.is_nan() {
                original
            } else {
                v.clamp(-1.0, 1.0)
            }
        };
        Self {
            left: side(self.left, Self::ORIGINAL.left),
            right: side(self.right, Self::ORIGINAL.right),
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::ORIGINAL
    }

    /// One side's position (`right`: the right input channel).
    pub fn side(&self, right: bool) -> f32 {
        if right { self.right } else { self.left }
    }

    /// With one side moved.
    pub fn with_side(self, right: bool, pan: f32) -> Self {
        if right {
            Self { right: pan, ..self }
        } else {
            Self { left: pan, ..self }
        }
    }
}

/// A mono source at `pan` by `law` (row 0; row 1 silent).
#[inline]
pub fn mono_pan(law: PanLaw, pan: f32) -> PanMatrix {
    let (l, r) = law.mono_gains(pan);
    [[l, r], [0.0, 0.0]]
}

/// A stereo source balanced at `pan`: a diagonal matrix (no crossfeed).
#[inline]
pub fn balance(pan: f32) -> PanMatrix {
    let (l, r) = stereo_balance(pan);
    [[l, 0.0], [0.0, r]]
}

/// A stereo source dual-panned: each input channel spread by `law` from
/// its own position.
///
/// ```text
/// out_L = in_L · law(left).L + in_R · law(right).L
/// out_R = in_L · law(left).R + in_R · law(right).R
/// ```
///
/// At [`DualPan::ORIGINAL`] this is the identity (no gain change); moving
/// the positions together narrows the image, moving both the same way
/// moves it, and `(+1, −1)` swaps the channels. Where the two positions
/// overlap the channels add: at the same position the source becomes
/// mono, `(in_L + in_R)` placed there by the law (with the default law,
/// both at centre give `0.707 (in_L + in_R)` on each side — correlated
/// material rises 3 dB, uncorrelated keeps its power). Each channel's
/// power follows the law, so with the −3 dB law the output power of
/// uncorrelated channels never changes.
#[inline]
pub fn dual_pan(law: PanLaw, pan: DualPan) -> PanMatrix {
    let pan = pan.clamped();
    let (ll, lr) = law.mono_gains(pan.left);
    let (rl, rr) = law.mono_gains(pan.right);
    [[ll, lr], [rl, rr]]
}

/// `m` scaled by `gain`.
#[inline]
pub fn scaled(m: PanMatrix, gain: f32) -> PanMatrix {
    let mut out = SILENT;
    for (o, r) in out.iter_mut().zip(m) {
        for (o, g) in o.iter_mut().zip(r) {
            *o = g * gain;
        }
    }
    out
}

/// Format a pan value for display ("C", "L42", "R100").
pub fn format_pan(pan: f32) -> String {
    let pct = (position(pan) * 100.0).round() as i32;
    match pct {
        0 => "C".to_string(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}

/// Format dual-pan positions ("L100 · R100").
pub fn format_dual(pan: DualPan) -> String {
    format!("{} · {}", format_pan(pan.left), format_pan(pan.right))
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
        assert_eq!(parse_pan("nan"), None);
        assert_eq!(parse_pan(&format_pan(-0.7)), Some(-0.7));
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    fn db(g: f32) -> f32 {
        20.0 * g.log10()
    }

    #[test]
    fn constant_power_reference_values() {
        let law = PanLaw::ConstantPower3dB;
        // (position, left, right) from θ = π(p + 1)/4 in f64.
        for p in [-1.0f64, -0.75, -0.5, 0.0, 0.5, 0.75, 1.0] {
            let theta = std::f64::consts::PI * (p + 1.0) / 4.0;
            let (l, r) = law.mono_gains(p as f32);
            assert!(approx(l, theta.cos() as f32), "{p}: {l}");
            assert!(approx(r, theta.sin() as f32), "{p}: {r}");
        }
        // The endpoints are exact, the centre is −3.0103 dB on both sides.
        assert_eq!(law.mono_gains(-1.0), (1.0, 0.0));
        assert_eq!(law.mono_gains(1.0), (0.0, 1.0));
        let (l, r) = law.mono_gains(0.0);
        assert_eq!(l, r);
        assert!(approx(l, std::f32::consts::FRAC_1_SQRT_2));
        assert!((db(l) + 3.0103).abs() < 1e-3);
        // 75 % left: cos(π/16), sin(π/16).
        let (l, r) = law.mono_gains(-0.75);
        assert!(approx(l, 0.980_785_3) && approx(r, 0.195_090_32));
        let (l, r) = law.mono_gains(0.75);
        assert!(approx(l, 0.195_090_32) && approx(r, 0.980_785_3));
    }

    #[test]
    fn constant_power_keeps_the_power() {
        let law = PanLaw::ConstantPower3dB;
        for i in -100..=100 {
            let (l, r) = law.mono_gains(i as f32 / 100.0);
            assert!((l * l + r * r - 1.0).abs() < 1e-6, "{i}");
        }
    }

    #[test]
    fn every_law_reaches_the_sides_exactly_and_mirrors() {
        for law in PanLaw::ALL {
            assert_eq!(law.mono_gains(-1.0), (1.0, 0.0), "{law:?}");
            assert_eq!(law.mono_gains(1.0), (0.0, 1.0), "{law:?}");
            for i in -20..=20 {
                let p = i as f32 / 20.0;
                let (l, r) = law.mono_gains(p);
                let (ml, mr) = law.mono_gains(-p);
                assert!(approx(l, mr) && approx(r, ml), "{law:?} {p}");
                // Moving right never raises the left.
                let (nl, nr) = law.mono_gains(p + 0.05);
                assert!(nl <= l + 1e-7 && nr >= r - 1e-7, "{law:?} {p}");
            }
        }
    }

    #[test]
    fn centre_attenuation_of_each_law() {
        assert!((PanLaw::ConstantPower3dB.centre_db() + 3.0103).abs() < 1e-3);
        assert!((PanLaw::Compromise4_5dB.centre_db() + 4.515).abs() < 1e-2);
        assert!((PanLaw::Linear6dB.centre_db() + 6.0206).abs() < 1e-3);
        assert!(PanLaw::Centre0dB.centre_db().abs() < 1e-6);
        // The linear law keeps the amplitude sum, the compromise sits
        // between the constant-power and the linear law.
        for i in -10..=10 {
            let p = i as f32 / 10.0;
            let (l, r) = PanLaw::Linear6dB.mono_gains(p);
            assert!(approx(l + r, 1.0));
            let (cl, _) = PanLaw::ConstantPower3dB.mono_gains(p);
            let (ml, _) = PanLaw::Compromise4_5dB.mono_gains(p);
            assert!(ml <= cl + 1e-7 && ml >= l - 1e-7, "{p}");
        }
    }

    #[test]
    fn bad_positions_never_reach_the_audio() {
        for law in PanLaw::ALL {
            assert_eq!(law.mono_gains(f32::NAN), law.mono_gains(0.0));
            assert_eq!(law.mono_gains(f32::INFINITY), (0.0, 1.0));
            assert_eq!(law.mono_gains(f32::NEG_INFINITY), (1.0, 0.0));
            assert_eq!(law.mono_gains(7.0), (0.0, 1.0));
            assert_eq!(law.mono_gains(-1.5), (1.0, 0.0));
            for p in [f32::MIN, -1e-30, 1e-30, f32::MAX] {
                let (l, r) = law.mono_gains(p);
                assert!(l.is_finite() && r.is_finite());
            }
        }
        assert_eq!(stereo_balance(f32::NAN), (1.0, 1.0));
        assert_eq!(
            DualPan {
                left: f32::NAN,
                right: f32::INFINITY
            }
            .clamped(),
            DualPan::ORIGINAL
        );
        assert_eq!(
            dual_pan(
                PanLaw::default(),
                DualPan {
                    left: f32::NAN,
                    right: f32::NAN
                }
            ),
            [[1.0, 0.0], [0.0, 1.0]]
        );
        assert_eq!(PanLaw::from_index(f32::NAN), PanLaw::default());
        assert_eq!(PanLaw::from_index(9.0), PanLaw::default());
        for law in PanLaw::ALL {
            assert_eq!(PanLaw::from_index(law.index() as f32), law);
        }
    }

    #[test]
    fn silence_stays_silent_and_finite_input_stays_finite() {
        for law in PanLaw::ALL {
            for i in -4..=4 {
                let p = i as f32 / 4.0;
                let m = dual_pan(law, DualPan { left: p, right: -p });
                for x in [0.0f32, 1.0, -1.0, 1e30, -1e-30] {
                    let out = [x * m[0][0] + x * m[1][0], x * m[0][1] + x * m[1][1]];
                    assert!(out.iter().all(|v| v.is_finite()));
                    if x == 0.0 {
                        assert_eq!(out, [0.0, 0.0]);
                    }
                }
                let (l, r) = law.mono_gains(p);
                assert_eq!((0.0 * l, 0.0 * r), (0.0, 0.0));
            }
        }
    }

    #[test]
    fn balance_attenuates_the_opposite_side_only() {
        assert_eq!(stereo_balance(0.0), (1.0, 1.0));
        assert_eq!(stereo_balance(0.5), (0.5, 1.0));
        assert_eq!(stereo_balance(-0.5), (1.0, 0.5));
        assert_eq!(stereo_balance(-0.75), (1.0, 0.25));
        assert_eq!(stereo_balance(-1.0), (1.0, 0.0));
        assert_eq!(stereo_balance(1.0), (0.0, 1.0));
        // A diagonal matrix: left stays left, right stays right.
        for i in -10..=10 {
            let m = balance(i as f32 / 10.0);
            assert_eq!((m[0][1], m[1][0]), (0.0, 0.0));
            assert!(m[0][0] <= 1.0 && m[1][1] <= 1.0);
        }
        // Unequal sides keep their own levels: L 0.8, R 0.2 at R50.
        let m = balance(0.5);
        let (l, r) = (0.8f32, 0.2f32);
        let out = (l * m[0][0] + r * m[1][0], l * m[0][1] + r * m[1][1]);
        assert_eq!(out, (0.4, 0.2));
    }

    #[test]
    fn dual_pan_at_its_defaults_is_the_identity() {
        for law in PanLaw::ALL {
            assert_eq!(
                dual_pan(law, DualPan::ORIGINAL),
                [[1.0, 0.0], [0.0, 1.0]],
                "{law:?}"
            );
        }
        // As balance at centre: switching modes changes nothing.
        assert_eq!(
            dual_pan(PanLaw::default(), DualPan::default()),
            balance(0.0)
        );
    }

    #[test]
    fn dual_pan_moves_each_channel_on_its_own() {
        let law = PanLaw::ConstantPower3dB;
        // Only the left input moves: row 1 stays where it was.
        let m = dual_pan(
            law,
            DualPan {
                left: 0.0,
                right: 1.0,
            },
        );
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert!(approx(m[0][0], c) && approx(m[0][1], c));
        assert_eq!(m[1], [0.0, 1.0]);
        // Only the right input moves.
        let m = dual_pan(
            law,
            DualPan {
                left: -1.0,
                right: -0.5,
            },
        );
        assert_eq!(m[0], [1.0, 0.0]);
        let (l, r) = law.mono_gains(-0.5);
        assert_eq!(m[1], [l, r]);
        // Narrowed to ±0.5: both channels reach both sides, mirrored.
        let m = dual_pan(
            law,
            DualPan {
                left: -0.5,
                right: 0.5,
            },
        );
        assert!(approx(m[0][0], m[1][1]) && approx(m[0][1], m[1][0]));
        assert!(m[0][0] > m[0][1]);
        // Moved right as a whole: the image keeps its width (0.5 apart).
        let m = dual_pan(
            law,
            DualPan {
                left: 0.25,
                right: 0.75,
            },
        );
        assert_eq!(m[0], {
            let (l, r) = law.mono_gains(0.25);
            [l, r]
        });
        assert_eq!(m[1], {
            let (l, r) = law.mono_gains(0.75);
            [l, r]
        });
        // Swapped: the channels change sides.
        assert_eq!(
            dual_pan(
                law,
                DualPan {
                    left: 1.0,
                    right: -1.0
                }
            ),
            [[0.0, 1.0], [1.0, 0.0]]
        );
        // Hard ends: both left → everything on the left at unity.
        assert_eq!(
            dual_pan(
                law,
                DualPan {
                    left: -1.0,
                    right: -1.0
                }
            ),
            [[1.0, 0.0], [1.0, 0.0]]
        );
    }

    #[test]
    fn dual_pan_keeps_each_channels_power_by_the_law() {
        // With the −3 dB law every input channel's power is spread, never
        // changed: uncorrelated channels keep the output's power.
        let law = PanLaw::ConstantPower3dB;
        for i in -8..=8 {
            for j in -8..=8 {
                let pan = DualPan {
                    left: i as f32 / 8.0,
                    right: j as f32 / 8.0,
                };
                let m = dual_pan(law, pan);
                for row in m {
                    assert!((row[0] * row[0] + row[1] * row[1] - 1.0).abs() < 1e-6);
                }
            }
        }
        // Both centred: the mono sum, by the law (−3 dB: 0.707 · (L + R)).
        let m = dual_pan(
            law,
            DualPan {
                left: 0.0,
                right: 0.0,
            },
        );
        let (l, r) = (0.6f32, -0.2f32);
        let out = [l * m[0][0] + r * m[1][0], l * m[0][1] + r * m[1][1]];
        assert!(approx(out[0], (l + r) * std::f32::consts::FRAC_1_SQRT_2));
        assert_eq!(out[0], out[1]);
        // The −6 dB law makes that the plain average.
        let m = dual_pan(
            PanLaw::Linear6dB,
            DualPan {
                left: 0.0,
                right: 0.0,
            },
        );
        assert_eq!(m, [[0.5, 0.5], [0.5, 0.5]]);
    }

    #[test]
    fn mono_pan_is_row_zero() {
        let m = mono_pan(PanLaw::ConstantPower3dB, -1.0);
        assert_eq!(m, [[1.0, 0.0], [0.0, 0.0]]);
        let m = scaled(mono_pan(PanLaw::Linear6dB, 0.0), 0.5);
        assert_eq!(m, [[0.25, 0.25], [0.0, 0.0]]);
    }

    #[test]
    fn pan_formatting() {
        assert_eq!(format_pan(0.0), "C");
        assert_eq!(format_pan(-0.42), "L42");
        assert_eq!(format_pan(1.0), "R100");
        assert_eq!(format_pan(f32::NAN), "C");
        assert_eq!(format_dual(DualPan::ORIGINAL), "L100 · R100");
    }

    #[test]
    fn saved_names_are_stable() {
        let names: Vec<String> = PanLaw::ALL
            .iter()
            .map(|l| serde_json::to_string(l).unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            [
                "\"constant_power_3db\"",
                "\"compromise_4_5db\"",
                "\"linear_6db\"",
                "\"centre_0db\""
            ]
        );
        assert_eq!(
            serde_json::to_string(&PanMode::Dual).unwrap_or_default(),
            "\"dual\""
        );
    }
}

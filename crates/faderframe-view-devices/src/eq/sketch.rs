//! EQ Sketch: a curve drawn across the display turned into bands.
//!
//! The stroke is read left to right (moving back erases what was drawn
//! past the pointer). Wherever it leaves the zero line a band appears: in
//! the middle a bell at the excursion's extreme, as wide as its
//! half-height; at either end a shelf — or, when the stroke dives steeply
//! and deep there, a cut whose slope is the steepness drawn.

use faderframe_plugin_host::eq::design::{BandShape, BandType, FLAT_TILT_OCTAVES, SLOPES};

/// Distance from the zero line (dB) that starts a band.
const LEAVE: f64 = 1.5;
/// How deep (dB) and steep (dB/oct) an end must go to be a cut.
const CUT_DEPTH: f64 = 10.0;
const CUT_STEEPNESS: f64 = 9.0;
/// Grid resolution (points per octave).
const PER_OCTAVE: f64 = 12.0;

/// A stroke in progress: (frequency, dB) points, left to right.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stroke {
    pub points: Vec<(f64, f64)>,
}

impl Stroke {
    /// The pointer moved to `freq`/`db`: moving back takes back what lay
    /// beyond.
    pub fn push(&mut self, freq: f64, db: f64) {
        while self.points.len() > 1 && self.points.last().is_some_and(|p| p.0 >= freq) {
            self.points.pop();
        }
        if self.points.last().is_none_or(|p| p.0 < freq) {
            self.points.push((freq, db));
        }
    }
}

/// The stroke on a logarithmic grid, linearly interpolated.
fn resample(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let (lo, hi) = (points[0].0, points[points.len() - 1].0);
    let n = ((hi / lo).log2() * PER_OCTAVE).ceil().max(1.0) as usize + 1;
    let mut j = 0;
    (0..n)
        .map(|i| {
            let f = lo * (hi / lo).powf(i as f64 / (n - 1) as f64);
            while j + 2 < points.len() && points[j + 1].0 < f {
                j += 1;
            }
            let (a, b) = (points[j], points[(j + 1).min(points.len() - 1)]);
            let t = if b.0 > a.0 {
                ((f / a.0).ln() / (b.0 / a.0).ln()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (f, a.1 + (b.1 - a.1) * t)
        })
        .collect()
}

/// The slope a cut drawn this steep gets.
fn snap_slope(dbs_per_octave: f64) -> f64 {
    SLOPES
        .iter()
        .copied()
        .filter(|s| (6.0..=96.0).contains(s))
        .min_by(|a, b| {
            (a - dbs_per_octave)
                .abs()
                .total_cmp(&(b - dbs_per_octave).abs())
        })
        .unwrap_or(24.0)
}

/// Q of a bell whose half-height spans `octaves`.
fn q_of(octaves: f64) -> f64 {
    let r = 2f64.powf(octaves.max(0.05));
    (r.sqrt() / (r - 1.0)).clamp(0.1, 12.0)
}

/// The bands a stroke draws, at most `max`, low to high.
pub(crate) fn bands(points: &[(f64, f64)], max: usize) -> Vec<BandShape> {
    if points.len() < 2 || points[points.len() - 1].0 <= points[0].0 * 1.01 {
        return Vec::new();
    }
    let g = resample(points);
    let n = g.len();
    // Runs off the zero line, split where the sign flips.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if g[i].1.abs() <= LEAVE {
            i += 1;
            continue;
        }
        let sign = g[i].1.signum();
        let start = i;
        while i < n && g[i].1.abs() > LEAVE && g[i].1.signum() == sign {
            i += 1;
        }
        runs.push((start, i - 1));
    }
    let octaves = |a: usize, b: usize| (g[b].0 / g[a].0).log2().abs();
    let mut out = Vec::new();
    for (a, b) in runs {
        let (left, right) = (a == 0, b == n - 1);
        let peak = (a..=b)
            .max_by(|x, y| g[*x].1.abs().total_cmp(&g[*y].1.abs()))
            .unwrap_or(a);
        let v = g[peak].1;
        let half = v.abs() / 2.0;
        let shape = |kind, freq, gain, q, slope| BandShape {
            kind,
            freq,
            gain,
            q,
            slope,
        };
        let band = if left && right {
            // Off the line all the way: a straight tilt if it only climbs or
            // only falls, else a broad bell.
            let rising = g.windows(2).all(|w| w[1].1 >= w[0].1 - 0.05);
            let falling = g.windows(2).all(|w| w[1].1 <= w[0].1 + 0.05);
            if (rising || falling) && octaves(0, n - 1) > 1.0 {
                let per_octave = (g[n - 1].1 - g[0].1) / octaves(0, n - 1);
                let centre = (g[0].0 * g[n - 1].0).sqrt();
                let at_centre = (g[0].1 + g[n - 1].1) / 2.0;
                let gain = per_octave * FLAT_TILT_OCTAVES;
                // Pivot where the line crosses zero.
                let pivot = centre
                    * 2f64.powf(-at_centre / per_octave.abs().max(0.1) * per_octave.signum());
                shape(
                    BandType::FlatTilt,
                    pivot.clamp(10.0, 30_000.0),
                    gain.clamp(-30.0, 30.0),
                    1.0,
                    12.0,
                )
            } else {
                shape(BandType::Bell, g[peak].0, v, q_of(octaves(a, b)), 12.0)
            }
        } else if left || right {
            // The edge it starts (or ends) at, and how steeply it comes back.
            let (edge, inner) = if left { (a, b) } else { (b, a) };
            let deep = g[edge].1;
            let crossing = |level: f64| {
                let mut k = edge;
                while k != inner && (g[k].1.abs() > level) {
                    k = if left { k + 1 } else { k - 1 };
                }
                k
            };
            let back = crossing(3.0);
            let deep_at = crossing(deep.abs() * 0.9);
            let steep = (deep.abs() - 3.0) / octaves(deep_at, back).max(0.1);
            if deep < -CUT_DEPTH && steep > CUT_STEEPNESS {
                let kind = if left {
                    BandType::LowCut
                } else {
                    BandType::HighCut
                };
                shape(
                    kind,
                    g[back].0,
                    0.0,
                    std::f64::consts::FRAC_1_SQRT_2,
                    snap_slope(steep),
                )
            } else {
                let kind = if left {
                    BandType::LowShelf
                } else {
                    BandType::HighShelf
                };
                // The plateau's level (the third nearest the edge), and
                // where it is halfway back.
                let len = ((b - a + 1) / 3).max(1);
                let span: Vec<usize> = if left {
                    (a..a + len).collect()
                } else {
                    (b + 1 - len..=b).collect()
                };
                let gain = span.iter().map(|i| g[*i].1).sum::<f64>() / span.len() as f64;
                let mid = crossing(gain.abs() / 2.0);
                shape(kind, g[mid].0, gain, std::f64::consts::FRAC_1_SQRT_2, 12.0)
            }
        } else {
            let mut lo = peak;
            while lo > a && g[lo - 1].1.abs() > half {
                lo -= 1;
            }
            let mut hi = peak;
            while hi < b && g[hi + 1].1.abs() > half {
                hi += 1;
            }
            shape(BandType::Bell, g[peak].0, v, q_of(octaves(lo, hi)), 12.0)
        };
        out.push(BandShape {
            gain: band.gain.clamp(-30.0, 30.0),
            ..band
        });
        if out.len() >= max {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_plugin_host::eq::design::analog_db;

    /// A stroke tracing `shapes` from 20 Hz to 20 kHz.
    fn trace(shapes: &[BandShape]) -> Vec<(f64, f64)> {
        (0..200)
            .map(|i| {
                let f = 20.0 * 1000f64.powf(i as f64 / 199.0);
                (f, shapes.iter().map(|s| analog_db(s, f)).sum())
            })
            .collect()
    }

    #[test]
    fn a_drawn_bell_is_a_bell() {
        let bell = BandShape {
            kind: BandType::Bell,
            freq: 1_000.0,
            gain: 8.0,
            q: 1.0,
            slope: 12.0,
        };
        let got = bands(&trace(&[bell]), 8);
        assert_eq!(got.len(), 1, "{got:?}");
        let b = got[0];
        assert_eq!(b.kind, BandType::Bell);
        assert!((b.freq / 1_000.0).log2().abs() < 0.1, "{b:?}");
        assert!((b.gain - 8.0).abs() < 0.5);
        assert!(b.q > 0.6 && b.q < 1.6, "{b:?}");
    }

    #[test]
    fn ends_become_cuts_or_shelves() {
        let cut = BandShape {
            kind: BandType::LowCut,
            freq: 80.0,
            gain: 0.0,
            q: 0.707,
            slope: 24.0,
        };
        let shelf = BandShape {
            kind: BandType::HighShelf,
            freq: 6_000.0,
            gain: 5.0,
            q: 0.707,
            slope: 12.0,
        };
        let got = bands(&trace(&[cut, shelf]), 8);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].kind, BandType::LowCut);
        assert!((got[0].freq / 80.0).log2().abs() < 0.6, "{:?}", got[0]);
        assert!(got[0].slope >= 18.0, "{:?}", got[0]);
        assert_eq!(got[1].kind, BandType::HighShelf);
        assert!((got[1].gain - 5.0).abs() < 1.0, "{:?}", got[1]);
    }

    #[test]
    fn moving_back_erases() {
        let mut s = Stroke::default();
        for f in [100.0, 200.0, 400.0, 800.0] {
            s.push(f, 3.0);
        }
        s.push(300.0, 1.0);
        assert_eq!(s.points.len(), 3);
        assert_eq!(s.points[2], (300.0, 1.0));
        // A flat line draws nothing.
        assert!(bands(&[(20.0, 0.0), (20_000.0, 0.5)], 8).is_empty());
    }
}

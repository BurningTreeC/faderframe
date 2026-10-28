//! Frequency bands and how FFT bins are summed into them.
//!
//! A band sums the *power* of the bins it covers. Bin `k` of an `N`-point
//! FFT at rate `r` stands for the frequencies `[(k − ½)·r/N, (k + ½)·r/N)`;
//! a bin straddling a band edge is shared between the two bands in
//! proportion to the overlap, so the bands of a contiguous layout add up
//! to the power of the bins they span (no bin is counted twice, none is
//! lost). DC (bin 0) only reaches a band starting below half a bin width,
//! and bands are cut at Nyquist (bands wholly above it are left out).
//!
//! Layouts: fractional-octave bands ([`BandLayout::fractional_octave`],
//! third octaves on the IEC 61260 base-10 series by default), the broad
//! regions engineers talk in ([`BandLayout::broad`]) and any custom edges
//! ([`BandLayout::custom`]).

use thiserror::Error;

/// One band: `lo` to `hi` Hz (`centre` = their geometric mean, or the
/// nominal centre of a fractional-octave band).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub lo: f64,
    pub hi: f64,
    pub centre: f64,
}

impl Band {
    /// The band's width in octaves.
    pub fn octaves(&self) -> f64 {
        (self.hi / self.lo.max(1e-9)).log2()
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum BandError {
    #[error("a band layout needs at least two edges")]
    TooFew,
    #[error("band edges must rise and be positive (edge {0} is not)")]
    NotRising(usize),
}

/// An ordered list of contiguous or separate bands (rising).
#[derive(Clone, Debug, PartialEq)]
pub struct BandLayout {
    bands: Vec<Band>,
}

/// The broad regions (Hz) used to summarise spectra: sub, low, low mids,
/// mids, high mids, presence, air. Conventional names, not a rule about
/// where instruments belong.
pub const BROAD_EDGES: [f64; 8] = [
    20.0, 60.0, 250.0, 500.0, 2_000.0, 4_000.0, 6_000.0, 20_000.0,
];

/// Names of the [`BROAD_EDGES`] regions.
pub const BROAD_NAMES: [&str; 7] = [
    "sub",
    "low",
    "low mids",
    "mids",
    "high mids",
    "presence",
    "air",
];

impl BandLayout {
    /// `per_octave` bands per octave whose centres lie from `lo` to `hi` Hz
    /// (inclusive, nominal), cut at `nyquist`. Third octaves (3) use the
    /// base-10 series of IEC 61260 (centres 1000·10^(k/10) Hz, edges a
    /// twentieth of a decade either side); other resolutions the base-2
    /// series (1000·2^(k/n), edges half a band either side).
    pub fn fractional_octave(per_octave: u32, lo: f64, hi: f64, nyquist: f64) -> Self {
        let n = f64::from(per_octave.max(1));
        let (centre, half): (Box<dyn Fn(i32) -> f64>, f64) = if per_octave == 3 {
            (
                Box::new(|k: i32| 1000.0 * 10f64.powf(f64::from(k) / 10.0)),
                10f64.powf(1.0 / 20.0),
            )
        } else {
            (
                Box::new(move |k: i32| 1000.0 * 2f64.powf(f64::from(k) / n)),
                2f64.powf(0.5 / n),
            )
        };
        let mut bands = Vec::new();
        // Centres from below `lo` to above `hi`: k over a generous range.
        for k in -60..=60 {
            let c = centre(k);
            if c < lo / half.max(1.0001) || c > hi * half {
                continue;
            }
            let (a, b) = (c / half, (c * half).min(nyquist));
            if a >= nyquist || b <= a {
                continue;
            }
            bands.push(Band {
                lo: a,
                hi: b,
                centre: c,
            });
        }
        Self { bands }
    }

    /// Third-octave bands from 25 Hz to 20 kHz (the default analysis
    /// resolution), cut at `nyquist`.
    pub fn third_octaves(nyquist: f64) -> Self {
        Self::fractional_octave(3, 25.0, 20_000.0, nyquist)
    }

    /// Contiguous bands between rising `edges` (Hz).
    pub fn custom(edges: &[f64]) -> Result<Self, BandError> {
        if edges.len() < 2 {
            return Err(BandError::TooFew);
        }
        for (i, w) in edges.windows(2).enumerate() {
            if !(w[0] > 0.0 && w[1] > w[0] && w[1].is_finite()) {
                return Err(BandError::NotRising(i + 1));
            }
        }
        Ok(Self {
            bands: edges
                .windows(2)
                .map(|w| Band {
                    lo: w[0],
                    hi: w[1],
                    centre: (w[0] * w[1]).sqrt(),
                })
                .collect(),
        })
    }

    /// The broad regions ([`BROAD_EDGES`]).
    pub fn broad() -> Self {
        // The edges are constant and rising.
        Self::custom(&BROAD_EDGES).unwrap_or(Self { bands: Vec::new() })
    }

    pub fn len(&self) -> usize {
        self.bands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    pub fn bands(&self) -> &[Band] {
        &self.bands
    }

    pub fn band(&self, i: usize) -> Option<Band> {
        self.bands.get(i).copied()
    }

    /// The band containing `hz`.
    pub fn index_of(&self, hz: f64) -> Option<usize> {
        self.bands.iter().position(|b| hz >= b.lo && hz < b.hi)
    }

    /// Bands `a..=b` as one range (lowest edge to highest).
    pub fn span(&self, a: usize, b: usize) -> Option<(f64, f64)> {
        Some((self.bands.get(a)?.lo, self.bands.get(b)?.hi))
    }

    /// Per band, the bins (and the share of each) whose power it sums for
    /// an `fft_size`-point FFT at `rate` (one-sided: bins 0 to N/2).
    pub fn bin_weights(&self, fft_size: usize, rate: f64) -> BinWeights {
        let n = fft_size.max(2);
        let width = rate / n as f64;
        let last = n / 2;
        let spans = self
            .bands
            .iter()
            .map(|band| {
                let mut out = Vec::new();
                let k0 = ((band.lo / width) - 0.5).floor().max(0.0) as usize;
                let k1 = (((band.hi / width) + 0.5).ceil() as usize).min(last);
                for k in k0..=k1 {
                    // The bin's frequencies (DC and Nyquist are half bins).
                    let a = ((k as f64 - 0.5) * width).max(0.0);
                    let b = ((k as f64 + 0.5) * width).min(last as f64 * width);
                    let overlap = (b.min(band.hi) - a.max(band.lo)).max(0.0);
                    let span = (b - a).max(1e-12);
                    if overlap > 0.0 {
                        out.push((k, (overlap / span) as f32));
                    }
                }
                out
            })
            .collect();
        BinWeights { spans }
    }
}

/// Bins per band with their shares (see [`BandLayout::bin_weights`]).
#[derive(Clone, Debug, PartialEq)]
pub struct BinWeights {
    spans: Vec<Vec<(usize, f32)>>,
}

impl BinWeights {
    pub fn band(&self, i: usize) -> &[(usize, f32)] {
        self.spans.get(i).map_or(&[], |v| v.as_slice())
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

/// A frequency in a short readable form ("80 Hz", "1.25 kHz").
pub fn format_hz(hz: f64) -> String {
    if hz >= 10_000.0 {
        format!("{:.0} kHz", hz / 1000.0)
    } else if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{:.0} kHz", k)
        } else {
            format!("{:.1} kHz", k)
        }
    } else {
        format!("{:.0} Hz", hz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_octaves_cover_the_audible_range_on_nominal_centres() {
        let l = BandLayout::third_octaves(24_000.0);
        assert_eq!(l.len(), 30, "25 Hz to 20 kHz");
        let first = l.band(0).unwrap_or(Band {
            lo: 0.0,
            hi: 0.0,
            centre: 0.0,
        });
        assert!((first.centre - 25.12).abs() < 0.1, "{first:?}");
        // Contiguous: each band starts where the last ends.
        for w in l.bands().windows(2) {
            assert!((w[0].hi - w[1].lo).abs() < 1e-9);
            assert!((w[0].octaves() - 1.0 / 3.0).abs() < 0.01);
        }
        assert_eq!(
            l.index_of(1000.0).map(|i| l.bands()[i].centre.round()),
            Some(1000.0)
        );
        // Cut at a lower Nyquist.
        let low = BandLayout::third_octaves(8_000.0);
        assert!(low.bands().iter().all(|b| b.hi <= 8_000.0));
        assert!(low.len() < l.len());
    }

    #[test]
    fn bin_shares_add_up_to_one_inside_contiguous_bands() {
        let l = BandLayout::third_octaves(24_000.0);
        let w = l.bin_weights(8192, 48_000.0);
        let mut total = vec![0.0f32; 4097];
        for b in 0..l.len() {
            for &(k, s) in w.band(b) {
                total[k] += s;
            }
        }
        // Bins wholly inside 25 Hz–20 kHz are counted exactly once.
        let width = 48_000.0 / 8192.0;
        let inside = |k: usize| {
            let (a, b) = ((k as f64 - 0.5) * width, (k as f64 + 0.5) * width);
            a >= l.bands()[0].lo && b <= l.bands()[l.len() - 1].hi
        };
        for (k, t) in total.iter().enumerate() {
            if inside(k) {
                assert!((t - 1.0).abs() < 1e-5, "bin {k}: {t}");
            }
            assert!(*t <= 1.0 + 1e-5);
        }
        assert_eq!(total[0], 0.0, "no DC");
    }

    #[test]
    fn custom_and_broad_layouts() {
        assert_eq!(BandLayout::custom(&[100.0]), Err(BandError::TooFew));
        assert_eq!(
            BandLayout::custom(&[100.0, 50.0]),
            Err(BandError::NotRising(1))
        );
        let c = BandLayout::custom(&[60.0, 120.0, 240.0]).unwrap_or(BandLayout::broad());
        assert_eq!(c.len(), 2);
        assert_eq!(c.span(0, 1), Some((60.0, 240.0)));
        assert_eq!(BandLayout::broad().len(), BROAD_NAMES.len());
        assert_eq!(format_hz(80.0), "80 Hz");
        assert_eq!(format_hz(1260.0), "1.3 kHz");
        assert_eq!(format_hz(2010.0), "2 kHz");
        assert_eq!(format_hz(12_600.0), "13 kHz");
    }
}

//! Headphone correction: what evens out the headphones, so the head's
//! responses reach the ears as measured. Read from
//!
//! * a parametric EQ in EqualizerAPO's syntax, as AutoEq writes it
//!   (`ParametricEQ.txt`): `Preamp: -6.4 dB`, `Filter 1: ON PK Fc 105 Hz
//!   Gain -3.0 dB Q 0.70`, with PK, LSC/HSC (and LS/HS, LS 6dB …), LP/HP,
//!   LPQ/HPQ, NO, BP and AP filters (RBJ cookbook biquads; `BW Oct` for Q);
//! * a graphic EQ (`GraphicEQ: 20 -6.3; 21 -6.3; …`), made a minimum-phase
//!   FIR at the session's rate (log-frequency interpolation, real cepstrum);
//! * an impulse response (one for both ears, or one per ear), resampled to
//!   the session's rate.

use crate::BinauralError;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterKind {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
    BandPass,
    AllPass,
}

/// One filter of a parametric EQ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filter {
    pub kind: FilterKind,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
}

/// A headphone correction.
#[derive(Clone, Debug)]
pub struct Correction {
    name: String,
    preamp_db: f64,
    filters: Vec<Filter>,
    /// A graphic EQ: (Hz, dB), rising in frequency.
    curve: Vec<(f64, f64)>,
    /// An impulse response per ear at its rate.
    ir: Option<(u32, [Vec<f32>; 2])>,
    serial: u64,
}

static SERIAL: AtomicU64 = AtomicU64::new(1);

/// Longest impulse response kept, at 48 kHz (scaled with the rate).
const MAX_IR: usize = 16_384;

/// What the renderer runs: a gain, biquads (b0, b1, b2, a1, a2) and a FIR
/// per ear.
pub(crate) struct Compiled {
    pub gain: f32,
    pub biquads: Vec<[f32; 5]>,
    pub fir: Option<[Vec<f32>; 2]>,
}

fn bad(line: &str, what: &str) -> BinauralError {
    BinauralError::Correction(format!("{what}: '{}'", line.trim()))
}

fn number(s: &str) -> Option<f64> {
    s.trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

impl Correction {
    /// From a parametric or graphic EQ in EqualizerAPO's syntax.
    pub fn parse(name: &str, text: &str) -> Result<Correction, BinauralError> {
        let mut preamp_db = 0.0;
        let mut filters = Vec::new();
        let mut curve = Vec::new();
        let mut any = false;
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let Some((key, rest)) = l.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            if key == "preamp" {
                let v = rest
                    .split_whitespace()
                    .next()
                    .and_then(number)
                    .ok_or_else(|| bad(line, "no preamp level"))?;
                preamp_db += v;
                any = true;
            } else if key == "filter" || key.starts_with("filter ") {
                if let Some(f) = parse_filter(line, rest)? {
                    filters.push(f);
                }
                any = true;
            } else if key == "graphiceq" {
                for point in rest.split(';') {
                    let mut it = point.split_whitespace();
                    let (Some(f), Some(g)) = (it.next(), it.next()) else {
                        continue;
                    };
                    let (Some(f), Some(g)) = (number(f), number(g)) else {
                        return Err(bad(point, "not a frequency and a level"));
                    };
                    if f > 0.0 {
                        curve.push((f, g));
                    }
                }
                curve.sort_by(|a, b| a.0.total_cmp(&b.0));
                any = true;
            }
        }
        if !any {
            return Err(BinauralError::Correction(format!(
                "{name}: no Preamp, Filter or GraphicEQ lines (EqualizerAPO's syntax)"
            )));
        }
        Ok(Correction {
            name: name.to_string(),
            preamp_db,
            filters,
            curve,
            ir: None,
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// From an impulse response: one channel for both ears, or one per
    /// ear, at `rate`.
    pub fn from_ir(
        name: &str,
        rate: u32,
        channels: &[Vec<f32>],
    ) -> Result<Correction, BinauralError> {
        let first = channels
            .first()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| BinauralError::Correction(format!("{name}: no audio")))?;
        let second = channels.get(1).unwrap_or(first);
        if rate < 8_000 {
            return Err(BinauralError::Correction(format!("{name}: rate {rate}")));
        }
        let max = (MAX_IR as u64 * u64::from(rate) / 48_000) as usize;
        let cut = |x: &[f32]| x.iter().take(max).copied().collect::<Vec<f32>>();
        Ok(Correction {
            name: name.to_string(),
            preamp_db: 0.0,
            filters: Vec::new(),
            curve: Vec::new(),
            ir: Some((rate, [cut(first), cut(second)])),
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Different for every load (graph node keys).
    pub fn serial(&self) -> u64 {
        self.serial
    }

    pub fn filters(&self) -> &[Filter] {
        &self.filters
    }

    pub fn preamp_db(&self) -> f64 {
        self.preamp_db
    }

    /// For menus: "7 filters, preamp −6.4 dB", "graphic EQ (127 points)",
    /// "impulse response (4096 taps)".
    pub fn describe(&self) -> String {
        if let Some((rate, ir)) = &self.ir {
            return format!("impulse response ({} taps at {rate} Hz)", ir[0].len());
        }
        let mut parts = Vec::new();
        if !self.filters.is_empty() {
            parts.push(format!(
                "{} filter{}",
                self.filters.len(),
                if self.filters.len() == 1 { "" } else { "s" }
            ));
        }
        if !self.curve.is_empty() {
            parts.push(format!("graphic EQ ({} points)", self.curve.len()));
        }
        if self.preamp_db != 0.0 {
            parts.push(format!("preamp {:+.1} dB", self.preamp_db));
        }
        if parts.is_empty() {
            "flat".into()
        } else {
            parts.join(", ")
        }
    }

    /// The graphic EQ's level at `freq` (interpolated over log frequency,
    /// held past its ends).
    fn curve_db(&self, freq: f64) -> f64 {
        let c = &self.curve;
        let Some(first) = c.first() else {
            return 0.0;
        };
        if freq <= first.0 {
            return first.1;
        }
        for w in c.windows(2) {
            let (a, b) = (w[0], w[1]);
            if freq <= b.0 {
                let t = (freq.ln() - a.0.ln()) / (b.0.ln() - a.0.ln()).max(1e-12);
                return a.1 + t * (b.1 - a.1);
            }
        }
        c.last().map_or(0.0, |l| l.1)
    }

    /// The level it applies at `freq` (dB) at `rate`, left ear.
    pub fn response_db(&self, freq: f64, rate: u32) -> f64 {
        let fs = f64::from(rate);
        if let Some((r, ir)) = &self.ir {
            let m = crate::magnitude_at(&ir[0], freq, f64::from(*r));
            return 20.0 * m.max(1e-12).log10();
        }
        let mut db = self.preamp_db + self.curve_db(freq);
        let w = std::f64::consts::TAU * freq / fs;
        let z1 = num_complex(w.cos(), -w.sin());
        let z2 = num_complex((2.0 * w).cos(), -(2.0 * w).sin());
        for f in &self.filters {
            let [b0, b1, b2, a0, a1, a2] = coefficients(f, fs);
            let num = add(add(num_complex(b0, 0.0), scale(z1, b1)), scale(z2, b2));
            let den = add(add(num_complex(a0, 0.0), scale(z1, a1)), scale(z2, a2));
            db += 20.0 * (abs(num) / abs(den).max(1e-300)).max(1e-12).log10();
        }
        db
    }

    /// What runs at `rate`.
    pub(crate) fn compile(&self, rate: u32) -> Compiled {
        let fs = f64::from(rate);
        let biquads = self
            .filters
            .iter()
            .filter(|f| f.freq > 0.0 && f.freq < fs * 0.5)
            .map(|f| {
                let [b0, b1, b2, a0, a1, a2] = coefficients(f, fs);
                [
                    (b0 / a0) as f32,
                    (b1 / a0) as f32,
                    (b2 / a0) as f32,
                    (a1 / a0) as f32,
                    (a2 / a0) as f32,
                ]
            })
            .collect();
        let fir = if let Some((r, ir)) = &self.ir {
            let ratio = fs / f64::from(*r);
            Some(if *r == rate {
                ir.clone()
            } else {
                [
                    crate::resample(&ir[0], ratio),
                    crate::resample(&ir[1], ratio),
                ]
            })
        } else if !self.curve.is_empty() {
            let n = if rate > 50_000 { 8_192 } else { 4_096 };
            let h = minimum_phase(|f| self.curve_db(f), fs, n);
            Some([h.clone(), h])
        } else {
            None
        };
        Compiled {
            gain: 10f64.powf(self.preamp_db / 20.0) as f32,
            biquads,
            fir,
        }
    }
}

impl PartialEq for Correction {
    fn eq(&self, other: &Self) -> bool {
        self.serial == other.serial
    }
}

/// One `Filter n: ON PK Fc 105 Hz Gain -3.0 dB Q 0.70` line (`None`: off).
fn parse_filter(line: &str, rest: &str) -> Result<Option<Filter>, BinauralError> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut i = 0;
    match words.first().map(|w| w.to_ascii_uppercase()) {
        Some(w) if w == "OFF" => return Ok(None),
        Some(w) if w == "ON" => i = 1,
        _ => {}
    }
    let kind_word = words
        .get(i)
        .ok_or_else(|| bad(line, "no filter type"))?
        .to_ascii_uppercase();
    i += 1;
    let kind = match kind_word.as_str() {
        "PK" | "PEQ" | "PEAK" => FilterKind::Peak,
        "LS" | "LSC" | "LSQ" | "LOWSHELF" => FilterKind::LowShelf,
        "HS" | "HSC" | "HSQ" | "HIGHSHELF" => FilterKind::HighShelf,
        "LP" | "LPQ" => FilterKind::LowPass,
        "HP" | "HPQ" => FilterKind::HighPass,
        "NO" => FilterKind::Notch,
        "BP" => FilterKind::BandPass,
        "AP" => FilterKind::AllPass,
        _ => return Err(bad(line, "an unknown filter type")),
    };
    let (mut freq, mut gain_db, mut q) = (None, 0.0, None);
    while i < words.len() {
        let w = words[i].to_ascii_lowercase();
        let next = words.get(i + 1).copied().and_then(number);
        match w.as_str() {
            "fc" => {
                freq = next;
                i += 2;
            }
            "gain" => {
                gain_db = next.ok_or_else(|| bad(line, "no gain"))?;
                i += 2;
            }
            "q" => {
                q = next;
                i += 2;
            }
            "bw" => {
                // "BW Oct 1.0": bandwidth in octaves.
                let bw = words.get(i + 2).copied().and_then(number);
                q = bw.filter(|b| *b > 0.0).map(|b| {
                    let p = 2f64.powf(b);
                    p.sqrt() / (p - 1.0)
                });
                i += 3;
            }
            _ => i += 1,
        }
    }
    let freq = freq
        .filter(|f| *f > 0.0)
        .ok_or_else(|| bad(line, "no frequency (Fc)"))?;
    let q = q
        .filter(|q| *q > 0.0)
        .unwrap_or(std::f64::consts::FRAC_1_SQRT_2);
    Ok(Some(Filter {
        kind,
        freq,
        gain_db,
        q,
    }))
}

/// RBJ cookbook coefficients (b0, b1, b2, a0, a1, a2).
fn coefficients(f: &Filter, fs: f64) -> [f64; 6] {
    let w0 = std::f64::consts::TAU * f.freq / fs;
    let (s, c) = w0.sin_cos();
    let alpha = s / (2.0 * f.q);
    let a = 10f64.powf(f.gain_db / 40.0);
    let sa = 2.0 * a.sqrt() * alpha;
    match f.kind {
        FilterKind::Peak => [
            1.0 + alpha * a,
            -2.0 * c,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * c,
            1.0 - alpha / a,
        ],
        FilterKind::LowShelf => [
            a * ((a + 1.0) - (a - 1.0) * c + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sa),
            (a + 1.0) + (a - 1.0) * c + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sa,
        ],
        FilterKind::HighShelf => [
            a * ((a + 1.0) + (a - 1.0) * c + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sa),
            (a + 1.0) - (a - 1.0) * c + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sa,
        ],
        FilterKind::LowPass => [
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        ],
        FilterKind::HighPass => [
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        ],
        FilterKind::Notch => [1.0, -2.0 * c, 1.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha],
        FilterKind::BandPass => [alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * c, 1.0 - alpha],
        FilterKind::AllPass => [
            1.0 - alpha,
            -2.0 * c,
            1.0 + alpha,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        ],
    }
}

// Complex numbers for `response_db` (f64 pairs).
fn num_complex(re: f64, im: f64) -> (f64, f64) {
    (re, im)
}
fn add(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 + b.0, a.1 + b.1)
}
fn scale(a: (f64, f64), k: f64) -> (f64, f64) {
    (a.0 * k, a.1 * k)
}
fn abs(a: (f64, f64)) -> f64 {
    a.0.hypot(a.1)
}

/// A minimum-phase FIR of `n` taps with the magnitude `db(f)`: the real
/// cepstrum of the log magnitude (on a grid four times finer, against
/// aliasing), folded to the causal side, exponentiated and transformed
/// back; the last eighth faded.
fn minimum_phase(db: impl Fn(f64) -> f64, fs: f64, n: usize) -> Vec<f32> {
    use realfft::RealFftPlanner;
    use realfft::num_complex::Complex64;
    let big = 4 * n;
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(big);
    let inv = planner.plan_fft_inverse(big);
    let bins = big / 2 + 1;
    let ln10 = std::f64::consts::LN_10;
    let mut spec: Vec<Complex64> = (0..bins)
        .map(|k| {
            let f = (k as f64 * fs / big as f64).max(fs / big as f64);
            Complex64::new(db(f) / 20.0 * ln10, 0.0)
        })
        .collect();
    let mut cep = vec![0.0f64; big];
    let _ = inv.process(&mut spec, &mut cep);
    for v in &mut cep {
        *v /= big as f64;
    }
    // Fold: the causal part doubled.
    for v in &mut cep[1..big / 2] {
        *v *= 2.0;
    }
    for v in cep.iter_mut().skip(big / 2 + 1) {
        *v = 0.0;
    }
    let mut s = fwd.make_output_vec();
    let _ = fwd.process(&mut cep, &mut s);
    for v in &mut s {
        *v = v.exp();
    }
    // The inverse wants real DC and Nyquist bins.
    s[0].im = 0.0;
    s[bins - 1].im = 0.0;
    let mut h = vec![0.0f64; big];
    let _ = inv.process(&mut s, &mut h);
    let fade = n / 8;
    (0..n)
        .map(|i| {
            let g = if i + fade >= n {
                let t = (i + fade - n) as f64 / fade as f64;
                0.5 + 0.5 * (std::f64::consts::PI * t).cos()
            } else {
                1.0
            };
            (h[i] / big as f64 * g) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTOEQ: &str = "Preamp: -6.4 dB
Filter 1: ON LSC Fc 105 Hz Gain 6.9 dB Q 0.70
Filter 2: ON PK Fc 1000 Hz Gain -4.0 dB Q 1.41
Filter 3: OFF PK Fc 3000 Hz Gain 9.0 dB Q 2.00
Filter 4: ON HSC Fc 10000 Hz Gain -2.0 dB Q 0.70
";

    #[test]
    fn a_parametric_eq_reads_and_shapes_as_written() {
        let c = Correction::parse("AutoEq", AUTOEQ).unwrap();
        assert_eq!(c.filters().len(), 3, "the one that is off is left out");
        assert_eq!(c.preamp_db(), -6.4);
        assert_eq!(c.describe(), "3 filters, preamp -6.4 dB");
        // The peak at its centre: −4 dB plus the preamp and what the
        // shelves add there.
        let at_1k = c.response_db(1000.0, 48_000);
        assert!((at_1k - (-6.4 - 4.0)).abs() < 0.3, "{at_1k}");
        // Deep down the low shelf's full gain, up high the high shelf's.
        assert!((c.response_db(20.0, 48_000) - (-6.4 + 6.9)).abs() < 0.3);
        assert!((c.response_db(20_000.0, 48_000) - (-6.4 - 2.0)).abs() < 0.3);
        // Compiled coefficients give the same response.
        let k = c.compile(48_000);
        assert_eq!(k.biquads.len(), 3);
        assert!((k.gain - 10f32.powf(-6.4 / 20.0)).abs() < 1e-6);
    }

    #[test]
    fn broken_lines_say_where() {
        let e = Correction::parse("x", "Filter 1: ON XY Fc 100 Hz").unwrap_err();
        assert!(e.to_string().contains("unknown filter type"), "{e}");
        let e = Correction::parse("x", "Filter 1: ON PK Gain 3 dB").unwrap_err();
        assert!(e.to_string().contains("Fc"), "{e}");
        assert!(Correction::parse("x", "hello\nworld").is_err());
        // Bandwidth in octaves: one octave is Q ≈ 1.41.
        let c = Correction::parse("x", "Filter: ON PK Fc 500 Hz Gain 3 dB BW Oct 1").unwrap();
        assert!((c.filters()[0].q - std::f64::consts::SQRT_2).abs() < 1e-3);
    }

    /// A graphic EQ becomes a FIR whose magnitude follows the curve.
    #[test]
    fn a_graphic_eq_becomes_a_minimum_phase_fir() {
        let c = Correction::parse(
            "GraphicEQ",
            "GraphicEQ: 20 6; 200 6; 2000 -3; 8000 -3; 20000 0",
        )
        .unwrap();
        let k = c.compile(48_000);
        let fir = k.fir.unwrap();
        for (f, want) in [(100.0, 6.0), (4000.0, -3.0), (600.0, c.curve_db(600.0))] {
            let got = 20.0 * crate::magnitude_at(&fir[0], f, 48_000.0).log10();
            assert!((got - want).abs() < 0.5, "{f} Hz: {got} (want {want})");
        }
        // Minimum phase: the energy at the front.
        let e = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>();
        assert!(e(&fir[0][..256]) > 0.95 * e(&fir[0]));
    }

    #[test]
    fn an_impulse_response_is_taken_per_ear_and_resampled() {
        let mut l = vec![0.0f32; 100];
        l[0] = 0.5;
        let c = Correction::from_ir("ir", 44_100, &[l]).unwrap();
        assert!((c.response_db(1000.0, 48_000) - 20.0 * 0.5f64.log10()).abs() < 0.01);
        let k = c.compile(48_000);
        let fir = k.fir.unwrap();
        assert!(fir[0].len() > 100, "resampled to 48 kHz");
        assert_eq!(fir[0], fir[1], "one channel for both ears");
        assert!(Correction::from_ir("ir", 48_000, &[]).is_err());
    }
}

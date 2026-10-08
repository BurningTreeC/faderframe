//! Colour: what a picture's colours mean (transfer and primaries, read
//! from its stream), and HDR or wide-gamut pictures mapped for an SDR
//! BT.709 screen.
//!
//! An SDR BT.709 picture is shown as its code values (GStreamer converts
//! the matrix and range). Anything else is decoded at 16 bits a channel
//! still in its own transfer and primaries, then mapped here: to linear
//! light (PQ: nits; HLG: the reference display's 1000-nit OOTF; SDR: a
//! 2.4 gamma), to BT.709 primaries, then BT.2390's EETF (on max(R, G, B),
//! so hues stay) from the mastering peak down to HDR's reference white
//! (203 nits, BT.2408) as SDR white — light below about 100 nits is kept,
//! reference white lands just under white and highlights roll off above
//! it — and back to code values with the SDR display's 2.4 gamma.

use serde::{Deserialize, Serialize};

/// How code values turn into light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transfer {
    /// SDR (BT.709, BT.601, sRGB…).
    #[default]
    Sdr,
    /// SMPTE ST 2084 (HDR10, Dolby Vision's base).
    Pq,
    /// ARIB STD-B67 (broadcast HDR).
    Hlg,
}

/// The primaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gamut {
    #[default]
    Bt709,
    Bt2020,
    /// DCI-P3 primaries with a D65 white (Display P3).
    P3,
}

/// What a picture's colours mean.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Colour {
    pub transfer: Transfer,
    pub gamut: Gamut,
    /// The brightest it was mastered to (nits), when the stream says.
    pub peak_nits: Option<f32>,
}

impl Colour {
    /// Whether it needs mapping for an SDR BT.709 screen.
    pub fn needs_mapping(&self) -> bool {
        self.transfer != Transfer::Sdr || self.gamut != Gamut::Bt709
    }

    /// "HDR10 (PQ, BT.2020)", "HLG (BT.2020)", "SDR (BT.2020)", or `None`
    /// for SDR BT.709.
    pub fn label(&self) -> Option<String> {
        let gamut = match self.gamut {
            Gamut::Bt709 => "BT.709",
            Gamut::Bt2020 => "BT.2020",
            Gamut::P3 => "P3",
        };
        match self.transfer {
            Transfer::Pq => Some(format!("HDR (PQ, {gamut})")),
            Transfer::Hlg => Some(format!("HDR (HLG, {gamut})")),
            Transfer::Sdr if self.gamut != Gamut::Bt709 => Some(format!("SDR ({gamut})")),
            Transfer::Sdr => None,
        }
    }

    /// What `caps` say (their colorimetry and mastering display).
    pub fn of_caps(caps: &gst::CapsRef) -> Self {
        let Some(s) = caps.structure(0) else {
            return Self::default();
        };
        let mut c = Self::default();
        if let Ok(text) = s.get::<String>("colorimetry")
            && let Ok(cm) = text.parse::<gst_video::VideoColorimetry>()
        {
            use gst_video::{VideoColorPrimaries as P, VideoTransferFunction as T};
            c.transfer = match cm.transfer() {
                T::Smpte2084 => Transfer::Pq,
                T::AribStdB67 => Transfer::Hlg,
                _ => Transfer::Sdr,
            };
            c.gamut = match cm.primaries() {
                P::Bt2020 => Gamut::Bt2020,
                P::Smpteeg432 | P::Smpterp431 => Gamut::P3,
                _ => Gamut::Bt709,
            };
        }
        // "mastering-display-info": primaries, white point, then max and
        // min luminance in 0.0001 nits (as a string of numbers).
        if let Ok(text) = s.get::<String>("mastering-display-info") {
            let n: Vec<f64> = text
                .split(':')
                .filter_map(|v| v.trim().parse().ok())
                .collect();
            if let Some(&max) = n.get(8)
                && max > 0.0
            {
                c.peak_nits = Some((max / 10_000.0) as f32);
            }
        }
        if c.peak_nits.is_none()
            && let Ok(text) = s.get::<String>("content-light-level")
            && let Some(max_cll) = text.split(':').next().and_then(|v| v.parse::<f32>().ok())
            && max_cll > 0.0
        {
            c.peak_nits = Some(max_cll);
        }
        c
    }
}

/// The tone map for `colour`, made once (its tables take a moment).
pub fn tone_map(colour: Colour) -> std::sync::Arc<ToneMap> {
    use std::sync::{Arc, Mutex};
    static MADE: Mutex<Vec<(Colour, Arc<ToneMap>)>> = Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((_, t)) = made.iter().find(|(c, _)| *c == colour) {
        return Arc::clone(t);
    }
    let t = Arc::new(ToneMap::new(colour));
    made.push((colour, Arc::clone(&t)));
    t
}

/// HDR's reference white (BT.2408): SDR white.
const REFERENCE_WHITE: f64 = 203.0;
/// The SDR display's gamma (BT.1886).
const GAMMA: f64 = 2.4;

/// PQ code value (0–1) to nits.
pub fn pq_to_nits(e: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (
        3424.0 / 4096.0,
        2413.0 / 4096.0 * 32.0,
        2392.0 / 4096.0 * 32.0,
    );
    let p = e.clamp(0.0, 1.0).powf(1.0 / m2);
    10_000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}

/// Nits to PQ code value (0–1).
pub fn nits_to_pq(l: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (
        3424.0 / 4096.0,
        2413.0 / 4096.0 * 32.0,
        2392.0 / 4096.0 * 32.0,
    );
    let y = (l / 10_000.0).clamp(0.0, 1.0).powf(m1);
    ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
}

/// HLG code value (0–1) to scene light (0–1).
fn hlg_to_scene(e: f64) -> f64 {
    let (a, b, c) = (0.178_832_77, 0.284_668_92, 0.559_910_73);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - c) / a).exp() + b) / 12.0
    }
}

/// Linear BT.2020 / P3 (D65) RGB to linear BT.709.
fn to_709(gamut: Gamut) -> [[f64; 3]; 3] {
    match gamut {
        Gamut::Bt709 => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        Gamut::Bt2020 => [
            [1.660_491, -0.587_641, -0.072_850],
            [-0.124_550, 1.132_900, -0.008_349],
            [-0.018_151, -0.100_579, 1.118_730],
        ],
        Gamut::P3 => [
            [1.224_940, -0.224_940, 0.0],
            [-0.042_057, 1.042_057, 0.0],
            [-0.019_638, -0.078_636, 1.098_274],
        ],
    }
}

/// Maps 16-bit RGBA of one colour description to 8-bit SDR BT.709 RGBA,
/// with tables: code value to light, HLG's OOTF by luminance, the
/// roll-off's gain by √light, light to code value.
pub struct ToneMap {
    /// Code value (16 bits) to linear light, SDR white = 1 (HLG: scene
    /// light 0–1).
    linear: Vec<f32>,
    matrix: [[f32; 3]; 3],
    transfer: Transfer,
    /// HLG: the display gain by scene luminance (`OOTF_STEPS`).
    ootf: Vec<f32>,
    /// HDR: the roll-off's gain by √(light / `brightest`).
    gain: Vec<f32>,
    brightest: f32,
    /// BT.2390's knee and target (PQ code values over the source's peak).
    knee: f64,
    target: f64,
    source_peak: f64,
    /// Linear (0–1) to an 8-bit code value.
    encode: Vec<u8>,
}

const ENCODE_STEPS: usize = 4096;
const OOTF_STEPS: usize = 4096;
const GAIN_STEPS: usize = 4096;

impl ToneMap {
    pub fn new(colour: Colour) -> Self {
        let linear = (0..65_536)
            .map(|v| {
                let e = v as f64 / 65_535.0;
                (match colour.transfer {
                    Transfer::Pq => pq_to_nits(e) / REFERENCE_WHITE,
                    Transfer::Hlg => hlg_to_scene(e),
                    Transfer::Sdr => e.powf(GAMMA),
                }) as f32
            })
            .collect();
        let matrix = to_709(colour.gamut).map(|row| row.map(|v| v as f32));
        // The 1000-nit reference display's OOTF (system gamma 1.2).
        let ootf = (0..OOTF_STEPS)
            .map(|i| {
                let y = (i as f64 / (OOTF_STEPS - 1) as f64).max(1e-6);
                (1000.0 / REFERENCE_WHITE * y.powf(0.2)) as f32
            })
            .collect();
        // PQ: the source's peak (mastered, else 1000 nits); HLG: its
        // reference display's 1000 nits.
        let peak = match colour.transfer {
            Transfer::Pq => colour
                .peak_nits
                .map_or(1000.0, f64::from)
                .max(REFERENCE_WHITE),
            _ => 1000.0,
        };
        let source_peak = nits_to_pq(peak);
        let target = nits_to_pq(REFERENCE_WHITE) / source_peak;
        let mut t = Self {
            linear,
            matrix,
            transfer: colour.transfer,
            ootf,
            gain: Vec::new(),
            brightest: (peak / REFERENCE_WHITE) as f32,
            knee: 1.5 * target - 0.5,
            target,
            source_peak,
            encode: (0..ENCODE_STEPS)
                .map(|i| {
                    let x = i as f64 / (ENCODE_STEPS - 1) as f64;
                    (x.powf(1.0 / GAMMA) * 255.0).round().clamp(0.0, 255.0) as u8
                })
                .collect(),
        };
        let brightest = t.brightest as f64;
        t.gain = (0..GAIN_STEPS)
            .map(|i| {
                let r = i as f64 / (GAIN_STEPS - 1) as f64;
                let l = r * r * brightest;
                if l <= 0.0 {
                    1.0
                } else {
                    (t.eetf(l) / l) as f32
                }
            })
            .collect();
        t
    }

    /// BT.2390's EETF: light `l` (SDR white = 1) to light with the
    /// source's peak at SDR white.
    fn eetf(&self, l: f64) -> f64 {
        let e1 = nits_to_pq(l * REFERENCE_WHITE) / self.source_peak;
        let ks = self.knee;
        let e2 = if e1 < ks {
            e1
        } else if ks >= 1.0 {
            e1.min(1.0)
        } else {
            let t = ((e1 - ks) / (1.0 - ks)).min(1.0);
            let (t2, t3) = (t * t, t * t * t);
            (2.0 * t3 - 3.0 * t2 + 1.0) * ks
                + (t3 - 2.0 * t2 + t) * (1.0 - ks)
                + (-2.0 * t3 + 3.0 * t2) * self.target
        };
        pq_to_nits(e2 * self.source_peak) / REFERENCE_WHITE
    }

    /// One pixel's linear BT.709 light (SDR white = 1, unclipped).
    #[inline]
    fn light(&self, px: [u16; 3]) -> [f32; 3] {
        let mut rgb = px.map(|v| self.linear[v as usize]);
        if self.transfer == Transfer::Hlg {
            let y = 0.2627 * rgb[0] + 0.6780 * rgb[1] + 0.0593 * rgb[2];
            let i = (y.clamp(0.0, 1.0) * (OOTF_STEPS - 1) as f32) as usize;
            let g = self.ootf[i];
            rgb = rgb.map(|v| v * g);
        }
        let m = &self.matrix;
        [0, 1, 2].map(|i| m[i][0] * rgb[0] + m[i][1] * rgb[1] + m[i][2] * rgb[2])
    }

    /// The roll-off: light with the source's peak at SDR white.
    #[inline]
    fn roll_off(&self, l: [f32; 3]) -> [f32; 3] {
        let max = l[0].max(l[1]).max(l[2]);
        if max <= 1e-6 {
            return l;
        }
        let k = if max >= self.brightest {
            1.0 / max
        } else {
            let r = (max / self.brightest).sqrt();
            self.gain[(r * (GAIN_STEPS - 1) as f32) as usize]
        };
        l.map(|c| c * k)
    }

    /// Map rows of RGBA64 (`stride` bytes apart) into opaque RGBA8.
    pub fn map(&self, data: &[u8], stride: usize, width: usize, height: usize, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(width * height * 4);
        let hdr = self.transfer != Transfer::Sdr;
        let steps = (ENCODE_STEPS - 1) as f32;
        for y in 0..height {
            let row = &data[y * stride..y * stride + width * 8];
            for px in row.as_chunks::<8>().0 {
                let v = |i: usize| u16::from_le_bytes([px[2 * i], px[2 * i + 1]]);
                let mut l = self.light([v(0), v(1), v(2)]).map(|c| c.max(0.0));
                if hdr {
                    l = self.roll_off(l);
                }
                for c in l {
                    out.push(self.encode[(c.min(1.0) * steps) as usize]);
                }
                out.push(255);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_goes_round() {
        for nits in [0.1, 1.0, 100.0, 203.0, 1000.0, 4000.0] {
            let back = pq_to_nits(nits_to_pq(nits));
            assert!((back - nits).abs() < nits * 1e-6, "{nits} → {back}");
        }
        assert!((nits_to_pq(203.0) - 0.5806).abs() < 0.001);
    }

    #[test]
    fn the_peak_is_sdr_white_and_highlights_roll_off() {
        let t = ToneMap::new(Colour {
            transfer: Transfer::Pq,
            gamut: Gamut::Bt2020,
            peak_nits: Some(1000.0),
        });
        // Below the knee nothing changes; the peak lands on SDR white.
        assert!((t.eetf(0.2) - 0.2).abs() < 1e-9);
        assert!((t.eetf(1000.0 / 203.0) - 1.0).abs() < 0.01);
        // Monotonic in between.
        let mut last = 0.0;
        for i in 1..200 {
            let v = t.eetf(i as f64 * 0.025);
            assert!(v >= last - 1e-9, "{v} after {last}");
            last = v;
        }
        // BT.2020 white is BT.709 white; a highlight is brought down
        // with its hue (the channels keep their ratios).
        let code = |nits: f64| (nits_to_pq(nits) * 65_535.0).round() as u16;
        let w = t.light([code(203.0); 3]);
        for c in w {
            assert!((c - 1.0).abs() < 0.01, "{w:?}");
        }
        let hi = t.roll_off([4.0, 2.0, 1.0]);
        assert!(hi[0] <= 1.0 && hi[0] > 0.9, "{hi:?}");
        assert!((hi[1] / hi[0] - 0.5).abs() < 1e-4 && (hi[2] / hi[0] - 0.25).abs() < 1e-4);
    }
}

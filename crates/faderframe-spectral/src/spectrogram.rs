//! The picture of a source's spectrum: columns over a span of source
//! frames, rows on a logarithmic frequency axis (20 Hz to Nyquist), each
//! cell the level in dB of the loudest bin it covers, mixed over the
//! channels (mean power), packed into a byte from [`FLOOR_DB`] to 0 dBFS
//! (a full-scale sine reads 0).

use crate::{Read, Stft, frame_size};

/// The quietest level shown.
pub const FLOOR_DB: f32 = -120.0;
/// The lowest frequency shown.
pub const LOW_HZ: f32 = 20.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Spectrogram {
    /// Source frames shown.
    pub from: i64,
    pub to: i64,
    pub columns: usize,
    pub rows: usize,
    pub low_hz: f32,
    pub high_hz: f32,
    /// `rows × columns`, row 0 the lowest band; 0 = [`FLOOR_DB`], 255 = 0
    /// dBFS.
    pub levels: Vec<u8>,
}

impl Spectrogram {
    /// The frequency at `row` (fractional; 0 = the bottom edge).
    pub fn hz_at(&self, row: f32) -> f32 {
        self.low_hz * (self.high_hz / self.low_hz).powf(row / self.rows as f32)
    }

    /// The row (fractional) at `hz`.
    pub fn row_at(&self, hz: f32) -> f32 {
        (hz.max(1e-3) / self.low_hz).ln() / (self.high_hz / self.low_hz).ln() * self.rows as f32
    }

    /// The level (dB) of a cell.
    pub fn db(&self, row: usize, column: usize) -> f32 {
        level_db(self.levels[row * self.columns + column])
    }

    /// Compute over source frames `from..to` (`columns` × `rows`).
    pub fn compute(
        read: &mut Read<'_>,
        channels: usize,
        from: i64,
        to: i64,
        columns: usize,
        rows: usize,
        rate: f64,
    ) -> Result<Spectrogram, String> {
        let (columns, rows) = (columns.max(1), rows.max(1));
        let span = (to - from).max(1) as f64;
        // Finer in time when zoomed in far.
        let mut n = frame_size(rate);
        while n > 1024 && span / columns as f64 * 8.0 < n as f64 {
            n /= 2;
        }
        let mut stft = Stft::new(n, rate);
        let high = (rate / 2.0) as f32;
        let low = LOW_HZ.min(high / 2.0);
        // Each row's bins (from, to) and the bin at its centre.
        let bin_of = |hz: f32| f64::from(hz) * n as f64 / rate;
        let ratio = high / low;
        let bands: Vec<(usize, usize, f64)> = (0..rows)
            .map(|r| {
                let a = low * ratio.powf(r as f32 / rows as f32);
                let b = low * ratio.powf((r + 1) as f32 / rows as f32);
                let c = low * ratio.powf((r as f32 + 0.5) / rows as f32);
                let (ka, kb) = (bin_of(a).ceil() as usize, bin_of(b).floor() as usize);
                (ka, kb.min(n / 2), bin_of(c))
            })
            .collect();
        // A full-scale sine's peak bin, for 0 dB.
        let full = (stft.window.iter().map(|w| f64::from(*w)).sum::<f64>() / 2.0).powi(2);
        let mut levels = vec![0u8; rows * columns];
        let mut buf = vec![vec![0.0f32; n]; channels.max(1)];
        let mut power = vec![0.0f64; n / 2 + 1];
        for col in 0..columns {
            let centre = from as f64 + (col as f64 + 0.5) * span / columns as f64;
            read(centre as i64 - n as i64 / 2, &mut buf)?;
            power.fill(0.0);
            for ch in &buf {
                stft.analyse(ch);
                for (p, x) in power.iter_mut().zip(&stft.spectrum) {
                    *p += f64::from(x.norm_sqr());
                }
            }
            for p in &mut power {
                *p /= channels.max(1) as f64 * full;
            }
            for (r, &(ka, kb, kc)) in bands.iter().enumerate() {
                let p = if kb >= ka {
                    power[ka..=kb].iter().copied().fold(0.0, f64::max)
                } else {
                    // Narrower than a bin: between the two nearest.
                    let i = (kc.floor() as usize).min(n / 2 - 1);
                    let k = kc - i as f64;
                    power[i] * (1.0 - k) + power[i + 1] * k
                };
                let db = 10.0 * p.max(1e-30).log10() as f32;
                levels[r * columns + col] = level_byte(db);
            }
        }
        Ok(Spectrogram {
            from,
            to,
            columns,
            rows,
            low_hz: low,
            high_hz: high,
            levels,
        })
    }
}

/// A level in dB as a byte (0 = [`FLOOR_DB`], 255 = 0 dB).
pub fn level_byte(db: f32) -> u8 {
    ((db - FLOOR_DB) / -FLOOR_DB * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// A byte as a level in dB.
pub fn level_db(v: u8) -> f32 {
    FLOOR_DB + f32::from(v) / 255.0 * -FLOOR_DB
}

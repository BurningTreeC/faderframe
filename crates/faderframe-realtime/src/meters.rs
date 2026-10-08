use crate::AtomicF32;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A contiguous range of meter channels in a [`MeterBank`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeterRange {
    pub first: u32,
    pub channels: u16,
}

impl MeterRange {
    #[inline]
    pub fn channel(self, ch: usize) -> Option<u32> {
        (ch < self.channels as usize).then(|| self.first + ch as u32)
    }
}

/// The quasi-peak programme meter's integration: a one-pole on the
/// rectified signal rising with this time constant (seconds) — a 10 ms
/// tone burst reads 2 dB below its steady level, as EBU Tech 3205 wants.
pub const PPM_ATTACK_S: f32 = 0.0033;
/// … and falling 24 dB in 2.8 s (EBU, BBC).
pub const PPM_FALL_DB_PER_S: f32 = 24.0 / 2.8;

#[derive(Debug, Default)]
struct MeterChannel {
    /// Max absolute sample value since the last read.
    peak: AtomicF32,
    /// Max per-block mean square since the last read.
    mean_square: AtomicF32,
    /// Sum of squares (f64 bits) and frames since the last read: the true
    /// RMS over the read period.
    energy: AtomicU64,
    frames: AtomicU64,
    /// Quasi-peak metering is on (only channels a PPM shows pay for it).
    ppm_on: AtomicBool,
    /// The quasi-peak envelope (the audio thread's state) and its maximum
    /// since the last read.
    ppm_env: AtomicF32,
    ppm: AtomicF32,
}

/// One channel's values as consumed by the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterReading {
    /// Linear sample peak since the previous read.
    pub peak: f32,
    /// Linear RMS (of the loudest block) since the previous read.
    pub rms: f32,
    /// Linear RMS of everything since the previous read (0 when nothing
    /// was measured).
    pub mean: f32,
    /// Frames measured since the previous read.
    pub frames: u64,
    /// Linear quasi-peak (EBU PPM ballistics) since the previous read, when
    /// switched on for the channel.
    pub ppm: f32,
}

/// Lock-free meter values flowing from the audio thread to the UI.
///
/// The audio thread *accumulates* (atomic max, or a compare-and-swap add
/// for the energy) and the single control-side reader *consumes* (atomic
/// swap to zero), so no peak is ever lost between UI frames regardless of
/// the relative rates. The GUI never touches DSP state directly.
#[derive(Debug)]
pub struct MeterBank {
    channels: Box<[MeterChannel]>,
    /// Per-sample quasi-peak coefficients for the sample rate.
    ppm_attack: AtomicF32,
    ppm_release: AtomicF32,
}

impl MeterBank {
    pub fn new(capacity: u32) -> Self {
        let bank = Self {
            channels: (0..capacity).map(|_| MeterChannel::default()).collect(),
            ppm_attack: AtomicF32::new(0.0),
            ppm_release: AtomicF32::new(1.0),
        };
        bank.set_rate(48_000.0);
        bank
    }

    pub fn capacity(&self) -> u32 {
        self.channels.len() as u32
    }

    /// The sample rate the quasi-peak ballistics are for.
    pub fn set_rate(&self, rate: f32) {
        let rate = rate.max(1.0);
        self.ppm_attack.store(
            1.0 - (-1.0 / (PPM_ATTACK_S * rate)).exp(),
            Ordering::Relaxed,
        );
        self.ppm_release.store(
            10f32.powf(-PPM_FALL_DB_PER_S / 20.0 / rate),
            Ordering::Relaxed,
        );
    }

    /// Switch quasi-peak metering on or off for a channel (control side).
    pub fn set_ppm(&self, index: u32, on: bool) {
        if let Some(c) = self.channels.get(index as usize) {
            c.ppm_on.store(on, Ordering::Relaxed);
            if !on {
                c.ppm_env.store(0.0, Ordering::Relaxed);
            }
        }
    }

    /// Accumulate a block's statistics (audio thread).
    #[inline]
    pub fn accumulate(&self, index: u32, peak: f32, mean_square: f32) {
        if let Some(c) = self.channels.get(index as usize) {
            c.peak.fetch_max_non_negative(peak, Ordering::Relaxed);
            c.mean_square
                .fetch_max_non_negative(mean_square, Ordering::Relaxed);
        }
    }

    /// Compute peak/mean-square of `samples` and accumulate (audio thread).
    #[inline]
    pub fn measure(&self, index: u32, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let Some(c) = self.channels.get(index as usize) else {
            return;
        };
        let mut peak = 0.0f32;
        let mut sum = 0.0f32;
        for &s in samples {
            peak = peak.max(s.abs());
            sum += s * s;
        }
        self.accumulate(index, peak, sum / samples.len() as f32);
        // The energy: added with a compare-and-swap (the reader may swap
        // it to zero at any moment).
        let mut old = c.energy.load(Ordering::Relaxed);
        loop {
            let new = (f64::from_bits(old) + f64::from(sum)).to_bits();
            match c
                .energy
                .compare_exchange_weak(old, new, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(now) => old = now,
            }
        }
        c.frames.fetch_add(samples.len() as u64, Ordering::Relaxed);
        if c.ppm_on.load(Ordering::Relaxed) {
            let attack = self.ppm_attack.load(Ordering::Relaxed);
            let release = self.ppm_release.load(Ordering::Relaxed);
            let mut env = c.ppm_env.load(Ordering::Relaxed);
            let mut most = 0.0f32;
            for &s in samples {
                let x = s.abs();
                if x > env {
                    env += (x - env) * attack;
                } else {
                    env *= release;
                }
                most = most.max(env);
            }
            // Denormals: the envelope rests at zero.
            if env < 1e-12 {
                env = 0.0;
            }
            c.ppm_env.store(env, Ordering::Relaxed);
            c.ppm.fetch_max_non_negative(most, Ordering::Relaxed);
        }
    }

    /// Consume the accumulated values (control thread).
    #[inline]
    pub fn take(&self, index: u32) -> MeterReading {
        match self.channels.get(index as usize) {
            Some(c) => {
                let energy = f64::from_bits(c.energy.swap(0, Ordering::Relaxed));
                let frames = c.frames.swap(0, Ordering::Relaxed);
                MeterReading {
                    peak: c.peak.swap(0.0, Ordering::Relaxed),
                    rms: c.mean_square.swap(0.0, Ordering::Relaxed).sqrt(),
                    mean: if frames > 0 {
                        (energy / frames as f64).sqrt() as f32
                    } else {
                        0.0
                    },
                    frames,
                    ppm: c.ppm.swap(0.0, Ordering::Relaxed),
                }
            }
            None => MeterReading::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_until_taken() {
        let bank = MeterBank::new(4);
        bank.measure(1, &[0.1, -0.5, 0.2]);
        bank.measure(1, &[0.3, 0.3]);
        let r = bank.take(1);
        assert_eq!(r.peak, 0.5);
        // Loudest block: mean square of [0.1,-0.5,0.2] = 0.1
        assert!((r.rms - 0.1f32.sqrt()).abs() < 1e-6);
        // Everything: (0.01 + 0.25 + 0.04 + 0.09 + 0.09) / 5.
        assert!((r.mean - (0.48f32 / 5.0).sqrt()).abs() < 1e-6);
        assert_eq!(r.frames, 5);
        assert_eq!(r.ppm, 0.0, "off unless asked");
        assert_eq!(bank.take(1), MeterReading::default());
        bank.measure(99, &[1.0]);
    }

    /// EBU Tech 3205: a steady tone reads its peak; a 10 ms burst 2 dB
    /// less, 5 ms about 4 dB less; the reading falls 24 dB in 2.8 s.
    #[test]
    fn the_quasi_peak_meter_keeps_to_the_ebu_ballistics() {
        let rate = 48_000.0f32;
        let bank = MeterBank::new(1);
        bank.set_rate(rate);
        bank.set_ppm(0, true);
        let tone = |ms: f32| -> Vec<f32> {
            (0..(rate * ms / 1000.0) as usize)
                .map(|i| (std::f32::consts::TAU * 5000.0 * i as f32 / rate).sin())
                .collect()
        };
        let reading = |ms: f32| {
            bank.set_ppm(0, false);
            bank.set_ppm(0, true);
            bank.take(0);
            for block in tone(ms).chunks(64) {
                bank.measure(0, block);
            }
            20.0 * bank.take(0).ppm.log10()
        };
        let steady = reading(500.0);
        assert!(steady.abs() < 0.5, "steady: {steady:.2} dB");
        let ten = reading(10.0) - steady;
        assert!((ten + 2.0).abs() < 0.5, "10 ms: {ten:+.2} dB");
        let five = reading(5.0) - steady;
        assert!((-5.5..-3.0).contains(&five), "5 ms: {five:+.2} dB");
        // The fall: 2.8 s of silence after a steady tone.
        reading(500.0);
        let silence = vec![0.0f32; 64];
        for _ in 0..(2.8 * rate / 64.0) as usize {
            bank.measure(0, &silence);
        }
        bank.take(0);
        bank.measure(0, &silence[..1]);
        let fell = 20.0 * bank.take(0).ppm.log10();
        assert!((fell + 24.0).abs() < 1.0, "after 2.8 s: {fell:.1} dB");
    }
}

//! What a built-in plugin's editor sees of its processor, without locks:
//! the live parameter values (automation included), the audio going in and
//! coming out and its sidechain (for analysers), input and output level meters, and a few
//! values the processor publishes (a dynamic band's gain). The editor also
//! talks back through it (the band it wants to hear on its own).
//!
//! Processors only fill the audio rings while an editor watches: the editor
//! calls [`AnalysisTap::watch`] every frame, and a processor that has not
//! seen it for a second of audio stops copying.
//!
//! The meters are PultEQFx's (by Simon Huber, used here under the MIT
//! licence): per channel the highest sample since the editor last looked,
//! the highest since the readout was cleared, and the mean square over
//! 300 ms, both as it stands and as sampled every 200 ms for the figure.

use crate::ParamValues;
use faderframe_realtime::AtomicF32;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

/// Frames each audio ring holds (a power of two).
pub const RING_FRAMES: usize = 1 << 14;
/// Channels the rings and meters keep.
pub const CHANNELS: usize = 2;

/// A stereo audio ring the processor writes and the editor reads.
pub struct AudioRing {
    /// Interleaved samples as `f32` bits.
    data: Box<[AtomicU32]>,
    /// Frames written so far.
    written: AtomicU64,
}

impl Default for AudioRing {
    fn default() -> Self {
        Self {
            data: (0..RING_FRAMES * CHANNELS)
                .map(|_| AtomicU32::new(0))
                .collect(),
            written: AtomicU64::new(0),
        }
    }
}

impl AudioRing {
    /// Append `frames` frames of `left`/`right` (audio thread).
    pub fn push(&self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        let mut at = self.written.load(Ordering::Relaxed) as usize;
        for i in 0..n {
            let k = (at % RING_FRAMES) * CHANNELS;
            self.data[k].store(left[i].to_bits(), Ordering::Relaxed);
            self.data[k + 1].store(right[i].to_bits(), Ordering::Relaxed);
            at += 1;
        }
        self.written.store(at as u64, Ordering::Release);
    }

    /// Frames written so far (editors read the new ones by comparing).
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// The latest `left.len()` frames (at most `RING_FRAMES`), oldest first.
    /// A frame the processor overwrites while it is copied may be a newer
    /// one: harmless for an analyser.
    pub fn latest(&self, left: &mut [f32], right: &mut [f32]) -> u64 {
        let end = self.written();
        let n = left.len().min(right.len()).min(RING_FRAMES);
        let start = end.saturating_sub(n as u64) as usize;
        let have = (end as usize - start).min(n);
        let pad = n - have;
        left[..pad].fill(0.0);
        right[..pad].fill(0.0);
        for i in 0..have {
            let k = ((start + i) % RING_FRAMES) * CHANNELS;
            left[pad + i] = f32::from_bits(self.data[k].load(Ordering::Relaxed));
            right[pad + i] = f32::from_bits(self.data[k + 1].load(Ordering::Relaxed));
        }
        end
    }
}

/// A non-negative `f32` in an atomic. Non-negative floats sort like their
/// bit patterns, so `fetch_max` on the bits raises the value.
#[derive(Default)]
struct Level(AtomicU32);

impl Level {
    fn raise(&self, value: f32) {
        self.0
            .fetch_max(magnitude(value).to_bits(), Ordering::Relaxed);
    }

    fn store(&self, value: f32) {
        self.0.store(magnitude(value).to_bits(), Ordering::Relaxed);
    }

    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
}

/// A NaN is no level; an infinity is a signal that blew up and reads as
/// far over.
fn magnitude(value: f32) -> f32 {
    if value.is_nan() || value <= 0.0 {
        0.0
    } else {
        value.min(f32::MAX)
    }
}

/// One level meter (what arrives, or what leaves).
#[derive(Default)]
pub struct Meter {
    peak: [Level; CHANNELS],
    held: [Level; CHANNELS],
    mean_square: [Level; CHANNELS],
    figure: [Level; CHANNELS],
}

impl Meter {
    /// What the audio thread measured since its last call.
    pub fn publish(&self, channel: usize, peak: f32, mean_square: f32) {
        let Some(slot) = self.peak.get(channel) else {
            return;
        };
        slot.raise(peak);
        self.held[channel].raise(peak);
        self.mean_square[channel].store(mean_square);
    }

    /// The highest sample since this was last asked.
    pub fn take_peak(&self, channel: usize) -> f32 {
        self.peak.get(channel).map_or(0.0, Level::take)
    }

    /// The highest sample since the readout was cleared.
    pub fn held(&self, channel: usize) -> f32 {
        self.held.get(channel).map_or(0.0, Level::load)
    }

    /// The mean square over the last 300 ms.
    pub fn mean_square(&self, channel: usize) -> f32 {
        self.mean_square.get(channel).map_or(0.0, Level::load)
    }

    pub fn publish_figure(&self, channel: usize, mean_square: f32) {
        if let Some(slot) = self.figure.get(channel) {
            slot.store(mean_square);
        }
    }

    /// The mean square the RMS figure shows (sampled every 200 ms).
    pub fn figure(&self, channel: usize) -> f32 {
        self.figure.get(channel).map_or(0.0, Level::load)
    }

    /// Start the held figure again.
    pub fn clear_held(&self) {
        self.held.iter().for_each(|l| l.store(0.0));
    }
}

/// How long the RMS level averages over (seconds).
const RMS_TIME: f64 = 0.3;
/// How often the RMS figure moves (seconds of audio).
const FIGURE_TIME: f64 = 0.2;

/// The audio thread's half of a meter on one channel.
#[derive(Clone, Copy, Default)]
pub struct MeterTap {
    peak: f32,
    mean_square: f64,
    coeff: f64,
    since_figure: usize,
    figure_every: usize,
}

impl MeterTap {
    pub fn new(sample_rate: f32) -> Self {
        let sr = f64::from(sample_rate.max(1.0));
        let figure_every = (FIGURE_TIME * sr).round() as usize;
        Self {
            coeff: 1.0 - (-1.0 / (RMS_TIME * sr)).exp(),
            since_figure: figure_every,
            figure_every,
            ..Self::default()
        }
    }

    #[inline]
    pub fn add(&mut self, sample: f32) {
        let m = sample.abs();
        if m > self.peak {
            self.peak = m;
        }
        if sample.is_finite() {
            let square = f64::from(sample) * f64::from(sample);
            self.mean_square += self.coeff * (square - self.mean_square);
        }
    }

    /// Hand the last `samples` to the editor.
    pub fn publish(&mut self, meter: &Meter, channel: usize, samples: usize) {
        if self.mean_square < 1e-20 {
            self.mean_square = 0.0;
        }
        meter.publish(channel, self.peak, self.mean_square as f32);
        self.peak = 0.0;
        self.since_figure += samples;
        if self.since_figure >= self.figure_every {
            self.since_figure = 0;
            meter.publish_figure(channel, self.mean_square as f32);
        }
    }

    pub fn reset(&mut self) {
        self.peak = 0.0;
        self.mean_square = 0.0;
        self.since_figure = self.figure_every;
    }
}

/// Seconds of audio without a heartbeat after which a processor stops
/// filling the rings.
const WATCH_TIMEOUT: f64 = 1.0;

/// The tap of one built-in plugin instance.
pub struct AnalysisTap {
    /// The live parameter values, shared with the processor.
    pub params: ParamValues,
    /// The audio arriving at the plugin and leaving it, and its sidechain
    /// input (when connected).
    pub input: AudioRing,
    pub output: AudioRing,
    pub sidechain: AudioRing,
    pub meter_in: Meter,
    pub meter_out: Meter,
    /// Values the processor publishes (meaning per plugin).
    values: Box<[AtomicF32]>,
    /// A band the editor wants to hear on its own (`-1`: none).
    listen: AtomicI32,
    heartbeat: AtomicU32,
    /// What a device's editor shows of its content (the samples it
    /// plays, …): set by the instance and read by the editor, both on the
    /// control thread (the audio thread never touches it).
    assets: std::sync::Mutex<Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>>,
}

impl AnalysisTap {
    pub fn new(params: ParamValues, values: usize) -> Self {
        Self {
            params,
            input: AudioRing::default(),
            output: AudioRing::default(),
            sidechain: AudioRing::default(),
            meter_in: Meter::default(),
            meter_out: Meter::default(),
            values: (0..values).map(|_| AtomicF32::new(0.0)).collect(),
            listen: AtomicI32::new(-1),
            heartbeat: AtomicU32::new(0),
            assets: std::sync::Mutex::new(None),
        }
    }

    /// Hand the editor what it shows (control thread only).
    pub fn set_assets(&self, assets: std::sync::Arc<dyn std::any::Any + Send + Sync>) {
        if let Ok(mut a) = self.assets.lock() {
            *a = Some(assets);
        }
    }

    /// What the instance handed the editor, if of type `T` (control
    /// thread only).
    pub fn assets<T: std::any::Any + Send + Sync>(&self) -> Option<std::sync::Arc<T>> {
        let a = self.assets.lock().ok()?.clone()?;
        a.downcast::<T>().ok()
    }

    /// The editor is looking (call every frame).
    pub fn watch(&self) {
        self.heartbeat.fetch_add(1, Ordering::Relaxed);
    }

    pub fn value(&self, i: usize) -> f32 {
        self.values
            .get(i)
            .map_or(0.0, |v| v.load(Ordering::Relaxed))
    }

    pub fn set_value(&self, i: usize, v: f32) {
        if let Some(slot) = self.values.get(i) {
            slot.store(v, Ordering::Relaxed);
        }
    }

    /// Raise a non-negative value to at least `v` (a peak the editor takes
    /// with [`AnalysisTap::take_value`], so none is missed between frames).
    pub fn raise_value(&self, i: usize, v: f32) {
        if let Some(slot) = self.values.get(i) {
            slot.fetch_max_non_negative(magnitude(v), Ordering::Relaxed);
        }
    }

    /// A raised value, reset to zero.
    pub fn take_value(&self, i: usize) -> f32 {
        self.values
            .get(i)
            .map_or(0.0, |slot| slot.swap(0.0, Ordering::Relaxed))
    }

    /// The band to hear on its own, if any.
    pub fn listen(&self) -> Option<usize> {
        usize::try_from(self.listen.load(Ordering::Relaxed)).ok()
    }

    pub fn set_listen(&self, band: Option<usize>) {
        let v = band.map_or(-1, |b| i32::try_from(b).unwrap_or(-1));
        self.listen.store(v, Ordering::Relaxed);
    }
}

/// The processor's view of whether an editor watches its tap.
#[derive(Clone, Copy, Debug, Default)]
pub struct Watching {
    seen: u32,
    quiet: u64,
    timeout: u64,
}

impl Watching {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            seen: 0,
            // Not watched until a heartbeat arrives.
            quiet: u64::MAX / 2,
            timeout: (WATCH_TIMEOUT * f64::from(sample_rate.max(1.0))) as u64,
        }
    }

    /// Whether an editor has looked within the last second of audio (call
    /// once per block of `frames`).
    pub fn check(&mut self, tap: &AnalysisTap, frames: usize) -> bool {
        let beat = tap.heartbeat.load(Ordering::Relaxed);
        if beat != self.seen {
            self.seen = beat;
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(frames as u64);
        }
        self.quiet <= self.timeout
    }
}

/// A linear magnitude in dBFS.
pub fn db(magnitude: f32) -> f32 {
    if magnitude > 0.0 {
        20.0 * magnitude.log10()
    } else {
        f32::NEG_INFINITY
    }
}

/// A mean square in dBFS, as an RMS level.
pub fn db_power(mean_square: f32) -> f32 {
    if mean_square > 0.0 {
        10.0 * mean_square.log10()
    } else {
        f32::NEG_INFINITY
    }
}

/// A level as readouts show it: a tenth of a decibel, overs signed,
/// silence as `-inf`.
pub fn readout(db: f32) -> String {
    if db.is_nan() || db <= -144.0 {
        return "-inf".into();
    }
    let tenths = (db * 10.0).round() / 10.0;
    if tenths > 0.0 {
        format!("+{tenths:.1}")
    } else if tenths == 0.0 {
        "0.0".into()
    } else {
        format!("{tenths:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_returns_the_latest_frames_oldest_first() {
        let ring = AudioRing::default();
        let l: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let r: Vec<f32> = (0..100).map(|i| -(i as f32)).collect();
        ring.push(&l, &r);
        let (mut a, mut b) = (vec![9.0; 8], vec![9.0; 8]);
        assert_eq!(ring.latest(&mut a, &mut b), 100);
        assert_eq!(a, (92..100).map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(b[7], -99.0);
        // More than written: zeros first.
        let (mut a, mut b) = (vec![9.0; 120], vec![9.0; 120]);
        ring.latest(&mut a, &mut b);
        assert_eq!(a[19], 0.0);
        assert_eq!(a[20], 0.0);
        assert_eq!(a[21], 1.0);
        // Wrapping round.
        for _ in 0..200 {
            ring.push(&l, &r);
        }
        let (mut a, mut b) = (vec![0.0; 4], vec![0.0; 4]);
        ring.latest(&mut a, &mut b);
        assert_eq!(a, [96.0, 97.0, 98.0, 99.0]);
    }

    #[test]
    fn a_full_scale_sine_reads_zero_peak_and_three_under_rms() {
        let meter = Meter::default();
        let mut tap = MeterTap::new(48_000.0);
        for block in 0..200 {
            for n in 0..480 {
                let t = (block * 480 + n + 12) as f32;
                tap.add((std::f32::consts::TAU * 1000.0 * t / 48_000.0).sin());
            }
            tap.publish(&meter, 0, 480);
        }
        assert_eq!(readout(db(meter.held(0))), "0.0");
        let rms = db_power(meter.mean_square(0));
        assert!((rms + 3.01).abs() < 0.05, "{rms}");
        assert!(meter.take_peak(0) > 0.99);
        assert_eq!(meter.take_peak(0), 0.0);
        meter.clear_held();
        assert_eq!(meter.held(0), 0.0);
    }

    #[test]
    fn processors_fill_the_rings_only_while_watched() {
        let tap = AnalysisTap::new(ParamValues::new(Vec::new()), 2);
        let mut w = Watching::new(1000.0);
        assert!(!w.check(&tap, 10), "nobody has looked yet");
        tap.watch();
        assert!(w.check(&tap, 10));
        assert!(w.check(&tap, 900), "within a second");
        assert!(!w.check(&tap, 200), "a second without a look");
        tap.watch();
        assert!(w.check(&tap, 10));
        tap.set_listen(Some(3));
        assert_eq!(tap.listen(), Some(3));
        tap.set_listen(None);
        assert_eq!(tap.listen(), None);
        tap.set_value(1, -4.5);
        assert_eq!(tap.value(1), -4.5);
        assert_eq!(tap.value(7), 0.0);
    }
}

//! Capturing signals inside a render: what Mix Intelligence's one-pass
//! analysis reads.
//!
//! An engine given a [`CapturePlan`] builds, besides everything it builds
//! anyway:
//!
//! * a **tap** on the strip of each track the plan names: the track's (a
//!   bus's, a return's) signal as it reaches its destination — after its
//!   devices, fader and pan (its strip's main output), before it is summed
//!   there. Taps are [`NodeRole::Heard`] sinks: the graph aligns them with
//!   the device outputs exactly as it aligns everything the listener hears
//!   (its own delay compensation: nothing is shifted here), so a tap's frame
//!   `k` and the master's frame `k` were played at the same timeline
//!   position, and a render that takes the graph's output latency off the
//!   front takes it off the taps alike;
//! * with [`CapturePlan::devices`], a **probe** after every device of every
//!   track (a container counts as one): two inputs, the device's input and
//!   its output, which the graph aligns with each other (the input delayed
//!   by the device's latency), so their difference is what the device did.
//!   Probes add up energies, peaks, samples over full scale, flattened
//!   peaks and invalid samples ([`ProbeStats`]); they are not aligned with
//!   the outputs (nobody hears them).
//!
//! Realtime: storage is made on the control thread while the graph is
//! built. A tap appends to a buffer of [`CapturePlan::block`] frames behind
//! a [`TryCell`] — it never waits: a busy or full buffer counts the frames
//! as missed — and a probe adds to atomics (one node writes each). Nothing
//! is built without a plan: an engine capturing nothing has no tap or probe
//! and pays nothing. The reader (the thread driving the render, between
//! device callbacks) takes what each tap holds after every callback
//! ([`SignalTap::take`]).

use crate::context::EngineContext;
use faderframe_audio_graph::{AudioBuffer, NodeIo, ProcessContext, Processor};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_realtime::TryCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Channels a tap or probe handles (as many as a strip).
pub const MAX_CHANNELS: usize = super::nodes::MAX_CHANNELS;

/// A run of this many samples of the same magnitude is a flattened peak …
pub const FLAT_RUN: u32 = 3;
/// … at this magnitude or more (−12 dBFS: clipping, not digital silence or
/// a held DC level near nothing).
pub const FLAT_LEVEL: f32 = 0.25;

/// What an engine captures (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapturePlan {
    /// Tracks whose signal is tapped where it leaves the strip.
    pub tracks: Vec<TrackId>,
    /// Probe every device of every track.
    pub devices: bool,
    /// The most frames between two reads (the device callback's length):
    /// what each tap holds.
    pub block: usize,
}

struct TapBuffer {
    data: Vec<Vec<f32>>,
    written: usize,
}

/// One track's signal as it reaches its destination (see the module docs).
pub struct SignalTap {
    track: TrackId,
    channels: usize,
    buffer: TryCell<TapBuffer>,
    missed: AtomicU64,
}

impl SignalTap {
    fn new(track: TrackId, channels: usize, block: usize) -> Self {
        let channels = channels.clamp(1, MAX_CHANNELS);
        Self {
            track,
            channels,
            buffer: TryCell::new(TapBuffer {
                data: vec![vec![0.0; block.max(1)]; channels],
                written: 0,
            }),
            missed: AtomicU64::new(0),
        }
    }

    pub fn track(&self) -> TrackId {
        self.track
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames the audio side could not keep (the buffer full or being
    /// read): a reader that takes after every callback sees none.
    pub fn missed(&self) -> u64 {
        self.missed.load(Ordering::Relaxed)
    }

    /// Reader: hand what was captured since the last take to `f` (every
    /// channel, equal lengths) and empty the buffer. `false` when the
    /// audio side holds it right now (nothing handed over).
    pub fn take(&self, f: impl FnOnce(&[&[f32]])) -> bool {
        let Some(mut b) = self.buffer.try_lock() else {
            return false;
        };
        let n = b.written;
        let mut views: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
        for (v, c) in views.iter_mut().zip(&b.data) {
            *v = &c[..n.min(c.len())];
        }
        f(&views[..self.channels]);
        b.written = 0;
        true
    }

    /// Audio side: append `n` frames of `input`.
    fn write(&self, input: &AudioBuffer, n: usize) {
        let Some(mut b) = self.buffer.try_lock() else {
            self.missed.fetch_add(n as u64, Ordering::Relaxed);
            return;
        };
        let at = b.written;
        let room = b.data.first().map_or(0, Vec::len).saturating_sub(at);
        let k = n.min(room);
        for (c, dst) in b.data.iter_mut().enumerate() {
            let dst = &mut dst[at..at + k];
            if c < input.num_channels() {
                dst.copy_from_slice(&input.channel(c)[..k]);
            } else {
                dst.fill(0.0);
            }
        }
        b.written = at + k;
        if k < n {
            self.missed.fetch_add((n - k) as u64, Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for SignalTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalTap")
            .field("track", &self.track)
            .field("channels", &self.channels)
            .finish()
    }
}

/// What a probe measured of a device (see the module docs): sums over
/// every channel and frame since the render started.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProbeStats {
    pub frames: u64,
    /// Sums of squares of the input and the output.
    pub in_energy: f64,
    pub out_energy: f64,
    /// Σ (output − input)², channel by channel (`None`: the device changes
    /// the channel count — a mono track widened).
    pub diff_energy: Option<f64>,
    /// Highest magnitudes (linear).
    pub in_peak: f32,
    pub out_peak: f32,
    /// Samples above full scale (|x| > 1).
    pub in_above: u64,
    pub out_above: u64,
    /// Runs of [`FLAT_RUN`] samples of exactly the same magnitude, at
    /// [`FLAT_LEVEL`] or more (flattened peaks).
    pub in_flat: u64,
    pub out_flat: u64,
    /// Samples that are not numbers (left out of the sums).
    pub in_invalid: u64,
    pub out_invalid: u64,
}

/// Which device a probe sits after, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeInfo {
    pub track: TrackId,
    pub plugin: PluginInstanceId,
    /// Its place among the track's audio devices (0: the first).
    pub position: usize,
    /// The device after it (`None`: the strip follows).
    pub next: Option<PluginInstanceId>,
    pub bypassed: bool,
    pub in_channels: usize,
    pub out_channels: usize,
}

const FRAMES: usize = 0;
const IN_ENERGY: usize = 1;
const OUT_ENERGY: usize = 2;
const DIFF_ENERGY: usize = 3;
const IN_PEAK: usize = 4;
const OUT_PEAK: usize = 5;
const IN_ABOVE: usize = 6;
const OUT_ABOVE: usize = 7;
const IN_FLAT: usize = 8;
const OUT_FLAT: usize = 9;
const IN_INVALID: usize = 10;
const OUT_INVALID: usize = 11;
const STATS: usize = 12;

/// A device's probe (see the module docs).
pub struct DeviceProbe {
    pub info: ProbeInfo,
    /// Counters and sums (f64/f32 bits where they are not counts): one
    /// node writes them at a time.
    stats: [AtomicU64; STATS],
}

impl DeviceProbe {
    fn new(info: ProbeInfo) -> Self {
        Self {
            info,
            stats: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn get(&self, i: usize) -> u64 {
        self.stats[i].load(Ordering::Relaxed)
    }

    fn add(&self, i: usize, v: u64) {
        if v > 0 {
            self.stats[i].fetch_add(v, Ordering::Relaxed);
        }
    }

    fn add_f64(&self, i: usize, v: f64) {
        let now = f64::from_bits(self.get(i));
        self.stats[i].store((now + v).to_bits(), Ordering::Relaxed);
    }

    fn max_f32(&self, i: usize, v: f32) {
        let now = f32::from_bits(self.get(i) as u32);
        if v > now {
            self.stats[i].store(u64::from(v.to_bits()), Ordering::Relaxed);
        }
    }

    /// What it measured so far.
    pub fn stats(&self) -> ProbeStats {
        let f = |i| f64::from_bits(self.get(i));
        let p = |i| f32::from_bits(self.get(i) as u32);
        ProbeStats {
            frames: self.get(FRAMES),
            in_energy: f(IN_ENERGY),
            out_energy: f(OUT_ENERGY),
            diff_energy: (self.info.in_channels == self.info.out_channels).then(|| f(DIFF_ENERGY)),
            in_peak: p(IN_PEAK),
            out_peak: p(OUT_PEAK),
            in_above: self.get(IN_ABOVE),
            out_above: self.get(OUT_ABOVE),
            in_flat: self.get(IN_FLAT),
            out_flat: self.get(OUT_FLAT),
            in_invalid: self.get(IN_INVALID),
            out_invalid: self.get(OUT_INVALID),
        }
    }
}

impl std::fmt::Debug for DeviceProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceProbe")
            .field("info", &self.info)
            .finish()
    }
}

/// What an engine captures, and where the captured signals are (control
/// thread; the nodes hold their own taps and probes).
pub struct Capture {
    plan: CapturePlan,
    taps: Mutex<Vec<Arc<SignalTap>>>,
    probes: Mutex<Vec<Arc<DeviceProbe>>>,
}

impl Capture {
    pub fn new(plan: CapturePlan) -> Arc<Self> {
        Arc::new(Self {
            plan,
            taps: Mutex::new(Vec::new()),
            probes: Mutex::new(Vec::new()),
        })
    }

    pub fn plan(&self) -> &CapturePlan {
        &self.plan
    }

    /// The taps built (in the plan's order of tracks).
    pub fn taps(&self) -> Vec<Arc<SignalTap>> {
        let taps = self.taps.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out: Vec<Arc<SignalTap>> = taps.clone();
        out.sort_by_key(|t| {
            self.plan
                .tracks
                .iter()
                .position(|x| *x == t.track)
                .unwrap_or(usize::MAX)
        });
        out
    }

    /// `track`'s tap.
    pub fn tap(&self, track: TrackId) -> Option<Arc<SignalTap>> {
        self.taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|t| t.track == track)
            .cloned()
    }

    /// The probes built (in the order they were built: tracks, then their
    /// devices).
    pub fn probes(&self) -> Vec<Arc<DeviceProbe>> {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// `track`'s tap for a graph being built (`None`: not planned). A
    /// rebuild keeps a tap of the same width.
    pub(crate) fn tap_for(&self, track: TrackId, channels: usize) -> Option<Arc<SignalTap>> {
        if !self.plan.tracks.contains(&track) {
            return None;
        }
        let mut taps = self.taps.lock().unwrap_or_else(PoisonError::into_inner);
        let channels = channels.clamp(1, MAX_CHANNELS);
        if let Some(t) = taps
            .iter()
            .find(|t| t.track == track && t.channels == channels)
        {
            return Some(Arc::clone(t));
        }
        taps.retain(|t| t.track != track);
        let tap = Arc::new(SignalTap::new(track, channels, self.plan.block));
        taps.push(Arc::clone(&tap));
        Some(tap)
    }

    /// The probe after a device for a graph being built (`None`: devices
    /// are not probed). A rebuild keeps the probe of the same device in
    /// the same place.
    pub(crate) fn probe_for(&self, info: ProbeInfo) -> Option<Arc<DeviceProbe>> {
        if !self.plan.devices {
            return None;
        }
        let mut probes = self.probes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = probes.iter().find(|p| p.info == info) {
            return Some(Arc::clone(p));
        }
        probes.retain(|p| !(p.info.track == info.track && p.info.plugin == info.plugin));
        let probe = Arc::new(DeviceProbe::new(info));
        probes.push(Arc::clone(&probe));
        Some(probe)
    }
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture").field("plan", &self.plan).finish()
    }
}

/// The node of a [`SignalTap`].
pub(crate) struct TapNode {
    tap: Arc<SignalTap>,
}

impl TapNode {
    pub(crate) fn new(tap: Arc<SignalTap>) -> Self {
        Self { tap }
    }
}

impl Processor<EngineContext> for TapNode {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        if let Some(input) = io.audio_in.first() {
            self.tap.write(input, io.frames.min(input.len()));
        }
    }
}

/// A run of equal magnitudes going on in one channel.
#[derive(Clone, Copy, Default)]
struct Run {
    last: f32,
    len: u32,
}

/// The node of a [`DeviceProbe`]: input 0 the device's input, input 1 its
/// output.
pub(crate) struct ProbeNode {
    probe: Arc<DeviceProbe>,
    runs: [[Run; MAX_CHANNELS]; 2],
}

impl ProbeNode {
    pub(crate) fn new(probe: Arc<DeviceProbe>) -> Self {
        Self {
            probe,
            runs: [[Run::default(); MAX_CHANNELS]; 2],
        }
    }

    /// One side's sums: energy, peak, samples above full scale, flattened
    /// runs, invalid samples.
    fn side(
        buf: &AudioBuffer,
        n: usize,
        runs: &mut [Run; MAX_CHANNELS],
    ) -> (f64, f32, u64, u64, u64) {
        let (mut energy, mut peak, mut above, mut flat, mut invalid) = (0.0f64, 0.0f32, 0, 0, 0);
        for (c, run) in runs.iter_mut().enumerate().take(buf.num_channels()) {
            for &x in &buf.channel(c)[..n] {
                if !x.is_finite() {
                    invalid += 1;
                    run.len = 0;
                    continue;
                }
                let a = x.abs();
                energy += f64::from(x) * f64::from(x);
                peak = peak.max(a);
                if a > 1.0 {
                    above += 1;
                }
                if a >= FLAT_LEVEL && a == run.last {
                    run.len += 1;
                    if run.len + 1 == FLAT_RUN {
                        flat += 1;
                    }
                } else {
                    run.len = 0;
                }
                run.last = a;
            }
        }
        (energy, peak, above, flat, invalid)
    }
}

impl Processor<EngineContext> for ProbeNode {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let (Some(input), Some(output)) = (io.audio_in.first(), io.audio_in.get(1)) else {
            return;
        };
        let n = io.frames.min(input.len()).min(output.len());
        let p = &self.probe;
        let [ins, outs] = &mut self.runs;
        let (ie, ip, ia, ifl, ii) = Self::side(input, n, ins);
        let (oe, op, oa, ofl, oi) = Self::side(output, n, outs);
        if input.num_channels() == output.num_channels() {
            let mut diff = 0.0f64;
            for c in 0..input.num_channels() {
                for (&a, &b) in input.channel(c)[..n].iter().zip(&output.channel(c)[..n]) {
                    if a.is_finite() && b.is_finite() {
                        let d = f64::from(b) - f64::from(a);
                        diff += d * d;
                    }
                }
            }
            p.add_f64(DIFF_ENERGY, diff);
        }
        p.add(FRAMES, n as u64);
        p.add_f64(IN_ENERGY, ie);
        p.add_f64(OUT_ENERGY, oe);
        p.max_f32(IN_PEAK, ip);
        p.max_f32(OUT_PEAK, op);
        p.add(IN_ABOVE, ia);
        p.add(OUT_ABOVE, oa);
        p.add(IN_FLAT, ifl);
        p.add(OUT_FLAT, ofl);
        p.add(IN_INVALID, ii);
        p.add(OUT_INVALID, oi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::ChannelLayout;

    fn buffer(channels: &[&[f32]]) -> AudioBuffer {
        let layout = ChannelLayout::from_channel_count(channels.len());
        let n = channels[0].len();
        let mut b = AudioBuffer::new(layout, n);
        b.set_len(n);
        for (c, x) in channels.iter().enumerate() {
            b.channel_mut(c).copy_from_slice(x);
        }
        b
    }

    #[test]
    fn a_tap_keeps_a_block_and_counts_what_does_not_fit() {
        let tap = SignalTap::new(TrackId(1), 2, 8);
        let b = buffer(&[&[1.0; 5], &[2.0; 5]]);
        tap.write(&b, 5);
        tap.write(&b, 5);
        assert_eq!(tap.missed(), 2);
        let mut got = Vec::new();
        assert!(tap.take(|c| got = c.iter().map(|x| x.to_vec()).collect()));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].len(), 8);
        assert_eq!(got[1][7], 2.0);
        // Emptied.
        assert!(tap.take(|c| assert!(c.iter().all(|x| x.is_empty()))));
    }

    #[test]
    fn a_probe_measures_what_the_device_did() {
        let probe = Arc::new(DeviceProbe::new(ProbeInfo {
            track: TrackId(1),
            plugin: PluginInstanceId(7),
            position: 0,
            next: None,
            bypassed: false,
            in_channels: 1,
            out_channels: 1,
        }));
        let node = ProbeNode::new(Arc::clone(&probe));
        let input: Vec<f32> = (0..64).map(|i| (i as f32 * 0.3).sin() * 0.9).collect();
        // Twice as loud, clipped at 1.
        let output: Vec<f32> = input.iter().map(|v| (v * 2.0).clamp(-1.0, 1.0)).collect();
        let mut runs = [[Run::default(); MAX_CHANNELS]; 2];
        let (ie, ip, ..) = ProbeNode::side(&buffer(&[&input]), 64, &mut runs[0]);
        assert!(ie > 0.0 && ip > 0.8);
        let (_, op, oa, ofl, _) = ProbeNode::side(&buffer(&[&output]), 64, &mut runs[1]);
        assert_eq!(op, 1.0);
        assert_eq!(oa, 0, "clamped at full scale, not above it");
        assert!(ofl > 0, "flattened peaks");
        // Through the node itself.
        node.probe.add(FRAMES, 64);
        assert_eq!(probe.stats().frames, 64);
        assert_eq!(probe.stats().diff_energy, Some(0.0));
    }
}

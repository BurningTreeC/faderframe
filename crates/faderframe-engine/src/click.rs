//! The metronome: a short decaying sine on every beat (accented on bar
//! downbeats), following the tempo and meter maps, mixed into the first two
//! device outputs after the graph. Live playback only — offline renders
//! never contain it.

use faderframe_audio::DeviceBuffers;
use faderframe_timeline::{MusicalTime, Timeline};
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// When the click sounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MetronomeMode {
    #[default]
    Off,
    /// Only while recording (and its pre-roll).
    Recording,
    Always,
}

impl MetronomeMode {
    pub const ALL: [MetronomeMode; 3] = [
        MetronomeMode::Off,
        MetronomeMode::Recording,
        MetronomeMode::Always,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Recording => "While recording",
            Self::Always => "Always",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Recording => "recording",
            Self::Always => "always",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Recording,
            2 => Self::Always,
            _ => Self::Off,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Recording => 1,
            Self::Always => 2,
        }
    }
}

/// Metronome settings shared with the control side.
#[derive(Debug)]
pub struct MetronomeShared {
    mode: AtomicU8,
    gain_bits: AtomicU32,
}

impl Default for MetronomeShared {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(0),
            gain_bits: AtomicU32::new(0.35f32.to_bits()),
        }
    }
}

impl MetronomeShared {
    pub fn set_mode(&self, mode: MetronomeMode) {
        self.mode.store(mode.to_u8(), Ordering::Relaxed);
    }

    pub fn mode(&self) -> MetronomeMode {
        MetronomeMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain_bits
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }
}

const CLICK_SECONDS: f64 = 0.045;
const MAX_CLICKS_PER_BLOCK: usize = 8;

/// Audio-thread click voice.
#[derive(Debug, Default)]
pub(crate) struct Click {
    remaining: usize,
    phase: f32,
    inc: f32,
    amp: f32,
    decay: f32,
}

impl Click {
    pub(crate) fn reset(&mut self) {
        self.remaining = 0;
    }

    fn start(&mut self, accent: bool, rate: f64, gain: f32) {
        let freq = if accent { 1760.0 } else { 1320.0 };
        self.inc = (std::f64::consts::TAU * freq / rate) as f32;
        self.phase = 0.0;
        self.amp = gain * if accent { 1.0 } else { 0.7 };
        self.remaining = (CLICK_SECONDS * rate) as usize;
        self.decay = (-1.0 / (0.009 * rate)).exp() as f32;
    }

    /// Render clicks for device frames `offset..offset + n` playing at
    /// timeline position `pos`. Realtime-safe.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        io: &mut dyn DeviceBuffers,
        offset: usize,
        n: usize,
        pos: i64,
        timeline: &Timeline,
        rate: f64,
        gain: f32,
    ) {
        // Beats starting inside this block.
        let mut starts = [(usize::MAX, false); MAX_CLICKS_PER_BLOCK];
        let mut count = 0;
        let q0 = timeline.tempo.samples_to_quarters(pos, rate);
        let q1 = timeline.tempo.samples_to_quarters(pos + n as i64, rate);
        let mut bar = timeline.meter.bar_at(MusicalTime::from_quarters(q0));
        let mut bar_start = timeline.meter.bar_start(bar).quarters();
        let mut beat_len = timeline
            .meter
            .signature_of_bar(bar)
            .beat_length()
            .quarters();
        let mut k = ((q0 - bar_start) / beat_len - 1e-9).ceil().max(0.0);
        loop {
            let mut beat = bar_start + k * beat_len;
            let next_bar = timeline.meter.bar_start(bar + 1).quarters();
            if beat >= next_bar - 1e-9 {
                bar += 1;
                bar_start = next_bar;
                beat_len = timeline
                    .meter
                    .signature_of_bar(bar)
                    .beat_length()
                    .quarters();
                k = 0.0;
                beat = bar_start;
            }
            if beat >= q1 || count == MAX_CLICKS_PER_BLOCK {
                break;
            }
            let s = timeline
                .tempo
                .musical_to_samples(MusicalTime::from_quarters(beat), rate);
            if s >= pos {
                starts[count] = (((s - pos) as usize).min(n.saturating_sub(1)), k == 0.0);
                count += 1;
            }
            k += 1.0;
        }
        if count == 0 && self.remaining == 0 {
            return;
        }
        let outs = io.output_channels().min(2);
        let mut next = 0;
        for i in 0..n {
            while next < count && starts[next].0 == i {
                self.start(starts[next].1, rate, gain);
                next += 1;
            }
            if self.remaining == 0 {
                continue;
            }
            let v = self.phase.sin() * self.amp;
            self.phase += self.inc;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }
            self.amp *= self.decay;
            self.remaining -= 1;
            for c in 0..outs {
                io.output(c)[offset + i] += v;
            }
        }
    }
}

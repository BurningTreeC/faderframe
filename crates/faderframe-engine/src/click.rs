//! The metronome: a click on every beat (accented on bar downbeats, off-beats
//! — the eighths between beats — when asked for), following the tempo and
//! meter maps, mixed into the first two device outputs after the graph
//! (unless switched off there) and into every cue mix's outputs that wants
//! it, at the cue mix's click level under its master level and mute
//! ([`MetronomeShared::set_cues`]). Live playback only — offline renders
//! never contain it.
//!
//! The sounds are made on the control thread ([`ClickSounds`]: a built-in
//! [`ClickSound`] synthesised, or the user's files) at the stream's rate and
//! handed over whole; the audio thread only plays them, at the levels in
//! [`MetronomeShared`].

use faderframe_audio::DeviceBuffers;
use faderframe_timeline::{MusicalTime, Timeline};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// Cue mixes the click can play in (see [`faderframe_project::cue::MAX_CUE_MIXES`]).
pub const MAX_CUE_CLICKS: usize = faderframe_project::cue::MAX_CUE_MIXES;

/// Where (and how loud) the click plays in a cue mix.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CueClick {
    /// The cue mix's first device output.
    pub first_channel: u16,
    /// 1 (mono) or 2.
    pub channels: u8,
    /// Linear: the cue's click level × its master level (0 while muted).
    pub gain: f32,
    /// The click is on in this cue mix.
    pub on: bool,
}

impl CueClick {
    /// One word: the gain's bits, the first channel, the channel count and
    /// the switch (bit 56) — read whole, never torn.
    fn pack(self) -> u64 {
        u64::from(self.gain.to_bits())
            | (u64::from(self.first_channel) << 32)
            | (u64::from(self.channels) << 48)
            | (u64::from(self.on) << 56)
    }

    fn unpack(v: u64) -> Self {
        Self {
            gain: f32::from_bits(v as u32),
            first_channel: (v >> 32) as u16,
            channels: (v >> 48) as u8,
            on: (v >> 56) & 1 == 1,
        }
    }
}

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

/// What a click marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickKind {
    /// The first beat of a bar.
    Accent,
    /// Every other beat.
    Beat,
    /// The eighth between two beats.
    OffBeat,
}

impl ClickKind {
    pub const ALL: [ClickKind; 3] = [ClickKind::Accent, ClickKind::Beat, ClickKind::OffBeat];

    fn index(self) -> usize {
        match self {
            Self::Accent => 0,
            Self::Beat => 1,
            Self::OffBeat => 2,
        }
    }
}

/// The built-in click sounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClickSound {
    /// A short sine ping (the classic electronic click).
    #[default]
    Classic,
    Woodblock,
    /// A stick on a rim.
    Stick,
    HiHat,
    Cowbell,
    /// A plain tone, held briefly.
    Beep,
}

impl ClickSound {
    pub const ALL: [ClickSound; 6] = [
        ClickSound::Classic,
        ClickSound::Woodblock,
        ClickSound::Stick,
        ClickSound::HiHat,
        ClickSound::Cowbell,
        ClickSound::Beep,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Click",
            Self::Woodblock => "Woodblock",
            Self::Stick => "Stick",
            Self::HiHat => "Hi-hat",
            Self::Cowbell => "Cowbell",
            Self::Beep => "Beep",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Classic => "click",
            Self::Woodblock => "woodblock",
            Self::Stick => "stick",
            Self::HiHat => "hihat",
            Self::Cowbell => "cowbell",
            Self::Beep => "beep",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// Metronome settings shared with the control side: when it clicks, its
/// volume and the level of each kind of click (linear gains; 0 = silent).
#[derive(Debug)]
pub struct MetronomeShared {
    mode: AtomicU8,
    gain_bits: AtomicU32,
    levels: [AtomicU32; 3],
    /// It clicks on the main outputs (the first two).
    main: AtomicBool,
    /// The cue mixes it may click in (the first `cue_count`).
    cues: [AtomicU64; MAX_CUE_CLICKS],
    cue_count: AtomicU8,
    /// It clicks only while counting in, never in the run a count-in
    /// started.
    count_in_only: AtomicBool,
}

impl Default for MetronomeShared {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(0),
            gain_bits: AtomicU32::new(0.35f32.to_bits()),
            levels: [
                AtomicU32::new(1.0f32.to_bits()),
                AtomicU32::new(0.7f32.to_bits()),
                AtomicU32::new(0.0f32.to_bits()),
            ],
            main: AtomicBool::new(true),
            cues: std::array::from_fn(|_| AtomicU64::new(0)),
            cue_count: AtomicU8::new(0),
            count_in_only: AtomicBool::new(false),
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

    /// Click only while counting in: once a count-in has counted in, the
    /// metronome keeps quiet until the transport stops, whatever its mode
    /// (the take records without the click).
    pub fn set_count_in_only(&self, on: bool) {
        self.count_in_only.store(on, Ordering::Relaxed);
    }

    pub fn count_in_only(&self) -> bool {
        self.count_in_only.load(Ordering::Relaxed)
    }

    /// The overall volume (linear).
    pub fn set_gain(&self, gain: f32) {
        self.gain_bits
            .store(gain.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }

    /// The level of one kind of click (linear; 0: it does not sound —
    /// off-beats are not even looked for).
    pub fn set_level(&self, kind: ClickKind, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 4.0)
        } else {
            0.0
        };
        self.levels[kind.index()].store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn level(&self, kind: ClickKind) -> f32 {
        f32::from_bits(self.levels[kind.index()].load(Ordering::Relaxed))
    }

    /// Whether the click plays on the main outputs (the first two; it may
    /// play in cue mixes only).
    pub fn set_main(&self, on: bool) {
        self.main.store(on, Ordering::Relaxed);
    }

    pub fn main(&self) -> bool {
        self.main.load(Ordering::Relaxed)
    }

    /// The cue mixes the click may play in (control thread; the first
    /// [`MAX_CUE_CLICKS`]).
    pub fn set_cues(&self, cues: &[CueClick]) {
        let n = cues.len().min(MAX_CUE_CLICKS);
        for (slot, c) in self.cues.iter().zip(cues) {
            slot.store(c.pack(), Ordering::Relaxed);
        }
        self.cue_count.store(n as u8, Ordering::Relaxed);
    }

    /// The cue mixes the click may play in now.
    pub fn cues(&self) -> impl Iterator<Item = CueClick> + '_ {
        let n = usize::from(self.cue_count.load(Ordering::Relaxed)).min(MAX_CUE_CLICKS);
        self.cues[..n]
            .iter()
            .map(|c| CueClick::unpack(c.load(Ordering::Relaxed)))
    }
}

/// Longest click kept (a user's file is cut there).
pub const MAX_CLICK_SECONDS: f64 = 2.0;

/// The metronome's sounds at one rate: per kind of click, stereo frames.
#[derive(Debug, Default)]
pub struct ClickSounds {
    pub rate: u32,
    sounds: [Box<[[f32; 2]]>; 3],
}

impl ClickSounds {
    /// From frames per kind (cut at [`MAX_CLICK_SECONDS`]).
    pub fn new(
        rate: u32,
        accent: Vec<[f32; 2]>,
        beat: Vec<[f32; 2]>,
        off_beat: Vec<[f32; 2]>,
    ) -> Self {
        let max = (MAX_CLICK_SECONDS * f64::from(rate)) as usize;
        let cut = |mut v: Vec<[f32; 2]>| {
            v.truncate(max);
            v.into_boxed_slice()
        };
        Self {
            rate,
            sounds: [cut(accent), cut(beat), cut(off_beat)],
        }
    }

    /// A built-in sound at `rate` (control thread).
    pub fn synth(sound: ClickSound, rate: u32) -> Self {
        let make = |kind| synth(sound, kind, f64::from(rate.max(1)));
        Self::new(
            rate,
            make(ClickKind::Accent),
            make(ClickKind::Beat),
            make(ClickKind::OffBeat),
        )
    }

    /// The frames of one kind of click.
    pub fn sound(&self, kind: ClickKind) -> &[[f32; 2]] {
        &self.sounds[kind.index()]
    }
}

/// A deterministic noise source (the same click every time).
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(self.0 >> 8) / f64::from(1u32 << 23) - 1.0
    }
}

/// A two-pole filter (RBJ's cookbook): band pass (constant peak) or high
/// pass.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn new(band: bool, freq: f64, q: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * (freq / rate).min(0.49);
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = if band {
            [alpha, 0.0, -alpha]
        } else {
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0]
        };
        Self {
            b: b.map(|v| v / a0),
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn tick(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// One click of a built-in sound, peak-normalised to 1.
fn synth(sound: ClickSound, kind: ClickKind, rate: f64) -> Vec<[f32; 2]> {
    use std::f64::consts::TAU;
    // Accents higher, off-beats lower (the pitched sounds).
    let pitch = match kind {
        ClickKind::Accent => 1.25,
        ClickKind::Beat => 1.0,
        ClickKind::OffBeat => 0.8,
    };
    let seconds = match sound {
        ClickSound::Classic => 0.045,
        ClickSound::Woodblock => 0.08,
        ClickSound::Stick => 0.05,
        ClickSound::HiHat => match kind {
            ClickKind::Accent => 0.25,
            ClickKind::Beat => 0.1,
            ClickKind::OffBeat => 0.06,
        },
        ClickSound::Cowbell => 0.3,
        ClickSound::Beep => match kind {
            ClickKind::OffBeat => 0.04,
            _ => 0.06,
        },
    };
    let n = (seconds * rate) as usize;
    let mut noise = Noise(0x5eed_1234);
    let mut out = Vec::with_capacity(n);
    let decay = |t: f64, tau: f64| (-t / tau).exp();
    match sound {
        ClickSound::Classic => {
            let f = 1320.0 * pitch;
            for i in 0..n {
                let t = i as f64 / rate;
                out.push((TAU * f * t).sin() * decay(t, 0.009));
            }
        }
        ClickSound::Woodblock => {
            let f = 1000.0 * pitch;
            for i in 0..n {
                let t = i as f64 / rate;
                out.push(
                    (TAU * f * t).sin() * decay(t, 0.018)
                        + 0.5 * (TAU * 2.76 * f * t).sin() * decay(t, 0.006)
                        + 0.3 * noise.next() * decay(t, 0.0008),
                );
            }
        }
        ClickSound::Stick => {
            let f = 2600.0 * pitch;
            let mut band = Biquad::new(true, f, 3.0, rate);
            for i in 0..n {
                let t = i as f64 / rate;
                out.push(
                    band.tick(noise.next()) * decay(t, 0.004)
                        + 0.4 * (TAU * 0.5 * f * t).sin() * decay(t, 0.01),
                );
            }
        }
        ClickSound::HiHat => {
            let tau = seconds / 5.0;
            let (mut h1, mut h2) = (
                Biquad::new(false, 7000.0, 0.7, rate),
                Biquad::new(false, 7000.0, 0.7, rate),
            );
            for i in 0..n {
                let t = i as f64 / rate;
                out.push(h2.tick(h1.tick(noise.next())) * decay(t, tau));
            }
        }
        ClickSound::Cowbell => {
            // Two square waves (band-limited: their odd harmonics below
            // Nyquist), as the classic drum machines' bell.
            let partials = [540.0 * pitch, 800.0 * pitch];
            let mut band = Biquad::new(true, 900.0 * pitch, 1.2, rate);
            for i in 0..n {
                let t = i as f64 / rate;
                let mut s = 0.0;
                for f in partials {
                    let mut h = 1.0;
                    while h * f < rate * 0.45 {
                        s += (TAU * h * f * t).sin() / h;
                        h += 2.0;
                    }
                }
                let env = 0.7 * decay(t, 0.015) + 0.3 * decay(t, 0.12);
                out.push(band.tick(s) * env);
            }
        }
        ClickSound::Beep => {
            let f = 1000.0 * pitch * if kind == ClickKind::Accent { 1.2 } else { 1.0 };
            let fade = (0.002 * rate).max(1.0);
            for i in 0..n {
                let t = i as f64 / rate;
                let edge = (i as f64 / fade).min((n - i) as f64 / fade).min(1.0);
                out.push((TAU * f * t).sin() * edge);
            }
        }
    }
    // A short fade at the end: no click where the sound is cut.
    let fade = ((0.003 * rate) as usize).clamp(1, n.max(1));
    let len = out.len();
    for (j, v) in out[len.saturating_sub(fade)..].iter_mut().enumerate() {
        let x = (j + 1) as f64 / fade as f64;
        *v *= 0.5 * (1.0 + (std::f64::consts::PI * x).cos());
    }
    let peak = out.iter().fold(0.0f64, |p, v| p.max(v.abs()));
    let norm = if peak > 0.0 { 1.0 / peak } else { 0.0 };
    out.iter()
        .map(|v| {
            let v = (v * norm) as f32;
            [v, v]
        })
        .collect()
}

const MAX_CLICKS_PER_BLOCK: usize = 8;
/// Clicks sounding at once (a long sound overlapping the next ones).
const VOICES: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    kind: u8,
    pos: usize,
    gain: f32,
    on: bool,
}

/// Audio-thread click voices.
#[derive(Debug)]
pub(crate) struct Click {
    /// The run of the play history it last clicked in (a new one cuts the
    /// sounding clicks: the playhead jumped).
    pub(crate) run: i64,
    /// It last clicked a count-in (the playback it counts in carries its
    /// clicks on).
    pub(crate) counting_in: bool,
    voices: [Voice; VOICES],
    /// The sounds (swapped in whole by the engine; the old ones go back to
    /// the control thread).
    pub(crate) sounds: Arc<ClickSounds>,
}

impl Click {
    /// With `sounds` (control thread).
    pub(crate) fn new(sounds: Arc<ClickSounds>) -> Self {
        Self {
            run: 0,
            counting_in: false,
            voices: [Voice::default(); VOICES],
            sounds,
        }
    }

    pub(crate) fn reset(&mut self) {
        for v in &mut self.voices {
            v.on = false;
        }
    }

    /// A click is still sounding.
    pub(crate) fn sounding(&self) -> bool {
        self.voices.iter().any(|v| v.on)
    }

    fn start(&mut self, kind: ClickKind, gain: f32) {
        if gain <= 0.0 || self.sounds.sound(kind).is_empty() {
            return;
        }
        // A free voice, else the one furthest along.
        let slot = self
            .voices
            .iter()
            .position(|v| !v.on)
            .unwrap_or_else(|| (0..VOICES).max_by_key(|&i| self.voices[i].pos).unwrap_or(0));
        self.voices[slot] = Voice {
            kind: kind.index() as u8,
            pos: 0,
            gain,
            on: true,
        };
    }

    /// Render clicks for device frames `offset..offset + n` playing at
    /// timeline position `pos` (negative before the project's start: its
    /// first tempo and metre carried back); `beats`: start the clicks of
    /// the beats in there (else only the sounding ones ring on).
    /// Realtime-safe.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        io: &mut dyn DeviceBuffers,
        offset: usize,
        n: usize,
        pos: i64,
        timeline: &Timeline,
        rate: f64,
        shared: &MetronomeShared,
        beats: bool,
    ) {
        if !beats && !self.sounding() {
            return;
        }
        let gain = shared.gain();
        let levels = ClickKind::ALL.map(|k| shared.level(k) * gain);
        // Steps per beat: two with off-beats.
        let per_beat = if levels[ClickKind::OffBeat.index()] > 0.0 {
            2.0
        } else {
            1.0
        };
        // Clicks starting inside this block.
        let mut starts = [(usize::MAX, ClickKind::Beat); MAX_CLICKS_PER_BLOCK];
        let mut count = 0;
        let q0 = timeline.tempo.samples_to_quarters(pos, rate);
        let q1 = timeline.tempo.samples_to_quarters(pos + n as i64, rate);
        let mut bar = timeline.meter.bar_at(MusicalTime::from_quarters(q0));
        let mut bar_start = timeline.meter.bar_start(bar).quarters();
        let step_of = |bar| {
            timeline
                .meter
                .signature_of_bar(bar)
                .beat_length()
                .quarters()
                / per_beat
        };
        let mut step = step_of(bar);
        let mut k = ((q0 - bar_start) / step - 1e-9).ceil().max(0.0);
        loop {
            let mut at = bar_start + k * step;
            let next_bar = timeline.meter.bar_start(bar + 1).quarters();
            if at >= next_bar - 1e-9 {
                bar += 1;
                bar_start = next_bar;
                step = step_of(bar);
                k = 0.0;
                at = bar_start;
            }
            // Not starting clicks: none looked for.
            if !beats || at >= q1 || count == MAX_CLICKS_PER_BLOCK {
                break;
            }
            let s = timeline
                .tempo
                .musical_to_samples(MusicalTime::from_quarters(at), rate);
            if s >= pos {
                let kind = if k == 0.0 {
                    ClickKind::Accent
                } else if (k as i64) % (per_beat as i64) == 0 {
                    ClickKind::Beat
                } else {
                    ClickKind::OffBeat
                };
                starts[count] = (((s - pos) as usize).min(n.saturating_sub(1)), kind);
                count += 1;
            }
            k += 1.0;
        }
        if count == 0 && !self.voices.iter().any(|v| v.on) {
            return;
        }
        // Where it plays: the main outputs (the first two), the cue mixes
        // that want it — (first channel, channels, gain).
        let all = io.output_channels();
        let mut dests = [(0usize, 0usize, 0.0f32); MAX_CUE_CLICKS + 1];
        let mut n_dests = 0;
        if shared.main() {
            dests[0] = (0, all.min(2), 1.0);
            n_dests = 1;
        }
        for c in shared.cues().filter(|c| c.on && c.gain > 0.0) {
            let first = usize::from(c.first_channel);
            let channels = usize::from(c.channels.max(1)).min(all.saturating_sub(first));
            if channels > 0 && n_dests < dests.len() {
                dests[n_dests] = (first, channels, c.gain);
                n_dests += 1;
            }
        }
        let mut next = 0;
        for i in 0..n {
            while next < count && starts[next].0 == i {
                let kind = starts[next].1;
                self.start(kind, levels[kind.index()]);
                next += 1;
            }
            let (mut l, mut r) = (0.0f32, 0.0f32);
            let mut any = false;
            for v in self.voices.iter_mut().filter(|v| v.on) {
                let sound = &self.sounds.sounds[usize::from(v.kind)];
                match sound.get(v.pos) {
                    Some(&[a, b]) => {
                        l += a * v.gain;
                        r += b * v.gain;
                        v.pos += 1;
                        any = true;
                    }
                    None => v.on = false,
                }
            }
            if !any {
                continue;
            }
            for &(first, channels, g) in &dests[..n_dests] {
                match channels {
                    0 => {}
                    1 => io.output(first)[offset + i] += 0.5 * (l + r) * g,
                    _ => {
                        io.output(first)[offset + i] += l * g;
                        io.output(first + 1)[offset + i] += r * g;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in sound has a click of every kind, peak 1, silent at the
    /// end (no click at its cut).
    #[test]
    fn the_built_in_sounds_are_whole_clicks() {
        for sound in ClickSound::ALL {
            let s = ClickSounds::synth(sound, 48_000);
            for kind in ClickKind::ALL {
                let frames = s.sound(kind);
                assert!(!frames.is_empty(), "{sound:?} {kind:?}");
                let peak = frames.iter().fold(0.0f32, |p, f| p.max(f[0].abs()));
                assert!((peak - 1.0).abs() < 1e-4, "{sound:?} {kind:?}: peak {peak}");
                let tail = frames[frames.len() - 1][0].abs();
                assert!(tail < 0.05, "{sound:?} {kind:?}: ends at {tail}");
            }
        }
    }
}

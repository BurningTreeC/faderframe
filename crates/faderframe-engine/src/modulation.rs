//! Modulators in the engine. The control side turns each track's
//! modulators ([`faderframe_project::Modulator`]) into a
//! [`ModulationSet`] — what they are and what they move, in the targets'
//! own units — that reaches the audio thread the way the timeline does. A
//! [`ModNode`] right after the track's input evaluates them once a block
//! into the track's [`ModBus`]; plugin nodes turn the routes to their
//! parameters into [`ParamMod`]s, and the strip moves its fader and pan
//! by the routes to them. Nothing here changes a value: modulation is an
//! offset on top of it.
//!
//! While the transport plays, modulators run from the song position
//! (synced rates by the beat, free rates by the second), so every playback
//! and every render of a passage sounds alike; stopped, they run on.
//!
//! Per-note sources live in the plugin nodes that get the notes
//! ([`NoteVoices`]): each sounding note has its own value, sent per voice
//! ([`NoteParamMod`]) to parameters that take modulation per note, and as
//! the newest note's to the device's other parameters.

use crate::context::EngineContext;
use crate::plugins::PluginHost;
use faderframe_audio_graph::{AudioBuffer, NodeIo, ProcessContext, Processor};
use faderframe_core::{ModulatorId, PluginInstanceId, TrackId};
use faderframe_midi::{MidiBuffer, MidiEvent};
use faderframe_plugin_host::{NoteParamMod, ParamMod};
use faderframe_project::modulation::{
    FollowSource, MAX_MODULATORS, ModRate, ModSource, ModTarget, key_value, note_envelope, steps_at,
};
use faderframe_project::{Project, TrackKind};
use faderframe_realtime::AtomicF32;
use faderframe_transport::TransportInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Most parameters of one plugin modulated at once.
pub const MAX_PARAM_MODS: usize = 64;
/// Most single voices' modulations handed to a plugin a block.
pub const MAX_NOTE_MODS: usize = 256;
/// Voices a plugin node follows for per-note modulation.
pub const MAX_NOTE_VOICES: usize = 32;

/// The outputs of a track's modulators this block (bipolar ones −1..1,
/// unipolar ones 0..1; 0 when off), in the order of the track's
/// [`TrackModulation::modulators`]. Written by its [`ModNode`], read by the
/// plugin nodes and the strip after it, and by the UI.
#[derive(Debug)]
pub struct ModBus {
    outputs: [AtomicF32; MAX_MODULATORS],
}

impl Default for ModBus {
    fn default() -> Self {
        Self {
            outputs: std::array::from_fn(|_| AtomicF32::new(0.0)),
        }
    }
}

impl ModBus {
    #[inline]
    pub fn get(&self, i: usize) -> f32 {
        self.outputs
            .get(i)
            .map_or(0.0, |o| o.load(Ordering::Relaxed))
    }

    #[inline]
    fn set(&self, i: usize, v: f32) {
        if let Some(o) = self.outputs.get(i) {
            o.store(v, Ordering::Relaxed);
        }
    }
}

/// A modulator as the audio thread plays it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModSpec {
    pub id: ModulatorId,
    pub source: ModSource,
    pub enabled: bool,
}

/// A route in the target's units: an output of 1 moves the target by
/// `depth × range` (plain units of a plugin parameter, fader travel, pan).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteSpec {
    /// Index into [`TrackModulation::modulators`].
    pub modulator: usize,
    pub target: ModTarget,
    pub depth: f32,
    pub range: f32,
    /// The target takes modulation per note (each voice its own).
    pub per_note: bool,
}

/// One track's modulation.
#[derive(Clone, Debug)]
pub struct TrackModulation {
    pub track: TrackId,
    pub modulators: Vec<ModSpec>,
    /// Ordered by target (routes to one parameter are neighbours).
    pub routes: Vec<RouteSpec>,
    pub bus: Arc<ModBus>,
}

impl PartialEq for TrackModulation {
    fn eq(&self, other: &Self) -> bool {
        self.track == other.track
            && self.modulators == other.modulators
            && self.routes == other.routes
            && Arc::ptr_eq(&self.bus, &other.bus)
    }
}

impl TrackModulation {
    fn per_note(&self, r: &RouteSpec) -> bool {
        self.modulators
            .get(r.modulator)
            .is_some_and(|m| m.source.per_note())
    }

    /// Do per-note modulators move `plugin`?
    pub fn moves_notes_of(&self, plugin: PluginInstanceId) -> bool {
        self.routes.iter().any(|r| {
            matches!(r.target, ModTarget::Plugin { plugin: p, .. } if p == plugin)
                && self.per_note(r)
        })
    }

    /// This block's offsets of the fader (in travel) and the pan.
    pub fn strip(&self) -> (f32, f32) {
        let (mut volume, mut pan) = (0.0, 0.0);
        for r in self.routes.iter().filter(|r| !self.per_note(r)) {
            let m = r.depth * r.range * self.bus.get(r.modulator);
            match r.target {
                ModTarget::Volume => volume += m,
                ModTarget::Pan => pan += m,
                ModTarget::Plugin { .. } => {}
            }
        }
        (volume, pan)
    }

    /// This block's modulation of `plugin`'s parameters into `out`
    /// (cleared first; never grows it past [`MAX_PARAM_MODS`]).
    pub fn plugin_mods(&self, plugin: PluginInstanceId, out: &mut Vec<ParamMod>) {
        out.clear();
        for r in self.routes.iter().filter(|r| !self.per_note(r)) {
            let ModTarget::Plugin {
                plugin: p,
                parameter,
            } = r.target
            else {
                continue;
            };
            if p != plugin {
                continue;
            }
            let share = r.depth * self.bus.get(r.modulator);
            if let Some(last) = out.last_mut()
                && last.parameter == parameter
            {
                last.share += share;
                last.amount += share * r.range;
            } else if out.len() < MAX_PARAM_MODS.min(out.capacity()) {
                out.push(ParamMod {
                    parameter,
                    share,
                    amount: share * r.range,
                });
            }
        }
    }
}

/// Every track's modulation (tracks without modulators are not listed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModulationSet {
    pub tracks: Vec<TrackModulation>,
}

impl ModulationSet {
    #[inline]
    pub fn track(&self, track: TrackId) -> Option<&TrackModulation> {
        self.tracks.iter().find(|t| t.track == track)
    }
}

/// Can `kind` have modulators? Tracks with an audio chain.
pub fn modulated_kind(kind: TrackKind) -> bool {
    matches!(
        kind,
        TrackKind::Audio
            | TrackKind::Instrument
            | TrackKind::Bus
            | TrackKind::Aux
            | TrackKind::Master
    )
}

/// What the graph knows of modulation: the tracks with modulators and the
/// other tracks their followers listen to (in the order of their inputs).
/// The graph is rebuilt when it changes.
pub fn shape(project: &Project) -> Vec<(TrackId, Vec<TrackId>)> {
    project
        .tracks
        .iter()
        .filter(|t| !t.modulators.is_empty() && modulated_kind(t.kind))
        .map(|t| {
            let mut keys = Vec::new();
            for m in &t.modulators {
                if let ModSource::Follower {
                    source: FollowSource::Track { track },
                    ..
                } = m.source
                    && track != t.id
                    && !keys.contains(&track)
                {
                    keys.push(track);
                }
            }
            (t.id, keys)
        })
        .collect()
}

/// The set for `project` (control side). Routes to parameters that do not
/// take modulation (or to plugins that are gone) are left out; buses are
/// kept per track across sets.
pub fn build_set(
    project: &Project,
    plugins: &PluginHost,
    buses: &mut HashMap<TrackId, Arc<ModBus>>,
) -> ModulationSet {
    let mut tracks = Vec::new();
    for t in &project.tracks {
        if t.modulators.is_empty() || !modulated_kind(t.kind) {
            continue;
        }
        let modulators: Vec<ModSpec> = t
            .modulators
            .iter()
            .take(MAX_MODULATORS)
            .map(|m| ModSpec {
                id: m.id,
                source: m.source.clone(),
                enabled: m.enabled,
            })
            .collect();
        let mut routes = Vec::new();
        for (i, m) in t.modulators.iter().take(MAX_MODULATORS).enumerate() {
            for r in &m.routes {
                let (range, per_note) = match r.target {
                    ModTarget::Volume => (1.0, false),
                    ModTarget::Pan => (2.0, false),
                    ModTarget::Plugin { plugin, parameter } => {
                        if !plugins.modulatable(plugin, parameter) {
                            continue;
                        }
                        let Some(info) = plugins
                            .parameters(plugin)
                            .and_then(|ps| ps.iter().find(|p| p.id == parameter))
                        else {
                            continue;
                        };
                        (
                            (info.max - info.min) as f32,
                            plugins.modulatable_per_note(plugin, parameter),
                        )
                    }
                };
                routes.push(RouteSpec {
                    modulator: i,
                    target: r.target,
                    depth: r.depth.clamp(-1.0, 1.0),
                    range,
                    per_note,
                });
            }
        }
        routes.sort_by_key(|r| target_order(r.target));
        let bus = Arc::clone(buses.entry(t.id).or_default());
        tracks.push(TrackModulation {
            track: t.id,
            modulators,
            routes,
            bus,
        });
    }
    buses.retain(|id, _| tracks.iter().any(|t| t.track == *id));
    ModulationSet { tracks }
}

fn target_order(t: ModTarget) -> (u8, u64, u32) {
    match t {
        ModTarget::Volume => (0, 0, 0),
        ModTarget::Pan => (1, 0, 0),
        ModTarget::Plugin { plugin, parameter } => (2, plugin.raw(), parameter.0),
    }
}

/// A modulator's running state.
#[derive(Clone, Copy, Debug, Default)]
struct ModState {
    id: Option<ModulatorId>,
    /// Cycles (or steps) run: the song's while playing, counted on while
    /// stopped.
    cycles: f64,
    /// An envelope follower's level (linear).
    env: f32,
}

/// Evaluates a track's modulators once a block (see the module docs).
/// Audio passes through from input 0 (the track's signal before its
/// devices); inputs 1.. carry the tracks its followers listen to.
pub struct ModNode {
    track: TrackId,
    keys: Vec<TrackId>,
    sample_rate: f64,
    states: [ModState; MAX_MODULATORS],
}

impl ModNode {
    pub fn new(track: TrackId, keys: Vec<TrackId>, sample_rate: f64) -> Self {
        Self {
            track,
            keys,
            sample_rate,
            states: [ModState::default(); MAX_MODULATORS],
        }
    }

    /// State `i` belongs to `id` (kept when a modulator moves in the list).
    fn claim(&mut self, i: usize, id: ModulatorId) {
        if self.states[i].id == Some(id) {
            return;
        }
        match self.states.iter().position(|s| s.id == Some(id)) {
            Some(j) => self.states.swap(i, j),
            None => {
                self.states[i] = ModState {
                    id: Some(id),
                    ..ModState::default()
                }
            }
        }
    }
}

/// The cycles at the block's start, counting on while stopped.
fn advance(st: &mut ModState, rate: ModRate, t: &TransportInfo, frames: usize, sr: f64) -> f64 {
    if t.playing {
        st.cycles = match rate {
            ModRate::Sync { beats } => t.quarter_position / beats.max(1e-3),
            ModRate::Hz { hz } => t.sample_position as f64 / sr * f64::from(hz.max(0.0)),
        };
        st.cycles
    } else {
        let c = st.cycles;
        st.cycles += rate.hz(t.tempo) * frames as f64 / sr;
        c
    }
}

/// A random value (−1..1) for step `k` of modulator `id`: the same on every
/// playback.
fn noise(id: ModulatorId, k: i64) -> f32 {
    let mut z = id
        .raw()
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(k as u64);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// Random steps at `cycles`, each sliding into the next over the last
/// `smooth` of it.
fn random_at(id: ModulatorId, cycles: f64, smooth: f32) -> f32 {
    let k = cycles.floor();
    let a = noise(id, k as i64);
    let s = smooth.clamp(0.0, 1.0);
    let f = (cycles - k) as f32;
    if s <= 0.0 || f <= 1.0 - s {
        return a;
    }
    let b = noise(id, k as i64 + 1);
    let t = (f - (1.0 - s)) / s;
    a + (b - a) * t * t * (3.0 - 2.0 * t)
}

/// An envelope follower over `input` this block: 0..1 from −60 dBFS to
/// 0 dBFS (after `gain_db`).
fn follow(
    st: &mut ModState,
    input: Option<&AudioBuffer>,
    frames: usize,
    (attack_ms, release_ms, gain_db): (f32, f32, f32),
    sr: f64,
) -> f32 {
    let coef = |ms: f32| (-1.0 / (f64::from(ms.max(0.1)) * 0.001 * sr)).exp() as f32;
    let (a, r) = (coef(attack_ms), coef(release_ms));
    let mut env = st.env;
    match input {
        Some(b) if b.num_channels() > 0 => {
            let chans = b.num_channels();
            for i in 0..frames.min(b.channel(0).len()) {
                let mut x = 0.0f32;
                for c in 0..chans {
                    x = x.max(b.channel(c)[i].abs());
                }
                let k = if x > env { a } else { r };
                env = k * env + (1.0 - k) * x;
            }
        }
        _ => {
            // Silence: release.
            env *= r.powi(frames as i32);
        }
    }
    if !env.is_finite() || env < 1e-9 {
        env = 0.0;
    }
    st.env = env;
    let db = faderframe_core::gain_to_db(env) + gain_db;
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

impl Processor<EngineContext> for ModNode {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        if let Some(out) = io.audio_out.first_mut() {
            match io.audio_in.first() {
                Some(input) => out.copy_from(input),
                None => out.clear(),
            }
        }
        let Some(tm) = cx.data.modulation.track(self.track) else {
            return;
        };
        let t = &cx.data.transport;
        let (n, sr) = (io.frames, self.sample_rate);
        for (i, m) in tm.modulators.iter().take(MAX_MODULATORS).enumerate() {
            // Per-note sources: the plugin nodes that get the notes.
            if m.source.per_note() {
                continue;
            }
            self.claim(i, m.id);
            let st = &mut self.states[i];
            let v = match &m.source {
                ModSource::Lfo { shape, rate, phase } => {
                    let c = advance(st, *rate, t, n, sr);
                    shape.at(c + f64::from(*phase))
                }
                ModSource::Steps { steps, rate, glide } => {
                    let c = advance(st, *rate, t, n, sr);
                    steps_at(steps, c, *glide).clamp(-1.0, 1.0)
                }
                ModSource::Random { rate, smooth } => {
                    let c = advance(st, *rate, t, n, sr);
                    random_at(m.id, c, *smooth)
                }
                ModSource::Follower {
                    source,
                    attack_ms,
                    release_ms,
                    gain_db,
                } => {
                    let input = match source {
                        FollowSource::Input => io.audio_in.first(),
                        FollowSource::Track { track } => self
                            .keys
                            .iter()
                            .position(|k| k == track)
                            .and_then(|j| io.audio_in.get(j + 1)),
                    };
                    follow(st, input, n, (*attack_ms, *release_ms, *gain_db), sr)
                }
                ModSource::Macro { value } => value.clamp(0.0, 1.0),
                _ => 0.0,
            };
            tm.bus.set(i, if m.enabled { v } else { 0.0 });
        }
    }

    fn reset(&mut self) {
        for s in &mut self.states {
            s.env = 0.0;
        }
    }
}

/// A note a plugin node follows.
#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    live: bool,
    channel: u8,
    key: u8,
    velocity: f32,
    /// Counts notes (the newest has the highest; random values hash it).
    serial: u64,
    /// Frames since its note-on (at the block's start).
    age: u64,
    /// Frames it was held, once released.
    held: Option<u64>,
}

/// The notes a plugin node gets, for per-note modulation.
#[derive(Debug)]
pub struct NoteVoices {
    voices: [Voice; MAX_NOTE_VOICES],
    serial: u64,
}

impl Default for NoteVoices {
    fn default() -> Self {
        Self {
            voices: [Voice::default(); MAX_NOTE_VOICES],
            serial: 0,
        }
    }
}

/// A per-note source's value for voice `v`, `at` frames after its start.
fn voice_value(m: &ModSpec, v: &Voice, at: u64, sr: f64, tempo: f64) -> f32 {
    if !m.enabled {
        return 0.0;
    }
    let ms = |f: u64| (f as f64 * 1000.0 / sr) as f32;
    match &m.source {
        ModSource::Velocity => v.velocity,
        ModSource::Key => key_value(v.key),
        ModSource::NoteEnvelope {
            attack_ms,
            decay_ms,
            sustain,
            release_ms,
        } => note_envelope(
            (*attack_ms, *decay_ms, *sustain, *release_ms),
            ms(at),
            v.held.map(|h| (ms(h), ms(at.saturating_sub(h)))),
        ),
        ModSource::NoteLfo { shape, rate, phase } => {
            shape.at(rate.hz(tempo) * at as f64 / sr + f64::from(*phase))
        }
        ModSource::NoteRandom => noise(m.id, v.serial as i64),
        _ => 0.0,
    }
}

/// Add a modulation of `parameter` (to one already there, or a new one).
fn add_mod(
    mods: &mut Vec<ParamMod>,
    parameter: faderframe_core::ParameterId,
    share: f32,
    range: f32,
) {
    if let Some(m) = mods.iter_mut().find(|m| m.parameter == parameter) {
        m.share += share;
        m.amount += share * range;
    } else if mods.len() < MAX_PARAM_MODS.min(mods.capacity()) {
        mods.push(ParamMod {
            parameter,
            share,
            amount: share * range,
        });
    }
}

impl NoteVoices {
    fn emit(
        tm: &TrackModulation,
        plugin: PluginInstanceId,
        v: &Voice,
        at: u64,
        offset: u32,
        (sr, tempo): (f64, f64),
        out: &mut Vec<NoteParamMod>,
    ) {
        for r in &tm.routes {
            let ModTarget::Plugin {
                plugin: p,
                parameter,
            } = r.target
            else {
                continue;
            };
            let Some(m) = tm.modulators.get(r.modulator) else {
                continue;
            };
            if p != plugin || !r.per_note || !m.source.per_note() {
                continue;
            }
            let amount = r.depth * r.range * voice_value(m, v, at, sr, tempo);
            if let Some(last) = out.last_mut()
                && last.parameter == parameter
                && last.key == v.key
                && last.channel == v.channel
                && last.sample_offset == offset
            {
                last.amount += amount;
            } else if out.len() < MAX_NOTE_MODS.min(out.capacity()) {
                out.push(NoteParamMod {
                    parameter,
                    channel: v.channel,
                    key: v.key,
                    amount,
                    sample_offset: offset,
                });
            }
        }
    }

    /// How long a released note is followed (its envelopes' release, at
    /// least half a second).
    fn tail(tm: &TrackModulation, sr: f64) -> u64 {
        let ms = tm
            .modulators
            .iter()
            .filter_map(|m| match m.source {
                ModSource::NoteEnvelope { release_ms, .. } => Some(release_ms),
                _ => None,
            })
            .fold(500.0f32, f32::max);
        (f64::from(ms) * sr / 1000.0) as u64
    }

    fn start(&mut self, channel: u8, key: u8, velocity: u8) -> usize {
        self.serial += 1;
        let i = self
            .voices
            .iter()
            .position(|v| v.live && v.channel == channel && v.key == key)
            .or_else(|| self.voices.iter().position(|v| !v.live))
            .unwrap_or_else(|| {
                // Steal the oldest released note, else the oldest.
                let released = self
                    .voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.held.is_some())
                    .min_by_key(|(_, v)| v.serial);
                released
                    .or_else(|| self.voices.iter().enumerate().min_by_key(|(_, v)| v.serial))
                    .map_or(0, |(i, _)| i)
            });
        self.voices[i] = Voice {
            live: true,
            channel,
            key,
            velocity: f32::from(velocity) / 127.0,
            serial: self.serial,
            age: 0,
            held: None,
        };
        i
    }

    /// Voice `i` released at `t` in this block (held from its start, which
    /// may be in this block too).
    fn release(&mut self, i: usize, t: u32, started: &[u32; MAX_NOTE_VOICES]) {
        let s = if started[i] == u32::MAX {
            0
        } else {
            started[i]
        };
        let v = &mut self.voices[i];
        v.held = Some(v.age + u64::from(t.saturating_sub(s)));
    }

    /// This block for `plugin`: follow the notes in `events`, put the
    /// single voices' modulation of its per-note parameters into `out`
    /// (cleared first), add the newest voice's to `mods` for its other
    /// parameters, and show it on the track's bus.
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        tm: &TrackModulation,
        plugin: PluginInstanceId,
        events: Option<&MidiBuffer>,
        frames: usize,
        transport: &faderframe_transport::TransportInfo,
        discontinuity: bool,
        mods: &mut Vec<ParamMod>,
        out: &mut Vec<NoteParamMod>,
    ) {
        out.clear();
        let timing = (transport.sample_rate.max(1.0), transport.tempo);
        let sr = timing.0;
        let n = frames as u64;
        if discontinuity {
            for v in self
                .voices
                .iter_mut()
                .filter(|v| v.live && v.held.is_none())
            {
                v.held = Some(v.age);
            }
        }
        // The notes sounding already, as of the block's start.
        for v in self.voices.iter().filter(|v| v.live) {
            Self::emit(tm, plugin, v, v.age, 0, timing, out);
        }
        // New notes (their modulation at their start) and releases.
        let mut started = [u32::MAX; MAX_NOTE_VOICES];
        if let Some(events) = events {
            for e in events.iter() {
                let t = e.sample_offset.min(n.saturating_sub(1) as u32);
                match e.event {
                    MidiEvent::NoteOn {
                        channel,
                        key,
                        velocity,
                    } if velocity > 0 => {
                        let i = self.start(channel, key, velocity);
                        started[i] = t;
                        let v = self.voices[i];
                        Self::emit(tm, plugin, &v, 0, t, timing, out);
                    }
                    MidiEvent::NoteOn { channel, key, .. }
                    | MidiEvent::NoteOff { channel, key, .. } => {
                        let found = self.voices.iter().position(|v| {
                            v.live && v.held.is_none() && v.channel == channel && v.key == key
                        });
                        if let Some(i) = found {
                            self.release(i, t, &started);
                        }
                    }
                    MidiEvent::ControlChange { controller, .. }
                        if controller == MidiEvent::CC_ALL_NOTES_OFF =>
                    {
                        for i in 0..MAX_NOTE_VOICES {
                            if self.voices[i].live && self.voices[i].held.is_none() {
                                self.release(i, t, &started);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // On to the next block's start; released notes end after the tail.
        let tail = Self::tail(tm, sr);
        for (i, v) in self.voices.iter_mut().enumerate().filter(|(_, v)| v.live) {
            let from = if started[i] == u32::MAX {
                0
            } else {
                u64::from(started[i])
            };
            v.age += n - from.min(n);
            if v.held.is_some_and(|h| v.age.saturating_sub(h) > tail) {
                v.live = false;
            }
        }
        // The newest note: the device's other parameters, and the bus.
        let newest = self
            .voices
            .iter()
            .filter(|v| v.live)
            .max_by_key(|v| (v.held.is_none(), v.serial))
            .copied();
        for (i, m) in tm.modulators.iter().enumerate().take(MAX_MODULATORS) {
            if !m.source.per_note() {
                continue;
            }
            let value = newest.map_or(0.0, |v| voice_value(m, &v, v.age, sr, timing.1));
            tm.bus.set(i, value);
            for r in &tm.routes {
                if r.modulator != i || r.per_note {
                    continue;
                }
                if let ModTarget::Plugin {
                    plugin: p,
                    parameter,
                } = r.target
                    && p == plugin
                {
                    add_mod(mods, parameter, r.depth * value, r.range);
                }
            }
        }
    }

    pub fn reset(&mut self) {
        for v in &mut self.voices {
            v.live = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_steps_repeat_and_stay_in_range() {
        let id = ModulatorId(7);
        for k in -50..50 {
            let v = noise(id, k);
            assert!((-1.0..=1.0).contains(&v));
            assert_eq!(v, noise(id, k), "the same every time");
        }
        assert_ne!(noise(id, 1), noise(id, 2));
        assert_ne!(noise(id, 1), noise(ModulatorId(8), 1));
        // Smooth: the step holds, then slides into the next one.
        assert_eq!(random_at(id, 3.2, 0.5), noise(id, 3));
        let mid = random_at(id, 3.75, 0.5);
        let (a, b) = (noise(id, 3), noise(id, 4));
        assert!((mid - (a + b) / 2.0).abs() < 1e-5);
        assert!((random_at(id, 3.9999, 0.5) - b).abs() < 1e-3);
    }

    fn notes_set(per_note: bool) -> (TrackModulation, PluginInstanceId) {
        let plugin = PluginInstanceId(9);
        let target = ModTarget::Plugin {
            plugin,
            parameter: faderframe_core::ParameterId(3),
        };
        let tm = TrackModulation {
            track: TrackId(1),
            modulators: vec![ModSpec {
                id: ModulatorId(1),
                source: ModSource::Velocity,
                enabled: true,
            }],
            routes: vec![RouteSpec {
                modulator: 0,
                target,
                depth: 0.5,
                range: 10.0,
                per_note,
            }],
            bus: Arc::default(),
        };
        (tm, plugin)
    }

    fn block(
        voices: &mut NoteVoices,
        tm: &TrackModulation,
        plugin: PluginInstanceId,
        events: &[(u32, MidiEvent)],
        frames: usize,
    ) -> (Vec<ParamMod>, Vec<NoteParamMod>) {
        let mut midi = MidiBuffer::with_capacity(16);
        for (t, e) in events {
            midi.push(faderframe_midi::TimedMidiEvent::new(*t, *e))
                .unwrap();
        }
        let transport = TransportInfo {
            sample_rate: 48_000.0,
            tempo: 120.0,
            ..TransportInfo::default()
        };
        let mut mods = Vec::with_capacity(MAX_PARAM_MODS);
        let mut out = Vec::with_capacity(MAX_NOTE_MODS);
        voices.process(
            tm,
            plugin,
            Some(&midi),
            frames,
            &transport,
            false,
            &mut mods,
            &mut out,
        );
        (mods, out)
    }

    fn on(key: u8, velocity: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: 0,
            key,
            velocity,
        }
    }

    #[test]
    fn single_voices_get_their_own_modulation_after_their_note_on() {
        let (tm, plugin) = notes_set(true);
        let mut v = NoteVoices::default();
        let (mods, out) = block(
            &mut v,
            &tm,
            plugin,
            &[(8, on(60, 127)), (20, on(64, 64))],
            256,
        );
        assert!(mods.is_empty());
        let got: Vec<(u8, u32, f32)> = out
            .iter()
            .map(|m| (m.key, m.sample_offset, m.amount))
            .collect();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].0, got[0].1), (60, 8));
        assert!((got[0].2 - 5.0).abs() < 1e-5);
        assert_eq!((got[1].0, got[1].1), (64, 20));
        assert!((got[1].2 - 5.0 * 64.0 / 127.0).abs() < 1e-5);
        // Both again from the next block's start; the bus has the newest.
        let (_, out) = block(&mut v, &tm, plugin, &[], 256);
        assert_eq!(
            out.iter()
                .map(|m| (m.key, m.sample_offset))
                .collect::<Vec<_>>(),
            [(60, 0), (64, 0)]
        );
        assert!((tm.bus.get(0) - 64.0 / 127.0).abs() < 1e-6);
        // Released: followed through its tail (half a second), then not.
        let off = MidiEvent::NoteOff {
            channel: 0,
            key: 60,
            velocity: 0,
        };
        block(&mut v, &tm, plugin, &[(5, off)], 256);
        let (_, out) = block(&mut v, &tm, plugin, &[], 256);
        assert_eq!(out.len(), 2, "still in its release");
        for _ in 0..100 {
            block(&mut v, &tm, plugin, &[], 256);
        }
        let (_, out) = block(&mut v, &tm, plugin, &[], 256);
        assert_eq!(out.iter().map(|m| m.key).collect::<Vec<_>>(), [64]);
    }

    #[test]
    fn other_parameters_follow_the_newest_note() {
        let (tm, plugin) = notes_set(false);
        let mut v = NoteVoices::default();
        let (mods, out) = block(
            &mut v,
            &tm,
            plugin,
            &[(0, on(60, 127)), (30, on(64, 64))],
            256,
        );
        assert!(out.is_empty());
        assert_eq!(mods.len(), 1);
        assert!((mods[0].share - 0.5 * 64.0 / 127.0).abs() < 1e-6);
        assert!((mods[0].amount - 5.0 * 64.0 / 127.0).abs() < 1e-5);
        // The newest released: the one still held counts.
        let off = MidiEvent::NoteOff {
            channel: 0,
            key: 64,
            velocity: 0,
        };
        let (mods, _) = block(&mut v, &tm, plugin, &[(0, off)], 256);
        assert!((mods[0].share - 0.5).abs() < 1e-6);
    }

    #[test]
    fn synced_rates_follow_the_song_position() {
        let mut st = ModState::default();
        let t = TransportInfo {
            playing: true,
            quarter_position: 6.0,
            ..TransportInfo::default()
        };
        // A cycle every half note: three cycles at quarter 6.
        assert_eq!(
            advance(&mut st, ModRate::Sync { beats: 2.0 }, &t, 64, 48_000.0),
            3.0
        );
        // Stopped: it runs on from there at the tempo.
        let t = TransportInfo {
            playing: false,
            tempo: 120.0,
            ..t
        };
        assert_eq!(
            advance(&mut st, ModRate::Sync { beats: 2.0 }, &t, 48_000, 48_000.0),
            3.0
        );
        assert!((st.cycles - 4.0).abs() < 1e-9, "{}", st.cycles);
    }
}

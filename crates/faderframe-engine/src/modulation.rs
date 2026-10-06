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

use crate::context::EngineContext;
use crate::plugins::PluginHost;
use faderframe_audio_graph::{AudioBuffer, NodeIo, ProcessContext, Processor};
use faderframe_core::{ModulatorId, PluginInstanceId, TrackId};
use faderframe_plugin_host::ParamMod;
use faderframe_project::modulation::{
    FollowSource, MAX_MODULATORS, ModRate, ModSource, ModTarget, steps_at,
};
use faderframe_project::{Project, TrackKind};
use faderframe_realtime::AtomicF32;
use faderframe_transport::TransportInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Most parameters of one plugin modulated at once.
pub const MAX_PARAM_MODS: usize = 64;

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
    /// This block's offsets of the fader (in travel) and the pan.
    pub fn strip(&self) -> (f32, f32) {
        let (mut volume, mut pan) = (0.0, 0.0);
        for r in &self.routes {
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
        for r in &self.routes {
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
                let range = match r.target {
                    ModTarget::Volume => 1.0,
                    ModTarget::Pan => 2.0,
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
                        (info.max - info.min) as f32
                    }
                };
                routes.push(RouteSpec {
                    modulator: i,
                    target: r.target,
                    depth: r.depth.clamp(-1.0, 1.0),
                    range,
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

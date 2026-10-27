//! Project → processing graph compiler (control thread).
//!
//! Per audio-producing track:
//!
//! ```text
//!  sources ──► TrackInput ──► insert 1 ──► … ──► ChannelStrip ──out0 (post)──► destination
//!  (clips,      (sum point,                       │    └─out1 (pre)
//!   monitor,     pre-FX tap)                      │
//!   instrument)                                   └─► sends (pre-FX / pre-fader / post-fader taps)
//! ```
//!
//! Destinations are other tracks' `TrackInput` nodes (buses, aux returns,
//! master) or device outputs; MIDI tracks feed an instrument's event input.
//! Cycles are rejected by the graph compiler; latency compensation happens
//! at every summing point.
//!
//! With render-ahead ([`AheadPlan`], see [`crate::ahead`]) the sources,
//! instrument and inserts of the planned tracks go into a second graph
//! ending in an `AheadWriter`; the realtime graph gets an `AheadReader`
//! (with the chain's latency) in front of their strips instead. Buses
//! rendered ahead, and the strips and sends reaching them, go into a third
//! graph, the shallow tier's, which reads those tracks' rings.

use crate::EngineError;
use crate::ahead::{AheadReader, AheadRing, AheadWriter};
use crate::context::EngineContext;
use crate::midi::{MidiFilter, MidiInputNode, MidiOutputSink, MidiShared, NO_PORT};
use crate::nodes::{
    AudioClipPlayer, ChannelStrip, Crosstalk, DeviceInputTap, DeviceOutputSink, FoldDown,
    HardwareReturn, HardwareSendGain, ListenOut, MidiClipPlayer, MonitorGate, ObjectRenderer,
    PluginNode, SendNode, StretchVoices, StripEcho,
};
use crate::plugins::PluginHost;
use crate::slots::SlotRegistry;
use faderframe_audio_graph::nodes::Passthrough;
use faderframe_audio_graph::{GraphBuilder, NodeId, NodeKey, NodeRole, NodeSpec, PrepareConfig};
use faderframe_core::PluginInstanceId;
use faderframe_core::{ChannelLayout, CueId, TrackId};
use faderframe_plugin_host::ProcessConfig;
use faderframe_project::{
    InputRouting, MonitorMode, OutputRouting, PluginSlot, Project, SendTap, Track, TrackKind,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// MIDI routing the graph builder needs.
#[derive(Clone, Default)]
pub struct MidiRouting {
    /// Input port keys → indices.
    pub inputs: HashMap<String, u16>,
    /// Output port keys → indices.
    pub outputs: HashMap<String, u16>,
    pub shared: std::sync::Arc<MidiShared>,
}

/// Result of building a graph description.
pub struct BuiltGraph {
    pub builder: GraphBuilder<EngineContext>,
    pub warnings: Vec<String>,
    /// What every node does and for whom (performance accounting). Node
    /// groups are track indices in project order.
    pub owners: Vec<(NodeId, NodeOwner)>,
    /// The render-ahead graph and the rings it fills (with an
    /// [`AheadPlan`]).
    pub ahead: Option<(GraphBuilder<EngineContext>, Vec<Arc<AheadRing>>)>,
    /// The shallow tier's graph and rings (buses rendered ahead).
    pub bus_ahead: Option<(GraphBuilder<EngineContext>, Vec<Arc<AheadRing>>)>,
    /// Effects bypassed by low-latency monitoring.
    pub bypassed_for_monitoring: Vec<PluginInstanceId>,
}

/// Which tracks are rendered ahead, and where their audio goes.
pub struct AheadPlan<'a> {
    pub tracks: &'a HashSet<TrackId>,
    /// Those of `tracks` whose strips and sends are rendered ahead too
    /// (they feed buses rendered ahead only).
    pub strips: &'a HashSet<TrackId>,
    /// Rings by track, kept across builds (replaced when their shape
    /// changes).
    pub rings: &'a mut HashMap<TrackId, Arc<AheadRing>>,
    /// Frames per ring.
    pub ring_frames: usize,
    /// The shallow tier's rings by track: strips' post-fader audio back to
    /// the audio thread, buses' outputs to their strips.
    pub bus_rings: &'a mut HashMap<TrackId, Arc<AheadRing>>,
    pub bus_ring_frames: usize,
    /// The cue paths' rings from the shallow tier to the audio thread, by
    /// (track or cue channel, port).
    pub cue_rings: &'a mut HashMap<(TrackId, u8), Arc<AheadRing>>,
    pub misses: Arc<AtomicU64>,
}

/// `rings[key]` if it has `channels` and `frames`, else a new one there.
fn ring_of<K: std::hash::Hash + Eq>(
    rings: &mut HashMap<K, Arc<AheadRing>>,
    key: K,
    channels: usize,
    frames: usize,
) -> Arc<AheadRing> {
    let channels = channels.max(1);
    match rings.get(&key) {
        Some(r) if r.channels() == channels && r.capacity() == frames => Arc::clone(r),
        _ => {
            let r = AheadRing::new(channels, frames);
            rings.insert(key, Arc::clone(&r));
            r
        }
    }
}

/// Buses and returns have a *cue path*: a second sum of their sources
/// as the cue mixes hear them (never muted, no solo), through a second
/// instance of each of their devices (cue shadows) and a solo-safe strip,
/// which their cue sends tap.
pub fn has_cue_path(t: &Track) -> bool {
    matches!(t.kind, TrackKind::Bus | TrackKind::Aux)
}

/// What is rendered ahead: the chains of `tracks`, and of those the
/// `strips` whose channel strips and sends are too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AheadSets {
    pub tracks: HashSet<TrackId>,
    pub strips: HashSet<TrackId>,
}

/// Can `t` be rendered ahead? Only what plays from the timeline alone and
/// feeds nothing back: audio and instrument tracks with plugins, not
/// frozen or armed, without input monitoring, live MIDI or an external
/// MIDI output, no sidechain inputs or pre-FX sends, and not fed by other
/// tracks.
pub fn ahead_eligible(
    project: &Project,
    t: &Track,
    live: &HashSet<TrackId>,
    fed: &HashSet<TrackId>,
) -> bool {
    let has_plugins = !t.inserts.is_empty() || t.instrument.is_some() || t.preamp.is_some();
    matches!(t.kind, TrackKind::Audio | TrackKind::Instrument)
        && has_plugins
        && t.freeze.is_none()
        && !fed.contains(&t.id)
        && plays_from_timeline(project, t, live)
}

/// Does `t` play from the timeline alone (whatever its kind)? Not armed,
/// no input monitoring, live MIDI or external MIDI output, no sidechain
/// inputs, no pre-FX sends, modulators that can run ahead, no launcher
/// clips, no hardware inserts.
fn plays_from_timeline(project: &Project, t: &Track, live: &HashSet<TrackId>) -> bool {
    let monitored =
        matches!(t.input, InputRouting::Hardware { .. }) && t.monitor != MonitorMode::Off;
    !t.record_arm
        && !monitored
        && !live.contains(&t.id)
        && t.midi_output.is_none()
        && t.slots().iter().all(|s| s.sidechain.is_none())
        && modulation_renders_ahead(t)
        && t.sends
            .iter()
            .all(|s| !s.enabled || s.tap != SendTap::PreFx)
        && project.track(t.id).is_some()
        // Plugin outputs other tracks take, or one it takes: those tracks
        // and this one connect on the audio thread.
        && t.input.plugin_output().is_none()
        && t.slots()
            .iter()
            .all(|s| project.plugin_output_tracks(s.id).is_empty())
        // Launched clips are played as they are launched.
        && !project.launcher.slots.keys().any(|k| k.track == t.id)
        // Outboard gear plays through the interface, on the audio thread.
        && t.slots()
            .iter()
            .all(|s| s.plugin.id != faderframe_core::builtin::HARDWARE_INSERT)
}

fn is_bus(kind: TrackKind) -> bool {
    matches!(kind, TrackKind::Bus | TrackKind::Aux | TrackKind::Master)
}

/// The tracks to render ahead ([`ahead_eligible`], not `edited`). With
/// `buses`, buses (auxes, the master) too, when everything that reaches
/// them is a strip rendered ahead: a track (or bus) whose output and sends
/// all go to buses rendered ahead, that keys no sidechain or follower
/// (those tap it on the audio thread), without crosstalk. Those strips and
/// the buses run in the shallow tier (see [`crate::ahead`]): their faders,
/// pan, mute and send levels are heard after a device callback and two
/// small blocks; automation stays exact. A bus without devices is only
/// worth it as a stage of a larger tree (its own strip rendered ahead).
///
/// `keep` (while playing) is what is rendered ahead now: nothing joins,
/// and a track whose strip has to come back to the audio thread comes
/// back whole (its strip alone would start with a gap).
pub fn ahead_sets(
    project: &Project,
    live: &HashSet<TrackId>,
    edited: &HashSet<TrackId>,
    buses: bool,
    keep: Option<&AheadSets>,
) -> AheadSets {
    use faderframe_project::modulation::{FollowSource, ModSource};
    let fed = fed_tracks(project);
    let mut tracks: HashSet<TrackId> = project
        .tracks
        .iter()
        .filter(|t| !edited.contains(&t.id) && ahead_eligible(project, t, live, &fed))
        .filter(|t| keep.is_none_or(|k| k.tracks.contains(&t.id)))
        .map(|t| t.id)
        .collect();
    let mut strips = HashSet::new();
    if !buses {
        if let Some(k) = keep {
            tracks.retain(|t| !k.strips.contains(t));
        }
        return AheadSets { tracks, strips };
    }
    let ok = |t: &Track| !edited.contains(&t.id) && plays_from_timeline(project, t, live);
    // Where each track's audio goes.
    let dests: Vec<(TrackId, Vec<TrackId>)> = project
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) || is_bus(t.kind))
        .map(|t| {
            let mut d: Vec<TrackId> = project.output_target(t).into_iter().collect();
            d.extend(t.sends.iter().filter(|s| s.enabled).map(|s| s.target));
            (t.id, d)
        })
        .collect();
    let mut keyed = HashSet::new();
    for t in &project.tracks {
        keyed.extend(t.slots().iter().filter_map(|s| s.sidechain));
        for m in &t.modulators {
            if let ModSource::Follower {
                source: FollowSource::Track { track },
                ..
            } = m.source
            {
                keyed.insert(track);
            }
        }
    }
    let has_devices = |id: &TrackId| {
        project
            .track(*id)
            .is_some_and(|t| !t.inserts.is_empty() || t.preamp.is_some())
    };
    let mut bus_set: HashSet<TrackId> = project
        .tracks
        .iter()
        .filter(|t| is_bus(t.kind) && t.freeze.is_none() && ok(t))
        .filter(|t| keep.is_none_or(|k| k.tracks.contains(&t.id)))
        .map(|t| t.id)
        .collect();
    let mut candidates: HashSet<TrackId> = project
        .tracks
        .iter()
        .filter(|t| {
            ok(t)
                && !project.crosstalk
                && !keyed.contains(&t.id)
                // Reaches a bus (not a device output).
                && project.output_target(t).is_some()
                && (is_bus(t.kind)
                    || (matches!(t.kind, TrackKind::Audio | TrackKind::Instrument)
                        && !fed.contains(&t.id)))
        })
        .filter(|t| keep.is_none_or(|k| k.strips.contains(&t.id)))
        .map(|t| t.id)
        .collect();
    loop {
        strips = dests
            .iter()
            .filter(|(t, d)| {
                candidates.contains(t)
                    && (!project.track(*t).is_some_and(|t| is_bus(t.kind)) || bus_set.contains(t))
                    && d.iter().all(|d| bus_set.contains(d))
            })
            .map(|(t, _)| *t)
            .collect();
        // While playing, a strip that has to come back brings its chain.
        let mut changed = false;
        if let Some(k) = keep {
            for f in k.strips.iter().filter(|f| !strips.contains(f)) {
                changed |= bus_set.remove(f) | candidates.remove(f) | tracks.remove(f);
            }
        }
        let before = bus_set.len();
        bus_set.retain(|b| {
            (has_devices(b) || strips.contains(b))
                && dests
                    .iter()
                    .filter(|(_, d)| d.contains(b))
                    .all(|(f, _)| strips.contains(f))
        });
        if !changed && bus_set.len() == before {
            break;
        }
    }
    tracks.extend(&bus_set);
    tracks.extend(&strips);
    AheadSets { tracks, strips }
}

/// Can `t`'s modulators run ahead of the playhead? Those that follow the
/// song position, the track's own input or its notes can; one that
/// listens to another track cannot (that track may not be rendered ahead),
/// nor can modulation of the fader or pan (the strip plays live).
fn modulation_renders_ahead(t: &Track) -> bool {
    use faderframe_project::modulation::{FollowSource, ModSource, ModTarget};
    t.modulators.iter().all(|m| {
        !matches!(
            m.source,
            ModSource::Follower {
                source: FollowSource::Track { .. },
                ..
            }
        ) && m
            .routes
            .iter()
            .all(|r| matches!(r.target, ModTarget::Plugin { .. }))
    })
}

/// Tracks that other tracks feed (outputs, sends, MIDI routing).
pub fn fed_tracks(project: &Project) -> HashSet<TrackId> {
    let mut fed = HashSet::new();
    for t in &project.tracks {
        if let Some(d) = project.output_target(t) {
            fed.insert(d);
        }
        if let OutputRouting::Track { track } = t.output {
            fed.insert(track);
        }
        for s in t.sends.iter().filter(|s| s.enabled) {
            fed.insert(s.target);
        }
        if project.plugin_output_source(t).is_some() {
            fed.insert(t.id);
        }
    }
    fed
}

/// The kind of work a graph node does for its track.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeWork {
    Clips,
    Midi,
    /// Live MIDI input.
    MidiInput,
    /// To an external MIDI device.
    MidiOutput,
    HardwareIn,
    Monitor,
    Input,
    Instrument,
    Insert,
    Strip,
    Send,
    HardwareOut,
    /// Rendered ahead (the reader in the realtime graph).
    Ahead,
    /// The track's modulators.
    Modulators,
}

/// The track (and plugin instance) a graph node works for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeOwner {
    pub track: TrackId,
    pub plugin: Option<PluginInstanceId>,
    pub work: NodeWork,
}

#[derive(Clone, Copy)]
enum Role {
    ClipPlayer = 1,
    MidiPlayer = 2,
    Instrument = 3,
    Input = 4,
    Insert = 5,
    Strip = 6,
    Send = 7,
    DeviceOut = 8,
    MidiInput = 9,
    Ahead = 10,
    Crosstalk = 11,
    Modulators = 12,
    ContainerSum = 13,
    ChainMix = 14,
    ChainNotes = 15,
    StripEcho = 16,
    Renderer = 17,
    HardwareSend = 18,
    HardwareReturn = 19,
    Console = 20,
    CueInput = 21,
    CueSend = 22,
    /// A bus's cue path: its sum.
    CueBusInput = 23,
    /// A bus's cue path: its strip.
    CueStrip = 24,
    /// A cue path's way from the shallow tier to the audio thread.
    CueAhead = 25,
    /// The talkback microphone.
    Talkback = 26,
    /// A cue mix after its level: its strip plus the talkback.
    CueOut = 27,
}

/// Stretcher voices a track's clip player needs: one per pitch-preserving
/// warp preset its clips use, two when such clips overlap.
pub fn stretch_voices(project: &Project, track: &faderframe_project::Track) -> StretchVoices {
    use faderframe_project::{ClipContent, WarpAlgorithm};
    let mut spans: [Vec<(i64, i64)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    // A launched clip plays alone (but may start while the arrangement's
    // clip still has its voice): one more of its kind.
    let mut launched = [0usize; 3];
    let slot_clips = project
        .launcher
        .slots
        .iter()
        .filter(|(k, _)| k.track == track.id)
        .filter_map(|(_, c)| project.clips.get(c));
    for (c, in_slot) in project
        .clips_of(track.id)
        .into_iter()
        .map(|c| (c, false))
        .chain(slot_clips.map(|c| (c, true)))
    {
        let ClipContent::Audio(a) = &c.content else {
            continue;
        };
        let pitched = a.pitch.as_ref().is_some_and(|e| e.plays_live());
        let warped = a
            .warp
            .as_ref()
            .filter(|w| !w.is_identity(a.source_offset, a.length));
        if c.muted || a.reversed || track.freeze.is_some() || (warped.is_none() && !pitched) {
            continue;
        }
        // Pitch edits play through PSOLA voices, warped or not.
        let i = match warped.map(|w| w.algorithm) {
            _ if pitched => 2,
            Some(WarpAlgorithm::Polyphonic) => 0,
            Some(WarpAlgorithm::Rhythmic) => 1,
            _ => continue,
        };
        if in_slot {
            launched[i] = 1;
            continue;
        }
        let start = c.start.ticks();
        spans[i].push((start, c.end(&project.timeline, project.sample_rate).ticks()));
    }
    let need = |v: &mut Vec<(i64, i64)>| -> usize {
        if v.is_empty() {
            return 0;
        }
        v.sort();
        let overlap = v.windows(2).any(|w| w[1].0 < w[0].1);
        if overlap { 2 } else { 1 }
    };
    let [mut poly, mut rhythmic, mut psola] = spans;
    StretchVoices {
        polyphonic: need(&mut poly) + launched[0],
        rhythmic: need(&mut rhythmic) + launched[1],
        psola: need(&mut psola) + launched[2],
        channels: track.layout.channel_count().max(2),
    }
}

/// FNV-1a over the identity of a node.
fn node_key(track: TrackId, role: Role, sub: u64, layouts: &[ChannelLayout]) -> NodeKey {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(track.raw());
    eat(role as u64);
    eat(sub);
    for l in layouts {
        // The layout itself: a strip into 5.1 is not one into six
        // discrete channels.
        match l {
            // A layout of the user's: its width and which one (interned:
            // one address per layout for the life of the process, and keys
            // are only compared within it). Never a built-in's key — a node
            // adopted across layouts keeps buffers of the other's width.
            ChannelLayout::Surround(faderframe_core::SurroundFormat::Custom(c)) => {
                eat(0x2_0000 + c.speakers.len() as u64);
                eat(std::ptr::from_ref(*c) as usize as u64);
            }
            ChannelLayout::Surround(f) => eat(0x1_0000
                + faderframe_core::SurroundFormat::ALL
                    .iter()
                    .position(|x| x == f)
                    .unwrap_or(0) as u64),
            other => eat(other.channel_count() as u64),
        }
    }
    NodeKey(h)
}

#[derive(Default, Clone, Copy)]
struct TrackNodes {
    input: Option<NodeId>,
    /// Dedicated input stage output; pre-FX sends include the preamp.
    preamp: Option<NodeId>,
    /// The signal after the inserts (before fader, mute and solo).
    post_fx: Option<NodeId>,
    strip: Option<NodeId>,
    midi: Option<NodeId>,
    midi_in: Option<NodeId>,
    /// The last of the track's MIDI effects (what MIDI it sends on).
    midi_fx: Option<NodeId>,
    /// `input` is in the render-ahead graph.
    input_ahead: bool,
    /// So are `strip` and `post_fx` (the strip is rendered ahead).
    strip_ahead: bool,
}

/// Everything needed to turn plugin slots into graph nodes.
struct PluginCx<'a> {
    plugins: &'a mut PluginHost,
    process: ProcessConfig,
    realtime: bool,
    /// Frames per device callback (0: unknown).
    device_block: usize,
    warnings: &'a mut Vec<String>,
    /// Per plugin, the highest of its output buses tracks take (their
    /// nodes get outputs up to it).
    taken: HashMap<PluginInstanceId, u16>,
    /// Per plugin, which of its buses tracks take.
    taken_buses: HashMap<PluginInstanceId, u64>,
    /// Low-latency monitoring: the tracks a live input goes through (those
    /// heard live and the buses, returns and master after them — see
    /// [`monitored_path`]), whose effects with more latency than the limit
    /// are bypassed.
    monitor_path: HashSet<TrackId>,
    monitor_limit: u32,
    /// The effects it bypassed.
    bypassed_for_monitoring: Vec<PluginInstanceId>,
    /// Building a bus's cue path: every plugin node is the slot's *cue
    /// shadow* (a second instance, [`PluginInstanceId::cue_shadow`]),
    /// keyed apart from the main one.
    shadow: bool,
}

impl PluginCx<'_> {
    /// The track id node keys are made with (a cue path's apart from the
    /// main path's).
    fn key_track(&self, t: &Track) -> TrackId {
        if self.shadow {
            TrackId(t.id.raw() | (1 << 62))
        } else {
            t.id
        }
    }

    fn audio_chain<'a>(&mut self, t: &'a Track) -> Vec<&'a PluginSlot> {
        let mut slots: Vec<_> = t.inserts.iter().collect();
        if let Some(preamp) = &t.preamp {
            // Generators can replace their input. In particular, older
            // projects can have a legacy instrument plus a visible synth
            // insert. Put the preamp after the last generator so subsequent
            // instruments cannot discard its output. A MIDI-controlled
            // effect is not an instrument and keeps its normal placement.
            let position = if t.kind == TrackKind::Instrument {
                slots
                    .iter()
                    .rposition(|s| {
                        self.plugins.instance(s).is_ok_and(|i| {
                            i.descriptor().category
                                == faderframe_plugin_host::PluginCategory::Instrument
                        })
                    })
                    .map_or(0, |i| i + 1)
            } else {
                0
            };
            slots.insert(position, preamp);
        }
        slots
    }

    /// Does the plugin in `slot` take notes (instruments, MIDI-controlled
    /// effects)?
    /// A MIDI effect: notes in, notes out, no audio.
    fn note_effect(&mut self, slot: &PluginSlot) -> bool {
        self.plugins.instance(slot).is_ok_and(|inst| {
            let d = inst.descriptor();
            d.note_inputs > 0 && d.note_outputs > 0 && d.audio_outputs.is_empty()
        })
    }

    fn takes_notes(&mut self, slot: &PluginSlot) -> bool {
        self.plugins.instance(slot).is_ok_and(|inst| {
            let d = inst.descriptor();
            d.note_inputs > 0 || d.category == faderframe_plugin_host::PluginCategory::Instrument
        })
    }

    /// Layout of the plugin's sidechain input (its second audio input),
    /// if it has one.
    fn sidechain_layout(&mut self, slot: &PluginSlot) -> Option<ChannelLayout> {
        let inst = self.plugins.instance(slot).ok()?;
        let port = inst.descriptor().audio_inputs.get(1)?;
        (port.channels > 0).then(|| ChannelLayout::from_channel_count(port.channels as usize))
    }

    /// A plugin's node and its latency.
    fn node(
        &mut self,
        b: &mut GraphBuilder<EngineContext>,
        slot: &PluginSlot,
        track: &Track,
        spec: NodeSpec,
        role: Role,
    ) -> (NodeId, u32) {
        let mut spec = spec;
        // Hosted formats get their own main port layouts; the graph converts
        // between them and the track (mono ↔ stereo).
        if slot.plugin.format != faderframe_project::PluginFormat::Builtin
            && let Ok(inst) = self.plugins.instance(slot)
        {
            let d = inst.descriptor();
            if let (Some(l), Some(p)) = (spec.audio_inputs.first_mut(), d.audio_inputs.first())
                && p.channels > 0
            {
                *l = ChannelLayout::from_channel_count(p.channels as usize);
            }
            if let (Some(l), Some(p)) = (spec.audio_outputs.first_mut(), d.audio_outputs.first())
                && p.channels > 0
            {
                *l = ChannelLayout::from_channel_count(p.channels as usize);
            }
        }
        // Extra output buses tracks take, up to the highest (the channels
        // the plugin gives them). A cue shadow plays its main output only.
        let extra = if self.shadow {
            0
        } else {
            self.taken.get(&slot.id).copied().unwrap_or(0)
        };
        if extra > 0
            && let Ok(inst) = self.plugins.instance(slot)
        {
            let buses: Vec<u16> = inst
                .descriptor()
                .audio_outputs
                .iter()
                .map(|p| p.channels)
                .collect();
            for b in 1..=usize::from(extra) {
                let channels = buses.get(b).copied().filter(|c| *c > 0).unwrap_or(2);
                spec = spec.audio_out(ChannelLayout::from_channel_count(usize::from(channels)));
            }
        }
        let layouts: Vec<ChannelLayout> = spec
            .audio_inputs
            .iter()
            .chain(spec.audio_outputs.iter())
            .copied()
            .collect();
        let process = ProcessConfig {
            sidechain: spec.audio_inputs.len() > 1,
            ..self.process
        };
        // Low-latency monitoring: an effect with more latency than the
        // limit on a live input's path is bypassed (an instrument — no
        // audio in — never is).
        let limit = self.monitor_limit;
        let monitored = self.monitor_path.contains(&track.id)
            && matches!(role, Role::Insert)
            && self.plugins.instance(slot).is_ok_and(|i| {
                !i.descriptor().audio_inputs.is_empty() && i.latency_samples() > limit
            });
        let bypass = slot.bypass || monitored;
        if monitored && !slot.bypass && !self.shadow {
            self.bypassed_for_monitoring.push(slot.id);
        }
        // A cue shadow: the slot's second instance, hosted under the
        // shadow id (its state and values follow the slot's).
        let shadow_slot;
        let host_slot: &PluginSlot = if self.shadow {
            shadow_slot = PluginSlot {
                id: slot.id.cue_shadow(),
                ..slot.clone()
            };
            &shadow_slot
        } else {
            slot
        };
        let taken = if self.shadow {
            1
        } else {
            self.taken_buses.get(&slot.id).copied().unwrap_or(0) | 1
        };
        if let Ok(instance) = self.plugins.instance(host_slot) {
            instance.configure_realtime(self.realtime && !slot.bypass);
            // The same in both graphs: latency must not depend on which.
            instance.configure_device_block(self.device_block);
            instance
                .configure_channels(spec.audio_outputs.first().map_or(0, |l| l.channel_count()));
            instance.configure_outputs(spec.audio_outputs.len().max(1));
            instance.configure_taken_outputs(taken);
        }
        let key_track = self.key_track(track);
        match self.plugins.activate(host_slot, &process) {
            Ok(p) => {
                // Latency and bypass change the node's behaviour, and a
                // restarted plugin's old processor is dead: all are part of
                // its identity, so a change yields the fresh processor
                // instead of adopting the old one.
                // Only a processor whose execution mode actually changes
                // needs a different adoption key. External plugins retain
                // their keys when moving between live and ahead graphs.
                let buffered = p.processor.preferred_block_size() != usize::MAX;
                let sub = host_slot.id.raw()
                    ^ ((buffered as u64) << 62)
                    ^ ((p.latency as u64) << 40)
                    ^ ((bypass as u64) << 63)
                    ^ p.activation.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                let channels = spec.audio_outputs.first().map_or(0, |l| l.channel_count());
                let latency = if bypass { 0 } else { p.latency };
                let node = PluginNode::new(
                    track.id,
                    slot.id,
                    p.processor,
                    p.latency,
                    bypass,
                    p.failed,
                    channels,
                    p.missed,
                    spec.event_inputs > 0,
                );
                (
                    b.add_node(
                        spec.key(node_key(key_track, role, sub, &layouts)),
                        Box::new(node),
                    ),
                    latency,
                )
            }
            Err(e) => {
                self.warnings.push(format!(
                    "'{}' on track '{}' is unavailable ({e}); passing audio through",
                    slot.plugin.name, track.name
                ));
                (b.add_node(spec, Box::new(Passthrough)), 0)
            }
        }
    }
}

/// A hardware insert on the live graph: send level → device output from
/// its Send Channel; device input from its Return Channel (latency: the
/// round trip after the send) into the device's second input, the chain
/// into its first. `None` when it is not one, is bypassed or this graph
/// has no interface (it is then an ordinary insert passing the dry
/// signal).
#[allow(clippy::too_many_arguments)]
fn add_hardware_insert(
    b: &mut GraphBuilder<EngineContext>,
    pcx: &mut PluginCx<'_>,
    owners: &mut Vec<(NodeId, NodeOwner)>,
    t: &Track,
    slot: &PluginSlot,
    prev: NodeId,
    layout: ChannelLayout,
    gi: u32,
) -> Result<Option<NodeId>, EngineError> {
    if slot.plugin.format != faderframe_project::PluginFormat::Builtin
        || slot.plugin.id != faderframe_core::builtin::HARDWARE_INSERT
        || slot.bypass
        || !pcx.realtime
    {
        return Ok(None);
    }
    let Some(params) = pcx
        .plugins
        .instance(slot)
        .ok()
        .and_then(|i| i.tap())
        .map(|tap| tap.params.clone())
    else {
        return Ok(None);
    };
    let (send_ch, ret_ch, _, trip) =
        faderframe_plugin_host::devices::hardware_insert::routing(&params);
    let label = format!("{} · {}", t.name, t.name_of(slot));
    let id = slot.id.raw();
    let level = b.add_node(
        NodeSpec::new(format!("{label} · Send"))
            .key(node_key(t.id, Role::HardwareSend, id, &[layout]))
            .group(gi)
            .audio_in(layout)
            .audio_out(layout),
        Box::new(HardwareSendGain::new(params)),
    );
    owners.push((
        level,
        NodeOwner {
            track: t.id,
            plugin: Some(slot.id),
            work: NodeWork::Insert,
        },
    ));
    let send = b.add_node(
        NodeSpec::new(format!("{label} · Hardware Out"))
            .key(node_key(
                t.id,
                Role::DeviceOut,
                id ^ (u64::from(send_ch) << 48),
                &[layout],
            ))
            .role(NodeRole::DeviceOutput {
                first_channel: send_ch,
            })
            .group(gi)
            .audio_in(layout),
        Box::new(DeviceOutputSink),
    );
    owners.push((
        send,
        NodeOwner {
            track: t.id,
            plugin: Some(slot.id),
            work: NodeWork::HardwareOut,
        },
    ));
    b.connect_audio(prev, 0, level, 0)?;
    b.connect_audio(level, 0, send, 0)?;
    let ret = b.add_node(
        NodeSpec::new(format!("{label} · Hardware In"))
            .key(node_key(
                t.id,
                Role::HardwareReturn,
                id ^ (u64::from(ret_ch) << 48) ^ (u64::from(trip) << 24),
                &[layout],
            ))
            .role(NodeRole::DeviceInput {
                first_channel: ret_ch,
            })
            .after(send)
            .group(gi)
            .audio_out(layout),
        Box::new(HardwareReturn { latency: trip }),
    );
    owners.push((
        ret,
        NodeOwner {
            track: t.id,
            plugin: Some(slot.id),
            work: NodeWork::HardwareIn,
        },
    ));
    let spec = NodeSpec::new(label)
        .group(gi)
        .audio_in(layout)
        .audio_in(layout)
        .audio_out(layout);
    let (node, _) = pcx.node(b, slot, t, spec, Role::Insert);
    owners.push((
        node,
        NodeOwner {
            track: t.id,
            plugin: Some(slot.id),
            work: NodeWork::Insert,
        },
    ));
    b.connect_audio(prev, 0, node, 0)?;
    b.connect_audio(ret, 0, node, 1)?;
    Ok(Some(node))
}

/// What a `layout` going to a device with `outputs` channels from
/// `first` is folded down to (`None`: it fits, or it is not a bed, or the
/// device is unknown): the largest bed that fits, else stereo, else mono.
fn fold_for(layout: ChannelLayout, outputs: usize, first: usize) -> Option<ChannelLayout> {
    if outputs == 0 {
        return None;
    }
    faderframe_core::surround::fold_into(layout, outputs.saturating_sub(first))
}

/// Destination layout for a track's post-fader output.
fn destination_layout(project: &Project, track: &Track) -> ChannelLayout {
    project.destination_layout(track)
}

/// How the master is listened to (never part of a render unless asked).
#[derive(Clone, Debug)]
pub struct Listen {
    /// On headphones: rendered binaurally with this room …
    pub binaural: Option<faderframe_binaural::Room>,
    /// … through this head's ears …
    pub head: faderframe_binaural::Head,
    /// … evened out for the headphones.
    pub correction: Option<Arc<faderframe_binaural::Correction>>,
    /// The way a change reaches the running listening node (no rebuild).
    pub link: Arc<crate::nodes::ListenLink>,
}

impl Default for Listen {
    fn default() -> Self {
        Self {
            binaural: None,
            head: faderframe_binaural::Head::default(),
            correction: None,
            link: crate::nodes::ListenLink::new(),
        }
    }
}

impl PartialEq for Listen {
    fn eq(&self, other: &Self) -> bool {
        self.binaural == other.binaural
            && self.head == other.head
            && self.correction == other.correction
            && Arc::ptr_eq(&self.link, &other.link)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_graph(
    project: &Project,
    slots: &mut SlotRegistry,
    plugins: &mut PluginHost,
    config: &PrepareConfig,
    routing: &MidiRouting,
    mut ahead: Option<AheadPlan<'_>>,
    monitor: &[PluginSlot],
    listen: &Listen,
    monitoring: &Monitoring,
    talkback: &crate::slots::Talkback,
) -> Result<BuiltGraph, EngineError> {
    let early = &monitoring.tracks;
    let midi_ports = &routing.inputs;
    // Cue mixes are live monitoring: a render's engine builds none.
    let cues: Vec<&faderframe_project::CueMix> = if plugins.realtime() {
        project.cue_mixes.iter().collect()
    } else {
        Vec::new()
    };
    // Tracks whose own strip has the cue mixes' taps (buses have a cue
    // path of their own instead).
    let cue_taps = |t: &Track| {
        !cues.is_empty() && faderframe_project::cue::feeds_cues(t.kind) && !has_cue_path(t)
    };
    let mut b = GraphBuilder::<EngineContext>::new();
    let mut ab = GraphBuilder::<EngineContext>::new();
    let mut used_rings: Vec<Arc<AheadRing>> = Vec::new();
    // The shallow tier (buses rendered ahead).
    let mut bb = GraphBuilder::<EngineContext>::new();
    let mut bus_used_rings: Vec<Arc<AheadRing>> = Vec::new();
    let mut warnings = Vec::new();
    let double_precision = plugins.double_precision();
    let mut pcx = PluginCx {
        realtime: plugins.realtime(),
        // Offline renders have no device: their output must not depend on
        // the block they are rendered in.
        device_block: if plugins.realtime() {
            config.device_block
        } else {
            0
        },
        plugins,
        process: ProcessConfig {
            sample_rate: config.sample_rate,
            max_block_size: config.max_block_size as u32,
            sidechain: false,
            double_precision,
        },
        warnings: &mut warnings,
        taken: project.taken_plugin_outputs(),
        taken_buses: project.taken_plugin_buses(),
        monitor_path: monitored_path(project, &monitoring.tracks),
        monitor_limit: monitoring.limit,
        bypassed_for_monitoring: Vec::new(),
        shadow: false,
    };
    // Nodes (in the audio thread's graph) of plugins whose extra outputs
    // tracks take.
    let mut output_nodes: HashMap<PluginInstanceId, NodeId> = HashMap::new();
    let mut nodes: HashMap<TrackId, TrackNodes> = HashMap::new();
    // Per instrument track: the nodes taking its own MIDI as it comes (the
    // first MIDI effect, else the instrument and every insert that takes
    // notes); MIDI tracks routed there feed the same ones.
    let mut note_inputs: HashMap<TrackId, Vec<NodeId>> = HashMap::new();
    // (plugin node, source track) of connected sidechain inputs.
    let mut sidechains: Vec<(NodeId, TrackId)> = Vec::new();
    // Modulated tracks and their followers' sources; (modulator node,
    // input, source track) of those inputs.
    let mod_shape = crate::modulation::shape(project);
    let mut followers: Vec<(NodeId, u16, TrackId)> = Vec::new();
    let mut owners: Vec<(NodeId, NodeOwner)> = Vec::new();
    let own = |owners: &mut Vec<(NodeId, NodeOwner)>,
               node: NodeId,
               track: TrackId,
               plugin: Option<PluginInstanceId>,
               work: NodeWork| {
        owners.push((
            node,
            NodeOwner {
                track,
                plugin,
                work,
            },
        ));
        node
    };

    // Strips rendered ahead: the latency of their signal (buses rendered
    // ahead are built after everything that reaches them, to know it).
    let mut strip_latency: HashMap<TrackId, u32> = HashMap::new();
    // Every track rendered ahead: the latency of its chain's end (a bus's
    // with what reaches it).
    let mut ahead_latency: HashMap<TrackId, u32> = HashMap::new();
    let order = match &ahead {
        Some(plan) if !plan.strips.is_empty() => ahead_order(project, plan.tracks),
        _ => (0..project.tracks.len()).collect(),
    };

    // Pass 1: per-track chains.
    for gi in order {
        let t = &project.tracks[gi];
        let gi = gi as u32;
        // VCAs have no audio: they scale their members' strips. Folders
        // only hold tracks.
        if matches!(t.kind, TrackKind::Vca | TrackKind::Folder) {
            continue;
        }
        let mut tn = TrackNodes::default();
        let frozen = t.freeze.is_some();
        // Frozen: the rendered audio is the chain's (stereo, if a device
        // widened a mono track).
        let layout = if frozen { t.chain_layout() } else { t.layout };
        // What the chain ends in: the strip's input.
        let end_layout = t.chain_layout();
        // Rendered ahead: the chain into the deep tier's graph and a
        // reader here for the strip. A bus's chain is in the shallow tier's
        // (what reaches it sums there). A strip rendered ahead is there too,
        // after its track's chain (from the deep tier through a ring), and
        // its audio comes back here for the meters.
        if let Some(plan) = ahead.as_mut().filter(|p| p.tracks.contains(&t.id)) {
            let strip_ahead = plan.strips.contains(&t.id);
            let bus = is_bus(t.kind);
            let dest = destination_layout(project, t);
            let realtime = pcx.realtime;
            pcx.realtime = false;
            let (input, end, chain_latency) = if bus {
                build_ahead_chain(&mut bb, &mut pcx, slots, project, t, config)?
            } else {
                build_ahead_chain(&mut ab, &mut pcx, slots, project, t, config)?
            };
            pcx.realtime = realtime;
            // What reaches a bus is aligned at its input to the slowest of
            // it.
            let fed_latency = project
                .tracks
                .iter()
                .filter(|f| {
                    project.output_target(f) == Some(t.id)
                        || f.sends.iter().any(|s| s.enabled && s.target == t.id)
                })
                .filter_map(|f| strip_latency.get(&f.id).copied())
                .max()
                .unwrap_or(0);
            let latency = fed_latency + chain_latency;
            ahead_latency.insert(t.id, latency);
            if bus {
                tn.input = Some(input);
                tn.input_ahead = true;
            }
            let channels = end_layout.channel_count();
            // A reader of `ring` in graph `g` reporting `latency`.
            let reader = |g: &mut GraphBuilder<EngineContext>,
                          ring: &Arc<AheadRing>,
                          latency: u32,
                          l: ChannelLayout,
                          group: Option<u32>,
                          misses: &Arc<AtomicU64>| {
                let mut spec = NodeSpec::new(format!("{} · Ahead", t.name))
                    .key(node_key(
                        t.id,
                        Role::Ahead,
                        u64::from(latency) ^ ring.identity().rotate_left(17),
                        &[l],
                    ))
                    .audio_out(l);
                if let Some(gi) = group {
                    spec = spec.group(gi);
                }
                g.add_node(
                    spec,
                    Box::new(AheadReader::new(
                        Arc::clone(ring),
                        latency,
                        Arc::clone(misses),
                    )),
                )
            };
            let writer = |g: &mut GraphBuilder<EngineContext>,
                          ring: &Arc<AheadRing>,
                          l: ChannelLayout,
                          from: NodeId|
             -> Result<(), EngineError> {
                let w = g.add_node(
                    NodeSpec::new(format!("{} · To Ring", t.name))
                        .key(node_key(t.id, Role::Ahead, 1 ^ ring.identity(), &[l]))
                        .audio_in(l),
                    Box::new(AheadWriter::new(Arc::clone(ring))),
                );
                g.connect_audio(from, 0, w, 0)?;
                Ok(())
            };
            if strip_ahead {
                // The chain's end in the shallow tier.
                let end = if bus {
                    end
                } else {
                    let ring = ring_of(plan.rings, t.id, channels, plan.ring_frames);
                    used_rings.push(Arc::clone(&ring));
                    writer(&mut ab, &ring, end_layout, end)?;
                    reader(
                        &mut bb,
                        &ring,
                        chain_latency,
                        end_layout,
                        None,
                        &plan.misses,
                    )
                };
                let strip = add_strip(
                    &mut bb,
                    project,
                    slots,
                    config,
                    t,
                    None,
                    end,
                    StripKind::Main {
                        quiet: true,
                        cue_taps: cue_taps(t),
                    },
                )?;
                let echo = ring_of(
                    plan.bus_rings,
                    t.id,
                    dest.channel_count(),
                    plan.bus_ring_frames,
                );
                bus_used_rings.push(Arc::clone(&echo));
                writer(&mut bb, &echo, dest, strip)?;
                let back = reader(&mut b, &echo, latency, dest, Some(gi), &plan.misses);
                own(&mut owners, back, t.id, None, NodeWork::Ahead);
                let sends = t
                    .sends
                    .iter()
                    .filter(|s| s.enabled)
                    .map(|s| Ok((s.id, slots.send(s.id)?)))
                    .collect::<Result<Vec<_>, crate::slots::SlotsExhausted>>()?;
                let meters = b.add_node(
                    NodeSpec::new(format!("{} · Meters", t.name))
                        .key(node_key(t.id, Role::StripEcho, 0, &[dest]))
                        .group(gi)
                        .audio_in(dest),
                    Box::new(StripEcho::new(
                        t.id,
                        slots.strip(t.id)?,
                        slots.meter(t.id, dest.channel_count())?,
                        sends,
                    )),
                );
                own(&mut owners, meters, t.id, None, NodeWork::Strip);
                b.connect_audio(back, 0, meters, 0)?;
                tn.post_fx = Some(end);
                tn.strip = Some(strip);
                tn.strip_ahead = true;
                strip_latency.insert(t.id, latency);
            } else {
                // The chain's output to the strip here: a bus's from the
                // shallow tier, a track's from the deep one.
                let ring = if bus {
                    let r = ring_of(plan.bus_rings, t.id, channels, plan.bus_ring_frames);
                    writer(&mut bb, &r, end_layout, end)?;
                    bus_used_rings.push(Arc::clone(&r));
                    r
                } else {
                    let r = ring_of(plan.rings, t.id, channels, plan.ring_frames);
                    writer(&mut ab, &r, end_layout, end)?;
                    used_rings.push(Arc::clone(&r));
                    r
                };
                let back = reader(&mut b, &ring, latency, end_layout, Some(gi), &plan.misses);
                own(&mut owners, back, t.id, None, NodeWork::Ahead);
                let strip = add_strip(
                    &mut b,
                    project,
                    slots,
                    config,
                    t,
                    Some(gi),
                    back,
                    StripKind::Main {
                        quiet: false,
                        cue_taps: cue_taps(t),
                    },
                )?;
                tn.post_fx = Some(back);
                tn.strip = Some(own(&mut owners, strip, t.id, None, NodeWork::Strip));
            }
            nodes.insert(t.id, tn);
            continue;
        }
        if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) && !frozen {
            // A MIDI track mutes here (it has no strip).
            let mute = if t.kind == TrackKind::Midi {
                Some(slots.midi_mute(t.id)?)
            } else {
                None
            };
            let player = MidiClipPlayer::new(t.id);
            let midi = b.add_node(
                NodeSpec::new(format!("{} · MIDI", t.name))
                    .key(node_key(t.id, Role::MidiPlayer, 0, &[]))
                    .group(gi)
                    .events_out(1),
                Box::new(match mute {
                    Some(m) => player.with_mute(m),
                    None => player,
                }),
            );
            tn.midi = Some(own(&mut owners, midi, t.id, None, NodeWork::Midi));
            // Live input (played through while the session says so) and
            // editor auditioning.
            let filter = match &t.input {
                InputRouting::Midi { port, channel } => Some(MidiFilter {
                    port: port
                        .as_ref()
                        .map(|k| midi_ports.get(k).copied().unwrap_or(NO_PORT)),
                    channel: *channel,
                }),
                _ => None,
            };
            let sub = filter.map_or(0x0200_0000, |f| {
                (f.port.map_or(0x1_0000, u64::from) << 8) | f.channel.map_or(0xFF, u64::from)
            });
            let live = slots.midi_live(t.id)?;
            let node = b.add_node(
                NodeSpec::new(format!("{} · MIDI In", t.name))
                    .key(node_key(t.id, Role::MidiInput, sub, &[]))
                    .group(gi)
                    .events_out(1),
                Box::new({
                    let node = MidiInputNode::new(
                        t.id,
                        filter,
                        live,
                        std::sync::Arc::clone(&routing.shared),
                    );
                    match mute {
                        Some(m) => node.with_mute(m),
                        None => node,
                    }
                }),
            );
            tn.midi_in = Some(own(&mut owners, node, t.id, None, NodeWork::MidiInput));
            // MIDI tracks send their MIDI on through their MIDI effects.
            if t.kind == TrackKind::Midi {
                let chain: Vec<&PluginSlot> = t.inserts.iter().collect();
                let fx = note_effects(&mut b, &mut pcx, t, &chain, &[midi, node], Some(gi))?;
                for (slot, n) in &fx {
                    own(&mut owners, *n, t.id, Some(*slot), NodeWork::Insert);
                }
                tn.midi_fx = fx.last().map(|(_, n)| *n);
            }
            // An external MIDI device.
            if let Some(out) = &t.midi_output {
                let port = routing.outputs.get(&out.port).copied().unwrap_or(NO_PORT);
                let sink = b.add_node(
                    NodeSpec::new(format!("{} · MIDI Out", t.name))
                        .role(NodeRole::EventOutput { port })
                        .group(gi)
                        .events_in(1)
                        .events_out(1),
                    Box::new(MidiOutputSink::new(out.channel)),
                );
                own(&mut owners, sink, t.id, None, NodeWork::MidiOutput);
                if let Some(fx) = tn.midi_fx {
                    b.connect_events(fx, 0, sink, 0)?;
                } else {
                    b.connect_events(midi, 0, sink, 0)?;
                    if let Some(live) = tn.midi_in {
                        b.connect_events(live, 0, sink, 0)?;
                    }
                }
            }
        }
        if t.kind == TrackKind::Midi {
            nodes.insert(t.id, tn);
            continue;
        }

        let input = b.add_node(
            NodeSpec::new(format!("{} · Input", t.name))
                .key(node_key(t.id, Role::Input, 0, &[layout]))
                .group(gi)
                .audio_in(layout)
                .audio_out(layout),
            Box::new(Passthrough),
        );
        tn.input = Some(own(&mut owners, input, t.id, None, NodeWork::Input));
        // The modulators, between the input and the devices (a follower of
        // the input hears the track before its devices).
        let mut chain_start = input;
        if let Some((_, keys)) = mod_shape.iter().find(|(id, _)| *id == t.id) {
            let sub = keys
                .iter()
                .fold(keys.len() as u64, |h, k| h.rotate_left(13) ^ k.raw());
            let mut spec = NodeSpec::new(format!("{} · Modulators", t.name))
                .key(node_key(t.id, Role::Modulators, sub, &[layout]))
                .group(gi)
                .audio_in(layout)
                .audio_out(layout);
            for k in keys {
                spec = spec.audio_in(project.track(*k).map_or(layout, |s| s.layout));
            }
            let node = b.add_node(
                spec,
                Box::new(crate::modulation::ModNode::new(
                    t.id,
                    keys.clone(),
                    config.sample_rate,
                )),
            );
            followers.extend(
                keys.iter()
                    .enumerate()
                    .map(|(i, k)| (node, i as u16 + 1, *k)),
            );
            own(&mut owners, node, t.id, None, NodeWork::Modulators);
            b.connect_audio(input, 0, node, 0)?;
            chain_start = node;
        }

        let chain = if frozen {
            Vec::new()
        } else {
            pcx.audio_chain(t)
        };
        // MIDI effects first: the track's MIDI through them in order; an
        // instrument gets what the effects above it made of it (a legacy
        // instrument, before every insert, what all of them made).
        let sources: Vec<NodeId> = [tn.midi, tn.midi_in].into_iter().flatten().collect();
        let fx = if t.kind == TrackKind::Instrument {
            note_effects(&mut b, &mut pcx, t, &chain, &sources, Some(gi))?
        } else {
            Vec::new()
        };
        for (slot, n) in &fx {
            own(&mut owners, *n, t.id, Some(*slot), NodeWork::Insert);
        }
        let mut takes_notes = Vec::new();
        if let Some((_, first)) = fx.first() {
            takes_notes.push(*first);
        }
        let all_fx: Vec<NodeId> = match fx.last() {
            Some((_, last)) => vec![*last],
            None => sources.clone(),
        };

        match t.kind {
            // Frozen: the rendered audio, straight to the strip.
            TrackKind::Audio | TrackKind::Instrument if frozen => {
                let latency = t.freeze.as_ref().map_or(0, |f| {
                    ((f64::from(f.latency) * config.sample_rate / f64::from(project.sample_rate))
                        .round()) as u32
                });
                let player = b.add_node(
                    NodeSpec::new(format!("{} · Frozen", t.name))
                        .key(node_key(
                            t.id,
                            Role::ClipPlayer,
                            0xF0_0000 ^ (u64::from(latency) << 32),
                            &[layout],
                        ))
                        .group(gi)
                        .audio_out(layout),
                    Box::new(AudioClipPlayer::new(t.id).with_latency(latency)),
                );
                own(&mut owners, player, t.id, None, NodeWork::Clips);
                b.connect_audio(player, 0, input, 0)?;
            }
            TrackKind::Audio => {
                let voices = stretch_voices(project, t);
                let player = b.add_node(
                    NodeSpec::new(format!("{} · Clips", t.name))
                        .key(node_key(t.id, Role::ClipPlayer, voices.key(), &[layout]))
                        .group(gi)
                        .audio_out(layout),
                    Box::new(
                        if voices.is_empty() {
                            AudioClipPlayer::new(t.id)
                        } else {
                            AudioClipPlayer::with_voices(
                                t.id,
                                voices,
                                config.sample_rate,
                                config.max_block_size,
                            )
                        }
                        .live(pcx.realtime),
                    ),
                );
                own(&mut owners, player, t.id, None, NodeWork::Clips);
                b.connect_audio(player, 0, input, 0)?;
                if let InputRouting::Hardware { first_channel } = t.input
                    && t.monitor != MonitorMode::Off
                {
                    let dev = b.add_node(
                        NodeSpec::new(format!("{} · Hardware In", t.name))
                            .role(NodeRole::DeviceInput { first_channel })
                            .group(gi)
                            .audio_out(layout),
                        Box::new(DeviceInputTap),
                    );
                    own(&mut owners, dev, t.id, None, NodeWork::HardwareIn);
                    let gate = b.add_node(
                        NodeSpec::new(format!("{} · Monitor", t.name))
                            .group(gi)
                            .audio_in(layout)
                            .audio_out(layout),
                        Box::new(MonitorGate::new(t.id, t.monitor, t.record_arm)),
                    );
                    own(&mut owners, gate, t.id, None, NodeWork::Monitor);
                    b.connect_audio(dev, 0, gate, 0)?;
                    b.connect_audio(gate, 0, input, 0)?;
                }
            }
            TrackKind::Instrument => {
                if let Some(slot) = &t.instrument {
                    let spec = NodeSpec::new(format!("{} · {}", t.name, t.name_of(slot)))
                        .group(gi)
                        .events_in(1)
                        .audio_out(layout);
                    let (inst, _) = pcx.node(&mut b, slot, t, spec, Role::Instrument);
                    own(&mut owners, inst, t.id, Some(slot.id), NodeWork::Instrument);
                    if pcx.taken.contains_key(&slot.id) {
                        output_nodes.insert(slot.id, inst);
                    }
                    for f in &all_fx {
                        b.connect_events(*f, 0, inst, 0)?;
                    }
                    b.connect_audio(inst, 0, input, 0)?;
                    if fx.is_empty() {
                        takes_notes.push(inst);
                    }
                }
            }
            _ => {}
        }

        let mut prev = chain_start;
        let mut events = sources;
        let mut raw = true;
        let mut layout = layout;
        for slot in chain {
            // A container: its chains side by side.
            if slot.plugin.is_container() {
                let mut links = ChainLinks {
                    project,
                    notes: t.kind == TrackKind::Instrument,
                    events: &events,
                    raw,
                    takes_notes: &mut takes_notes,
                    sidechains: &mut sidechains,
                };
                (prev, _) = add_container(
                    &mut b,
                    &mut pcx,
                    slots,
                    &mut owners,
                    t,
                    slot,
                    (prev, layout),
                    gi,
                    0,
                    &mut links,
                )?;
                continue;
            }
            // A MIDI effect (built above): what follows gets its notes.
            if let Some((_, n)) = fx.iter().find(|(id, _)| *id == slot.id) {
                events = vec![*n];
                raw = false;
                continue;
            }
            // Outboard gear (live only: renders have no interface and pass
            // it by): the chain to the interface, its return into the
            // device's second input, the round trip the return's latency.
            if let Some(node) =
                add_hardware_insert(&mut b, &mut pcx, &mut owners, t, slot, prev, layout, gi)?
            {
                prev = node;
                continue;
            }
            // Inserts that take notes (a synth placed as an insert, MIDI-
            // controlled effects) get the track's MIDI too.
            let notes = t.kind == TrackKind::Instrument && pcx.takes_notes(slot);
            // A device that widens a mono track takes it as stereo (the
            // graph duplicates it) and everything after it is stereo.
            layout = slot.plugin.output_layout(layout);
            let mut spec = NodeSpec::new(format!("{} · {}", t.name, t.name_of(slot)))
                .group(gi)
                .audio_in(layout)
                .audio_out(layout);
            if notes {
                spec = spec.events_in(1);
            }
            // A sidechain source that has a pre-fader signal and does not
            // depend on this track.
            let key = slot.sidechain.filter(|&src| {
                project
                    .track(src)
                    .is_some_and(|s| s.kind.has_audio() && s.kind != TrackKind::Midi)
                    && !project.reaches(t.id, src, None)
            });
            let key = key.and_then(|src| Some((src, pcx.sidechain_layout(slot)?)));
            if let Some((_, l)) = key {
                spec = spec.audio_in(l);
            }
            let (node, _) = pcx.node(&mut b, slot, t, spec, Role::Insert);
            if let Some((src, _)) = key {
                sidechains.push((node, src));
            }
            if pcx.taken.contains_key(&slot.id) {
                output_nodes.insert(slot.id, node);
            }
            own(&mut owners, node, t.id, Some(slot.id), NodeWork::Insert);
            if t.preamp.as_ref().is_some_and(|p| p.id == slot.id) {
                tn.preamp = Some(node);
            }
            b.connect_audio(prev, 0, node, 0)?;
            if notes {
                for e in &events {
                    b.connect_events(*e, 0, node, 0)?;
                }
                // Before any MIDI effect: MIDI tracks routed here reach it
                // too.
                if raw {
                    takes_notes.push(node);
                }
            }
            prev = node;
        }

        let strip = add_strip(
            &mut b,
            project,
            slots,
            config,
            t,
            Some(gi),
            prev,
            StripKind::Main {
                quiet: false,
                cue_taps: cue_taps(t),
            },
        )?;
        tn.post_fx = Some(prev);
        tn.strip = Some(own(&mut owners, strip, t.id, None, NodeWork::Strip));
        nodes.insert(t.id, tn);
        if !takes_notes.is_empty() {
            note_inputs.insert(t.id, takes_notes);
        }
    }

    // Plugins' outputs into the tracks taking them, the main one (right
    // after the plugin) too; both on the audio thread: neither renders
    // ahead.
    for t in &project.tracks {
        let Some((plugin, bus)) = t.input.plugin_output() else {
            continue;
        };
        if let (Some(&node), Some(tn)) = (output_nodes.get(&plugin), nodes.get(&t.id))
            && let Some(input) = tn.input.filter(|_| !tn.input_ahead)
        {
            b.connect_audio(node, bus, input, 0)?;
        }
    }

    // Pass 2: outputs and sends. Objects join the master's output in its
    // renderer (after the loop: the master may come later).
    let has_objects = project.objects().next().is_some();
    let mut renderer: Option<NodeId> = None;
    let mut object_outs: Vec<NodeId> = Vec::new();
    for (gi, t) in project.tracks.iter().enumerate() {
        let gi = gi as u32;
        let Some(tn) = nodes.get(&t.id).copied() else {
            continue;
        };
        if t.kind == TrackKind::Midi {
            if let OutputRouting::Track { track } = t.output
                && let (Some(midi), Some(targets)) = (tn.midi, note_inputs.get(&track))
            {
                for &inst in targets {
                    if let Some(fx) = tn.midi_fx {
                        b.connect_events(fx, 0, inst, 0)?;
                    } else {
                        b.connect_events(midi, 0, inst, 0)?;
                        if let Some(live) = tn.midi_in {
                            b.connect_events(live, 0, inst, 0)?;
                        }
                    }
                }
            }
            continue;
        }
        let Some(strip) = tn.strip else { continue };
        // A strip rendered ahead connects there (to buses rendered ahead).
        let ahead_strip = tn.strip_ahead;
        let g = if ahead_strip { &mut bb } else { &mut b };
        let mut owned = |node: NodeId, work: NodeWork, plugin: Option<PluginInstanceId>| {
            if !ahead_strip {
                own(&mut owners, node, t.id, plugin, work);
            }
        };
        // A monitored album song's inserts after the master strip (post
        // fader, as the album renders the song).
        let mut out = strip;
        if t.kind == TrackKind::Master {
            let layout = destination_layout(project, t);
            for slot in monitor {
                let spec = NodeSpec::new(format!(
                    "Album · {}",
                    faderframe_project::device_name(monitor, slot)
                ))
                .group(gi)
                .audio_in(layout)
                .audio_out(layout);
                let (node, _) = pcx.node(g, slot, t, spec, Role::Insert);
                owned(node, NodeWork::Insert, Some(slot.id));
                g.connect_audio(out, 0, node, 0)?;
                out = node;
            }
            if has_objects && !ahead_strip {
                let meter = slots.meter(t.id, layout.channel_count())?;
                let node = g.add_node(
                    NodeSpec::new(format!("{} · Objects", t.name))
                        .key(node_key(t.id, Role::Renderer, 0, &[layout]))
                        .group(gi)
                        .audio_in(layout)
                        .audio_out(layout),
                    Box::new(ObjectRenderer::new(t.id, meter)),
                );
                owned(node, NodeWork::Strip, None);
                g.connect_audio(out, 0, node, 0)?;
                out = node;
                renderer = Some(node);
            }
        }
        // The node's graph is `g`'s.
        let reachable = |n: &TrackNodes| n.input.filter(|_| n.input_ahead == ahead_strip);
        match t.output {
            // An object (into the master: by default or by name).
            OutputRouting::Master | OutputRouting::Track { .. }
                if has_objects && !ahead_strip && project.is_object(t) =>
            {
                object_outs.push(out);
            }
            OutputRouting::Master | OutputRouting::Track { .. } => {
                if let Some(dst) = project
                    .output_target(t)
                    .and_then(|d| nodes.get(&d))
                    .and_then(reachable)
                {
                    connect_out(g, out, dst, early.contains(&t.id))?;
                }
            }
            OutputRouting::Hardware { first_channel } if !ahead_strip => {
                let mut dest = destination_layout(project, t);
                if t.kind == TrackKind::Master {
                    // Listening: headphones (binaural, on the device's first
                    // two channels), a bed folded to the device, the mono
                    // check. Its key leaves out how it is heard: a rebuild
                    // keeps the running node (switches reach it without
                    // one, crossfaded).
                    let to = fold_for(dest, config.device_outputs, first_channel as usize)
                        .unwrap_or(dest);
                    let mono = slots.monitor_mono()?;
                    let node = g.add_node(
                        NodeSpec::new(format!("{} · Listen", t.name))
                            .key(node_key(t.id, Role::DeviceOut, 0x115E0, &[dest, to]))
                            .group(gi)
                            .audio_in(dest)
                            .audio_out(to),
                        Box::new(ListenOut::new(
                            dest,
                            to,
                            listen,
                            config.sample_rate as u32,
                            config.max_block_size,
                            mono,
                            slots.bass_management()?,
                            slots.mix()?,
                        )),
                    );
                    owned(node, NodeWork::HardwareOut, None);
                    g.connect_audio(out, 0, node, 0)?;
                    out = node;
                    dest = to;
                } else if let Some(to) =
                    fold_for(dest, config.device_outputs, first_channel as usize)
                {
                    // A bed wider than the device: folded down to what it
                    // plays.
                    let fold = g.add_node(
                        NodeSpec::new(format!("{} · Fold-down", t.name))
                            .key(node_key(t.id, Role::DeviceOut, 0xF01D, &[dest, to]))
                            .group(gi)
                            .audio_in(dest)
                            .audio_out(to),
                        Box::new(FoldDown::new(dest, to, slots.mix()?)),
                    );
                    owned(fold, NodeWork::HardwareOut, None);
                    g.connect_audio(out, 0, fold, 0)?;
                    out = fold;
                    dest = to;
                }
                let hw = g.add_node(
                    NodeSpec::new(format!("{} · Hardware Out", t.name))
                        .key(node_key(
                            t.id,
                            Role::DeviceOut,
                            first_channel as u64,
                            &[dest],
                        ))
                        .role(NodeRole::DeviceOutput { first_channel })
                        .group(gi)
                        .audio_in(dest),
                    Box::new(DeviceOutputSink),
                );
                owned(hw, NodeWork::HardwareOut, None);
                connect_out(g, out, hw, early.contains(&t.id))?;
            }
            OutputRouting::Hardware { .. } | OutputRouting::None => {}
        }
        for send in t.sends.iter().filter(|s| s.enabled) {
            let Some(target) = project.track(send.target) else {
                continue;
            };
            let Some(dst) = nodes.get(&send.target).and_then(reachable) else {
                continue;
            };
            let (tap_node, tap_port, tap_layout) = match send.tap {
                SendTap::PreFx => (tn.preamp.or(tn.input).unwrap_or(strip), 0, t.layout),
                SendTap::PreFader => (strip, 1, t.chain_layout()),
                // Its own pan places the source (not the main mix's image).
                SendTap::PostFader if places_itself(t, send, target) => {
                    let kind = StripKind::Main {
                        quiet: false,
                        cue_taps: cue_taps(t),
                    };
                    match unpanned_ports(project, t, kind).0 {
                        Some(port) => (strip, port, t.chain_layout()),
                        None => (strip, 0, destination_layout(project, t)),
                    }
                }
                SendTap::PostFader => (strip, 0, destination_layout(project, t)),
            };
            let level = slots.send(send.id)?;
            // Taken before the panner, a mono or stereo track's send
            // follows the panner into a bed.
            let surround = |l: ChannelLayout| matches!(l, ChannelLayout::Surround(_));
            let follow = (matches!(send.tap, SendTap::PreFx | SendTap::PreFader)
                && !surround(t.layout)
                && surround(target.layout))
            .then(|| slots.strip_of(t.id))
            .flatten();
            let diffuse = follow.is_some() && is_diffuse(project, t);
            let mut node = SendNode::new(t.id, send.id, level)
                .with_pan(slots.send_pan(send.id)?)
                .with_layouts(tap_layout, target.layout, follow, slots.mix()?);
            if diffuse {
                node = node.with_diffuser(config.sample_rate);
            }
            let node = g.add_node(
                NodeSpec::new(format!("{} → {}", t.name, target.name))
                    .key(node_key(
                        t.id,
                        Role::Send,
                        send.id.raw()
                            | (u64::from(follow.is_some()) << 63)
                            | (u64::from(diffuse) << 62),
                        &[tap_layout, target.layout],
                    ))
                    .group(gi)
                    .audio_in(tap_layout)
                    .audio_out(target.layout),
                Box::new(if ahead_strip { node.quiet() } else { node }),
            );
            owned(node, NodeWork::Send, None);
            g.connect_audio(tap_node, tap_port, node, 0)?;
            connect_out(g, node, dst, early.contains(&t.id))?;
        }
    }
    if let Some(r) = renderer {
        for o in object_outs {
            b.connect_audio(o, 0, r, 0)?;
        }
    }

    // Pass 3: the cue mixes. Each sums its sends into its own strip (its
    // level and mute, dimmed for talkback), adds the talkback microphone
    // after it and plays on its outputs: a heard output like the
    // master's, aligned with it, never through the master's listening.
    // Every track that feeds cue mixes sends to each from its strip's cue
    // tap (before or after the fader, never muted), a send it has none of
    // silent — so the sends' levels, pans and switches are parameters.
    // Buses and returns send from their *cue path* (below): their sources
    // as the cue hears them, through their devices' cue shadows.
    let talk_in = talkback.input.filter(|_| !cues.is_empty()).map(|ch| {
        b.add_node(
            NodeSpec::new("Talkback · Hardware In")
                .key(node_key(
                    TALKBACK_CHANNEL,
                    Role::Talkback,
                    u64::from(ch),
                    &[ChannelLayout::Mono],
                ))
                .role(NodeRole::DeviceInput { first_channel: ch })
                .audio_out(ChannelLayout::Mono),
            Box::new(DeviceInputTap),
        )
    });
    // Per cue mix: its sum on the audio thread, and (made when a send is
    // rendered ahead) its sum in the shallow tier with the latency of what
    // reaches it.
    let mut cue_inputs: HashMap<CueId, NodeId> = HashMap::new();
    let mut cue_ahead: HashMap<CueId, (NodeId, u32)> = HashMap::new();
    for cue in &cues {
        let ch = cue.channel();
        let layout = cue.layout;
        let input = b.add_node(
            NodeSpec::new(format!("Cue · {}", cue.name))
                .key(node_key(ch, Role::CueInput, 0, &[layout]))
                .audio_in(layout)
                .audio_out(layout),
            Box::new(Passthrough),
        );
        let talk = slots.cue_talk(cue.id)?;
        let strip = b.add_node(
            NodeSpec::new(format!("Cue · {} · Strip", cue.name))
                .key(node_key(ch, Role::Strip, 1, &[layout, layout]))
                .audio_in(layout)
                .audio_out(layout),
            Box::new(
                ChannelStrip::new(
                    ch,
                    slots.strip(ch)?,
                    slots.meter(ch, layout.channel_count())?,
                    slots.mix()?,
                )
                .with_layouts(layout, layout)
                .with_trim(talk.dim),
            ),
        );
        own(&mut owners, strip, ch, None, NodeWork::Strip);
        b.connect_audio(input, 0, strip, 0)?;
        // After its level and dim: the talkback (never dimmed, heard at
        // once), then its outputs.
        let out = b.add_node(
            NodeSpec::new(format!("Cue · {} · Out", cue.name))
                .key(node_key(ch, Role::CueOut, 0, &[layout]))
                .audio_in(layout)
                .audio_out(layout),
            Box::new(Passthrough),
        );
        b.connect_audio(strip, 0, out, 0)?;
        if let Some(mic) = talk_in {
            let node = b.add_node(
                NodeSpec::new(format!("Talkback → {}", cue.name))
                    .key(node_key(
                        ch,
                        Role::Talkback,
                        0,
                        &[ChannelLayout::Mono, layout],
                    ))
                    .audio_in(ChannelLayout::Mono)
                    .audio_out(layout),
                Box::new(SendNode::plain(ch, talk.talk).with_layouts(
                    ChannelLayout::Mono,
                    layout,
                    None,
                    slots.mix()?,
                )),
            );
            own(&mut owners, node, ch, None, NodeWork::Send);
            b.connect_audio(mic, 0, node, 0)?;
            b.connect_audio_early(node, 0, out, 0)?;
        }
        let first_channel = cue.first_channel;
        let hw = b.add_node(
            NodeSpec::new(format!("Cue · {} · Hardware Out", cue.name))
                .key(node_key(
                    ch,
                    Role::DeviceOut,
                    u64::from(first_channel),
                    &[layout],
                ))
                .role(NodeRole::DeviceOutput { first_channel })
                .audio_in(layout),
            Box::new(DeviceOutputSink),
        );
        own(&mut owners, hw, ch, None, NodeWork::HardwareOut);
        b.connect_audio(out, 0, hw, 0)?;
        cue_inputs.insert(cue.id, input);
    }

    // The buses' cue paths, feeders first: a sum of their sources' cue
    // images (post-fader, panned, never muted) and of their sends taken
    // from the never-muted taps, through cue shadows of their devices
    // (outboard gear plays once, in the main mix) into a solo-safe strip.
    // A bus whose chain is rendered ahead has its cue path in the shallow
    // tier too (its sources are strips rendered ahead there); a cue path
    // there feeding one on the audio thread crosses through a ring.
    let mut cue_paths: HashMap<TrackId, CuePath> = HashMap::new();
    if !cues.is_empty() {
        for bi in cue_bus_order(project) {
            let t = &project.tracks[bi];
            let gi = bi as u32;
            let in_bb = ahead.as_ref().is_some_and(|p| p.tracks.contains(&t.id));
            let layout = t.layout;
            let spec = NodeSpec::new(format!("{} · Cue", t.name))
                .key(node_key(t.id, Role::CueBusInput, 0, &[layout]))
                .audio_in(layout)
                .audio_out(layout);
            let sum = if in_bb {
                bb.add_node(spec, Box::new(Passthrough))
            } else {
                let n = b.add_node(spec.group(gi), Box::new(Passthrough));
                own(&mut owners, n, t.id, None, NodeWork::Input);
                n
            };
            let mut fed_latency = 0u32;
            // Its sources' post-fader cue images (sub-buses: their cue
            // paths), and the sends reaching it from the never-muted taps.
            let mut feeds: Vec<(CueTap, Option<(&Track, &faderframe_project::AuxSend)>)> =
                Vec::new();
            for src in &project.tracks {
                if !faderframe_project::cue::feeds_cues(src.kind) {
                    continue;
                }
                if project.output_target(src) == Some(t.id)
                    && let Some(tap) =
                        cue_tap(project, &nodes, &cue_paths, &strip_latency, src, true)
                {
                    feeds.push((tap, None));
                }
                for send in src.sends.iter().filter(|x| x.enabled && x.target == t.id) {
                    let tap = match send.tap {
                        SendTap::PreFx => cue_prefx_tap(&nodes, &cue_paths, src),
                        SendTap::PreFader => {
                            cue_tap(project, &nodes, &cue_paths, &strip_latency, src, false)
                        }
                        SendTap::PostFader => {
                            cue_tap(project, &nodes, &cue_paths, &strip_latency, src, true).map(
                                |tap| {
                                    if places_itself(src, send, t) {
                                        unpanned_tap(project, tap, src)
                                    } else {
                                        tap
                                    }
                                },
                            )
                        }
                    };
                    if let Some(tap) = tap {
                        feeds.push((tap, Some((src, send))));
                    }
                }
            }
            for (tap, send) in feeds {
                let early_src = early.contains(&tap.track);
                // A send: its node where the tap is (the same slots as the
                // main one's, quiet).
                let (from, port, from_layout) = match send {
                    Some((src, send)) => {
                        let level = slots.send(send.id)?;
                        let surround = |l: ChannelLayout| matches!(l, ChannelLayout::Surround(_));
                        let follow = (matches!(send.tap, SendTap::PreFx | SendTap::PreFader)
                            && !surround(src.layout)
                            && surround(t.layout))
                        .then(|| slots.strip_of(src.id))
                        .flatten();
                        let diffuse = follow.is_some() && is_diffuse(project, src);
                        let mut node = SendNode::new(src.id, send.id, level)
                            .with_pan(slots.send_pan(send.id)?)
                            .with_layouts(tap.layout, t.layout, follow, slots.mix()?)
                            .quiet();
                        if diffuse {
                            node = node.with_diffuser(config.sample_rate);
                        }
                        let spec = NodeSpec::new(format!("{} → {} · Cue", src.name, t.name))
                            .key(node_key(
                                src.id,
                                Role::CueSend,
                                send.id.raw()
                                    | (u64::from(follow.is_some()) << 63)
                                    | (u64::from(diffuse) << 62)
                                    | (1 << 58),
                                &[tap.layout, t.layout],
                            ))
                            .audio_in(tap.layout)
                            .audio_out(t.layout);
                        let n = if tap.in_bb {
                            let n = bb.add_node(spec, Box::new(node));
                            bb.connect_audio(tap.node, tap.port, n, 0)?;
                            n
                        } else {
                            let n = b.add_node(spec.group(gi), Box::new(node));
                            own(&mut owners, n, src.id, None, NodeWork::Send);
                            b.connect_audio(tap.node, tap.port, n, 0)?;
                            n
                        };
                        (n, 0, t.layout)
                    }
                    None => (tap.node, tap.port, tap.layout),
                };
                match (in_bb, tap.in_bb) {
                    (true, true) => {
                        bb.connect_audio(from, port, sum, 0)?;
                        fed_latency = fed_latency.max(tap.latency);
                    }
                    (false, true) => {
                        let back = cross(
                            &mut bb,
                            &mut b,
                            ahead.as_mut(),
                            &mut bus_used_rings,
                            &mut owners,
                            from,
                            port,
                            from_layout,
                            tap.latency,
                            (tap.track, port as u8 | (u8::from(send.is_some()) << 4)),
                            &format!("{} → {} · Cue", tap.name, t.name),
                        )?;
                        b.connect_audio(back, 0, sum, 0)?;
                    }
                    // A cue path rendered ahead has every source there.
                    (true, false) => {}
                    (false, false) => {
                        if early_src {
                            b.connect_audio_early(from, port, sum, 0)?;
                        } else {
                            b.connect_audio(from, port, sum, 0)?;
                        }
                    }
                }
            }
            // Through the cue shadows of its devices into its strip.
            let realtime = pcx.realtime;
            if in_bb {
                pcx.realtime = false;
            }
            let g = if in_bb { &mut bb } else { &mut b };
            let (end, chain_latency, prefx) = build_cue_chain(
                g,
                &mut pcx,
                slots,
                &mut owners,
                &mut sidechains,
                project,
                t,
                config,
                sum,
                (!in_bb).then_some(gi),
            )?;
            pcx.realtime = realtime;
            let strip = add_strip(
                g,
                project,
                slots,
                config,
                t,
                (!in_bb).then_some(gi),
                end,
                StripKind::Cue,
            )?;
            if !in_bb {
                own(&mut owners, strip, t.id, None, NodeWork::Strip);
            }
            cue_paths.insert(
                t.id,
                CuePath {
                    prefx,
                    strip,
                    in_bb,
                    latency: fed_latency + chain_latency,
                },
            );
        }
    }

    // The cue sends: every channel into every cue mix.
    for cue in &cues {
        let ch = cue.channel();
        let layout = cue.layout;
        let Some(&input) = cue_inputs.get(&cue.id) else {
            continue;
        };
        let post = cue.post_fader();
        for (gi, t) in project.tracks.iter().enumerate() {
            if !faderframe_project::cue::feeds_cues(t.kind) {
                continue;
            }
            let Some(tap) = cue_tap(project, &nodes, &cue_paths, &strip_latency, t, post) else {
                continue;
            };
            // After the fader its own pan places the source, as before it
            // (not the main mix's image, which it would only balance).
            let tap = if cue_places_itself(t, cue) {
                unpanned_tap(project, tap, t)
            } else {
                tap
            };
            let s = slots.cue_send(t.id, cue.id)?;
            let spec = NodeSpec::new(format!("{} → {}", t.name, cue.name))
                .key(node_key(
                    t.id,
                    Role::CueSend,
                    cue.id.raw() | (u64::from(tap.port) << 60),
                    &[tap.layout, layout],
                ))
                .audio_in(tap.layout)
                .audio_out(layout);
            let node =
                SendNode::cue(t.id, cue.id, s).with_layouts(tap.layout, layout, None, slots.mix()?);
            if tap.in_bb {
                let n = bb.add_node(spec, Box::new(node.quiet()));
                bb.connect_audio(tap.node, tap.port, n, 0)?;
                let (sum, latency) = match cue_ahead.get(&cue.id) {
                    Some(&(sum, l)) => (sum, l),
                    None => (
                        bb.add_node(
                            NodeSpec::new(format!("Cue · {} · Ahead", cue.name))
                                .key(node_key(ch, Role::CueAhead, 0, &[layout]))
                                .audio_in(layout)
                                .audio_out(layout),
                            Box::new(Passthrough),
                        ),
                        0,
                    ),
                };
                bb.connect_audio(n, 0, sum, 0)?;
                cue_ahead.insert(cue.id, (sum, latency.max(tap.latency)));
            } else {
                let n = b.add_node(spec.group(gi as u32), Box::new(node));
                own(&mut owners, n, t.id, None, NodeWork::Send);
                b.connect_audio(tap.node, tap.port, n, 0)?;
                connect_out(&mut b, n, input, early.contains(&t.id))?;
            }
        }
    }
    // What was sent in the shallow tier comes back to the audio thread
    // with the latency of the slowest of it.
    for cue in &cues {
        let (Some(&(sum, latency)), Some(&input)) =
            (cue_ahead.get(&cue.id), cue_inputs.get(&cue.id))
        else {
            continue;
        };
        let back = cross(
            &mut bb,
            &mut b,
            ahead.as_mut(),
            &mut bus_used_rings,
            &mut owners,
            sum,
            0,
            cue.layout,
            latency,
            (cue.channel(), 0),
            &format!("Cue · {} · Ahead", cue.name),
        )?;
        b.connect_audio(back, 0, input, 0)?;
    }
    let bypassed_for_monitoring = std::mem::take(&mut pcx.bypassed_for_monitoring);
    // Sidechains tap their source after its inserts: before its fader,
    // mute and solo, so a muted "ghost" track can still key.
    for (node, src) in sidechains {
        if let Some(tap) = nodes.get(&src).and_then(|n| n.post_fx) {
            b.connect_audio(tap, 0, node, 1)?;
        }
    }
    // Followers hear their source tracks the same way, unless that would
    // close a loop (two tracks following each other: the second hears
    // nothing).
    for (node, port, src) in followers {
        if let Some(tap) = nodes.get(&src).and_then(|n| n.post_fx)
            && !b.would_cycle_with(&[(tap, node)])?
        {
            b.connect_audio(tap, 0, node, port)?;
        }
    }
    // Each direction taps the clean post-insert signal, never another leak.
    // Bus/aux/VCA strips break adjacency; hidden MIDI and the pinned master do not.
    if project.crosstalk {
        let visible: Vec<_> = project
            .tracks
            .iter()
            .filter(|t| !matches!(t.kind, TrackKind::Master | TrackKind::Midi))
            .collect();
        for pair in visible.windows(2) {
            let [left, right] = [pair[0], pair[1]];
            if ![left, right]
                .iter()
                .all(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
            {
                continue;
            }
            let (Some(l), Some(r)) = (nodes.get(&left.id), nodes.get(&right.id)) else {
                continue;
            };
            let (Some(l_src), Some(l_dst), Some(r_src), Some(r_dst)) =
                (l.post_fx, l.strip, r.post_fx, r.strip)
            else {
                continue;
            };
            // Sends, outputs and sidechains can already link these channels.
            // Omit both leak directions if they would close a routing cycle.
            if b.would_cycle_with(&[(l_src, r_dst), (r_src, l_dst)])? {
                warnings.push(format!(
                    "Crosstalk skipped between {} and {} to avoid routing feedback",
                    left.name, right.name
                ));
                continue;
            }
            for (donor, receiver, from, to) in
                [(left, right, l_src, r_dst), (right, left, r_src, l_dst)]
            {
                let Some(gi) = project.track_index(receiver.id) else {
                    continue;
                };
                let leak = b.add_node(
                    NodeSpec::new(format!("{} → {} · Crosstalk", donor.name, receiver.name))
                        .key(node_key(
                            receiver.id,
                            Role::Crosstalk,
                            donor.id.raw(),
                            &[donor.layout, receiver.layout],
                        ))
                        .group(gi as u32)
                        .audio_in(receiver.layout)
                        .audio_out(receiver.layout),
                    Box::new(Crosstalk::new(donor.id, slots.strip(donor.id)?)),
                );
                own(&mut owners, leak, receiver.id, None, NodeWork::Strip);
                b.connect_audio(from, 0, leak, 0)?;
                b.connect_audio(leak, 0, to, 0)?;
            }
        }
    }
    Ok(BuiltGraph {
        builder: b,
        bypassed_for_monitoring,
        warnings,
        owners,
        bus_ahead: ahead.as_ref().map(|_| (bb, bus_used_rings)),
        ahead: ahead.map(|_| (ab, used_rings)),
    })
}

/// What a container's devices connect to besides the audio.
struct ChainLinks<'a> {
    project: &'a Project,
    /// The track plays notes (an instrument track): devices that take
    /// notes get them.
    notes: bool,
    /// Where the notes come from at the container's place.
    events: &'a [NodeId],
    /// They are the track's own (no MIDI effect before): MIDI tracks
    /// routed to the track reach the chains too.
    raw: bool,
    takes_notes: &'a mut Vec<NodeId>,
    /// (node, source track) of sidechain inputs to connect.
    sidechains: &'a mut Vec<(NodeId, TrackId)>,
}

/// A container in a track's chain: its chains side by side from `prev`
/// (each its devices, then its level and balance) into a sum; the graph
/// aligns the chains' latencies there. Each chain's devices that take
/// notes get those in its key range (`ChainNotes`). A container without
/// chains, or bypassed, passes `prev` on. Returns its output and latency
/// (its slowest chain's).
#[allow(clippy::too_many_arguments)]
fn add_container(
    b: &mut GraphBuilder<EngineContext>,
    pcx: &mut PluginCx<'_>,
    slots: &mut SlotRegistry,
    owners: &mut Vec<(NodeId, NodeOwner)>,
    t: &faderframe_project::Track,
    container: &PluginSlot,
    (prev, layout): (NodeId, ChannelLayout),
    gi: u32,
    depth: usize,
    links: &mut ChainLinks<'_>,
) -> Result<(NodeId, u32), EngineError> {
    let Some(chains) = t.containers.get(&container.id).filter(|c| !c.is_empty()) else {
        return Ok((prev, 0));
    };
    if container.bypass {
        return Ok((prev, 0));
    }
    let mut latency = 0u32;
    let own = |owners: &mut Vec<(NodeId, NodeOwner)>, node, plugin| {
        owners.push((
            node,
            NodeOwner {
                track: t.id,
                plugin: Some(plugin),
                work: NodeWork::Insert,
            },
        ));
    };
    let key_track = pcx.key_track(t);
    let sum = b.add_node(
        NodeSpec::new(format!("{} · {}", t.name, t.name_of(container)))
            .key(node_key(
                key_track,
                Role::ContainerSum,
                container.id.raw(),
                &[layout],
            ))
            .group(gi)
            .audio_in(layout)
            .audio_out(layout),
        Box::new(Passthrough),
    );
    own(owners, sum, container.id);
    for (i, chain) in chains.iter().enumerate() {
        let mut from = prev;
        let mut chain_latency = 0u32;
        // The chain's notes: those of its key range.
        let wants_notes = links.notes
            && chain
                .inserts
                .iter()
                .any(|s| s.plugin.is_container() || pcx.takes_notes(s));
        let notes = if wants_notes {
            let sub = container.id.raw().rotate_left(8)
                ^ i as u64
                ^ (u64::from(chain.key_low) << 40)
                ^ (u64::from(chain.key_high) << 48);
            let f = b.add_node(
                NodeSpec::new(format!(
                    "{} · {} · {} · Notes",
                    t.name,
                    t.name_of(container),
                    chain.name
                ))
                .key(node_key(key_track, Role::ChainNotes, sub, &[]))
                .group(gi)
                .events_in(1)
                .events_out(1),
                Box::new(crate::nodes::ChainNotes::new(chain.key_low, chain.key_high)),
            );
            own(owners, f, container.id);
            for e in links.events {
                b.connect_events(*e, 0, f, 0)?;
            }
            if links.raw {
                links.takes_notes.push(f);
            }
            Some(f)
        } else {
            None
        };
        for slot in &chain.inserts {
            if slot.plugin.is_container() {
                if depth + 1 < faderframe_project::container::MAX_DEPTH {
                    // Inside: the notes of this chain.
                    let inner_events: Vec<NodeId> = notes.into_iter().collect();
                    let mut inner = ChainLinks {
                        project: links.project,
                        notes: links.notes,
                        events: &inner_events,
                        raw: false,
                        takes_notes: links.takes_notes,
                        sidechains: links.sidechains,
                    };
                    let (out, l) = add_container(
                        b,
                        pcx,
                        slots,
                        owners,
                        t,
                        slot,
                        (from, layout),
                        gi,
                        depth + 1,
                        &mut inner,
                    )?;
                    from = out;
                    chain_latency += l;
                }
                continue;
            }
            let mut spec = NodeSpec::new(format!("{} · {}", t.name, t.name_of(slot)))
                .group(gi)
                .audio_in(layout)
                .audio_out(layout);
            let takes = notes.is_some() && pcx.takes_notes(slot);
            if takes {
                spec = spec.events_in(1);
            }
            // A sidechain from another track (one that does not depend on
            // this one).
            let key = slot.sidechain.filter(|&src| {
                links
                    .project
                    .track(src)
                    .is_some_and(|s| s.kind.has_audio() && s.kind != TrackKind::Midi)
                    && !links.project.reaches(t.id, src, None)
            });
            let key = key.and_then(|src| Some((src, pcx.sidechain_layout(slot)?)));
            if let Some((_, l)) = key {
                spec = spec.audio_in(l);
            }
            let (node, l) = pcx.node(b, slot, t, spec, Role::Insert);
            chain_latency += l;
            own(owners, node, slot.id);
            if let Some((src, _)) = key {
                links.sidechains.push((node, src));
            }
            b.connect_audio(from, 0, node, 0)?;
            if let (true, Some(f)) = (takes, notes) {
                b.connect_events(f, 0, node, 0)?;
            }
            from = node;
        }
        let mix = b.add_node(
            NodeSpec::new(format!(
                "{} · {} · {}",
                t.name,
                t.name_of(container),
                chain.name
            ))
            .key(node_key(
                key_track,
                Role::ChainMix,
                container.id.raw().rotate_left(8) ^ i as u64,
                &[layout],
            ))
            .group(gi)
            .audio_in(layout)
            .audio_out(layout),
            Box::new(crate::nodes::ChainMix::new(slots.chain(container.id, i)?)),
        );
        own(owners, mix, container.id);
        b.connect_audio(from, 0, mix, 0)?;
        b.connect_audio(mix, 0, sum, 0)?;
        latency = latency.max(chain_latency);
    }
    Ok((sum, latency))
}

/// Tracks heard live: a hardware input monitored (always, or while armed
/// with auto monitoring) or live MIDI played into it. With low-latency
/// monitoring their effects with latency are bypassed and what leaves
/// them (outputs, sends) is summed as it comes ([`connect_out`]) — heard at
/// once instead of waiting for the plugin latency elsewhere.
pub fn live_monitored(project: &Project, live: &HashSet<TrackId>) -> HashSet<TrackId> {
    project
        .tracks
        .iter()
        .filter(|t| {
            let input = matches!(t.input, InputRouting::Hardware { .. })
                && match t.monitor {
                    MonitorMode::Off => false,
                    MonitorMode::Input => true,
                    MonitorMode::Auto => t.record_arm,
                };
            input || live.contains(&t.id)
        })
        .map(|t| t.id)
        .collect()
}

/// Low-latency monitoring as a graph is built: the tracks heard live
/// (their outputs and sends early) and the latency an effect on their
/// path may keep (frames).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Monitoring {
    pub tracks: HashSet<TrackId>,
    pub limit: u32,
}

/// The tracks a live input goes through: those heard live and every bus,
/// return and master their outputs and sends reach, on and on.
pub fn monitored_path(project: &Project, live: &HashSet<TrackId>) -> HashSet<TrackId> {
    let mut path: HashSet<TrackId> = live.clone();
    let mut todo: Vec<TrackId> = live.iter().copied().collect();
    while let Some(id) = todo.pop() {
        let Some(t) = project.track(id) else { continue };
        let next = project
            .output_target(t)
            .into_iter()
            .chain(t.sends.iter().filter(|s| s.enabled).map(|s| s.target));
        for n in next {
            if path.insert(n) {
                todo.push(n);
            }
        }
    }
    path
}

/// A track's output or send into `to`: summed as it arrives when `early`
/// (low-latency monitoring), else aligned with the other paths.
fn connect_out(
    g: &mut GraphBuilder<EngineContext>,
    from: NodeId,
    to: NodeId,
    early: bool,
) -> Result<(), faderframe_audio_graph::GraphError> {
    if early {
        g.connect_audio_early(from, 0, to, 0)
    } else {
        g.connect_audio(from, 0, to, 0)
    }
}

/// The tracks whose spread into a bed is diffuse (their strips and the
/// sends following them get decorrelation filters).
pub fn diffuse_tracks(project: &Project) -> Vec<TrackId> {
    project
        .tracks
        .iter()
        .filter(|t| is_diffuse(project, t))
        .map(|t| t.id)
        .collect()
}

fn is_diffuse(project: &Project, t: &Track) -> bool {
    project.surround_panned(t).is_some() && project.effective_surround(t).diffuse
}

/// What a parameter edit can change of the graph's shape (a change
/// rebuilds it): the diffuse spreads (their decorrelation filters), where
/// folder buses take their tracks' default outputs (moving a track into or
/// out of one, see `Project::output_target`), and the post-fader sends
/// that place their source themselves ([`places_itself`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamShape {
    pub diffuse: Vec<TrackId>,
    pub folder_routes: Vec<(TrackId, TrackId)>,
    pub placing_sends: Vec<faderframe_core::SendId>,
}

pub fn param_shape(project: &Project) -> ParamShape {
    let folder_routes = if project.tracks.iter().any(Track::is_folder_bus) {
        project
            .tracks
            .iter()
            .filter(|t| t.output == OutputRouting::Master)
            .filter_map(|t| project.folder_bus_of(t).map(|f| (t.id, f.id)))
            .collect()
    } else {
        Vec::new()
    };
    let placing_sends = project
        .tracks
        .iter()
        .flat_map(|t| {
            t.sends
                .iter()
                .filter(|s| {
                    project
                        .track(s.target)
                        .is_some_and(|target| places_itself(t, s, target))
                })
                .map(|s| s.id)
        })
        .collect();
    ParamShape {
        diffuse: diffuse_tracks(project),
        folder_routes,
        placing_sends,
    }
}

/// A post-fader send with a pan of its own places the track's source
/// itself (a mono source by the pan law, a stereo one balanced) into a
/// stereo destination, from the strip's post-fader *unpanned* output —
/// the track's own pan (or dual pan) is the main mix's, not the send's.
/// Without one ("as tapped") it follows the main mix's image.
fn places_itself(t: &Track, send: &faderframe_project::AuxSend, target: &Track) -> bool {
    send.enabled
        && send.tap == SendTap::PostFader
        && send.pan.is_some()
        && target.layout == ChannelLayout::Stereo
        && matches!(
            t.chain_layout(),
            ChannelLayout::Mono | ChannelLayout::Stereo
        )
}

/// A cue send placing its source itself: into a stereo cue mix after the
/// fader, from the strip's never-muted post-fader *unpanned* output, so
/// its pan places a mono or stereo source (the pan law, a balance) as it
/// does before the fader — not balancing the main mix's panned image.
/// (A mono cue mix, or a surround channel, takes the image as it is.)
fn cue_places_itself(t: &Track, cue: &faderframe_project::CueMix) -> bool {
    cue.post_fader()
        && cue.layout == ChannelLayout::Stereo
        && faderframe_project::cue::feeds_cues(t.kind)
        && matches!(
            t.chain_layout(),
            ChannelLayout::Mono | ChannelLayout::Stereo
        )
}

/// Does a cue mix take `t` from its never-muted post-fader unpanned
/// output ([`cue_places_itself`])?
fn cue_unpanned(project: &Project, t: &Track) -> bool {
    project
        .cue_mixes
        .iter()
        .any(|cue| cue_places_itself(t, cue))
}

/// Does `t`'s strip have the post-fader unpanned output (a send of it
/// [`places_itself`])?
fn has_unpanned(project: &Project, t: &Track) -> bool {
    t.sends.iter().any(|s| {
        project
            .track(s.target)
            .is_some_and(|target| places_itself(t, s, target))
    })
}

/// Which strip [`add_strip`] builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StripKind {
    /// The track's own: live or rendered ahead (`quiet`), with the cue
    /// mixes' taps (outputs 2 and 3) when `cue_taps`.
    Main { quiet: bool, cue_taps: bool },
    /// A bus's cue path's: solo-safe and quiet, outputs 0 (post-fader,
    /// panned, the destination layout), 1 (pre-fader) and 2 (the cue
    /// mixes' pre-fader tap).
    Cue,
}

/// A track's channel strip, fed from `from`.
#[allow(clippy::too_many_arguments)]
fn add_strip(
    b: &mut GraphBuilder<EngineContext>,
    project: &Project,
    slots: &mut SlotRegistry,
    config: &PrepareConfig,
    t: &Track,
    group: Option<u32>,
    from: NodeId,
    kind: StripKind,
) -> Result<NodeId, EngineError> {
    let layout = t.chain_layout();
    let cue = kind == StripKind::Cue;
    let (quiet, cue_taps) = match kind {
        StripKind::Main { quiet, cue_taps } => (quiet, cue_taps),
        StripKind::Cue => (true, false),
    };
    // The console's channel line amplifier, before the strip (wherever
    // the strip is built, the stage comes with it).
    let from = match &project.console {
        Some(c) if faderframe_project::console::has_channel_stage(t.kind) => {
            let mut spec = NodeSpec::new(format!("{} · Console", t.name))
                .key(node_key(
                    t.id,
                    Role::Console,
                    u64::from(c.family) | (u64::from(cue) << 32),
                    &[layout],
                ))
                .audio_in(layout)
                .audio_out(layout);
            if let Some(g) = group {
                spec = spec.group(g);
            }
            let node = b.add_node(
                spec,
                Box::new(crate::nodes::ConsoleChannel::new(
                    usize::from(c.family),
                    layout.channel_count(),
                    config.sample_rate,
                    slots.console_drive()?,
                )),
            );
            b.connect_audio(from, 0, node, 0)?;
            node
        }
        _ => from,
    };
    let dest = destination_layout(project, t);
    let strip_slots = slots.strip(t.id)?;
    let meter = slots.meter(t.id, dest.channel_count())?;
    // An object pans without its LFE send; with objects the master's
    // meters follow the renderer that adds them.
    let object = project.is_object(t);
    let metered_elsewhere =
        t.kind == TrackKind::Master && project.objects().next().is_some() && !quiet;
    let diffuse = is_diffuse(project, t);
    // A send placing the source itself: the post-fader unpanned output;
    // with the cue taps, its never-muted twin for the cue paths' copies of
    // those sends and for cue sends after the fader (the cue path of a bus
    // is never muted: its one unpanned output serves both).
    let (unpanned_port, cue_unpanned_port) = unpanned_ports(project, t, kind);
    // What the processor is made with is in its key: a rebuild must not
    // hand a strip made without the cue taps (cue mixes added while
    // playing) to a node with them — its cue outputs would stay unwritten
    // and its unpanned output land on the wrong port — nor the reverse.
    // Not whether it is `quiet`: that only says which graph it renders in
    // (ahead or live; adoption stays within a graph), and summing points
    // add their sources in key order, so a strip rendered ahead must keep
    // the key it has live or the sums round differently.
    let mut spec = NodeSpec::new(if cue {
        format!("{} · Cue Strip", t.name)
    } else {
        format!("{} · Strip", t.name)
    })
    .key(node_key(
        t.id,
        if cue { Role::CueStrip } else { Role::Strip },
        u64::from(object)
            | (u64::from(metered_elsewhere) << 1)
            | (u64::from(diffuse) << 2)
            | (u64::from(unpanned_port.is_some()) << 3)
            | (u64::from(cue_unpanned_port.is_some()) << 4)
            | (u64::from(cue_taps) << 5),
        &[layout, dest],
    ))
    .audio_in(layout)
    .audio_out(dest)
    .audio_out(layout);
    if cue_taps {
        spec = spec.audio_out(layout).audio_out(dest);
    }
    if cue {
        spec = spec.audio_out(layout);
    }
    // (After the others, in port order.)
    for _ in unpanned_port.iter().chain(&cue_unpanned_port) {
        spec = spec.audio_out(layout);
    }
    if let Some(g) = group {
        spec = spec.group(g);
    }
    let mut strip =
        ChannelStrip::new(t.id, strip_slots, meter, slots.mix()?).with_layouts(layout, dest);
    if cue_taps {
        strip = strip.with_cue_image();
    }
    if object {
        strip = strip.object();
    }
    if metered_elsewhere {
        strip = strip.metered_elsewhere();
    }
    if diffuse {
        strip = strip.with_diffuser(config.sample_rate);
    }
    if cue {
        strip = strip.solo_safe();
    }
    if let Some(port) = unpanned_port {
        strip = strip.with_unpanned(usize::from(port));
    }
    if let Some(port) = cue_unpanned_port {
        strip = strip.with_cue_unpanned(usize::from(port));
    }
    let strip = b.add_node(spec, Box::new(if quiet { strip.quiet() } else { strip }));
    b.connect_audio(from, 0, strip, 0)?;
    Ok(strip)
}

/// A strip's post-fader unpanned output (for its sends placing the
/// source themselves) and its never-muted twin (with the cue taps: for the
/// cue paths' copies of those sends and for cue sends after the fader),
/// each when something takes it, after the strip's other outputs: a
/// track's after outputs 0 and 1 and its cue taps (2, 3); a bus's cue
/// path's after its 0–2 (that strip is never muted: no twin, its one
/// unpanned output serves the sends and the cue mixes).
fn unpanned_ports(project: &Project, t: &Track, kind: StripKind) -> (Option<u16>, Option<u16>) {
    let sends = has_unpanned(project, t);
    match kind {
        StripKind::Main { cue_taps: true, .. } => {
            let twin = sends || cue_unpanned(project, t);
            (sends.then_some(4), twin.then_some(4 + u16::from(sends)))
        }
        StripKind::Main { .. } => (sends.then_some(2), None),
        StripKind::Cue => ((sends || cue_unpanned(project, t)).then_some(3), None),
    }
}

/// The talkback microphone's node key channel (no track of its own).
const TALKBACK_CHANNEL: TrackId = TrackId(0x7A1B_BAC4);

/// A bus's cue path (see pass 3 of [`build_graph`]).
#[derive(Clone, Copy)]
struct CuePath {
    /// After its preamp (what a pre-FX send taps).
    prefx: NodeId,
    /// Its solo-safe strip.
    strip: NodeId,
    /// In the shallow tier's graph.
    in_bb: bool,
    /// The latency of its strip's input (when in the shallow tier).
    latency: u32,
}

/// Where a channel's cue signal is taken: a node and port, its layout,
/// which graph it is in and (in the shallow tier) its latency.
#[derive(Clone, Copy)]
struct CueTap<'a> {
    track: TrackId,
    name: &'a str,
    node: NodeId,
    port: u16,
    layout: ChannelLayout,
    in_bb: bool,
    latency: u32,
}

/// `t`'s never-muted tap for the cue mixes: after the fader (`post`: the
/// post-fader image, panned, in the destination layout) or before it (the
/// chain layout) — a bus from its cue path's strip, a track from its own
/// strip.
fn cue_tap<'a>(
    project: &Project,
    nodes: &HashMap<TrackId, TrackNodes>,
    cue_paths: &HashMap<TrackId, CuePath>,
    strip_latency: &HashMap<TrackId, u32>,
    t: &'a Track,
    post: bool,
) -> Option<CueTap<'a>> {
    let layout = if post {
        destination_layout(project, t)
    } else {
        t.chain_layout()
    };
    if has_cue_path(t) {
        let c = cue_paths.get(&t.id)?;
        Some(CueTap {
            track: t.id,
            name: &t.name,
            node: c.strip,
            port: if post { 0 } else { 2 },
            layout,
            in_bb: c.in_bb,
            latency: c.latency,
        })
    } else {
        let n = nodes.get(&t.id)?;
        Some(CueTap {
            track: t.id,
            name: &t.name,
            node: n.strip?,
            port: if post { 3 } else { 2 },
            layout,
            in_bb: n.strip_ahead,
            latency: strip_latency.get(&t.id).copied().unwrap_or(0),
        })
    }
}

/// `tap` (a post-fader cue tap of `t`) moved to its strip's never-muted
/// post-fader *unpanned* output: what a send placing the source itself
/// ([`places_itself`]) takes on a cue path, and a cue send after the fader
/// ([`cue_places_itself`]) — a bus's cue path's strip (never muted), a
/// track's twin of the main mix's unpanned output. (As it was when the
/// strip has none.)
fn unpanned_tap<'a>(project: &Project, tap: CueTap<'a>, t: &Track) -> CueTap<'a> {
    let port = if has_cue_path(t) {
        unpanned_ports(project, t, StripKind::Cue).0
    } else {
        unpanned_ports(
            project,
            t,
            StripKind::Main {
                quiet: false,
                cue_taps: true,
            },
        )
        .1
    };
    match port {
        Some(port) => CueTap {
            port,
            layout: t.chain_layout(),
            ..tap
        },
        None => tap,
    }
}

/// `t`'s pre-FX tap for a cue path's send: a bus's cue path after its
/// preamp, a track's input (never muted, never rendered ahead).
fn cue_prefx_tap<'a>(
    nodes: &HashMap<TrackId, TrackNodes>,
    cue_paths: &HashMap<TrackId, CuePath>,
    t: &'a Track,
) -> Option<CueTap<'a>> {
    if has_cue_path(t) {
        let c = cue_paths.get(&t.id)?;
        Some(CueTap {
            track: t.id,
            name: &t.name,
            node: c.prefx,
            port: 0,
            layout: t.layout,
            in_bb: c.in_bb,
            latency: 0,
        })
    } else {
        let n = nodes.get(&t.id)?;
        let node = n.preamp.or(n.input).or(n.strip)?;
        (!n.input_ahead).then_some(CueTap {
            track: t.id,
            name: &t.name,
            node,
            port: 0,
            layout: t.layout,
            in_bb: false,
            latency: 0,
        })
    }
}

/// Audio of the shallow tier's `from` on the audio thread: a writer there
/// into a ring (kept across builds by `key`) and a reader here reporting
/// `latency`.
#[allow(clippy::too_many_arguments)]
fn cross(
    bb: &mut GraphBuilder<EngineContext>,
    b: &mut GraphBuilder<EngineContext>,
    plan: Option<&mut AheadPlan<'_>>,
    used: &mut Vec<Arc<AheadRing>>,
    owners: &mut Vec<(NodeId, NodeOwner)>,
    from: NodeId,
    port: u16,
    layout: ChannelLayout,
    latency: u32,
    key: (TrackId, u8),
    name: &str,
) -> Result<NodeId, EngineError> {
    let Some(plan) = plan else {
        return Err(faderframe_audio_graph::GraphError::UnknownNode(from).into());
    };
    let ring = ring_of(
        plan.cue_rings,
        key,
        layout.channel_count(),
        plan.bus_ring_frames,
    );
    used.push(Arc::clone(&ring));
    let sub = u64::from(key.1) << 8;
    let w = bb.add_node(
        NodeSpec::new(format!("{name} · To Ring"))
            .key(node_key(
                key.0,
                Role::CueAhead,
                sub | 1 ^ ring.identity(),
                &[layout],
            ))
            .audio_in(layout),
        Box::new(AheadWriter::new(Arc::clone(&ring))),
    );
    bb.connect_audio(from, port, w, 0)?;
    let r = b.add_node(
        NodeSpec::new(format!("{name} · Ahead"))
            .key(node_key(
                key.0,
                Role::CueAhead,
                sub | 2 ^ (u64::from(latency) << 16) ^ ring.identity().rotate_left(17),
                &[layout],
            ))
            .audio_out(layout),
        Box::new(AheadReader::new(ring, latency, Arc::clone(&plan.misses))),
    );
    owners.push((
        r,
        NodeOwner {
            track: key.0,
            plugin: None,
            work: NodeWork::Ahead,
        },
    ));
    Ok(r)
}

/// The buses with cue paths, feeders first (a bus after the buses whose
/// outputs or sends reach it).
fn cue_bus_order(project: &Project) -> Vec<usize> {
    let mut left: Vec<usize> = (0..project.tracks.len())
        .filter(|&i| has_cue_path(&project.tracks[i]))
        .collect();
    let mut order = Vec::new();
    let mut placed: HashSet<TrackId> = HashSet::new();
    while !left.is_empty() {
        let before = left.len();
        left.retain(|&i| {
            let id = project.tracks[i].id;
            let ready = project.tracks.iter().all(|f| {
                !has_cue_path(f)
                    || placed.contains(&f.id)
                    || f.id == id
                    || (project.output_target(f) != Some(id)
                        && !f.sends.iter().any(|s| s.enabled && s.target == id))
            });
            if ready {
                order.push(i);
                placed.insert(id);
            }
            !ready
        });
        if left.len() == before {
            // A loop (refused by the routing): whatever is left, in order.
            order.append(&mut left);
        }
    }
    order
}

/// A bus's devices on its cue path, from `input` (its cue sum): the cue
/// shadow of each (a second instance; outboard gear is passed by — it
/// plays once, in the main mix), containers included. Returns the chain's
/// end, its latency and the node after the preamp.
#[allow(clippy::too_many_arguments)]
fn build_cue_chain(
    g: &mut GraphBuilder<EngineContext>,
    pcx: &mut PluginCx<'_>,
    slots: &mut SlotRegistry,
    owners: &mut Vec<(NodeId, NodeOwner)>,
    sidechains: &mut Vec<(NodeId, TrackId)>,
    project: &Project,
    t: &Track,
    config: &PrepareConfig,
    input: NodeId,
    group: Option<u32>,
) -> Result<(NodeId, u32, NodeId), EngineError> {
    let _ = config;
    let was = pcx.shadow;
    pcx.shadow = true;
    let result = cue_chain_nodes(g, pcx, slots, owners, sidechains, project, t, input, group);
    pcx.shadow = was;
    result
}

#[allow(clippy::too_many_arguments)]
fn cue_chain_nodes(
    g: &mut GraphBuilder<EngineContext>,
    pcx: &mut PluginCx<'_>,
    slots: &mut SlotRegistry,
    owners: &mut Vec<(NodeId, NodeOwner)>,
    sidechains: &mut Vec<(NodeId, TrackId)>,
    project: &Project,
    t: &Track,
    input: NodeId,
    group: Option<u32>,
) -> Result<(NodeId, u32, NodeId), EngineError> {
    let chain = pcx.audio_chain(t);
    let mut prev = input;
    let mut prefx = input;
    let mut layout = t.layout;
    let mut latency = 0u32;
    let mut takes_notes = Vec::new();
    for slot in chain {
        if slot.plugin.is_container() {
            let mut links = ChainLinks {
                project,
                notes: false,
                events: &[],
                raw: false,
                takes_notes: &mut takes_notes,
                sidechains,
            };
            let (out, l) = add_container(
                g,
                pcx,
                slots,
                owners,
                t,
                slot,
                (prev, layout),
                group.unwrap_or(0),
                0,
                &mut links,
            )?;
            latency += l;
            prev = out;
            continue;
        }
        if slot.plugin.id == faderframe_core::builtin::HARDWARE_INSERT {
            continue;
        }
        layout = slot.plugin.output_layout(layout);
        let mut spec = NodeSpec::new(format!("{} · {} · Cue", t.name, t.name_of(slot)))
            .audio_in(layout)
            .audio_out(layout);
        if let Some(gi) = group {
            spec = spec.group(gi);
        }
        // A sidechain source that has a pre-fader signal and does not
        // depend on this track (as the main path's).
        let key = slot.sidechain.filter(|&src| {
            project
                .track(src)
                .is_some_and(|s| s.kind.has_audio() && s.kind != TrackKind::Midi)
                && !project.reaches(t.id, src, None)
        });
        let key = key.and_then(|src| Some((src, pcx.sidechain_layout(slot)?)));
        if let Some((_, l)) = key {
            spec = spec.audio_in(l);
        }
        let (node, l) = pcx.node(g, slot, t, spec, Role::Insert);
        if let Some((src, _)) = key {
            sidechains.push((node, src));
        }
        latency += l;
        if group.is_some() {
            owners.push((
                node,
                NodeOwner {
                    track: t.id,
                    plugin: Some(slot.id),
                    work: NodeWork::Insert,
                },
            ));
        }
        g.connect_audio(prev, 0, node, 0)?;
        if t.preamp.as_ref().is_some_and(|p| p.id == slot.id) {
            prefx = node;
        }
        prev = node;
    }
    Ok((prev, latency, prefx))
}

/// A track's MIDI effects in `chain` order, the first fed by `sources`,
/// each by the one before: the nodes by slot (in order).
fn note_effects(
    b: &mut GraphBuilder<EngineContext>,
    pcx: &mut PluginCx<'_>,
    t: &Track,
    chain: &[&PluginSlot],
    sources: &[NodeId],
    group: Option<u32>,
) -> Result<Vec<(faderframe_core::PluginInstanceId, NodeId)>, EngineError> {
    let mut out = Vec::new();
    let mut from: Vec<NodeId> = sources.to_vec();
    for slot in chain {
        if !pcx.note_effect(slot) {
            continue;
        }
        let mut spec = NodeSpec::new(format!("{} · {}", t.name, t.name_of(slot)))
            .events_in(1)
            .events_out(1);
        if let Some(g) = group {
            spec = spec.group(g);
        }
        let (node, _) = pcx.node(b, slot, t, spec, Role::Insert);
        for f in &from {
            b.connect_events(*f, 0, node, 0)?;
        }
        from = vec![node];
        out.push((slot.id, node));
    }
    Ok(out)
}

/// Track indices for building with buses rendered ahead: every other
/// track first (in project order), then those buses, each after the buses
/// that reach it.
fn ahead_order(project: &Project, tracks: &HashSet<TrackId>) -> Vec<usize> {
    let ahead_bus = |t: &Track| is_bus(t.kind) && tracks.contains(&t.id);
    let mut order: Vec<usize> = (0..project.tracks.len())
        .filter(|&i| !ahead_bus(&project.tracks[i]))
        .collect();
    let mut left: Vec<usize> = (0..project.tracks.len())
        .filter(|&i| ahead_bus(&project.tracks[i]))
        .collect();
    let mut placed: HashSet<TrackId> = HashSet::new();
    while !left.is_empty() {
        let before = left.len();
        left.retain(|&i| {
            let b = project.tracks[i].id;
            let ready = project.tracks.iter().all(|f| {
                !ahead_bus(f)
                    || placed.contains(&f.id)
                    || f.id == b
                    || (project.output_target(f) != Some(b)
                        && !f.sends.iter().any(|s| s.enabled && s.target == b))
            });
            if ready {
                order.push(i);
                placed.insert(b);
            }
            !ready
        });
        if left.len() == before {
            // A loop (the compiler rejects it): any order.
            order.append(&mut left);
        }
    }
    order
}

/// An anticipated track's sources, instrument and inserts in the ahead
/// graph `ab` (a bus: its input and inserts); returns the chain's input
/// (where buses sum what reaches them), its end and its latency.
fn build_ahead_chain(
    ab: &mut GraphBuilder<EngineContext>,
    pcx: &mut PluginCx<'_>,
    slots: &mut SlotRegistry,
    project: &Project,
    t: &Track,
    config: &PrepareConfig,
) -> Result<(NodeId, NodeId, u32), EngineError> {
    // Frozen: the rendered audio is the chain's (stereo, if a device
    // widened a mono track).
    let layout = if t.freeze.is_some() {
        t.chain_layout()
    } else {
        t.layout
    };
    let mut latency = 0u32;
    let input = ab.add_node(
        NodeSpec::new(format!("{} · Input", t.name))
            .key(node_key(t.id, Role::Input, 0, &[layout]))
            .audio_in(layout)
            .audio_out(layout),
        Box::new(Passthrough),
    );
    let mut midi = None;
    let frozen = t.freeze.as_ref();
    let chain = if frozen.is_some() {
        Vec::new()
    } else {
        pcx.audio_chain(t)
    };
    let mut fx = Vec::new();
    match t.kind {
        // Frozen: the rendered audio.
        TrackKind::Audio | TrackKind::Instrument if frozen.is_some() => {
            let frozen_latency = frozen.map_or(0, |f| {
                ((f64::from(f.latency) * config.sample_rate / f64::from(project.sample_rate))
                    .round()) as u32
            });
            let player = ab.add_node(
                NodeSpec::new(format!("{} · Frozen", t.name))
                    .key(node_key(
                        t.id,
                        Role::ClipPlayer,
                        0xF0_0000 ^ (u64::from(frozen_latency) << 32),
                        &[layout],
                    ))
                    .audio_out(layout),
                Box::new(AudioClipPlayer::new(t.id).with_latency(frozen_latency)),
            );
            latency += frozen_latency;
            ab.connect_audio(player, 0, input, 0)?;
        }
        TrackKind::Instrument => {
            let player = ab.add_node(
                NodeSpec::new(format!("{} · MIDI", t.name))
                    .key(node_key(t.id, Role::MidiPlayer, 0, &[]))
                    .events_out(1),
                Box::new(MidiClipPlayer::new(t.id)),
            );
            midi = Some(player);
            fx = note_effects(ab, pcx, t, &chain, &[player], None)?;
            if let Some(slot) = &t.instrument {
                let spec = NodeSpec::new(format!("{} · {}", t.name, t.name_of(slot)))
                    .events_in(1)
                    .audio_out(layout);
                let (inst, l) = pcx.node(ab, slot, t, spec, Role::Instrument);
                latency += l;
                let from = fx.last().map_or(player, |(_, n)| *n);
                ab.connect_events(from, 0, inst, 0)?;
                ab.connect_audio(inst, 0, input, 0)?;
            }
        }
        TrackKind::Audio => {
            let voices = stretch_voices(project, t);
            let player = ab.add_node(
                NodeSpec::new(format!("{} · Clips", t.name))
                    .key(node_key(t.id, Role::ClipPlayer, voices.key(), &[layout]))
                    .audio_out(layout),
                Box::new(if voices.is_empty() {
                    AudioClipPlayer::new(t.id)
                } else {
                    AudioClipPlayer::with_voices(
                        t.id,
                        voices,
                        config.sample_rate,
                        config.max_block_size,
                    )
                }),
            );
            ab.connect_audio(player, 0, input, 0)?;
        }
        // Buses: what reaches them.
        _ => {}
    }
    let mut prev = input;
    // The modulators (rendered ahead: they follow the song position, the
    // track's own input and its notes only).
    if !t.modulators.is_empty() && crate::modulation::modulated_kind(t.kind) {
        let node = ab.add_node(
            NodeSpec::new(format!("{} · Modulators", t.name))
                .key(node_key(t.id, Role::Modulators, 0, &[layout]))
                .audio_in(layout)
                .audio_out(layout),
            Box::new(crate::modulation::ModNode::new(
                t.id,
                Vec::new(),
                config.sample_rate,
            )),
        );
        ab.connect_audio(input, 0, node, 0)?;
        prev = node;
    }
    // Owners are not profiled in the ahead graph; nor are sidechains or
    // routed MIDI tracks part of it (such tracks stay live).
    let (mut owners, mut takes_notes, mut sidechains) = (Vec::new(), Vec::new(), Vec::new());
    let mut layout = layout;
    for slot in chain {
        if let Some((_, n)) = fx.iter().find(|(id, _)| *id == slot.id) {
            midi = Some(*n);
            continue;
        }
        if slot.plugin.is_container() {
            let events: Vec<NodeId> = midi.into_iter().collect();
            let mut links = ChainLinks {
                project,
                notes: t.kind == TrackKind::Instrument,
                events: &events,
                raw: false,
                takes_notes: &mut takes_notes,
                sidechains: &mut sidechains,
            };
            let (out, l) = add_container(
                ab,
                pcx,
                slots,
                &mut owners,
                t,
                slot,
                (prev, layout),
                0,
                0,
                &mut links,
            )?;
            latency += l;
            prev = out;
            continue;
        }
        let notes = t.kind == TrackKind::Instrument && pcx.takes_notes(slot);
        // A device that widens a mono track takes it as stereo (the graph
        // duplicates it) and everything after it is stereo.
        layout = slot.plugin.output_layout(layout);
        let mut spec = NodeSpec::new(format!("{} · {}", t.name, t.name_of(slot)))
            .audio_in(layout)
            .audio_out(layout);
        if notes {
            spec = spec.events_in(1);
        }
        let (node, l) = pcx.node(ab, slot, t, spec, Role::Insert);
        latency += l;
        ab.connect_audio(prev, 0, node, 0)?;
        if notes && let Some(midi) = midi {
            ab.connect_events(midi, 0, node, 0)?;
        }
        prev = node;
    }
    Ok((input, prev, latency))
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use faderframe_core::SurroundFormat;
    use faderframe_core::surround::{LayoutDef, SpeakerDef};

    fn custom(name: &str, speakers: usize) -> SurroundFormat {
        LayoutDef {
            name: name.into(),
            speakers: (0..speakers)
                .map(|i| SpeakerDef {
                    label: format!("S{i}"),
                    azimuth: 170.0 - 340.0 * i as f32 / speakers as f32,
                    elevation: 0.0,
                    lfe: false,
                })
                .collect(),
        }
        .intern()
        .unwrap()
    }

    /// Nodes keep their processors across rebuilds while their key holds
    /// (`adopt_state_from`), buffers sized for the layout included: the
    /// user's speaker layouts must key apart from each other and from the
    /// built-in formats (all once keyed as LCR — a master moved between
    /// them kept the listening node of the other width, and its crossfade
    /// read past its buffer on the audio thread).
    #[test]
    fn speaker_layouts_of_the_users_key_their_nodes_apart() {
        let key = |f: SurroundFormat| {
            node_key(
                TrackId(1),
                Role::DeviceOut,
                0,
                &[ChannelLayout::Surround(f)],
            )
        };
        let hexagon = custom("Key Test Hexagon", 6);
        let octagon = custom("Key Test Octagon", 8);
        assert_ne!(key(hexagon), key(octagon));
        assert_ne!(key(hexagon), key(SurroundFormat::Lcr));
        assert_ne!(key(octagon), key(SurroundFormat::Lcr));
        for f in SurroundFormat::ALL {
            assert_ne!(key(hexagon), key(f), "{}", f.name());
        }
        assert_eq!(
            key(hexagon),
            key(custom("Key Test Hexagon", 6)),
            "the same layout"
        );
    }
}

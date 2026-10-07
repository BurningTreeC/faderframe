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
    ListenOut, MidiClipPlayer, MonitorGate, ObjectRenderer, PluginNode, SendNode, StretchVoices,
    StripEcho,
};
use crate::plugins::PluginHost;
use crate::slots::SlotRegistry;
use faderframe_audio_graph::nodes::Passthrough;
use faderframe_audio_graph::{GraphBuilder, NodeId, NodeKey, NodeRole, NodeSpec, PrepareConfig};
use faderframe_core::PluginInstanceId;
use faderframe_core::{ChannelLayout, PanLaw, TrackId};
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
    pub misses: Arc<AtomicU64>,
}

/// `rings[track]` if it has `channels` and `frames`, else a new one there.
fn ring_of(
    rings: &mut HashMap<TrackId, Arc<AheadRing>>,
    track: TrackId,
    channels: usize,
    frames: usize,
) -> Arc<AheadRing> {
    let channels = channels.max(1);
    match rings.get(&track) {
        Some(r) if r.channels() == channels && r.capacity() == frames => Arc::clone(r),
        _ => {
            let r = AheadRing::new(channels, frames);
            rings.insert(track, Arc::clone(&r));
            r
        }
    }
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
/// clips.
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
        // Launched clips are played as they are launched.
        && !project.launcher.slots.keys().any(|k| k.track == t.id)
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
        let pitched = a.pitch.as_ref().is_some_and(|e| e.edited());
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
        eat(match l {
            ChannelLayout::Surround(f) => {
                0x1_0000
                    + faderframe_core::SurroundFormat::ALL
                        .iter()
                        .position(|x| x == f)
                        .unwrap_or(0) as u64
            }
            other => other.channel_count() as u64,
        });
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
}

impl PluginCx<'_> {
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
        if let Ok(instance) = self.plugins.instance(slot) {
            instance.configure_realtime(self.realtime && !slot.bypass);
            // The same in both graphs: latency must not depend on which.
            instance.configure_device_block(self.device_block);
            instance
                .configure_channels(spec.audio_outputs.first().map_or(0, |l| l.channel_count()));
        }
        match self.plugins.activate(slot, &process) {
            Ok(p) => {
                // Latency and bypass change the node's behaviour, and a
                // restarted plugin's old processor is dead: all are part of
                // its identity, so a change yields the fresh processor
                // instead of adopting the old one.
                // Only a processor whose execution mode actually changes
                // needs a different adoption key. External plugins retain
                // their keys when moving between live and ahead graphs.
                let buffered = p.processor.preferred_block_size() != usize::MAX;
                let sub = slot.id.raw()
                    ^ ((buffered as u64) << 62)
                    ^ ((p.latency as u64) << 40)
                    ^ ((slot.bypass as u64) << 63)
                    ^ p.activation.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                let channels = spec.audio_outputs.first().map_or(0, |l| l.channel_count());
                let latency = if slot.bypass { 0 } else { p.latency };
                let node = PluginNode::new(
                    track.id,
                    slot.id,
                    p.processor,
                    p.latency,
                    slot.bypass,
                    p.failed,
                    channels,
                );
                (
                    b.add_node(
                        spec.key(node_key(track.id, role, sub, &layouts)),
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Listen {
    /// On headphones: rendered binaurally with this room.
    pub binaural: Option<faderframe_binaural::Room>,
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
    listen: Listen,
) -> Result<BuiltGraph, EngineError> {
    let midi_ports = &routing.inputs;
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
    };
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
        let layout = t.layout;
        let frozen = t.freeze.is_some();
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
            if bus {
                tn.input = Some(input);
                tn.input_ahead = true;
            }
            let channels = layout.channel_count();
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
                    writer(&mut ab, &ring, layout, end)?;
                    reader(&mut bb, &ring, chain_latency, layout, None, &plan.misses)
                };
                let strip = add_strip(&mut bb, project, slots, t, None, end, true)?;
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
                    writer(&mut bb, &r, layout, end)?;
                    bus_used_rings.push(Arc::clone(&r));
                    r
                } else {
                    let r = ring_of(plan.rings, t.id, channels, plan.ring_frames);
                    writer(&mut ab, &r, layout, end)?;
                    used_rings.push(Arc::clone(&r));
                    r
                };
                let back = reader(&mut b, &ring, latency, layout, Some(gi), &plan.misses);
                own(&mut owners, back, t.id, None, NodeWork::Ahead);
                let strip = add_strip(&mut b, project, slots, t, Some(gi), back, false)?;
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
                        Box::new(MonitorGate::new(t.monitor, t.record_arm)),
                    );
                    own(&mut owners, gate, t.id, None, NodeWork::Monitor);
                    b.connect_audio(dev, 0, gate, 0)?;
                    b.connect_audio(gate, 0, input, 0)?;
                }
            }
            TrackKind::Instrument => {
                if let Some(slot) = &t.instrument {
                    let spec = NodeSpec::new(format!("{} · {}", t.name, slot.plugin.name))
                        .group(gi)
                        .events_in(1)
                        .audio_out(layout);
                    let (inst, _) = pcx.node(&mut b, slot, t, spec, Role::Instrument);
                    own(&mut owners, inst, t.id, Some(slot.id), NodeWork::Instrument);
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
                    prev,
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
            // Inserts that take notes (a synth placed as an insert, MIDI-
            // controlled effects) get the track's MIDI too.
            let notes = t.kind == TrackKind::Instrument && pcx.takes_notes(slot);
            let mut spec = NodeSpec::new(format!("{} · {}", t.name, slot.plugin.name))
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

        let strip = add_strip(&mut b, project, slots, t, Some(gi), prev, false)?;
        tn.post_fx = Some(prev);
        tn.strip = Some(own(&mut owners, strip, t.id, None, NodeWork::Strip));
        nodes.insert(t.id, tn);
        if !takes_notes.is_empty() {
            note_inputs.insert(t.id, takes_notes);
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
                let spec = NodeSpec::new(format!("Album · {}", slot.plugin.name))
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
            OutputRouting::Master if has_objects && !ahead_strip && project.is_object(t) => {
                object_outs.push(out);
            }
            OutputRouting::Master | OutputRouting::Track { .. } => {
                if let Some(dst) = project
                    .output_target(t)
                    .and_then(|d| nodes.get(&d))
                    .and_then(reachable)
                {
                    g.connect_audio(out, 0, dst, 0)?;
                }
            }
            OutputRouting::Hardware { first_channel } if !ahead_strip => {
                let mut dest = destination_layout(project, t);
                if t.kind == TrackKind::Master {
                    // Listening: headphones (binaural), a bed folded to the
                    // device, the mono check.
                    let to = if listen.binaural.is_some() {
                        ChannelLayout::Stereo
                    } else {
                        fold_for(dest, config.device_outputs, first_channel as usize)
                            .unwrap_or(dest)
                    };
                    let room = listen.binaural.map_or(0, |r| 1 + r as u64);
                    let mono = slots.monitor_mono()?;
                    let node = g.add_node(
                        NodeSpec::new(format!("{} · Listen", t.name))
                            .key(node_key(t.id, Role::DeviceOut, 0x115E0 + room, &[dest, to]))
                            .group(gi)
                            .audio_in(dest)
                            .audio_out(to),
                        Box::new(ListenOut::new(
                            dest,
                            to,
                            listen.binaural,
                            config.sample_rate as u32,
                            config.max_block_size,
                            mono,
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
                        Box::new(FoldDown::new(dest, to)),
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
                g.connect_audio(out, 0, hw, 0)?;
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
                SendTap::PreFader => (strip, 1, t.layout),
                SendTap::PostFader => (strip, 0, destination_layout(project, t)),
            };
            let level = slots.send(send.id)?;
            // Taken before the panner, a mono or stereo track's send
            // follows the panner into a bed.
            let surround = |l: ChannelLayout| matches!(l, ChannelLayout::Surround(_));
            let follow = (matches!(send.tap, SendTap::PreFx | SendTap::PreFader)
                && !surround(t.layout)
                && surround(target.layout))
            .then(|| slots.strip_of(t.id).map(|s| s.surround))
            .flatten();
            let node =
                SendNode::new(t.id, send.id, level).with_layouts(tap_layout, target.layout, follow);
            let node = g.add_node(
                NodeSpec::new(format!("{} → {}", t.name, target.name))
                    .key(node_key(
                        t.id,
                        Role::Send,
                        send.id.raw() | (u64::from(follow.is_some()) << 63),
                        &[tap_layout, target.layout],
                    ))
                    .group(gi)
                    .audio_in(tap_layout)
                    .audio_out(target.layout),
                Box::new(if ahead_strip { node.quiet() } else { node }),
            );
            owned(node, NodeWork::Send, None);
            g.connect_audio(tap_node, tap_port, node, 0)?;
            g.connect_audio(node, 0, dst, 0)?;
        }
    }
    if let Some(r) = renderer {
        for o in object_outs {
            b.connect_audio(o, 0, r, 0)?;
        }
    }
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
    prev: NodeId,
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
    let layout = t.layout;
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
    let sum = b.add_node(
        NodeSpec::new(format!("{} · {}", t.name, container.plugin.name))
            .key(node_key(
                t.id,
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
                    t.name, container.plugin.name, chain.name
                ))
                .key(node_key(t.id, Role::ChainNotes, sub, &[]))
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
                        from,
                        gi,
                        depth + 1,
                        &mut inner,
                    )?;
                    from = out;
                    chain_latency += l;
                }
                continue;
            }
            let mut spec = NodeSpec::new(format!("{} · {}", t.name, slot.plugin.name))
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
                t.name, container.plugin.name, chain.name
            ))
            .key(node_key(
                t.id,
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

/// A track's channel strip, fed from `from` (`quiet`: rendered ahead).
fn add_strip(
    b: &mut GraphBuilder<EngineContext>,
    project: &Project,
    slots: &mut SlotRegistry,
    t: &Track,
    group: Option<u32>,
    from: NodeId,
    quiet: bool,
) -> Result<NodeId, EngineError> {
    let layout = t.layout;
    let dest = destination_layout(project, t);
    let strip_slots = slots.strip(t.id)?;
    let meter = slots.meter(t.id, dest.channel_count())?;
    // An object pans without its LFE send; with objects the master's
    // meters follow the renderer that adds them.
    let object = project.is_object(t);
    let metered_elsewhere =
        t.kind == TrackKind::Master && project.objects().next().is_some() && !quiet;
    let mut spec = NodeSpec::new(format!("{} · Strip", t.name))
        .key(node_key(
            t.id,
            Role::Strip,
            u64::from(object) | (u64::from(metered_elsewhere) << 1),
            &[layout, dest],
        ))
        .audio_in(layout)
        .audio_out(dest)
        .audio_out(layout);
    if let Some(g) = group {
        spec = spec.group(g);
    }
    let mut strip =
        ChannelStrip::new(t.id, strip_slots, meter, PanLaw::default()).with_layouts(layout, dest);
    if object {
        strip = strip.object();
    }
    if metered_elsewhere {
        strip = strip.metered_elsewhere();
    }
    let strip = b.add_node(spec, Box::new(if quiet { strip.quiet() } else { strip }));
    b.connect_audio(from, 0, strip, 0)?;
    Ok(strip)
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
        let mut spec = NodeSpec::new(format!("{} · {}", t.name, slot.plugin.name))
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
    let layout = t.layout;
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
                let spec = NodeSpec::new(format!("{} · {}", t.name, slot.plugin.name))
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
            let (out, l) =
                add_container(ab, pcx, slots, &mut owners, t, slot, prev, 0, 0, &mut links)?;
            latency += l;
            prev = out;
            continue;
        }
        let notes = t.kind == TrackKind::Instrument && pcx.takes_notes(slot);
        let mut spec = NodeSpec::new(format!("{} · {}", t.name, slot.plugin.name))
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

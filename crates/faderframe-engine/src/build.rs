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

use crate::EngineError;
use crate::context::EngineContext;
use crate::midi::{MidiFilter, MidiInputNode, MidiOutputSink, MidiShared, NO_PORT};
use crate::nodes::{
    AudioClipPlayer, ChannelStrip, DeviceInputTap, DeviceOutputSink, MidiClipPlayer, MonitorGate,
    PluginNode, SendNode, StretchVoices,
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
use std::collections::HashMap;

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
}

/// Stretcher voices a track's clip player needs: one per pitch-preserving
/// warp preset its clips use, two when such clips overlap.
pub fn stretch_voices(project: &Project, track: &faderframe_project::Track) -> StretchVoices {
    use faderframe_project::{ClipContent, WarpAlgorithm};
    let mut spans: [Vec<(i64, i64)>; 2] = [Vec::new(), Vec::new()];
    for c in project.clips_of(track.id) {
        let ClipContent::Audio(a) = &c.content else {
            continue;
        };
        let Some(w) = a.warp.as_ref() else { continue };
        if c.muted
            || a.reversed
            || track.freeze.is_some()
            || w.is_identity(a.source_offset, a.length)
        {
            continue;
        }
        let i = match w.algorithm {
            WarpAlgorithm::Polyphonic => 0,
            WarpAlgorithm::Rhythmic => 1,
            WarpAlgorithm::Varispeed => continue,
        };
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
    let [mut poly, mut rhythmic] = spans;
    StretchVoices {
        polyphonic: need(&mut poly),
        rhythmic: need(&mut rhythmic),
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
        eat(l.channel_count() as u64);
    }
    NodeKey(h)
}

#[derive(Default, Clone, Copy)]
struct TrackNodes {
    input: Option<NodeId>,
    /// The signal after the inserts (before fader, mute and solo).
    post_fx: Option<NodeId>,
    strip: Option<NodeId>,
    instrument: Option<NodeId>,
    midi: Option<NodeId>,
    midi_in: Option<NodeId>,
}

/// Everything needed to turn plugin slots into graph nodes.
struct PluginCx<'a> {
    plugins: &'a mut PluginHost,
    process: ProcessConfig,
    warnings: &'a mut Vec<String>,
}

impl PluginCx<'_> {
    /// Does the plugin in `slot` take notes (instruments, MIDI-controlled
    /// effects)?
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

    fn node(
        &mut self,
        b: &mut GraphBuilder<EngineContext>,
        slot: &PluginSlot,
        track: &Track,
        spec: NodeSpec,
        role: Role,
    ) -> NodeId {
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
        match self.plugins.activate(slot, &process) {
            Ok(p) => {
                // Latency and bypass change the node's behaviour, and a
                // restarted plugin's old processor is dead: all are part of
                // its identity, so a change yields the fresh processor
                // instead of adopting the old one.
                let sub = slot.id.raw()
                    ^ ((p.latency as u64) << 40)
                    ^ ((slot.bypass as u64) << 63)
                    ^ p.activation.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                let channels = spec.audio_outputs.first().map_or(0, |l| l.channel_count());
                let node = PluginNode::new(
                    track.id,
                    slot.id,
                    p.processor,
                    p.latency,
                    slot.bypass,
                    p.failed,
                    channels,
                );
                b.add_node(
                    spec.key(node_key(track.id, role, sub, &layouts)),
                    Box::new(node),
                )
            }
            Err(e) => {
                self.warnings.push(format!(
                    "'{}' on track '{}' is unavailable ({e}); passing audio through",
                    slot.plugin.name, track.name
                ));
                b.add_node(spec, Box::new(Passthrough))
            }
        }
    }
}

/// Destination layout for a track's post-fader output.
fn destination_layout(project: &Project, track: &Track) -> ChannelLayout {
    match project.output_target(track).and_then(|t| project.track(t)) {
        Some(dst) if dst.kind.has_audio() => dst.layout,
        _ => track.layout,
    }
}

pub fn build_graph(
    project: &Project,
    slots: &mut SlotRegistry,
    plugins: &mut PluginHost,
    config: &PrepareConfig,
    routing: &MidiRouting,
) -> Result<BuiltGraph, EngineError> {
    let midi_ports = &routing.inputs;
    let mut b = GraphBuilder::<EngineContext>::new();
    let mut warnings = Vec::new();
    let mut pcx = PluginCx {
        plugins,
        process: ProcessConfig {
            sample_rate: config.sample_rate,
            max_block_size: config.max_block_size as u32,
            sidechain: false,
        },
        warnings: &mut warnings,
    };
    let mut nodes: HashMap<TrackId, TrackNodes> = HashMap::new();
    // (plugin node, source track) of connected sidechain inputs.
    let mut sidechains: Vec<(NodeId, TrackId)> = Vec::new();
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

    // Pass 1: per-track chains.
    for (gi, t) in project.tracks.iter().enumerate() {
        let gi = gi as u32;
        // VCAs have no audio: they scale their members' strips.
        if t.kind == TrackKind::Vca {
            continue;
        }
        let mut tn = TrackNodes::default();
        let layout = t.layout;
        let frozen = t.freeze.is_some();
        if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) && !frozen {
            let midi = b.add_node(
                NodeSpec::new(format!("{} · MIDI", t.name))
                    .key(node_key(t.id, Role::MidiPlayer, 0, &[]))
                    .group(gi)
                    .events_out(1),
                Box::new(MidiClipPlayer::new(t.id)),
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
                Box::new(MidiInputNode::new(
                    t.id,
                    filter,
                    live,
                    std::sync::Arc::clone(&routing.shared),
                )),
            );
            tn.midi_in = Some(own(&mut owners, node, t.id, None, NodeWork::MidiInput));
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
                b.connect_events(midi, 0, sink, 0)?;
                if let Some(live) = tn.midi_in {
                    b.connect_events(live, 0, sink, 0)?;
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

        match t.kind {
            // Frozen: the rendered audio, straight to the strip.
            TrackKind::Audio | TrackKind::Instrument if frozen => {
                let player = b.add_node(
                    NodeSpec::new(format!("{} · Frozen", t.name))
                        .key(node_key(t.id, Role::ClipPlayer, 0xF0_0000, &[layout]))
                        .group(gi)
                        .audio_out(layout),
                    Box::new(AudioClipPlayer::new(t.id)),
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
                    let inst = pcx.node(&mut b, slot, t, spec, Role::Instrument);
                    own(&mut owners, inst, t.id, Some(slot.id), NodeWork::Instrument);
                    if let Some(midi) = tn.midi {
                        b.connect_events(midi, 0, inst, 0)?;
                    }
                    if let Some(live) = tn.midi_in {
                        b.connect_events(live, 0, inst, 0)?;
                    }
                    b.connect_audio(inst, 0, input, 0)?;
                    tn.instrument = Some(inst);
                }
            }
            _ => {}
        }

        let mut prev = input;
        for slot in t.inserts.iter().filter(|_| !frozen) {
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
            let node = pcx.node(&mut b, slot, t, spec, Role::Insert);
            if let Some((src, _)) = key {
                sidechains.push((node, src));
            }
            own(&mut owners, node, t.id, Some(slot.id), NodeWork::Insert);
            b.connect_audio(prev, 0, node, 0)?;
            if notes {
                if let Some(midi) = tn.midi {
                    b.connect_events(midi, 0, node, 0)?;
                }
                if let Some(live) = tn.midi_in {
                    b.connect_events(live, 0, node, 0)?;
                }
                // MIDI tracks routed here reach it too.
                tn.instrument.get_or_insert(node);
            }
            prev = node;
        }

        let dest = destination_layout(project, t);
        let strip_slots = slots.strip(t.id)?;
        let meter = slots.meter(t.id)?;
        let strip = b.add_node(
            NodeSpec::new(format!("{} · Strip", t.name))
                .key(node_key(t.id, Role::Strip, 0, &[layout, dest]))
                .group(gi)
                .audio_in(layout)
                .audio_out(dest)
                .audio_out(layout),
            Box::new(ChannelStrip::new(
                t.id,
                strip_slots,
                meter,
                PanLaw::default(),
            )),
        );
        b.connect_audio(prev, 0, strip, 0)?;
        tn.post_fx = Some(prev);
        tn.strip = Some(own(&mut owners, strip, t.id, None, NodeWork::Strip));
        nodes.insert(t.id, tn);
    }

    // Pass 2: outputs and sends.
    for (gi, t) in project.tracks.iter().enumerate() {
        let gi = gi as u32;
        let Some(tn) = nodes.get(&t.id).copied() else {
            continue;
        };
        if t.kind == TrackKind::Midi {
            if let OutputRouting::Track { track } = t.output
                && let (Some(midi), Some(inst)) =
                    (tn.midi, nodes.get(&track).and_then(|n| n.instrument))
            {
                b.connect_events(midi, 0, inst, 0)?;
                if let Some(live) = tn.midi_in {
                    b.connect_events(live, 0, inst, 0)?;
                }
            }
            continue;
        }
        let Some(strip) = tn.strip else { continue };
        match t.output {
            OutputRouting::Master | OutputRouting::Track { .. } => {
                if let Some(dst) = project
                    .output_target(t)
                    .and_then(|d| nodes.get(&d))
                    .and_then(|n| n.input)
                {
                    b.connect_audio(strip, 0, dst, 0)?;
                }
            }
            OutputRouting::Hardware { first_channel } => {
                let dest = destination_layout(project, t);
                let out = b.add_node(
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
                own(&mut owners, out, t.id, None, NodeWork::HardwareOut);
                b.connect_audio(strip, 0, out, 0)?;
            }
            OutputRouting::None => {}
        }
        for send in t.sends.iter().filter(|s| s.enabled) {
            let Some(target) = project.track(send.target) else {
                continue;
            };
            let Some(dst) = nodes.get(&send.target).and_then(|n| n.input) else {
                continue;
            };
            let (tap_node, tap_port, tap_layout) = match send.tap {
                SendTap::PreFx => (tn.input.unwrap_or(strip), 0, t.layout),
                SendTap::PreFader => (strip, 1, t.layout),
                SendTap::PostFader => (strip, 0, destination_layout(project, t)),
            };
            let level = slots.send(send.id)?;
            let node = b.add_node(
                NodeSpec::new(format!("{} → {}", t.name, target.name))
                    .key(node_key(
                        t.id,
                        Role::Send,
                        send.id.raw(),
                        &[tap_layout, target.layout],
                    ))
                    .group(gi)
                    .audio_in(tap_layout)
                    .audio_out(target.layout),
                Box::new(SendNode::new(t.id, send.id, level)),
            );
            own(&mut owners, node, t.id, None, NodeWork::Send);
            b.connect_audio(tap_node, tap_port, node, 0)?;
            b.connect_audio(node, 0, dst, 0)?;
        }
    }
    // Sidechains tap their source after its inserts: before its fader,
    // mute and solo, so a muted "ghost" track can still key.
    for (node, src) in sidechains {
        if let Some(tap) = nodes.get(&src).and_then(|n| n.post_fx) {
            b.connect_audio(tap, 0, node, 1)?;
        }
    }
    Ok(BuiltGraph {
        builder: b,
        warnings,
        owners,
    })
}

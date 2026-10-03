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
use crate::midi::{MidiFilter, MidiInputNode, NO_PORT};
use crate::nodes::{
    AudioClipPlayer, ChannelStrip, DeviceInputTap, DeviceOutputSink, MidiClipPlayer, MonitorGate,
    PluginNode, SendNode,
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
        match self.plugins.activate(slot, &self.process) {
            Ok(p) => {
                // Latency and bypass change the node's behaviour, so they are
                // part of its identity: a change yields a fresh processor.
                let sub = slot.id.raw() ^ ((p.latency as u64) << 40) ^ ((slot.bypass as u64) << 63);
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
    midi_ports: &HashMap<String, u16>,
) -> Result<BuiltGraph, EngineError> {
    let mut b = GraphBuilder::<EngineContext>::new();
    let mut warnings = Vec::new();
    let mut pcx = PluginCx {
        plugins,
        process: ProcessConfig {
            sample_rate: config.sample_rate,
            max_block_size: config.max_block_size as u32,
        },
        warnings: &mut warnings,
    };
    let mut nodes: HashMap<TrackId, TrackNodes> = HashMap::new();
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
        let mut tn = TrackNodes::default();
        let layout = t.layout;
        if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            let midi = b.add_node(
                NodeSpec::new(format!("{} · MIDI", t.name))
                    .key(node_key(t.id, Role::MidiPlayer, 0, &[]))
                    .group(gi)
                    .events_out(1),
                Box::new(MidiClipPlayer::new(t.id)),
            );
            tn.midi = Some(own(&mut owners, midi, t.id, None, NodeWork::Midi));
            // Live input (played through while the session says so).
            if let InputRouting::Midi { port, channel } = &t.input {
                let filter = MidiFilter {
                    port: port
                        .as_ref()
                        .map(|k| midi_ports.get(k).copied().unwrap_or(NO_PORT)),
                    channel: *channel,
                };
                let sub = (filter.port.map_or(0x1_0000, u64::from) << 8)
                    | filter.channel.map_or(0xFF, u64::from);
                let live = slots.midi_live(t.id)?;
                let node = b.add_node(
                    NodeSpec::new(format!("{} · MIDI In", t.name))
                        .key(node_key(t.id, Role::MidiInput, sub, &[]))
                        .group(gi)
                        .events_out(1),
                    Box::new(MidiInputNode::new(filter, live)),
                );
                tn.midi_in = Some(own(&mut owners, node, t.id, None, NodeWork::MidiInput));
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
            TrackKind::Audio => {
                let player = b.add_node(
                    NodeSpec::new(format!("{} · Clips", t.name))
                        .key(node_key(t.id, Role::ClipPlayer, 0, &[layout]))
                        .group(gi)
                        .audio_out(layout),
                    Box::new(AudioClipPlayer::new(t.id)),
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
                } else {
                    pcx.warnings
                        .push(format!("instrument track '{}' has no instrument", t.name));
                }
            }
            _ => {}
        }

        let mut prev = input;
        for slot in &t.inserts {
            let spec = NodeSpec::new(format!("{} · {}", t.name, slot.plugin.name))
                .group(gi)
                .audio_in(layout)
                .audio_out(layout);
            let node = pcx.node(&mut b, slot, t, spec, Role::Insert);
            own(&mut owners, node, t.id, Some(slot.id), NodeWork::Insert);
            b.connect_audio(prev, 0, node, 0)?;
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
    Ok(BuiltGraph {
        builder: b,
        warnings,
        owners,
    })
}

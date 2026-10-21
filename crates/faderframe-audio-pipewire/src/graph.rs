//! The PipeWire graph as seen through the registry, the devices it offers,
//! and which links make FaderFrame's node play on and record from the
//! devices asked for. Pure: tested without a server.

use faderframe_audio::{DeviceInfo, StreamRouting};
use std::collections::HashMap;

/// Media classes of nodes FaderFrame plays on.
const PLAYBACK: [&str; 2] = ["Audio/Sink", "Audio/Duplex"];
/// Media classes of nodes FaderFrame records from.
const CAPTURE: [&str; 3] = ["Audio/Source", "Audio/Source/Virtual", "Audio/Duplex"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NodeInfo {
    pub class: String,
    pub name: String,
    pub description: String,
    pub priority: i64,
    /// `clock.name`, else the device object it belongs to.
    pub clock: Option<String>,
}

impl NodeInfo {
    pub fn plays(&self) -> bool {
        PLAYBACK.contains(&self.class.as_str())
    }

    pub fn records(&self) -> bool {
        CAPTURE.contains(&self.class.as_str())
    }

    fn label(&self) -> &str {
        if self.description.is_empty() {
            &self.name
        } else {
            &self.description
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PortInfo {
    pub node: u32,
    /// `port.direction == "in"`.
    pub input: bool,
    /// Position on its node.
    pub index: u32,
    /// `audio.channel` (FL, AUX0), else `port.name`.
    pub channel: String,
    /// A sink's monitor port (an output that copies what it plays).
    pub monitor: bool,
}

/// The server's defaults (the `default` metadata): node names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Defaults {
    pub sink: Option<String>,
    pub source: Option<String>,
}

/// The graph's clock settings (the `settings` metadata).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settings {
    pub rate: u32,
    pub allowed_rates: Vec<u32>,
    pub quantum: u32,
    pub min_quantum: u32,
    pub max_quantum: u32,
}

impl Settings {
    /// Apply one `settings` metadata property.
    pub fn set(&mut self, key: &str, value: Option<&str>) {
        let number = || {
            value
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(0)
        };
        match key {
            "clock.rate" => self.rate = number(),
            "clock.quantum" => self.quantum = number(),
            "clock.min-quantum" => self.min_quantum = number(),
            "clock.max-quantum" => self.max_quantum = number(),
            "clock.allowed-rates" => {
                self.allowed_rates = value
                    .unwrap_or_default()
                    .split(|c: char| !c.is_ascii_digit())
                    .filter_map(|n| n.parse().ok())
                    .collect();
            }
            _ => {}
        }
    }
}

/// The node name in a `default` metadata value (`{ "name": "…" }`).
pub(crate) fn json_name(value: &str) -> Option<String> {
    let at = value.find("\"name\"")? + "\"name\"".len();
    let rest = value[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

#[derive(Debug, Default)]
pub(crate) struct Graph {
    pub nodes: HashMap<u32, NodeInfo>,
    pub ports: HashMap<u32, PortInfo>,
    /// Links by global id: (output port, input port).
    pub links: HashMap<u32, (u32, u32)>,
}

impl Graph {
    pub fn remove(&mut self, id: u32) -> bool {
        let link = self.links.remove(&id).is_some();
        self.nodes.remove(&id).is_some() | self.ports.remove(&id).is_some() | link
    }

    /// The global id of the link between two ports, if there is one.
    pub fn link_between(&self, out_port: u32, in_port: u32) -> Option<u32> {
        self.links
            .iter()
            .find(|(_, pair)| **pair == (out_port, in_port))
            .map(|(id, _)| *id)
    }

    /// A node by its `node.name`.
    pub fn by_name(&self, name: &str) -> Option<u32> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.name == name)
            .min_by_key(|(id, _)| **id)
            .map(|(id, _)| *id)
    }

    /// The highest-priority node that plays (records).
    pub fn best(&self, playback: bool) -> Option<u32> {
        self.nodes
            .iter()
            .filter(|(_, n)| if playback { n.plays() } else { n.records() })
            .max_by_key(|(id, n)| (n.priority, std::cmp::Reverse(**id)))
            .map(|(id, _)| *id)
    }

    /// The default node of a direction: the server's default if it is
    /// there, else the highest-priority one.
    pub fn default_node(&self, playback: bool, defaults: &Defaults) -> Option<u32> {
        let named = if playback {
            &defaults.sink
        } else {
            &defaults.source
        };
        named
            .as_deref()
            .and_then(|n| self.by_name(n))
            .filter(|id| {
                let n = &self.nodes[id];
                if playback { n.plays() } else { n.records() }
            })
            .or_else(|| self.best(playback))
    }

    /// A node's ports of one direction, in port order (no monitor ports).
    pub fn ports_of(&self, node: u32, input: bool) -> Vec<u32> {
        let mut ports: Vec<(u32, u32)> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == node && p.input == input && !p.monitor)
            .map(|(id, p)| (p.index, *id))
            .collect();
        ports.sort_unstable();
        ports.into_iter().map(|(_, id)| id).collect()
    }

    /// Every device node FaderFrame can play on or record from.
    pub fn devices(&self, defaults: &Defaults, settings: &Settings) -> Vec<DeviceInfo> {
        let default_sink = self.default_node(true, defaults);
        let default_source = self.default_node(false, defaults);
        let mut out: Vec<(i64, DeviceInfo)> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.plays() || n.records())
            .map(|(id, n)| {
                let names = |input: bool| -> Vec<String> {
                    self.ports_of(*id, input)
                        .iter()
                        .map(|p| self.ports[p].channel.clone())
                        .collect()
                };
                let output_names = if n.plays() { names(true) } else { Vec::new() };
                let input_names = if n.records() {
                    names(false)
                } else {
                    Vec::new()
                };
                let info = DeviceInfo {
                    id: n.name.clone(),
                    name: n.label().to_string(),
                    input_channels: input_names.len() as u16,
                    output_channels: output_names.len() as u16,
                    input_names,
                    output_names,
                    sample_rates: settings.allowed_rates.clone(),
                    current_sample_rate: (settings.rate > 0).then_some(settings.rate),
                    current_buffer_size: (settings.quantum > 0).then_some(settings.quantum),
                    buffer_sizes: (settings.min_quantum > 0 && settings.max_quantum > 0)
                        .then_some((settings.min_quantum, settings.max_quantum)),
                    clock: n.clock.clone(),
                    is_default: Some(*id) == default_sink || Some(*id) == default_source,
                };
                (n.priority, info)
            })
            .collect();
        out.sort_by(|(pa, a), (pb, b)| pb.cmp(pa).then_with(|| a.name.cmp(&b.name)));
        out.into_iter().map(|(_, d)| d).collect()
    }
}

/// What the stream asks for: devices by node name (empty: the default).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
}

/// The links that connect the stream's node to its devices, and how its
/// channels are connected through them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// (output port, input port) pairs.
    pub links: Vec<(u32, u32)>,
    pub routing: StreamRouting,
}

/// Plan the links from node `ours` to the wanted devices. The wanted
/// devices' channels follow one another on the node's ports; a device that
/// is not there keeps its place (`known`: channel counts seen before, by
/// node name), and when none of a direction's devices is there, the
/// default device stands in until one returns.
pub(crate) fn plan(
    graph: &Graph,
    ours: u32,
    wanted: &Wanted,
    defaults: &Defaults,
    known: &HashMap<String, usize>,
) -> Plan {
    let mut out = Plan::default();
    let our_out = graph.ports_of(ours, false);
    let our_in = graph.ports_of(ours, true);
    out.routing.outputs = vec![None; our_out.len()];
    out.routing.inputs = vec![None; our_in.len()];
    for playback in [true, false] {
        let names = if playback {
            &wanted.outputs
        } else {
            &wanted.inputs
        };
        // The device nodes in channel order, `None` holding a missing
        // device's place.
        let mut spans: Vec<(Option<u32>, usize)> = Vec::new();
        for name in names {
            match graph.by_name(name).filter(|id| {
                let n = &graph.nodes[id];
                if playback { n.plays() } else { n.records() }
            }) {
                Some(id) => spans.push((Some(id), graph.ports_of(id, playback).len())),
                None => {
                    out.routing.missing.push(name.clone());
                    spans.push((None, known.get(name).copied().unwrap_or(0)));
                }
            }
        }
        if spans.iter().all(|(id, _)| id.is_none()) {
            // Nothing asked for, or nothing of it there: the default.
            spans = graph
                .default_node(playback, defaults)
                .map(|id| (Some(id), graph.ports_of(id, playback).len()))
                .into_iter()
                .collect();
        }
        let ours = if playback { &our_out } else { &our_in };
        let mut channel = 0usize;
        for (node, width) in spans {
            let Some(node) = node else {
                channel += width;
                continue;
            };
            let info = &graph.nodes[&node];
            let label = info.label().to_string();
            if !out.routing.devices.contains(&label) {
                out.routing.devices.push(label.clone());
            }
            for theirs in graph.ports_of(node, playback) {
                let Some(&mine) = ours.get(channel) else {
                    break;
                };
                let name = Some(format!("{label} · {}", graph.ports[&theirs].channel));
                if playback {
                    out.links.push((mine, theirs));
                    out.routing.outputs[channel] = name;
                } else {
                    out.links.push((theirs, mine));
                    out.routing.inputs[channel] = name;
                }
                channel += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph: our node 1 (2 in, 4 out), interface "a" (sink 10 with 2
    /// playback ports and a monitor, source 11 with 2 capture ports),
    /// interface "b" (sink 20, 2 ports), a laptop sink 30.
    fn graph() -> Graph {
        let mut g = Graph::default();
        let node = |class: &str, name: &str, priority: i64| NodeInfo {
            class: class.into(),
            name: name.into(),
            description: format!("{name} desc"),
            priority,
            clock: Some(format!("clock {name}")),
        };
        g.nodes.insert(1, NodeInfo::default());
        g.nodes.insert(10, node("Audio/Sink", "a.out", 1000));
        g.nodes.insert(11, node("Audio/Source", "a.in", 2000));
        g.nodes.insert(20, node("Audio/Sink", "b.out", 500));
        g.nodes.insert(30, node("Audio/Sink", "laptop", 100));
        let mut id = 100;
        let mut port = |g: &mut Graph, node: u32, input: bool, index: u32, monitor: bool| {
            id += 1;
            g.ports.insert(
                id,
                PortInfo {
                    node,
                    input,
                    index,
                    channel: format!("ch{index}"),
                    monitor,
                },
            );
            id
        };
        for i in 0..2 {
            port(&mut g, 1, true, i, false);
        }
        for i in 0..4 {
            port(&mut g, 1, false, 2 + i, false);
        }
        for (node, n) in [(10, 2), (20, 2), (30, 2)] {
            for i in 0..n {
                port(&mut g, node, true, i, false);
                port(&mut g, node, false, 10 + i, true);
            }
        }
        for i in 0..2 {
            port(&mut g, 11, false, i, false);
        }
        g
    }

    #[test]
    fn devices_list_sinks_and_sources_with_their_channels() {
        let g = graph();
        let defaults = Defaults {
            sink: Some("b.out".into()),
            source: None,
        };
        let settings = Settings {
            rate: 48_000,
            allowed_rates: vec![44_100, 48_000],
            quantum: 256,
            min_quantum: 32,
            max_quantum: 2048,
        };
        let devices = g.devices(&defaults, &settings);
        assert_eq!(devices.len(), 4);
        assert_eq!(devices[0].id, "a.in", "by priority");
        let a_out = devices.iter().find(|d| d.id == "a.out").unwrap();
        assert_eq!(a_out.output_channels, 2, "monitor ports left out");
        assert_eq!(a_out.input_channels, 0);
        assert_eq!(a_out.output_names, vec!["ch0", "ch1"]);
        assert_eq!(a_out.buffer_sizes, Some((32, 2048)));
        assert!(devices.iter().find(|d| d.id == "b.out").unwrap().is_default);
        assert!(!a_out.is_default, "the server's default wins over priority");
        assert!(devices.iter().find(|d| d.id == "a.in").unwrap().is_default);
    }

    #[test]
    fn the_default_device_is_used_when_nothing_is_asked_for() {
        let g = graph();
        let p = plan(
            &g,
            1,
            &Wanted::default(),
            &Defaults::default(),
            &HashMap::new(),
        );
        // Highest-priority sink a.out on outputs 1–2, its source on inputs.
        assert_eq!(p.links.len(), 4);
        assert_eq!(p.routing.output(0), Some("a.out desc · ch0"));
        assert_eq!(p.routing.output(2), None);
        assert_eq!(p.routing.input(1), Some("a.in desc · ch1"));
        assert!(p.routing.missing.is_empty());
        // The server's default moves it.
        let p = plan(
            &g,
            1,
            &Wanted::default(),
            &Defaults {
                sink: Some("laptop".into()),
                source: None,
            },
            &HashMap::new(),
        );
        assert_eq!(p.routing.output(0), Some("laptop desc · ch0"));
    }

    #[test]
    fn two_interfaces_play_side_by_side_and_a_missing_one_keeps_its_place() {
        let mut g = graph();
        let wanted = Wanted {
            outputs: vec!["a.out".into(), "b.out".into()],
            inputs: vec![],
        };
        let p = plan(&g, 1, &wanted, &Defaults::default(), &HashMap::new());
        assert_eq!(p.routing.output(0), Some("a.out desc · ch0"));
        assert_eq!(p.routing.output(2), Some("b.out desc · ch0"));
        assert_eq!(p.routing.output(3), Some("b.out desc · ch1"));
        assert_eq!(
            p.routing.devices,
            vec!["a.out desc", "b.out desc", "a.in desc"]
        );
        // Interface a is unplugged: b stays on outputs 3–4.
        g.nodes.remove(&10);
        let known = HashMap::from([("a.out".to_string(), 2usize)]);
        let p = plan(&g, 1, &wanted, &Defaults::default(), &known);
        assert_eq!(p.routing.output(0), None);
        assert_eq!(p.routing.output(2), Some("b.out desc · ch0"));
        assert_eq!(p.routing.missing, vec!["a.out".to_string()]);
    }

    #[test]
    fn with_every_asked_device_gone_the_default_stands_in() {
        let g = graph();
        let wanted = Wanted {
            outputs: vec!["gone".into()],
            inputs: vec!["also gone".into()],
        };
        let p = plan(
            &g,
            1,
            &wanted,
            &Defaults {
                sink: Some("laptop".into()),
                source: None,
            },
            &HashMap::new(),
        );
        assert_eq!(p.routing.output(0), Some("laptop desc · ch0"));
        assert_eq!(p.routing.input(0), Some("a.in desc · ch0"));
        assert_eq!(p.routing.missing.len(), 2);
    }

    #[test]
    fn a_sink_is_not_taken_for_an_input() {
        let g = graph();
        let wanted = Wanted {
            outputs: vec![],
            inputs: vec!["b.out".into()],
        };
        let p = plan(&g, 1, &wanted, &Defaults::default(), &HashMap::new());
        assert_eq!(p.routing.missing, vec!["b.out".to_string()]);
        assert_eq!(p.routing.input(0), Some("a.in desc · ch0"), "the default");
    }

    #[test]
    fn settings_and_defaults_parse() {
        let mut s = Settings::default();
        s.set("clock.allowed-rates", Some("[ 44100, 48000 ]"));
        s.set("clock.min-quantum", Some("64"));
        assert_eq!(s.allowed_rates, vec![44_100, 48_000]);
        assert_eq!(s.min_quantum, 64);
        assert_eq!(
            json_name(r#"{ "name": "alsa_output.usb-x" }"#).as_deref(),
            Some("alsa_output.usb-x")
        );
        assert_eq!(json_name(r#"{"name":"a\"b"}"#).as_deref(), Some("a\"b"));
        assert_eq!(json_name("{}"), None);
    }
}

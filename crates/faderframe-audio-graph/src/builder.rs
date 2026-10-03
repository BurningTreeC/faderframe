use crate::compiled::{self, CompiledGraph};
use crate::{GraphError, PrepareConfig, Processor};
use faderframe_core::ChannelLayout;

/// Index of a node inside one [`GraphBuilder`] / [`CompiledGraph`].
///
/// Not stable across rebuilds; use a [`NodeKey`] to identify "the same"
/// node in successive graphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Stable identity of a node across graph rebuilds.
///
/// When a new graph replaces an old one, processors whose keys match are
/// carried over (see [`CompiledGraph::adopt_state_from`]) so that filter
/// states, envelopes, smoothing and plugin instances survive routing edits.
/// A key must encode everything that makes two processors interchangeable
/// (role, channel layout, plugin instance ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey(pub u64);

/// Special meaning of a node for the code driving the graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeRole {
    #[default]
    Generic,
    /// Its audio outputs are filled by the driver with device input
    /// channels starting at `first_channel` before the graph runs.
    DeviceInput { first_channel: u16 },
    /// The driver copies its first audio input to device output channels
    /// starting at `first_channel` after the graph runs.
    DeviceOutput { first_channel: u16 },
}

/// Port configuration and metadata of a node.
#[derive(Clone, Debug, Default)]
pub struct NodeSpec {
    pub label: String,
    pub key: Option<NodeKey>,
    pub role: NodeRole,
    pub audio_inputs: Vec<ChannelLayout>,
    pub audio_outputs: Vec<ChannelLayout>,
    pub event_inputs: u16,
    pub event_outputs: u16,
    /// Accounting group (e.g. the track the node belongs to): processing
    /// time is also summed per group and callback ([`crate::NodeTimings`]).
    pub group: Option<u32>,
}

impl NodeSpec {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn key(mut self, key: NodeKey) -> Self {
        self.key = Some(key);
        self
    }

    pub fn group(mut self, group: u32) -> Self {
        self.group = Some(group);
        self
    }

    pub fn role(mut self, role: NodeRole) -> Self {
        self.role = role;
        self
    }

    pub fn audio_in(mut self, layout: ChannelLayout) -> Self {
        self.audio_inputs.push(layout);
        self
    }

    pub fn audio_out(mut self, layout: ChannelLayout) -> Self {
        self.audio_outputs.push(layout);
        self
    }

    pub fn events_in(mut self, count: u16) -> Self {
        self.event_inputs = count;
        self
    }

    pub fn events_out(mut self, count: u16) -> Self {
        self.event_outputs = count;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeKind {
    Audio,
    Events,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    pub kind: EdgeKind,
    pub from: NodeId,
    pub from_port: u16,
    pub to: NodeId,
    pub to_port: u16,
}

pub(crate) struct NodeDesc<C> {
    pub spec: NodeSpec,
    pub processor: Box<dyn Processor<C>>,
}

/// Mutable graph description (control thread).
pub struct GraphBuilder<C> {
    pub(crate) nodes: Vec<NodeDesc<C>>,
    pub(crate) edges: Vec<Edge>,
}

impl<C> Default for GraphBuilder<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> GraphBuilder<C> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn add_node(&mut self, spec: NodeSpec, processor: Box<dyn Processor<C>>) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(NodeDesc { spec, processor });
        id
    }

    pub fn spec(&self, node: NodeId) -> Option<&NodeSpec> {
        self.nodes.get(node.0 as usize).map(|n| &n.spec)
    }

    fn check_port(
        &self,
        node: NodeId,
        port: u16,
        output: bool,
        kind: EdgeKind,
    ) -> Result<(), GraphError> {
        let spec = &self
            .nodes
            .get(node.0 as usize)
            .ok_or(GraphError::UnknownNode(node))?
            .spec;
        let count = match (kind, output) {
            (EdgeKind::Audio, true) => spec.audio_outputs.len(),
            (EdgeKind::Audio, false) => spec.audio_inputs.len(),
            (EdgeKind::Events, true) => spec.event_outputs as usize,
            (EdgeKind::Events, false) => spec.event_inputs as usize,
        };
        if (port as usize) < count {
            Ok(())
        } else {
            Err(GraphError::InvalidPort {
                node: spec.label.clone(),
                direction: if output { "output" } else { "input" },
                kind: match kind {
                    EdgeKind::Audio => "audio",
                    EdgeKind::Events => "event",
                },
                port,
            })
        }
    }

    fn connect(&mut self, edge: Edge) -> Result<(), GraphError> {
        self.check_port(edge.from, edge.from_port, true, edge.kind)?;
        self.check_port(edge.to, edge.to_port, false, edge.kind)?;
        if self.edges.contains(&edge) {
            return Err(GraphError::DuplicateEdge {
                from: self.nodes[edge.from.0 as usize].spec.label.clone(),
                to: self.nodes[edge.to.0 as usize].spec.label.clone(),
            });
        }
        self.edges.push(edge);
        Ok(())
    }

    /// Connect an audio output port to an audio input port. Multiple edges
    /// into one input are summed (with latency alignment).
    pub fn connect_audio(
        &mut self,
        from: NodeId,
        from_port: u16,
        to: NodeId,
        to_port: u16,
    ) -> Result<(), GraphError> {
        self.connect(Edge {
            kind: EdgeKind::Audio,
            from,
            from_port,
            to,
            to_port,
        })
    }

    /// Connect an event output port to an event input port. Multiple edges
    /// into one input are merged in time order.
    pub fn connect_events(
        &mut self,
        from: NodeId,
        from_port: u16,
        to: NodeId,
        to_port: u16,
    ) -> Result<(), GraphError> {
        self.connect(Edge {
            kind: EdgeKind::Events,
            from,
            from_port,
            to,
            to_port,
        })
    }

    /// Validate, order, latency-compensate and allocate (control thread).
    ///
    /// Calls `prepare` on every processor. On a cycle the error lists the
    /// labels of the nodes involved.
    pub fn compile(self, config: &PrepareConfig) -> Result<CompiledGraph<C>, GraphError> {
        compiled::compile(self, config)
    }
}

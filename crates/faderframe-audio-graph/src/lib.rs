//! Directed audio/event processing graph.
//!
//! The engine never processes "a list of tracks". It processes a graph of
//! nodes (sources, inserts, channel strips, sends, buses, hardware I/O)
//! connected by audio and event edges.
//!
//! Lifecycle:
//!
//! 1. **Build** (control thread): [`GraphBuilder`] collects nodes and edges
//!    and validates ports as they are connected.
//! 2. **Compile** (control thread): [`GraphBuilder::compile`] detects
//!    cycles, computes a topological order, propagates latency and inserts
//!    delay-compensation lines on every edge into a summing point, allocates
//!    all buffers, and records dependency information for future parallel
//!    scheduling. Everything that allocates happens here.
//! 3. **Process** (audio thread): [`CompiledGraph::process`] runs the nodes
//!    in order; [`CompiledGraph::process_parallel`] runs independent jobs
//!    (fused node chains) concurrently on a `faderframe_realtime::WorkerPool`,
//!    most expensive path first. Neither allocates, locks or does I/O, and
//!    both produce bit-identical output.
//!
//! The crate is generic over an engine-defined context type `C` handed to
//! every processor, so it knows nothing about projects, transports or
//! backends.

#![forbid(unsafe_code)]

mod buffer;
mod builder;
mod compiled;
mod delay;
mod error;
pub mod nodes;
mod processor;
mod topology;

pub use buffer::{AudioBuffer, for_each_channel_route};
pub use builder::{GraphBuilder, NodeId, NodeKey, NodeRole, NodeSpec};
pub use compiled::{CompiledGraph, GraphStats, NodeTimings};
pub use error::GraphError;
pub use processor::{NodeIo, PrepareConfig, ProcessContext, Processor};
pub use topology::{TopologyError, topological_order};

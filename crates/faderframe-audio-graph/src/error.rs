use crate::NodeId;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("unknown graph node {0:?}")]
    UnknownNode(NodeId),
    #[error("node '{node}' has no {direction} {kind} port {port}")]
    InvalidPort {
        node: String,
        direction: &'static str,
        kind: &'static str,
        port: u16,
    },
    #[error("duplicate connection from '{from}' to '{to}'")]
    DuplicateEdge { from: String, to: String },
    #[error("routing would create a feedback loop: {}", .0.join(" → "))]
    Cycle(Vec<String>),
    #[error("graph exceeds limits: {0}")]
    Limit(&'static str),
}

/// Metrics describing the connectivity and structure around a single node.
#[derive(Debug, Clone)]
pub struct NodeMetrics {
    /// Number of incoming edges (all kinds).
    pub incoming_edge_count: usize,
    /// Number of outgoing edges (all kinds).
    pub outgoing_edge_count: usize,
    /// Number of outgoing `Calls` edges (functions this node calls).
    pub call_count: usize,
    /// Number of incoming `Calls` edges (functions that call this node).
    pub caller_count: usize,
    /// Number of direct containment children.
    pub child_count: usize,
    /// Depth of the node in the containment hierarchy.
    pub depth: usize,
}

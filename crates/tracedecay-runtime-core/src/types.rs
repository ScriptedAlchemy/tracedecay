//! Compatibility façade for contracts now owned by focused workspace crates.

// The `source_edit` result types stay owned by `tracedecay-application`; the
// kernel no longer re-exports them, because doing so was the last edge from
// this crate back up into the contract crate. Consumers reach them through the
// root `crate::types` shim, which unions both halves.
pub use tracedecay_domain::code_intelligence::{
    BuildContextOptions, CodeBlock, Edge, EdgeKind, ExtractionResult, FileRecord, GraphStats,
    IndexCoverageHint, Node, NodeKind, OutputFormat, ResolutionResult, ResolvedRef, SearchResult,
    Subgraph, TaskContext, TraversalDirection, TraversalOptions, UnresolvedRef, Visibility,
    generate_node_id,
};
pub use tracedecay_domain::observability::CostTurn;

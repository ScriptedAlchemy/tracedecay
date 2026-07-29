//! Compatibility façade for contracts now owned by focused workspace crates.

pub use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveHint, MoveResult, MultiEditResult,
};
pub use tracedecay_domain::code_intelligence::{
    BuildContextOptions, CodeBlock, Edge, EdgeKind, ExtractionResult, FileRecord, GraphStats,
    IndexCoverageHint, Node, NodeKind, OutputFormat, ResolutionResult, ResolvedRef, SearchResult,
    Subgraph, TaskContext, TraversalDirection, TraversalOptions, UnresolvedRef, Visibility,
    generate_node_id,
};
pub use tracedecay_domain::observability::CostTurn;

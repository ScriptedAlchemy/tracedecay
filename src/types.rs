//! Stable root type surface for code-intelligence and source-edit contracts.
//!
//! Code-intelligence contracts are owned by `tracedecay-domain`. These explicit
//! re-exports keep the historical `crate::types::…` paths resolving without an
//! internal compatibility façade in the runtime kernel.

pub use tracedecay_domain::code_intelligence::{
    BuildContextOptions, CodeBlock, Edge, EdgeKind, ExtractionResult, FileRecord, GraphStats,
    IndexCoverageHint, Node, NodeKind, OutputFormat, ResolutionResult, ResolvedRef, SearchResult,
    Subgraph, TaskContext, TraversalDirection, TraversalOptions, UnresolvedRef, Visibility,
    generate_node_id,
};

/// The source-edit result types are owned by `tracedecay-application`. The
/// kernel deliberately does not re-export them (that edge would point back up
/// out of the kernel), so the root shim unions both halves to keep every
/// historical `crate::types::{EditResult, …}` path resolving.
pub use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveHint, MoveResult, MultiEditResult,
    RenameFileEditV1, RenameResult, RenameSymbolBindingV1,
};

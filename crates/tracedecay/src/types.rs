//! Stable root type surface for code-intelligence and source-edit contracts.
//!
//! Graph node, edge, and extraction contracts are owned by `tracedecay-domain`
//! because many crates share them. The traversal, search, and
//! context-assembly shapes below are consumed only by this crate, so they are
//! defined here: an edit to a root-only shape must not invalidate every crate
//! that depends on the domain.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub use tracedecay_domain::code_intelligence::{
    Edge, EdgeKind, ExtractionResult, GraphStats, Node, NodeKind, UnresolvedRef, Visibility,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub content_hash: String,
    pub size: u64,
    pub modified_at: i64,
    pub indexed_at: i64,
    pub node_count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Subgraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub node: Node,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCoverageHint {
    pub message: String,
    pub skipped_dirs: Vec<String>,
    pub suggested_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraversalDirection {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalOptions {
    pub max_depth: u32,
    pub edge_kinds: Option<Vec<EdgeKind>>,
    pub node_kinds: Option<Vec<NodeKind>>,
    pub direction: TraversalDirection,
    pub limit: u32,
    pub include_start: bool,
}

impl Default for TraversalOptions {
    fn default() -> Self {
        TraversalOptions {
            max_depth: 3,
            edge_kinds: None,
            node_kinds: None,
            direction: TraversalDirection::Outgoing,
            limit: 100,
            include_start: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildContextOptions {
    pub max_nodes: usize,
    pub max_code_blocks: usize,
    pub max_code_block_size: usize,
    pub include_code: bool,
    pub format: OutputFormat,
    pub search_limit: usize,
    pub traversal_depth: usize,
    pub min_score: f64,
    /// Additional keywords to search for beyond those extracted from the query.
    /// Enables agent-driven synonym expansion (e.g. `"authentication"` → `["login", "session"]`).
    pub extra_keywords: Vec<String>,
    /// Node IDs to exclude from results (for session deduplication across calls).
    pub exclude_node_ids: HashSet<String>,
    /// When true, merge code blocks from the same file whose line ranges are
    /// adjacent or overlapping into a single block.
    pub merge_adjacent: bool,
    /// Maximum symbols from a single file in context results. Prevents one
    /// large file from dominating the output. `None` means no cap (defaults
    /// to `max_nodes`).
    pub max_per_file: Option<usize>,
    /// When set, only nodes whose `file_path` starts with this prefix are
    /// considered as entry points. Graph expansion may still traverse outside
    /// the prefix (traversals are unscoped).
    pub path_prefix: Option<String>,
}

impl Default for BuildContextOptions {
    fn default() -> Self {
        BuildContextOptions {
            max_nodes: 20,
            max_code_blocks: 5,
            max_code_block_size: 1500,
            include_code: true,
            format: OutputFormat::Markdown,
            search_limit: 3,
            traversal_depth: 1,
            min_score: 0.0,
            extra_keywords: Vec::new(),
            exclude_node_ids: HashSet::new(),
            merge_adjacent: false,
            max_per_file: None,
            path_prefix: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputFormat {
    Markdown,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    pub query: String,
    pub summary: String,
    pub subgraph: Subgraph,
    pub entry_points: Vec<Node>,
    pub code_blocks: Vec<CodeBlock>,
    pub related_files: Vec<String>,
    /// IDs of all returned nodes (pass to next call's `exclude_node_ids` for dedup).
    pub seen_node_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBlock {
    pub content: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionResult {
    pub resolved: Vec<ResolvedRef>,
    pub unresolved: Vec<UnresolvedRef>,
    pub total: usize,
    pub resolved_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRef {
    pub original: UnresolvedRef,
    pub target_node_id: String,
    pub confidence: f64,
    pub resolved_by: String,
}

//! Portable graph-navigation handlers over one source-bound verified query.

mod context_markdown;
mod context_support;
mod dispatch;
mod lexical_routing;
mod navigation;
mod primitive_surface;
mod search;
mod search_evidence;
mod search_freshness;
mod verified;

pub use dispatch::dispatch_tool;
pub use navigation::{
    compute_impact, compute_node, handle_by_qualified_name, handle_derives, handle_signature,
};
pub use search::{
    compute_redundancy, compute_rename_preview, compute_similar, handle_context,
    handle_find_exact_symbol, handle_search,
};
pub use verified::{
    GRAPH_RELATION_READ_LIMIT, VerifiedNeighbor, cost_to_expand_verified, graph_occurrence_id,
    graph_symbol_corrupt, graph_symbol_end_line,
    graph_symbol_location_value, graph_symbol_paths, graph_symbols_in_scope, line_for_byte_offset,
    nodes_addressed_by_args, required_graph_file_path, required_graph_metadata,
    single_graph_adjacency_batch, traverse_verified_neighbors,
};

use tracedecay_contracts::retrieval::PrimitiveNotFoundV1;
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::{ToolResult, text_tool_result};

pub(crate) fn user_line(line: u32) -> u32 {
    line.saturating_add(1)
}

pub(super) fn require_positive_depth(max_depth: u32) -> Result<()> {
    if max_depth == 0 {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: max_depth must be at least 1".to_owned(),
        });
    }
    Ok(())
}

pub fn node_not_found(node_id: &str) -> Result<ToolResult> {
    not_found_tool_result(&node_not_found_result(node_id))
}

pub(crate) fn node_not_found_result(node_id: &str) -> PrimitiveNotFoundV1 {
    PrimitiveNotFoundV1 {
        status: "not_found".to_owned(),
        reason_code: "node_not_found".to_owned(),
        node_id: node_id.to_owned(),
        message: format!("Node not found: {node_id}"),
    }
}

pub fn not_found_tool_result(output: &PrimitiveNotFoundV1) -> Result<ToolResult> {
    Ok(
        text_tool_result(&serde_json::to_string_pretty(output)?, vec![])
            .with_semantic_error(true)
            .with_failure_message(format!("node not found: {}", output.node_id)),
    )
}

pub(crate) fn graph_tool_completion(
    result: tracedecay_contracts::graph_tool::GraphToolResultV1,
    touched_files: Vec<String>,
) -> tracedecay_contracts::graph_tool::GraphToolCompletionV1 {
    tracedecay_contracts::graph_tool::GraphToolCompletionV1 {
        result,
        touched_files,
    }
}

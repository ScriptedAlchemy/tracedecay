//! Portable graph-navigation handlers over one source-bound verified query.

mod navigation;
mod verified;

pub use navigation::{
    handle_by_qualified_name, handle_callees, handle_callers, handle_callers_for, handle_derives,
    handle_impact, handle_implementations, handle_impls, handle_node, handle_signature,
};
pub use verified::{
    GRAPH_RELATION_READ_LIMIT, VerifiedNeighbor, canonical_relation_kind,
    canonical_relation_kind_name, cost_to_expand_verified, graph_name_matches, graph_occurrence_id,
    graph_symbol_corrupt, graph_symbol_end_line, graph_symbol_location_value, graph_symbol_paths,
    graph_symbols_in_scope, line_for_byte_offset, nodes_addressed_by_args,
    required_graph_file_path, required_graph_metadata, single_graph_adjacency_batch,
    traverse_verified_neighbors, verified_neighbor_value, verified_trait_dispatch_targets,
};

use tracedecay_application::retrieval::PrimitiveNotFoundV1;
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::{ToolResult, text_tool_result};

pub(super) fn user_line(line: u32) -> u32 {
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
    let output = PrimitiveNotFoundV1 {
        status: "not_found".to_owned(),
        reason_code: "node_not_found".to_owned(),
        node_id: node_id.to_owned(),
        message: format!("Node not found: {node_id}"),
    };
    Ok(
        text_tool_result(&serde_json::to_string_pretty(&output)?, vec![])
            .with_semantic_error(true)
            .with_failure_message(format!("node not found: {node_id}")),
    )
}

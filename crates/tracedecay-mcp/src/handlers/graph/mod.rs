//! Portable graph-navigation handlers over one source-bound verified query.

mod clones;
mod context;
mod context_markdown;
mod context_support;
mod exact_symbol;
mod lexical_routing;
mod navigation;
mod primitive_surface;
mod rename_preview;
mod search;
mod search_evidence;
mod search_freshness;
mod verified;

pub use clones::{compute_redundancy, compute_similar};
pub use context::compute_context;
pub(crate) use context_markdown::render_context;
pub use exact_symbol::compute_find_exact_symbol;
pub use navigation::{
    compute_by_qualified_name, compute_derives, compute_impact, compute_node, compute_signature,
};
pub use rename_preview::compute_rename_preview;
pub use search::compute_search;
pub(crate) use search::render_search;
pub(crate) use search_freshness::freshness_lines;
pub use search_freshness::graph_read_freshness;
pub use verified::{
    GRAPH_RELATION_READ_LIMIT, VerifiedNeighbor, cost_to_expand_verified, graph_occurrence_id,
    graph_symbol_corrupt, graph_symbol_end_line, graph_symbol_paths, graph_symbols_in_scope,
    line_for_byte_offset, nodes_addressed_by_selector, required_graph_file_path,
    required_graph_metadata, single_graph_adjacency_batch, traverse_verified_neighbors,
};

use std::cell::RefCell;
use std::collections::BTreeMap;

use tracedecay_contracts::retrieval::PrimitiveNotFoundV1;
use tracedecay_domain::SymbolOccurrenceId;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::text::edit_distance_within;
use tracedecay_graph_query::VerifiedGraphQuery;

use primitive_surface::symbol_location;

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

const NODE_SUGGESTION_LIMIT: usize = 5;

/// Not-found answer carrying the served symbols nearest to the requested
/// id: a typo'd or truncated occurrence id, or a name passed as an id.
pub(crate) fn node_not_found_result(
    graph: &VerifiedGraphQuery,
    node_id: &str,
    occurrence: &SymbolOccurrenceId,
) -> Result<PrimitiveNotFoundV1> {
    let query = occurrence.as_str();
    let max_distance = (query.chars().count() / 3).clamp(1, 3);
    let nearest = RefCell::new(BTreeMap::<(usize, String), SymbolOccurrenceId>::new());
    graph.find_symbols(
        &|candidate, binding, metadata| {
            // Only symbols `symbol_location` can render; unbound edge targets
            // carry neither extraction metadata nor a logical file.
            let Some(metadata) = metadata else {
                return false;
            };
            if binding
                .and_then(|binding| binding.logical_path.as_ref())
                .is_none()
            {
                return false;
            }
            let distance = [
                candidate.as_str(),
                metadata.simple_name.as_str(),
                metadata.qualified_name.as_str(),
            ]
            .into_iter()
            .filter_map(|text| edit_distance_within(query, text, max_distance))
            .min();
            if let Some(distance) = distance {
                let mut nearest = nearest.borrow_mut();
                nearest.insert((distance, candidate.as_str().to_owned()), candidate.clone());
                if nearest.len() > NODE_SUGGESTION_LIMIT {
                    nearest.pop_last();
                }
            }
            false
        },
        1,
    )?;
    let mut suggestions = Vec::new();
    for occurrence in nearest.into_inner().into_values() {
        if let Some(symbol) = graph.symbol_summary(&occurrence)? {
            suggestions.push(symbol_location(&symbol)?);
        }
    }
    Ok(PrimitiveNotFoundV1 {
        status: "not_found".to_owned(),
        reason_code: "node_not_found".to_owned(),
        node_id: node_id.to_owned(),
        message: format!("Node not found: {node_id}"),
        suggestions,
        freshness: None,
    })
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
        code_graph: None,
        analytics: None,
        cost: None,
    }
}

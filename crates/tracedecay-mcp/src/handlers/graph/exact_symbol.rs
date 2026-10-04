//! `tracedecay_find_exact_symbol`: bare-name lookup against the served graph.

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    FindExactSymbolMatchV1, FindExactSymbolResultV1, FindExactSymbolSurfaceRequestV1,
};
use tracedecay_domain::errors::Result;

use crate::McpToolContext;
use crate::handlers::dependency_hints;
use crate::handlers::support::decode_primitive_request;

use super::{
    graph_symbol_paths, graph_symbols_in_scope, graph_tool_completion, required_graph_file_path,
    required_graph_metadata, user_line,
};

/// Bare-name lookup against `idx_nodes_name`, no BM25 scoring, no fuzzy
/// match, no qualified-name suffix walk. Returns every node whose `name`
/// column equals the query exactly. Useful when you already know the symbol
/// and want the apples-to-apples cost of an index hit instead of
/// `tracedecay_search`'s ranked query.
#[tracing::instrument(name = "mcp.graph.find_exact_symbol.total", level = "trace", skip_all)]
pub async fn compute_find_exact_symbol(
    ctx: &McpToolContext<'_>,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
    ignored_dependency_admission: Option<
        &dyn tracedecay_application::code_index::CodeIndexIgnoredDependencyAdmissionPortV1,
    >,
) -> Result<GraphToolCompletionV1> {
    let request: FindExactSymbolSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_find_exact_symbol")?;
    let name = request.name.as_str();
    let limit = request.limit.map_or(20, |v| v.min(200) as usize);

    let mut nodes = {
        let _span = tracing::trace_span!("mcp.graph.find_exact_symbol.graph").entered();
        {
            let nodes = graph.resolve_simple_name(name, None, limit.saturating_mul(4))?;
            graph_symbols_in_scope(nodes, scope_prefix)?
        }
    };
    if nodes.is_empty() && request.lazy_index_ignored_dependencies.unwrap_or(false) {
        tracing::Instrument::instrument(
            dependency_hints::admit_verified_ignored_dependency(
                ctx,
                ignored_dependency_admission,
                graph,
                name,
                scope_prefix,
            ),
            tracing::trace_span!("mcp.graph.find_exact_symbol.admit"),
        )
        .await?;
    }
    if nodes.len() > limit {
        nodes.truncate(limit);
    }

    let touched_files = graph_symbol_paths(&nodes)?;
    let matches = nodes
        .iter()
        .map(|node| {
            let metadata = required_graph_metadata(node)?;
            let file_path = required_graph_file_path(node)?;
            Ok(FindExactSymbolMatchV1 {
                id: node.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                qualified_name: metadata.qualified_name.clone(),
                kind: metadata.kind.clone(),
                file: file_path.to_owned(),
                line: user_line(metadata.start_line),
                signature: metadata.signature.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(graph_tool_completion(
        GraphToolResultV1::FindExactSymbol(FindExactSymbolResultV1 {
            name: request.name.clone(),
            count: matches.len() as u64,
            matches,
            freshness: None,
        }),
        touched_files,
    ))
}

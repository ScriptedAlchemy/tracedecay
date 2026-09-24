//! Dependency-clean graph-navigation handlers over [`VerifiedGraphQuery`].

use serde_json::{Value, json};
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    ImpactNodeV1, ImpactResultV1, NodeDepthSurfaceRequestV1, NodeDetailsV1, NodeExpansionCostV1,
    NodeResultV1, NodeSurfaceRequestV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

use crate::{ToolResult, decode_primitive_request, generic_tool_result, text_tool_result};

use super::{
    GRAPH_RELATION_READ_LIMIT, cost_to_expand_verified, graph_occurrence_id, graph_symbol_corrupt,
    graph_symbol_end_line, graph_symbol_location_value, graph_symbol_paths, graph_tool_completion,
    node_not_found_result, nodes_addressed_by_args, require_positive_depth, required_graph_file_path,
    required_graph_metadata, user_line,
};

#[hotpath::measure(label = "mcp.graph.impact.total")]
pub async fn compute_impact(
    graph: &VerifiedGraphQuery,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: NodeDepthSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_impact")?;
    let max_depth = request.max_depth.map_or(3, |value| value.min(10));
    require_positive_depth(max_depth)?;

    let occurrence = graph_occurrence_id(&request.node_id)?;
    let impact = hotpath::measure_block!(
        "mcp.graph.impact.graph",
        graph.impact(
            std::slice::from_ref(&occurrence),
            &[],
            max_depth,
            50_000,
            GRAPH_RELATION_READ_LIMIT,
        )?
    );
    let summaries = impact
        .impacted
        .iter()
        .map(|item| item.summary.clone())
        .collect::<Vec<_>>();
    let touched_files = graph_symbol_paths(&summaries)?;
    let nodes = impact
        .impacted
        .iter()
        .map(|item| {
            let metadata = required_graph_metadata(&item.summary)?;
            Ok(ImpactNodeV1 {
                id: item.summary.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                file: required_graph_file_path(&item.summary)?.to_owned(),
                line: user_line(metadata.start_line),
                depth: item.depth,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let result = ImpactResultV1 {
        node_count: nodes.len(),
        complete: impact.complete,
        unavailable_fields: vec!["edge_count".to_owned()],
        nodes,
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::Impact(result),
        touched_files,
    ))
}

#[hotpath::measure(label = "mcp.graph.node.total")]
pub async fn compute_node(graph: &VerifiedGraphQuery, args: Value) -> Result<GraphToolCompletionV1> {
    let request: NodeSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_node")?;
    let occurrence = graph_occurrence_id(&request.node_id)?;
    let node = hotpath::measure_block!("mcp.graph.node.graph", graph.symbol_summary(&occurrence)?);

    match node {
        Some(n) => {
            let metadata = required_graph_metadata(&n)?;
            let file_path = required_graph_file_path(&n)?;
            let touched_files = vec![file_path.to_owned()];
            let file_size_bytes = bound_source_file_len(graph, file_path)?;
            let end_line = graph_symbol_end_line(metadata)?;
            let complexity = metadata.exact_complexity();
            let cyclomatic_complexity = complexity
                .map(|complexity| {
                    complexity.cyclomatic().ok_or_else(|| {
                        graph_symbol_corrupt(format!(
                            "verified graph symbol '{}' branch count overflows complexity",
                            n.occurrence.as_str()
                        ))
                    })
                })
                .transpose()?;
            let mut unavailable_fields = vec![
                "assertions",
                "attrs_start_line",
                "returns",
                "unchecked_calls",
                "unsafe_blocks",
            ];
            if complexity.is_none() {
                unavailable_fields.extend([
                    "branches",
                    "cyclomatic_complexity",
                    "loops",
                    "max_nesting",
                ]);
                unavailable_fields.sort_unstable();
            }
            let line_count = end_line - metadata.start_line + 1;
            let details = NodeDetailsV1 {
                    id: n.occurrence.as_str().to_owned(),
                    name: metadata.simple_name.clone(),
                    kind: metadata.kind.clone(),
                    qualified_name: metadata.qualified_name.clone(),
                    file: file_path.to_owned(),
                    start_line: user_line(metadata.start_line),
                    end_line: user_line(end_line),
                    signature: metadata.signature.clone(),
                    docstring: metadata.docstring.clone(),
                    is_async: metadata.is_async,
                    derives: metadata.derives.clone(),
                    visibility: metadata.visibility.clone(),
                    branches: complexity.map(|complexity| complexity.branches),
                    loops: complexity.map(|complexity| complexity.loops),
                    max_nesting: complexity.map(|complexity| complexity.max_nesting),
                    cyclomatic_complexity,
                    complexity_analysis: metadata.complexity_analysis,
                    cost_to_expand: NodeExpansionCostV1 {
                        body: u64::from(line_count) * 20,
                        full_file: file_size_bytes / 4,
                    },
                unavailable_fields: unavailable_fields.into_iter().map(str::to_owned).collect(),
            };
            Ok(graph_tool_completion(
                GraphToolResultV1::Node(NodeResultV1::Found(Box::new(details))),
                touched_files,
            ))
        }
        None => Ok(graph_tool_completion(
            GraphToolResultV1::Node(NodeResultV1::NotFound(node_not_found_result(
                &request.node_id,
            ))),
            Vec::new(),
        )),
    }
}

/// Cross-run node lookup by name.
#[hotpath::measure(label = "mcp.graph.by_qualified_name.total")]
pub async fn handle_by_qualified_name(
    graph: &VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let qname = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: qualified_name".to_string(),
        })?;

    let nodes = hotpath::measure_block!(
        "mcp.graph.by_qualified_name.graph",
        graph.resolve_qualified_name(qname, None, 1_000)?
    );
    let touched_files = graph_symbol_paths(&nodes)?;
    let items = nodes
        .iter()
        .map(graph_symbol_location_value)
        .collect::<Result<Vec<_>>>()?;

    let value = hotpath::measure_block!("mcp.graph.by_qualified_name.serialize", json!(items));
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &value,
        touched_files,
    ))
}

/// Signature-only lookup (no body) by qualified name or node ID. Returns
/// the public-API surface of a symbol so callers can avoid reading the
/// source file just to inspect the signature.
#[hotpath::measure(label = "mcp.graph.signature.total")]
pub async fn handle_signature(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let nodes = hotpath::measure_block!(
        "mcp.graph.signature.graph",
        nodes_addressed_by_args(graph, &args)?
    );
    let touched_files = graph_symbol_paths(&nodes)?;

    let mut items: Vec<Value> = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let metadata = required_graph_metadata(n)?;
        let file_path = required_graph_file_path(n)?;
        let file_size_bytes = bound_source_file_len(graph, file_path)?;
        let end_line = graph_symbol_end_line(metadata)?;
        items.push(json!({
            "node_id": n.occurrence.as_str(),
            "name": metadata.simple_name,
            "qualified_name": metadata.qualified_name,
            "kind": metadata.kind,
            "visibility": metadata.visibility,
            "signature": metadata.signature,
            "docstring": metadata.docstring,
            "is_async": metadata.is_async,
            "file": file_path,
            "start_line": user_line(metadata.start_line),
            "end_line": user_line(end_line),
            "cost_to_expand": cost_to_expand_verified(metadata, file_size_bytes)?,
            "unavailable_fields": ["attrs_start_line"],
        }));
    }

    let value = hotpath::measure_block!("mcp.graph.signature.serialize", json!(items));
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &value,
        touched_files,
    ))
}

/// Derive annotations attached to a symbol. Accepts `node_id` or
/// `qualified_name`. Macro expansion is outside the retained syntax evidence.
#[hotpath::measure(label = "mcp.graph.derives.total")]
pub async fn handle_derives(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let nodes = hotpath::measure_block!(
        "mcp.graph.derives.graph",
        nodes_addressed_by_args(graph, &args)?
    );
    if nodes.is_empty() {
        return Ok(text_tool_result("No matching symbol found.", Vec::new()));
    }

    let touched_files = graph_symbol_paths(&nodes)?;
    let mut items = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let metadata = required_graph_metadata(node)?;
        let file_path = required_graph_file_path(node)?;
        let derives = metadata
            .derives
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "evidence_class": "syntax_exact",
                    "unavailable_fields": ["generated_trait_impl", "generated_methods"],
                })
            })
            .collect::<Vec<_>>();
        items.push(json!({
            "node_id": node.occurrence.as_str(),
            "name": metadata.simple_name,
            "qualified_name": metadata.qualified_name,
            "kind": metadata.kind,
            "file": file_path,
            "line": user_line(metadata.start_line),
            "derives": derives,
        }));
    }

    let output = hotpath::measure_block!("mcp.graph.derives.serialize", json!(items));
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        touched_files,
    ))
}

fn bound_source_file_len(graph: &VerifiedGraphQuery, file_path: &str) -> Result<u64> {
    let (absolute, _) = graph.resolve_indexed_source_file(file_path)?;
    Ok(std::fs::metadata(absolute)?.len())
}

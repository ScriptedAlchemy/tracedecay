//! Dependency-clean graph-navigation handlers over [`VerifiedGraphQuery`].

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use tracedecay_application::retrieval::{
    CalleeV1, CalleesSurfaceRequestV1, ImpactNodeV1, ImpactResultV1, ImpactSurfaceRequestV1,
    NodeDetailsV1, NodeExpansionCostV1, NodeSurfaceRequestV1,
};
use tracedecay_domain::RelationEdgeKindV1;
use tracedecay_domain::code_intelligence::{EdgeKind, NodeKind};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::{CodeGraphSymbolSummaryV1, VerifiedGraphQuery};

use crate::{
    ToolResult, decode_primitive_request, generic_tool_result, require_node_id, text_tool_result,
    unique_file_paths,
};

use super::{
    GRAPH_RELATION_READ_LIMIT, canonical_relation_kind, canonical_relation_kind_name,
    cost_to_expand_verified, graph_name_matches, graph_occurrence_id, graph_symbol_corrupt,
    graph_symbol_end_line, graph_symbol_location_value, graph_symbol_paths, node_not_found,
    nodes_addressed_by_args, require_positive_depth, required_graph_file_path,
    required_graph_metadata, single_graph_adjacency_batch, traverse_verified_neighbors, user_line,
    verified_neighbor_value, verified_trait_dispatch_targets,
};

#[hotpath::measure(label = "mcp.graph.callers.total")]
pub async fn handle_callers(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(3, |v| v.min(10) as u32);
    require_positive_depth(max_depth)?;

    let occurrence = graph_occurrence_id(node_id)?;
    let results = hotpath::measure_block!(
        "mcp.graph.callers.graph",
        traverse_verified_neighbors(
            graph,
            occurrence,
            &[RelationEdgeKindV1::Calls],
            true,
            max_depth as usize,
        )?
    );
    let summaries = results
        .iter()
        .map(|result| result.symbol.clone())
        .collect::<Vec<_>>();
    let touched_files = graph_symbol_paths(&summaries)?;
    let items = results
        .iter()
        .map(verified_neighbor_value)
        .collect::<Result<Vec<_>>>()?;

    let value = hotpath::measure_block!("mcp.graph.callers.serialize", json!(items));
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &value,
        touched_files,
    ))
}

/// Beyond the direct `Calls` edges, this handler also surfaces *trait
/// dispatch targets*: when a callee is a method whose enclosing scope is a
/// trait, the concrete impl methods reachable through that trait are added
/// to the result list and tagged with `dispatch_via_trait: true`. The
/// original trait-method entry is preserved so callers can still see what
/// they statically called.
///
/// Dispatch resolution skipped when `resolve_dispatch=false` is passed.
#[hotpath::measure(label = "mcp.graph.callees.total")]
pub async fn handle_callees(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let request: CalleesSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_callees")?;
    let max_depth = request.max_depth.map_or(3, |value| value.min(10));
    require_positive_depth(max_depth)?;
    let resolve_dispatch = request.resolve_dispatch.unwrap_or(true);

    let occurrence = graph_occurrence_id(&request.node_id)?;
    let results = hotpath::measure_block!(
        "mcp.graph.callees.graph",
        traverse_verified_neighbors(
            graph,
            occurrence,
            &[RelationEdgeKindV1::Calls],
            false,
            max_depth as usize,
        )?
    );
    let mut seen = results
        .iter()
        .map(|result| result.symbol.occurrence.clone())
        .collect::<HashSet<_>>();

    let mut items = results
        .iter()
        .map(|result| {
            let metadata = required_graph_metadata(&result.symbol)?;
            Ok(CalleeV1 {
                node_id: result.symbol.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                file: required_graph_file_path(&result.symbol)?.to_owned(),
                line: user_line(metadata.start_line),
                edge_kind: canonical_relation_kind_name(result.edge_kind).to_owned(),
                dispatch_via_trait: false,
                depth: Some(u32::try_from(result.depth).map_err(|_| {
                    graph_symbol_corrupt("callee traversal depth exceeds u32".to_owned())
                })?),
                dispatch_from: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if resolve_dispatch {
        hotpath::measure_block!("mcp.graph.callees.dispatch", {
            for callee in &results {
                for impl_method in verified_trait_dispatch_targets(graph, &callee.symbol)? {
                    if !seen.insert(impl_method.occurrence.clone()) {
                        continue;
                    }
                    let metadata = required_graph_metadata(&impl_method)?;
                    items.push(CalleeV1 {
                        node_id: impl_method.occurrence.as_str().to_owned(),
                        name: metadata.simple_name.clone(),
                        kind: metadata.kind.clone(),
                        file: required_graph_file_path(&impl_method)?.to_owned(),
                        line: user_line(metadata.start_line),
                        edge_kind: "calls".to_owned(),
                        dispatch_via_trait: true,
                        depth: None,
                        dispatch_from: Some(callee.symbol.occurrence.as_str().to_owned()),
                    });
                }
            }
        });
    }

    let touched_files = unique_file_paths(items.iter().map(|item| item.file.as_str()));

    let value =
        hotpath::measure_block!("mcp.graph.callees.serialize", serde_json::to_value(items)?);
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &value,
        touched_files,
    ))
}

#[hotpath::measure(label = "mcp.graph.impact.total")]
pub async fn handle_impact(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let request: ImpactSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_impact")?;
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

    let output = hotpath::measure_block!(
        "mcp.graph.impact.serialize",
        serde_json::to_value(ImpactResultV1 {
            node_count: nodes.len(),
            complete: impact.complete,
            unavailable_fields: vec!["edge_count".to_owned()],
            nodes,
        })?
    );

    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        touched_files,
    ))
}

#[hotpath::measure(label = "mcp.graph.node.total")]
pub async fn handle_node(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
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
            let cyclomatic_complexity = metadata.branches.checked_add(1).ok_or_else(|| {
                graph_symbol_corrupt(format!(
                    "verified graph symbol '{}' branch count overflows complexity",
                    n.occurrence.as_str()
                ))
            })?;
            let line_count = end_line - metadata.start_line + 1;
            let output = hotpath::measure_block!(
                "mcp.graph.node.serialize",
                serde_json::to_value(NodeDetailsV1 {
                    id: n.occurrence.as_str().to_owned(),
                    name: metadata.simple_name.clone(),
                    kind: metadata.kind.clone(),
                    qualified_name: metadata.qualified_name.clone(),
                    file: file_path.to_owned(),
                    start_line: user_line(metadata.start_line),
                    end_line: user_line(end_line),
                    signature: metadata.signature.clone(),
                    visibility: metadata.visibility.clone(),
                    branches: metadata.branches,
                    loops: metadata.loops,
                    max_nesting: metadata.max_nesting,
                    cyclomatic_complexity,
                    cost_to_expand: NodeExpansionCostV1 {
                        body: u64::from(line_count) * 20,
                        full_file: file_size_bytes / 4,
                    },
                    unavailable_fields: [
                        "assertions",
                        "attrs_start_line",
                        "derives",
                        "docstring",
                        "is_async",
                        "returns",
                        "unchecked_calls",
                        "unsafe_blocks",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                })?
            );
            Ok(generic_tool_result(
                Some(graph.project_root()?),
                &args,
                &output,
                touched_files,
            ))
        }
        None => node_not_found(&request.node_id),
    }
}

/// Bulk caller lookup over many IDs.
#[hotpath::measure(label = "mcp.graph.callers_for.total")]
pub async fn handle_callers_for(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let node_ids: Vec<String> = args
        .get("node_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if node_ids.is_empty() {
        return Err(TraceDecayError::Config {
            message: "callers_for requires non-empty node_ids".to_string(),
        });
    }

    // Default to "calls" but allow any kind (or empty string for all kinds).
    let kind_arg = args.get("kind").and_then(|v| v.as_str()).unwrap_or("calls");
    let kinds: Vec<RelationEdgeKindV1> = if kind_arg.is_empty() {
        Vec::new()
    } else {
        match EdgeKind::from_str(kind_arg) {
            Some(k) => vec![canonical_relation_kind(k)?],
            None => {
                return Err(TraceDecayError::Config {
                    message: format!("unknown edge kind: {kind_arg}"),
                });
            }
        }
    };

    let max_per_item = args
        .get("max_per_item")
        .and_then(serde_json::Value::as_u64)
        .map_or(1000usize, |v| v.min(10_000) as usize);

    let occurrences = node_ids
        .iter()
        .map(|node_id| graph_occurrence_id(node_id))
        .collect::<Result<Vec<_>>>()?;
    let batches = hotpath::measure_block!(
        "mcp.graph.callers_for.graph",
        graph.callers(&occurrences, &kinds, 2_000_000)?
    );
    if batches.len() != node_ids.len() {
        return Err(graph_symbol_corrupt(format!(
            "verified graph returned {} caller batches for {} symbols",
            batches.len(),
            node_ids.len()
        )));
    }

    let mut truncated = false;
    let mut by_target = HashMap::new();
    for (target, callers) in node_ids.iter().zip(batches) {
        if callers.len() > max_per_item {
            truncated = true;
        }
        by_target.insert(
            target,
            callers
                .into_iter()
                .take(max_per_item)
                .map(|edge| edge.neighbor.occurrence.as_str().to_owned())
                .collect::<Vec<_>>(),
        );
    }

    // Ensure every requested ID appears in the response, even if no callers.
    let result_map: HashMap<&String, Vec<String>> = node_ids
        .iter()
        .map(|id| (id, by_target.remove(id).unwrap_or_default()))
        .collect();

    let output = hotpath::measure_block!(
        "mcp.graph.callers_for.serialize",
        json!({
            "callers": result_map,
            "truncated": truncated,
            "max_per_item": max_per_item,
        })
    );
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        vec![],
    ))
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
            "file": file_path,
            "start_line": user_line(metadata.start_line),
            "end_line": user_line(end_line),
            "cost_to_expand": cost_to_expand_verified(metadata, file_size_bytes)?,
            "unavailable_fields": ["attrs_start_line", "docstring", "is_async"],
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

/// Index of `impl Trait for Type` blocks.
///
/// Both `trait` and `type` arguments are optional. With neither, every impl
/// in the graph is returned (capped by `limit`). Surfaces trait-dispatch
/// information that is otherwise hidden behind raw `Implements` edges.
#[hotpath::measure(label = "mcp.graph.impls.total")]
pub async fn handle_impls(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let trait_filter = args.get("trait").and_then(|v| v.as_str());
    let type_filter = args.get("type").and_then(|v| v.as_str());
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(100, |v| v.min(1000) as usize);

    let mut after = None;
    let mut results = Vec::new();
    let mut examined = 0usize;
    let mut generation_complete = false;
    hotpath::measure_block!("mcp.graph.impls.graph", {
        while results.len() <= limit {
            if examined >= 500_000 {
                return Err(TraceDecayError::ProjectRoute {
                    reason_code: "verified-code-graph-budget-exhausted".to_owned(),
                    retryable: false,
                    detail: "impl census exceeded 500000 verified symbols".to_owned(),
                });
            }
            let page = graph.symbols_page(after.as_ref(), 1_024)?;
            examined = examined.saturating_add(page.symbols.len());
            after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
            for impl_node in page.symbols {
                let metadata = required_graph_metadata(&impl_node)?;
                if metadata.kind != NodeKind::Impl.as_str()
                    || type_filter.is_some_and(|query| !graph_name_matches(metadata, query))
                {
                    continue;
                }
                let traits = single_graph_adjacency_batch(graph.callees(
                    std::slice::from_ref(&impl_node.occurrence),
                    &[RelationEdgeKindV1::Implements],
                    GRAPH_RELATION_READ_LIMIT,
                )?)?;
                let trait_node = traits.into_iter().next().map(|edge| edge.neighbor);
                if trait_filter.is_some_and(|query| {
                    trait_node
                        .as_ref()
                        .and_then(|node| node.metadata.as_ref())
                        .is_none_or(|metadata| !graph_name_matches(metadata, query))
                }) {
                    continue;
                }
                results.push((impl_node, trait_node));
                if results.len() > limit {
                    break;
                }
            }
            if !page.has_more {
                generation_complete = true;
                break;
            }
        }
    });
    let truncated = !generation_complete || results.len() > limit;
    results.truncate(limit);

    let result_paths = results
        .iter()
        .map(|(impl_node, _)| required_graph_file_path(impl_node))
        .collect::<Result<Vec<_>>>()?;
    let touched_files = unique_file_paths(result_paths.into_iter());

    let items = results
        .iter()
        .map(|(impl_node, trait_node)| {
            let metadata = required_graph_metadata(impl_node)?;
            let file_path = required_graph_file_path(impl_node)?;
            let trait_metadata = trait_node
                .as_ref()
                .map(required_graph_metadata)
                .transpose()?;
            Ok(json!({
                "impl_id": impl_node.occurrence.as_str(),
                "type": metadata.simple_name,
                "qualified_name": metadata.qualified_name,
                "trait": trait_metadata.map(|value| value.simple_name.as_str()),
                "trait_qualified_name": trait_metadata.map(|value| value.qualified_name.as_str()),
                "trait_id": trait_node.as_ref().map(|value| value.occurrence.as_str()),
                "file": file_path,
                "start_line": user_line(metadata.start_line),
                "end_line": user_line(graph_symbol_end_line(metadata)?),
                "signature": metadata.signature,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    let output = hotpath::measure_block!(
        "mcp.graph.impls.serialize",
        json!({
            "count": items.len(),
            "truncated": truncated,
            "impls": items,
        })
    );
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        touched_files,
    ))
}

/// Derive annotations are not published in the
/// verified code graph generation, so a matched symbol reports a typed
/// evidence-unavailable route error. Accepts `node_id` or `qualified_name`.
#[hotpath::measure(label = "mcp.graph.derives.total")]
pub async fn handle_derives(graph: &VerifiedGraphQuery, args: Value) -> Result<ToolResult> {
    let nodes = hotpath::measure_block!(
        "mcp.graph.derives.graph",
        nodes_addressed_by_args(graph, &args)?
    );
    if nodes.is_empty() {
        return Ok(text_tool_result("No matching symbol found.", Vec::new()));
    }
    Err(TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-evidence-unavailable".to_owned(),
        retryable: false,
        detail: "derive annotations are not published in the verified code graph generation"
            .to_owned(),
    })
}

/// Trait / method implementor lookup.
#[hotpath::measure(label = "mcp.graph.implementations.total")]
pub async fn handle_implementations(
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let trait_name = args.get("trait").and_then(|v| v.as_str());
    let method_name = args.get("method").and_then(|v| v.as_str());

    if trait_name.is_none() && method_name.is_none() {
        return Err(TraceDecayError::Config {
            message: "missing required parameter: 'trait' or 'method'".to_string(),
        });
    }
    if trait_name.is_some() && method_name.is_some() {
        return Err(TraceDecayError::Config {
            message: "tracedecay_implementations: 'trait' and 'method' are mutually exclusive"
                .to_string(),
        });
    }

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(20, |v| v.clamp(1, 200) as usize);

    let mut entries: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    hotpath::measure_block!("mcp.graph.implementations.graph", {
        if let Some(name) = trait_name {
            let candidates = graph.resolve_simple_name(name, None, 50)?;
            let trait_nodes: Vec<_> = candidates
                .into_iter()
                .filter(|node| {
                    node.metadata.as_ref().is_some_and(|metadata| {
                        matches!(
                            NodeKind::from_str(&metadata.kind),
                            Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
                        )
                    })
                })
                .collect();
            if trait_nodes.is_empty() {
                return Ok(text_tool_result(
                    &format!("No trait or interface named '{name}' found."),
                    vec![],
                ));
            }

            for trait_node in trait_nodes {
                let trait_metadata = required_graph_metadata(&trait_node)?;
                let implementors = single_graph_adjacency_batch(graph.callers(
                    std::slice::from_ref(&trait_node.occurrence),
                    &[RelationEdgeKindV1::Implements],
                    GRAPH_RELATION_READ_LIMIT,
                )?)?;
                for implementor in implementors {
                    let impl_node = implementor.neighbor;
                    let impl_metadata = required_graph_metadata(&impl_node)?;
                    let impl_file = required_graph_file_path(&impl_node)?;
                    if scope_prefix.is_some_and(|prefix| !impl_file.starts_with(prefix)) {
                        continue;
                    }
                    let methods = collect_method_bodies(graph, &impl_node)?;
                    if !touched.iter().any(|path| path == impl_file) {
                        touched.push(impl_file.to_owned());
                    }
                    entries.push(json!({
                        "type": impl_metadata.simple_name,
                        "qualified_name": impl_metadata.qualified_name,
                        "kind": impl_metadata.kind,
                        "file": impl_file,
                        "line": user_line(impl_metadata.start_line),
                        "trait": trait_metadata.qualified_name,
                        "methods": methods,
                    }));
                    if entries.len() >= limit {
                        break;
                    }
                }
                if entries.len() >= limit {
                    break;
                }
            }
        } else if let Some(name) = method_name {
            let nodes = graph.resolve_simple_name(name, None, limit.saturating_mul(4))?;
            let mut method_nodes = Vec::new();
            for node in nodes {
                let metadata = required_graph_metadata(&node)?;
                if !matches!(
                    NodeKind::from_str(&metadata.kind),
                    Some(NodeKind::Function | NodeKind::Method)
                ) {
                    continue;
                }
                let file_path = required_graph_file_path(&node)?;
                if scope_prefix.is_none_or(|prefix| file_path.starts_with(prefix)) {
                    method_nodes.push(node);
                    if method_nodes.len() == limit {
                        break;
                    }
                }
            }
            if method_nodes.is_empty() {
                return Ok(text_tool_result(
                    &format!("No function or method named '{name}' found."),
                    vec![],
                ));
            }
            for n in method_nodes {
                let metadata = required_graph_metadata(&n)?;
                let file_path = required_graph_file_path(&n)?;
                let source = graph.read_indexed_source_file(file_path)?;
                let end_line = graph_symbol_end_line(metadata)?;
                let body =
                    crate::handlers::info::extract_lines(&source, metadata.start_line, end_line);
                if !touched.iter().any(|path| path == file_path) {
                    touched.push(file_path.to_owned());
                }
                entries.push(json!({
                    "name": metadata.simple_name,
                    "qualified_name": metadata.qualified_name,
                    "kind": metadata.kind,
                    "file": file_path,
                    "line": user_line(metadata.start_line),
                    "end_line": user_line(end_line),
                    "signature": metadata.signature,
                    "body": body,
                }));
            }
        }
    });

    let payload = hotpath::measure_block!(
        "mcp.graph.implementations.serialize",
        json!({
            "match_count": entries.len(),
            "implementations": entries,
        })
    );
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &payload,
        touched,
    ))
}

fn bound_source_file_len(graph: &VerifiedGraphQuery, file_path: &str) -> Result<u64> {
    let (absolute, _) = graph.resolve_indexed_source_file(file_path)?;
    Ok(std::fs::metadata(absolute)?.len())
}

fn collect_method_bodies(
    graph: &VerifiedGraphQuery,
    impl_node: &CodeGraphSymbolSummaryV1,
) -> Result<Vec<Value>> {
    let children = single_graph_adjacency_batch(graph.callees(
        std::slice::from_ref(&impl_node.occurrence),
        &[RelationEdgeKindV1::Contains],
        GRAPH_RELATION_READ_LIMIT,
    )?)?;
    let mut methods = Vec::new();
    for child in children {
        let child = child.neighbor;
        let metadata = required_graph_metadata(&child)?;
        if !matches!(
            NodeKind::from_str(&metadata.kind),
            Some(NodeKind::Method | NodeKind::Function)
        ) {
            continue;
        }
        let file_path = required_graph_file_path(&child)?.to_owned();
        methods.push((file_path, metadata.start_line, child));
    }
    methods.sort_by(|left, right| {
        (&left.0, left.1, &left.2.occurrence).cmp(&(&right.0, right.1, &right.2.occurrence))
    });

    let mut out: Vec<Value> = Vec::new();
    for (file_path, _, child) in methods {
        let metadata = required_graph_metadata(&child)?;
        let source = graph.read_indexed_source_file(&file_path)?;
        let end_line = graph_symbol_end_line(metadata)?;
        let body = crate::handlers::info::extract_lines(&source, metadata.start_line, end_line);
        out.push(json!({
            "name": metadata.simple_name,
            "kind": metadata.kind,
            "line": user_line(metadata.start_line),
            "signature": metadata.signature,
            "body": body,
        }));
    }
    Ok(out)
}

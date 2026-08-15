//! Generation-pinned graph projection adapters shared by MCP graph handlers.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde_json::{Value, json};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};

use crate::context::{
    CONTEXT_CODE_HEADING, CONTEXT_ENTRY_POINTS_HEADING, CONTEXT_RELATED_SYMBOLS_HEADING,
};
use crate::errors::{Result, TraceDecayError};
use crate::path_tree::format_compact_path_list;
use crate::tracedecay::queries::graph::VerifiedGraphQuery;
use crate::types::{EdgeKind, NodeKind};

pub(super) const GRAPH_RELATION_READ_LIMIT: usize = 50_000;

/// Admits a caller-supplied node id as a graph occurrence id. Every graph
/// handler that takes a node id funnels through this one guard, so a blank
/// value is rejected as a typed argument error naming the parameter before
/// canonicality parsing ever sees it — including handlers that decode a
/// typed request DTO and therefore bypass `require_node_id`.
pub(super) fn graph_occurrence_id(raw: &str) -> Result<SymbolOccurrenceId> {
    if raw.trim().is_empty() {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: node_id must not be empty".to_string(),
        });
    }
    SymbolOccurrenceId::new(raw.to_owned()).map_err(|error| TraceDecayError::Config {
        message: format!("invalid graph symbol occurrence: {error}"),
    })
}

pub(super) fn nodes_addressed_by_args(
    graph: &VerifiedGraphQuery,
    args: &Value,
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    let node_id = args
        .get("node_id")
        .or_else(|| args.get("id"))
        .and_then(Value::as_str);
    if let Some(node_id) = node_id {
        let occurrence = graph_occurrence_id(node_id)?;
        return Ok(graph.symbol_summary(&occurrence)?.into_iter().collect());
    }
    let Some(qualified_name) = args.get("qualified_name").and_then(Value::as_str) else {
        return Err(TraceDecayError::Config {
            message: "missing required parameter: qualified_name or node_id".to_owned(),
        });
    };
    graph.resolve_qualified_name(qualified_name, None, 1_000)
}

pub(super) fn required_graph_metadata(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<&tracedecay_code_index::lineage::LineageSymbolRecordV1> {
    symbol.metadata.as_ref().ok_or_else(|| {
        graph_symbol_corrupt(format!(
            "verified graph symbol '{}' has no extraction metadata",
            symbol.occurrence.as_str()
        ))
    })
}

pub(super) fn required_graph_file_path(symbol: &CodeGraphSymbolSummaryV1) -> Result<&str> {
    symbol
        .binding
        .as_ref()
        .and_then(|binding| binding.logical_path.as_deref())
        .ok_or_else(|| {
            graph_symbol_corrupt(format!(
                "verified graph symbol '{}' has no logical file binding",
                symbol.occurrence.as_str()
            ))
        })
}

pub(super) fn graph_symbol_end_line(
    metadata: &tracedecay_code_index::lineage::LineageSymbolRecordV1,
) -> Result<u32> {
    if metadata.line_span == 0 {
        return Err(graph_symbol_corrupt(format!(
            "verified graph symbol '{}' has an empty line span",
            metadata.occurrence.as_str()
        )));
    }
    metadata
        .start_line
        .checked_add(metadata.line_span - 1)
        .ok_or_else(|| {
            graph_symbol_corrupt(format!(
                "verified graph symbol '{}' line span overflows its start line",
                metadata.occurrence.as_str()
            ))
        })
}

pub(super) fn graph_symbol_paths(symbols: &[CodeGraphSymbolSummaryV1]) -> Result<Vec<String>> {
    let mut paths = symbols
        .iter()
        .map(required_graph_file_path)
        .collect::<Result<Vec<_>>>()?;
    paths.sort_unstable();
    paths.dedup();
    Ok(paths.into_iter().map(str::to_owned).collect())
}

pub(super) fn graph_symbols_in_scope(
    symbols: Vec<CodeGraphSymbolSummaryV1>,
    scope_prefix: Option<&str>,
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    let mut scoped = Vec::new();
    for symbol in symbols {
        let file_path = required_graph_file_path(&symbol)?;
        if scope_prefix.is_none_or(|prefix| file_path.starts_with(prefix)) {
            scoped.push(symbol);
        }
    }
    Ok(scoped)
}

pub(super) fn graph_symbol_location_value(symbol: &CodeGraphSymbolSummaryV1) -> Result<Value> {
    let metadata = required_graph_metadata(symbol)?;
    let file_path = required_graph_file_path(symbol)?;
    Ok(json!({
        "node_id": symbol.occurrence.as_str(),
        "name": metadata.simple_name,
        "qualified_name": metadata.qualified_name,
        "kind": metadata.kind,
        "file": file_path,
        "start_line": metadata.start_line.saturating_add(1),
        "end_line": graph_symbol_end_line(metadata)?.saturating_add(1),
        "unavailable_fields": ["attrs_start_line"],
    }))
}

pub(super) fn graph_name_matches(
    metadata: &tracedecay_code_index::lineage::LineageSymbolRecordV1,
    query: &str,
) -> bool {
    let query = query.to_ascii_lowercase();
    metadata.simple_name.to_ascii_lowercase().contains(&query)
        || metadata
            .qualified_name
            .to_ascii_lowercase()
            .contains(&query)
}

pub(super) fn canonical_relation_kind(kind: EdgeKind) -> Result<RelationEdgeKindV1> {
    match kind {
        EdgeKind::Calls => Ok(RelationEdgeKindV1::Calls),
        EdgeKind::Uses => Ok(RelationEdgeKindV1::Uses),
        EdgeKind::TypeOf => Ok(RelationEdgeKindV1::TypeOf),
        EdgeKind::Contains => Ok(RelationEdgeKindV1::Contains),
        EdgeKind::Implements => Ok(RelationEdgeKindV1::Implements),
        EdgeKind::Extends => Ok(RelationEdgeKindV1::Extends),
        EdgeKind::Annotates => Ok(RelationEdgeKindV1::Annotates),
        EdgeKind::Returns => Ok(RelationEdgeKindV1::Returns),
        EdgeKind::Receives => Ok(RelationEdgeKindV1::Receives),
        EdgeKind::DerivesMacro => Err(TraceDecayError::Config {
            message: "derive-macro relations are not published in the verified code graph"
                .to_owned(),
        }),
    }
}

pub(super) fn canonical_relation_kind_name(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "calls",
        RelationEdgeKindV1::Uses => "uses",
        RelationEdgeKindV1::TypeOf => "type_of",
        RelationEdgeKindV1::Contains => "contains",
        RelationEdgeKindV1::Implements => "implements",
        RelationEdgeKindV1::Extends => "extends",
        RelationEdgeKindV1::Annotates => "annotates",
        RelationEdgeKindV1::Returns => "returns",
        RelationEdgeKindV1::Receives => "receives",
    }
}

pub(super) fn single_graph_adjacency_batch<T>(mut batches: Vec<Vec<T>>) -> Result<Vec<T>> {
    if batches.len() != 1 {
        return Err(graph_symbol_corrupt(format!(
            "verified graph adjacency returned {} batches for one symbol",
            batches.len()
        )));
    }
    Ok(batches.remove(0))
}

pub(super) struct VerifiedNeighbor {
    pub(super) symbol: CodeGraphSymbolSummaryV1,
    pub(super) edge_kind: RelationEdgeKindV1,
    pub(super) depth: usize,
}

pub(super) fn traverse_verified_neighbors(
    graph: &VerifiedGraphQuery,
    seed: SymbolOccurrenceId,
    kinds: &[RelationEdgeKindV1],
    incoming: bool,
    max_depth: usize,
) -> Result<Vec<VerifiedNeighbor>> {
    let mut seen = HashSet::from([seed.clone()]);
    let mut frontier = vec![seed];
    let mut results = Vec::new();
    for depth in 1..=max_depth {
        if frontier.is_empty() {
            break;
        }
        let batches = if incoming {
            graph.callers(&frontier, kinds, GRAPH_RELATION_READ_LIMIT)?
        } else {
            graph.callees(&frontier, kinds, GRAPH_RELATION_READ_LIMIT)?
        };
        if batches.len() != frontier.len() {
            return Err(graph_symbol_corrupt(format!(
                "verified graph returned {} adjacency batches for {} frontier symbols",
                batches.len(),
                frontier.len()
            )));
        }
        let mut next = Vec::new();
        for edge in batches.into_iter().flatten() {
            let occurrence = edge.neighbor.occurrence.clone();
            if !seen.insert(occurrence.clone()) {
                continue;
            }
            next.push(occurrence);
            results.push(VerifiedNeighbor {
                symbol: edge.neighbor,
                edge_kind: edge.edge.kind,
                depth,
            });
        }
        frontier = next;
    }
    Ok(results)
}

pub(super) fn verified_neighbor_value(result: &VerifiedNeighbor) -> Result<Value> {
    let metadata = required_graph_metadata(&result.symbol)?;
    Ok(json!({
        "node_id": result.symbol.occurrence.as_str(),
        "name": metadata.simple_name,
        "kind": metadata.kind,
        "file": required_graph_file_path(&result.symbol)?,
        "line": metadata.start_line.saturating_add(1),
        "edge_kind": canonical_relation_kind_name(result.edge_kind),
        "depth": result.depth,
    }))
}

pub(super) fn verified_trait_dispatch_targets(
    graph: &VerifiedGraphQuery,
    method: &CodeGraphSymbolSummaryV1,
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    let method_metadata = required_graph_metadata(method)?;
    if !matches!(
        NodeKind::from_str(&method_metadata.kind),
        Some(NodeKind::Method | NodeKind::Function)
    ) {
        return Ok(Vec::new());
    }
    let parents = single_graph_adjacency_batch(graph.callers(
        std::slice::from_ref(&method.occurrence),
        &[RelationEdgeKindV1::Contains],
        GRAPH_RELATION_READ_LIMIT,
    )?)?;
    let traits = parents
        .into_iter()
        .map(|edge| edge.neighbor)
        .filter(|parent| {
            parent.metadata.as_ref().is_some_and(|metadata| {
                matches!(
                    NodeKind::from_str(&metadata.kind),
                    Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
                )
            })
        })
        .collect::<Vec<_>>();
    let trait_occurrences = traits
        .iter()
        .map(|node| node.occurrence.clone())
        .collect::<Vec<_>>();
    if trait_occurrences.is_empty() {
        return Ok(Vec::new());
    }
    let implementors = graph
        .callers(
            &trait_occurrences,
            &[RelationEdgeKindV1::Implements],
            GRAPH_RELATION_READ_LIMIT,
        )?
        .into_iter()
        .flatten()
        .map(|edge| edge.neighbor.occurrence)
        .collect::<Vec<_>>();
    if implementors.is_empty() {
        return Ok(Vec::new());
    }
    let mut targets = Vec::new();
    for child in graph
        .callees(
            &implementors,
            &[RelationEdgeKindV1::Contains],
            GRAPH_RELATION_READ_LIMIT,
        )?
        .into_iter()
        .flatten()
    {
        let metadata = required_graph_metadata(&child.neighbor)?;
        if matches!(
            NodeKind::from_str(&metadata.kind),
            Some(NodeKind::Method | NodeKind::Function)
        ) && metadata.simple_name == method_metadata.simple_name
        {
            targets.push(child.neighbor);
        }
    }
    Ok(targets)
}

pub(super) fn verified_context_markdown(
    task: &str,
    symbols: &[Value],
    related: &[Value],
    code: &[Value],
) -> Result<String> {
    let mut output = format!("# Context for {task}\n\n{CONTEXT_CODE_HEADING}\n");
    if code.is_empty() {
        for symbol in symbols {
            let _ = writeln!(
                output,
                "- **{}** ({}) — {}:{}",
                field_str(symbol, "name")?,
                field_str(symbol, "kind")?,
                field_str(symbol, "file")?,
                field_i64(symbol, "start_line")?,
            );
        }
    } else {
        for block in code {
            let _ = writeln!(
                output,
                "#### {}:{}\n```\n{}\n```",
                field_str(block, "file")?,
                field_i64(block, "start_line")?,
                field_str(block, "code")?,
            );
        }
    }
    output.push('\n');
    output.push_str(CONTEXT_RELATED_SYMBOLS_HEADING);
    output.push('\n');
    for symbol in related {
        let _ = writeln!(
            output,
            "- **{}** ({}) — {}:{}",
            field_str(symbol, "name")?,
            field_str(symbol, "kind")?,
            field_str(symbol, "file")?,
            field_i64(symbol, "start_line")?,
        );
    }
    output.push('\n');
    output.push_str(CONTEXT_ENTRY_POINTS_HEADING);
    output.push('\n');
    for symbol in symbols.iter().take(5) {
        let _ = writeln!(output, "- `{}`", field_str(symbol, "qualified_name")?);
    }
    Ok(output)
}

pub(super) fn append_verified_plan_context(
    graph: &VerifiedGraphQuery,
    symbols: &[CodeGraphSymbolSummaryV1],
    output: &mut String,
) -> Result<()> {
    output.push_str("\n### Extension Points\n");
    let mut found_extension = false;
    for node in symbols {
        let metadata = required_graph_metadata(node)?;
        if matches!(
            NodeKind::from_str(&metadata.kind),
            Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
        ) && matches!(metadata.visibility.as_str(), "pub" | "public")
        {
            let implementors = single_graph_adjacency_batch(graph.callers(
                std::slice::from_ref(&node.occurrence),
                &[RelationEdgeKindV1::Implements],
                GRAPH_RELATION_READ_LIMIT,
            )?)?;
            let _ = writeln!(
                output,
                "- **{}** ({}) - {}:{} ({} implementors)",
                metadata.simple_name,
                metadata.kind,
                required_graph_file_path(node)?,
                metadata.start_line.saturating_add(1),
                implementors.len(),
            );
            found_extension = true;
        }
    }
    if !found_extension {
        output.push_str("_No public traits/interfaces found in context._\n");
    }
    if symbols.is_empty() {
        return Ok(());
    }
    output.push_str("\n### Test Coverage\n");
    let annotated_files = graph.test_annotated_logical_files(None, 500_000, 2_000_000)?;
    let mut test_files = HashSet::new();
    for symbol in symbols {
        for caller in traverse_verified_neighbors(
            graph,
            symbol.occurrence.clone(),
            &[RelationEdgeKindV1::Calls],
            true,
            2,
        )? {
            let file_path = required_graph_file_path(&caller.symbol)?;
            if crate::tracedecay::is_test_file(file_path) || annotated_files.contains(file_path) {
                test_files.insert(file_path.to_owned());
            }
        }
    }
    if test_files.is_empty() {
        output.push_str("_No test files found covering these modules._\n");
    } else {
        let mut sorted = test_files.into_iter().collect::<Vec<_>>();
        sorted.sort();
        output.push_str(&format_compact_path_list(
            sorted.iter().map(String::as_str),
            "- ",
            "",
        ));
        output.push('\n');
    }
    Ok(())
}

pub(super) fn cost_to_expand_verified(
    metadata: &tracedecay_code_index::lineage::LineageSymbolRecordV1,
    file_size_bytes: u64,
) -> Result<Value> {
    let end_line = graph_symbol_end_line(metadata)?;
    let line_count = end_line - metadata.start_line + 1;
    Ok(json!({
        "body": u64::from(line_count) * 20,
        "full_file": file_size_bytes / 4,
    }))
}

pub(super) fn line_for_byte_offset(source: &str, byte_offset: u64) -> Result<u32> {
    let offset = usize::try_from(byte_offset).map_err(|_| {
        graph_symbol_corrupt("graph evidence byte offset exceeds this platform".to_owned())
    })?;
    if offset > source.len() {
        return Err(graph_symbol_corrupt(format!(
            "graph evidence byte offset {offset} exceeds source length {}",
            source.len()
        )));
    }
    u32::try_from(
        source.as_bytes()[..offset]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
    )
    .map_err(|_| graph_symbol_corrupt("graph evidence line exceeds u32".to_owned()))
}

pub(super) fn graph_symbol_corrupt(detail: String) -> TraceDecayError {
    TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-symbol-corrupt".to_owned(),
        retryable: false,
        detail,
    }
}

fn field_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| {
        graph_symbol_corrupt(format!(
            "verified context value has no string field '{key}'"
        ))
    })
}

fn field_i64(value: &Value, key: &str) -> Result<i64> {
    value.get(key).and_then(Value::as_i64).ok_or_else(|| {
        graph_symbol_corrupt(format!(
            "verified context value has no integer field '{key}'"
        ))
    })
}

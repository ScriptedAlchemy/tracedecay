//! `tracedecay_test_risk` and `tracedecay_test_map`.

use super::*;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};

const MAX_TEST_MAP_FILE_SYMBOLS: usize = 50_000;
const MAX_TEST_MAP_IMPACT_SYMBOLS: usize = 20_000;
const MAX_TEST_MAP_RELATIONS_PER_HOP: usize = 20_000;

/// Handles `tracedecay_test_risk` tool calls.
pub(crate) async fn handle_test_risk(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(20, |v| v.min(200) as usize);
    let path_prefix = effective_path(&args, scope_prefix);
    let include_tested = args
        .get("include_tested")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let report = crate::graph::health::test_risk::analyze_test_risk(
        cg,
        graph,
        path_prefix,
        include_tested,
        limit,
    )
    .await?;
    let output = serde_json::to_value(report).map_err(|err| TraceDecayError::Config {
        message: format!("failed to serialize test risk report: {err}"),
    })?;

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
    ))
}

/// Handles `tracedecay_test_map` tool calls.
pub(crate) async fn handle_test_map(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let source_nodes = if let Some(file) = args.get("file").and_then(|v| v.as_str()) {
        let nodes = graph.symbols_in_logical_file(file, MAX_TEST_MAP_FILE_SYMBOLS + 1)?;
        if nodes.len() > MAX_TEST_MAP_FILE_SYMBOLS {
            return Err(test_map_unavailable(
                "verified test-map file census exceeded its symbol budget",
            ));
        }
        nodes
    } else if let Some(node_id) = args
        .get("node_id")
        .or(args.get("id"))
        .and_then(|v| v.as_str())
    {
        let occurrence = SymbolOccurrenceId::new(node_id.to_owned()).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid test-map symbol occurrence: {error}"),
            }
        })?;
        graph.symbol_summary(&occurrence)?.into_iter().collect()
    } else {
        return Err(TraceDecayError::Config {
            message: "missing required parameter: 'file' or 'node_id'".to_string(),
        });
    };

    let mut coverage_map: Vec<Value> = Vec::new();
    let mut uncovered: Vec<Value> = Vec::new();
    let mut all_test_files: HashSet<String> = HashSet::new();
    let test_evidence = crate::graph::health::test_risk::verified_test_evidence(graph)?;

    for node in &source_nodes {
        let (metadata, source_file) =
            crate::graph::health::test_risk::verified_test_symbol_parts(node)?;
        if !NodeKind::from_str(&metadata.kind).is_some_and(|kind| kind.is_callable_kind()) {
            continue;
        }
        let impact = graph.impact(
            std::slice::from_ref(&node.occurrence),
            &[RelationEdgeKindV1::Calls],
            3,
            MAX_TEST_MAP_IMPACT_SYMBOLS,
            MAX_TEST_MAP_RELATIONS_PER_HOP,
        )?;
        if !impact.complete {
            return Err(test_map_unavailable(
                "verified test-map caller expansion exceeded its budget",
            ));
        }
        let mut test_callers = Vec::new();
        for caller in impact.impacted {
            if caller.summary.occurrence == node.occurrence {
                continue;
            }
            let (caller_metadata, caller_file) =
                crate::graph::health::test_risk::verified_test_symbol_parts(&caller.summary)?;
            if !crate::tracedecay::is_test_file(caller_file)
                && !test_evidence
                    .test_annotated
                    .contains(caller.summary.occurrence.as_str())
            {
                continue;
            }
            all_test_files.insert(caller_file.to_owned());
            test_callers.push(json!({
                "test_name": caller_metadata.simple_name,
                "test_file": caller_file,
                "test_line": caller_metadata.start_line.saturating_add(1),
                "attribution_depth": caller.depth,
            }));
        }

        if test_callers.is_empty() {
            uncovered.push(json!({
                "id": node.occurrence.as_str(),
                "name": metadata.simple_name,
                "file": source_file,
                "line": metadata.start_line.saturating_add(1),
            }));
        } else {
            coverage_map.push(json!({
                "source_name": metadata.simple_name,
                "source_id": node.occurrence.as_str(),
                "source_file": source_file,
                "source_line": metadata.start_line.saturating_add(1),
                "tests": test_callers,
            }));
        }
    }

    let mut test_file_list: Vec<String> = all_test_files.into_iter().collect();
    test_file_list.sort();

    let output = json!({
        "covered_symbols": coverage_map.len(),
        "uncovered_symbols": uncovered.len(),
        "test_files": test_file_list,
        "coverage": coverage_map,
        "uncovered": uncovered,
    });

    let touched_files = source_nodes
        .iter()
        .map(crate::graph::health::test_risk::verified_test_symbol_parts)
        .collect::<Result<Vec<_>>>()?;
    let touched_files = unique_file_paths(touched_files.into_iter().map(|(_, file)| file));
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}

fn test_map_unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("verified-test-evidence-unavailable", false, detail)
}

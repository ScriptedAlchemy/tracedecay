//! `tracedecay_test_risk` and `tracedecay_test_map`.

use super::*;

/// Handles `tracedecay_test_risk` tool calls.
pub(crate) async fn handle_test_risk(
    cg: &TraceDecay,
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

    let report =
        crate::graph::health::test_risk::analyze_test_risk(cg, path_prefix, include_tested, limit)
            .await?;
    let output = serde_json::to_value(report).map_err(|err| TraceDecayError::Config {
        message: format!("failed to serialize test risk report: {err}"),
    })?;

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_test_map` tool calls.
pub(crate) async fn handle_test_map(
    cg: &TraceDecay,
    args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let source_nodes = if let Some(file) = args.get("file").and_then(|v| v.as_str()) {
        cg.get_nodes_by_file(file).await?
    } else if let Some(node_id) = args
        .get("node_id")
        .or(args.get("id"))
        .and_then(|v| v.as_str())
    {
        cg.get_node(node_id).await?.into_iter().collect()
    } else {
        return Err(TraceDecayError::Config {
            message: "provide either 'file' or 'node_id'".to_string(),
        });
    };

    let mut coverage_map: Vec<Value> = Vec::new();
    let mut uncovered: Vec<Value> = Vec::new();
    let mut all_test_files: HashSet<String> = HashSet::new();

    for node in &source_nodes {
        if !node.kind.is_callable_kind() {
            continue;
        }

        let callers = cg.get_callers(&node.id, 3).await?;
        // Batch-check which callers have #[test] annotations (inline test modules).
        let caller_ids: Vec<String> = callers.iter().map(|(n, _)| n.id.clone()).collect();
        let test_annotated = cg.get_test_annotated_node_ids(&caller_ids).await?;
        let test_callers: Vec<Value> = callers
            .iter()
            .filter(|(n, _)| {
                crate::tracedecay::is_test_file(&n.file_path) || test_annotated.contains(&n.id)
            })
            .map(|(n, _)| {
                all_test_files.insert(n.file_path.clone());
                json!({
                    "test_name": n.name,
                    "test_file": n.file_path,
                    "test_line": n.start_line,
                })
            })
            .collect();

        if test_callers.is_empty() {
            uncovered.push(json!({
                "id": node.id,
                "name": node.name,
                "file": node.file_path,
                "line": node.start_line,
            }));
        } else {
            coverage_map.push(json!({
                "source_name": node.name,
                "source_id": node.id,
                "source_file": node.file_path,
                "source_line": node.start_line,
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

    let touched_files = unique_file_paths(source_nodes.iter().map(|n| n.file_path.as_str()));
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

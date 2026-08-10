//! `tracedecay_dead_code` — symbols with no incoming edges.

use super::*;

/// Handles `tracedecay_dead_code` tool calls.
pub(crate) async fn handle_dead_code(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let kinds: Vec<NodeKind> = args.get("kinds").and_then(|v| v.as_array()).map_or_else(
        || vec![NodeKind::Function, NodeKind::Method],
        |arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(NodeKind::from_str))
                .collect()
        },
    );

    let include_public = args
        .get("include_public")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(100, |value| value.clamp(1, 1_000) as usize);
    let dead = graph.find_dead_code(&kinds, include_public, limit).await?;
    let mut items = Vec::with_capacity(dead.len());
    let mut files = Vec::with_capacity(dead.len());
    for symbol in dead {
        let binding = symbol
            .binding
            .ok_or_else(|| TraceDecayError::ProjectRoute {
                reason_code: "verified-dead-code-evidence-incomplete".to_owned(),
                retryable: false,
                detail: "a dead-code candidate has no generation-pinned file binding".to_owned(),
            })?;
        let file = binding
            .logical_path
            .ok_or_else(|| TraceDecayError::ProjectRoute {
                reason_code: "verified-dead-code-evidence-incomplete".to_owned(),
                retryable: false,
                detail: "a dead-code candidate has no generation-pinned logical path".to_owned(),
            })?;
        let metadata = symbol
            .metadata
            .ok_or_else(|| TraceDecayError::ProjectRoute {
                reason_code: "verified-dead-code-evidence-incomplete".to_owned(),
                retryable: false,
                detail: "a dead-code candidate has no extraction-attested symbol metadata"
                    .to_owned(),
            })?;
        if !crate::path_scope::path_matches_scope(&file, scope_prefix) {
            continue;
        }
        files.push(file.clone());
        items.push(json!({
            "id": symbol.occurrence.as_str(),
            "name": metadata.simple_name,
            "kind": metadata.kind,
            "file": file,
            "line": metadata.start_line,
            "signature": metadata.signature,
        }));
    }
    let touched_files = unique_file_paths(files.iter().map(String::as_str));
    let output = json!({
        "dead_code_count": items.len(),
        "symbols": items,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}

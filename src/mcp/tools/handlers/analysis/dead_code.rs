//! `tracedecay_dead_code` — symbols with no incoming edges.

use super::*;

/// Handles `tracedecay_dead_code` tool calls.
pub(crate) async fn handle_dead_code(
    cg: &TraceDecay,
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
    let dead = cg
        .find_dead_code_bounded(&kinds, include_public, limit)
        .await?;
    let dead = filter_by_scope(dead, scope_prefix, |n| &n.file_path);

    let touched_files = unique_file_paths(dead.iter().map(|n| n.file_path.as_str()));

    let items: Vec<Value> = dead
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "name": n.name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "line": n.start_line,
                "signature": n.signature,
            })
        })
        .collect();

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

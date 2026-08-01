//! `tracedecay_hotspots` — churn-weighted complexity ranking.

use super::*;

/// Handles `tracedecay_hotspots` tool calls.
pub(crate) async fn handle_hotspots(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    require_positive_limit(limit, "tracedecay_hotspots")?;

    let hotspots = cg.get_hotspot_nodes(scope_prefix, limit).await?;
    let mut items: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for (node, incoming, outgoing) in hotspots {
        touched.push(node.file_path.clone());
        items.push(json!({
            "id": node.id,
            "name": node.name,
            "kind": node.kind.as_str(),
            "file": node.file_path,
            "line": node.start_line,
            "incoming": incoming,
            "outgoing": outgoing,
            "total": incoming + outgoing,
        }));
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    let output = json!({
        "hotspot_count": items.len(),
        "hotspots": items,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}

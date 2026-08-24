use super::daemon::daemon_tool_json;

pub(crate) async fn handle_bench(
    queries: Option<String>,
    json: bool,
    path: Option<String>,
    max_nodes: usize,
) -> tracedecay::errors::Result<()> {
    let resolved =
        super::scope::resolve_project_scope(tracedecay::config::resolve_path(path)).await?;
    let queries_toml = queries
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to read query file: {error}"),
        })?;
    let result = daemon_tool_json(
        Some(&resolved.project_path),
        "tracedecay_admin_project",
        serde_json::json!({
            "action": "bench",
            "queries_toml": queries_toml,
            "json": json,
            "max_nodes": max_nodes,
        }),
    )
    .await?;
    let output = result
        .get("output")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "daemon bench response omitted output".to_string(),
        })?;
    print!("{output}");
    Ok(())
}

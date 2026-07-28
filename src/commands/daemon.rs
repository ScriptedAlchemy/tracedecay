pub(crate) async fn daemon_tool_json(
    project_path: Option<&std::path::Path>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> tracedecay::errors::Result<serde_json::Value> {
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        project_path.map(std::path::Path::to_path_buf),
        None,
        false,
        false,
    )?;
    let result = tracedecay::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
    let payload = tracedecay::daemon::tool_json_payload(&result, tool_name)?;
    if payload
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Ok(payload);
    }
    let handle = payload
        .get("handle")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "daemon tool {tool_name} returned truncated JSON without a retrieval handle"
            ),
        })?;
    let retrieved = tracedecay::daemon::call_default_tool(
        &handshake,
        "tracedecay_retrieve",
        serde_json::json!({ "handle": handle, "format": "json" }),
    )
    .await?;
    let retrieved = tracedecay::daemon::tool_json_payload(&retrieved, "tracedecay_retrieve")?;
    let content = retrieved
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon retrieval for {tool_name} omitted response content"),
        })?;
    serde_json::from_str(content).map_err(Into::into)
}

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
    let blocks = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no content blocks"),
        })?;
    for text in blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
    {
        if let Ok(value) = serde_json::from_str(text) {
            return Ok(value);
        }
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!("daemon tool {tool_name} returned no JSON payload"),
    })
}

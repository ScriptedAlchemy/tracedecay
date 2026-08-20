use tokio::time::Instant;

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
    recover_truncated_payload(&handshake, tool_name, result, None).await
}

/// Deadline-carrying variant for CLI journeys that deliberately trigger a cold
/// project open and wait it out (`tracedecay status` after a daemon restart).
/// The caller's wall-clock deadline bounds the open wait and the truncation
/// recovery fetch, so the command cannot outlive its own budget on private
/// retry clocks.
pub(crate) async fn daemon_tool_json_until(
    deadline: Instant,
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
    let result = tracedecay::daemon::call_default_tool_awaiting_project_open(
        &handshake, tool_name, arguments, deadline,
    )
    .await?;
    recover_truncated_payload(&handshake, tool_name, result, Some(deadline)).await
}

async fn recover_truncated_payload(
    handshake: &tracedecay::daemon::DaemonHandshake,
    tool_name: &str,
    result: serde_json::Value,
    deadline: Option<Instant>,
) -> tracedecay::errors::Result<serde_json::Value> {
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
    let arguments = serde_json::json!({ "handle": handle, "format": "json" });
    let retrieved = match deadline {
        Some(deadline) => {
            tracedecay::daemon::call_default_tool_awaiting_project_open(
                handshake,
                "tracedecay_retrieve",
                arguments,
                deadline,
            )
            .await?
        }
        None => {
            tracedecay::daemon::call_default_tool(handshake, "tracedecay_retrieve", arguments)
                .await?
        }
    };
    let retrieved = tracedecay::daemon::tool_json_payload(&retrieved, "tracedecay_retrieve")?;
    let content = retrieved
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon retrieval for {tool_name} omitted response content"),
        })?;
    serde_json::from_str(content).map_err(Into::into)
}

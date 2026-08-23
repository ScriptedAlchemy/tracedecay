use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

/// Parse a positive millisecond duration from `name`, falling back to
/// `default`. Values above `max` fail closed so CLI budgets cannot exceed
/// the supported monotonic range.
pub(crate) fn env_duration_ms(
    name: &str,
    default: Duration,
    max: Duration,
) -> tracedecay::errors::Result<Duration> {
    let deadline = std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(default);
    if deadline > max {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("{name} exceeds the supported monotonic deadline range"),
        });
    }
    Ok(deadline)
}

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

pub(crate) fn is_truncation_envelope(value: &Value) -> bool {
    value.get("truncated").and_then(Value::as_bool) == Some(true)
        && value
            .get("original_chars")
            .and_then(Value::as_u64)
            .is_some()
        && value.get("preview").and_then(Value::as_str).is_some()
}

pub(crate) fn reject_truncation_envelope(
    value: &Value,
    tool_name: &str,
) -> tracedecay::errors::Result<()> {
    if !is_truncation_envelope(value) {
        return Ok(());
    }
    let original_chars = value.get("original_chars").and_then(Value::as_u64);
    let handle = value.get("handle").and_then(Value::as_str);
    let message = match (original_chars, handle) {
        (Some(chars), Some(handle)) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars); \
             recover with tracedecay_retrieve handle={handle}"
        ),
        (Some(chars), None) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars) \
             without a retrieval handle"
        ),
        _ => format!("daemon tool {tool_name} returned truncated JSON"),
    };
    Err(tracedecay::errors::TraceDecayError::Config { message })
}

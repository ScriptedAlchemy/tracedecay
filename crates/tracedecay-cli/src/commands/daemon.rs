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
) -> tracedecay_runtime_core::errors::Result<Duration> {
    let deadline = std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(default);
    if deadline > max {
        return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!("{name} exceeds the supported monotonic deadline range"),
        });
    }
    Ok(deadline)
}

/// Resolves the daemon handshake for the current client. One labeled
/// boundary so a slow CLI invocation can attribute time to client identity
/// resolution separately from the daemon round-trip itself.
#[hotpath::measure(label = "cli.daemon.handshake")]
fn client_handshake(
    project_path: Option<&std::path::Path>,
) -> tracedecay_runtime_core::errors::Result<tracedecay_daemon_protocol::DaemonHandshake> {
    tracedecay::daemon::handshake_for_current_client(
        project_path.map(std::path::Path::to_path_buf),
        None,
        false,
        false,
    )
}

/// One-shot daemon tool call using the shared `TRACEDECAY_TOOL_DEADLINE_MS`
/// envelope (default 120s) via `tracedecay::daemon::call_default_tool`.
pub(crate) async fn daemon_tool_json(
    project_path: Option<&std::path::Path>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> tracedecay_runtime_core::errors::Result<serde_json::Value> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.daemon.tool").set(&tool_name);
    let handshake = client_handshake(project_path)?;
    let result = hotpath::future!(
        tracedecay::daemon::call_default_tool(&handshake, tool_name, arguments),
        label = "cli.daemon.request"
    )
    .await?;
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
) -> tracedecay_runtime_core::errors::Result<serde_json::Value> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.daemon.tool").set(&tool_name);
    let handshake = client_handshake(project_path)?;
    // Distinct from `cli.daemon.request`: this lifetime includes waiting out a
    // cold project open, so aggregating the two would conflate daemon latency
    // with deliberate open waits.
    let result = hotpath::future!(
        tracedecay::daemon::call_default_tool_awaiting_project_open(
            &handshake, tool_name, arguments, deadline,
        ),
        label = "cli.daemon.request_open_wait"
    )
    .await?;
    recover_truncated_payload(&handshake, tool_name, result, Some(deadline)).await
}

async fn recover_truncated_payload(
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
    tool_name: &str,
    result: serde_json::Value,
    deadline: Option<Instant>,
) -> tracedecay_runtime_core::errors::Result<serde_json::Value> {
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
        .ok_or_else(
            || tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!(
                    "daemon tool {tool_name} returned truncated JSON without a retrieval handle"
                ),
            },
        )?;
    let arguments = serde_json::json!({ "handle": handle, "format": "json" });
    let retrieved = match deadline {
        Some(deadline) => {
            hotpath::future!(
                tracedecay::daemon::call_default_tool_awaiting_project_open(
                    handshake,
                    "tracedecay_retrieve",
                    arguments,
                    deadline,
                ),
                label = "cli.daemon.recovery_fetch"
            )
            .await?
        }
        None => {
            hotpath::future!(
                tracedecay::daemon::call_default_tool(handshake, "tracedecay_retrieve", arguments),
                label = "cli.daemon.recovery_fetch"
            )
            .await?
        }
    };
    let retrieved = tracedecay::daemon::tool_json_payload(&retrieved, "tracedecay_retrieve")?;
    let content = retrieved
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(
            || tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!("daemon retrieval for {tool_name} omitted response content"),
            },
        )?;
    serde_json::from_str(content).map_err(Into::into)
}

/// Recover a truncated MCP tool result while keeping the MCP envelope shape
/// `tracedecay tool` prints. Status unwraps to the inner JSON; this path must
/// leave `content[*].text` as the recovered payload so `--format json` and
/// `--json` callers still parse the tool schema rather than a handle envelope.
pub(crate) async fn recover_truncated_mcp_result(
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
    tool_name: &str,
    result: serde_json::Value,
    deadline: Option<Instant>,
) -> tracedecay_runtime_core::errors::Result<serde_json::Value> {
    let Ok(payload) = tracedecay::daemon::tool_json_payload(&result, tool_name) else {
        return Ok(result);
    };
    if payload
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Ok(result);
    }
    let recovered =
        recover_truncated_payload(handshake, tool_name, result.clone(), deadline).await?;
    let text = serde_json::to_string(&recovered)?;
    let mut recovered_result = result;
    let blocks = recovered_result
        .get_mut("content")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(
            || tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!("daemon tool {tool_name} returned no content blocks"),
            },
        )?;
    let mut replaced = false;
    for block in blocks {
        let Some(block_text) = block.get("text").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(block_payload) = serde_json::from_str::<serde_json::Value>(block_text) else {
            continue;
        };
        if is_truncation_envelope(&block_payload) {
            block["text"] = serde_json::Value::String(text.clone());
            replaced = true;
            break;
        }
    }
    if !replaced {
        return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} omitted its truncation payload"),
        });
    }
    Ok(recovered_result)
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
) -> tracedecay_runtime_core::errors::Result<()> {
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
    Err(tracedecay_runtime_core::errors::TraceDecayError::Config { message })
}

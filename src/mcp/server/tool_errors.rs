//! Semantic tool-failure classification and JSON-RPC error-response mapping.

use serde_json::{Value, json};

use crate::errors::TraceDecayError;
use crate::mcp::response_handles::RESPONSE_RETRIEVE_TOOL;
use crate::mcp::tools::ToolResult;
use crate::mcp::transport::{ErrorCode, JsonRpcResponse};

fn plain_text_tool_failure(text: &str) -> bool {
    text.starts_with("git error:") || text.starts_with("git diff failed:")
}

fn value_has_semantic_error(value: &Value) -> bool {
    value
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content.iter().any(|item| {
                let Some(text) = item.get("text").and_then(Value::as_str) else {
                    return false;
                };
                let trimmed = text.trim_start();
                if plain_text_tool_failure(trimmed) {
                    return true;
                }
                if !trimmed.starts_with('{') {
                    return false;
                }
                let Ok(payload) = serde_json::from_str::<Value>(trimmed) else {
                    return false;
                };
                payload.get("success").and_then(Value::as_bool) == Some(false)
                    || payload.get("error").is_some_and(|error| !error.is_null())
                    || payload
                        .get("failed")
                        .and_then(Value::as_u64)
                        .is_some_and(|failed| failed > 0)
                    || payload
                        .get("exit_code")
                        .is_some_and(|code| !code.is_null() && code.as_i64() != Some(0))
            })
        })
}

/// Whether an MCP tool result should be classified as a semantic failure for
/// analytics/`isError` purposes.
///
/// Handlers that build results structurally (e.g. edit tools, whose result
/// struct carries a `success: bool`) call
/// [`crate::mcp::tools::ToolResult::with_semantic_error`] to record the
/// outcome directly — that marker is authoritative and wins over the
/// rendered text. Handlers that have not been migrated to set the marker
/// leave it `None`, and this falls back to the pre-existing text-based
/// heuristic (`value_has_semantic_error`) that sniffs the rendered response
/// text for JSON failure shapes or known plain-text failure prefixes.
pub(crate) fn tool_result_has_semantic_error(result: &ToolResult) -> bool {
    result
        .semantic_error()
        .unwrap_or_else(|| value_has_semantic_error(&result.value))
}

/// Reason to record for a semantically-failed tool result, for the
/// `failure_reason` analytics field. Prefers the handler-supplied structural
/// [`ToolResult::failure_message`] (e.g. an edit result's `message`, such as
/// "`old_str` not found"); falls back to the rendered response's first text
/// block for handlers that only signal failure via `value_has_semantic_error`
/// text heuristics. Callers must only invoke this once the result is already
/// known to be a semantic failure — it does not itself re-check that.
pub(crate) fn semantic_failure_reason(result: &ToolResult) -> Option<String> {
    if let Some(message) = result.failure_message() {
        return Some(message.to_string());
    }
    result
        .value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| {
            content
                .iter()
                .find_map(|item| item.get("text").and_then(Value::as_str))
        })
        .map(|text| text.trim_start().to_string())
}

pub(crate) fn mark_semantic_tool_error(result: &mut ToolResult) {
    if !tool_result_has_semantic_error(result) {
        return;
    }
    if let Some(obj) = result.value.as_object_mut() {
        obj.insert("isError".to_string(), json!(true));
    }
}

/// Canonical wire problem kind for reason codes minted inside this crate's
/// MCP dispatch and application-surface layers. The boundary owns this
/// translation so clients (and the catalog sweep) can read a truthful
/// `kind` alongside the machine `code` instead of inferring from prose.
fn project_route_problem_kind(reason_code: &str) -> Option<&'static str> {
    match reason_code {
        "tool_dispatch_deadline_exceeded" => Some("deadline_exceeded"),
        "tool_dispatch_cancelled" => Some("cancelled"),
        // The client stopped awaiting an admitted effect worker. Neither
        // cancellation nor a timeout may claim the effect did not happen, so
        // this is its own terminal rather than a flavour of the two above.
        "tool_dispatch_effect_unknown" => Some("effect_unknown"),
        "tool_dispatch_shutdown"
        | "mcp_dispatch_effect_journey_unverified"
        | "application_surface_unavailable" => Some("unavailable"),
        "application_surface_not_found_or_not_authorized" => Some("denied"),
        _ => None,
    }
}

/// Map response-handle failures onto actionable JSON-RPC errors at the MCP
/// boundary so clients can distinguish bad input from cache/runtime problems.
pub(crate) fn tool_error_response(
    id: Value,
    tool_name: &str,
    error: &TraceDecayError,
) -> JsonRpcResponse {
    if let Some((reason_code, retryable, detail)) = error.project_route_context() {
        let code = if retryable {
            ErrorCode::InternalError
        } else {
            ErrorCode::InvalidParams
        };
        let mut data = json!({
            "tool": tool_name,
            "reason_code": reason_code,
            "retryable": retryable,
            "detail": detail,
        });
        if let (Some(kind), Some(object)) = (
            project_route_problem_kind(reason_code),
            data.as_object_mut(),
        ) {
            object.insert("kind".to_string(), json!(kind));
            object.insert("code".to_string(), json!(reason_code));
        }
        return JsonRpcResponse::error_with_data(
            id,
            code,
            format!("tool project route failed: {detail}"),
            Some(data),
        );
    }
    if tool_name == "tracedecay_hook_runtime"
        && let Some(data) = crate::mcp::tools::structured_hook_error_data(error)
    {
        let detail = data
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("Claude observation ingest failed");
        return JsonRpcResponse::error_with_data(
            id,
            ErrorCode::InternalError,
            format!("tool execution failed: {detail}"),
            Some(data),
        );
    }
    if tool_name == RESPONSE_RETRIEVE_TOOL {
        match error {
            TraceDecayError::Config { message }
                if message.starts_with("missing required parameter: handle") =>
            {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InvalidParams,
                    "tracedecay_retrieve requires the `handle` argument copied from a truncated MCP response envelope."
                        .to_string(),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "missing_handle_argument",
                        "retryable": false,
                        "retry_instruction": "Call `tracedecay_retrieve` again with the exact `handle` value emitted by the truncated response envelope."
                    })),
                );
            }
            TraceDecayError::Config { message }
                if message.starts_with("invalid response handle") =>
            {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InvalidParams,
                    message.clone(),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "invalid_handle",
                        "retryable": false,
                        "retry_instruction": "Pass the exact `handle` string from a truncated MCP response envelope; do not shorten or edit it."
                    })),
                );
            }
            TraceDecayError::File { message, .. }
                if message.starts_with("corrupt response-handle record") =>
            {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InternalError,
                    "tool execution failed: cached response handle record is unreadable."
                        .to_string(),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "corrupt_handle_record",
                        "retryable": true,
                        "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle."
                    })),
                );
            }
            TraceDecayError::File { .. } => {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InternalError,
                    "tool execution failed: failed to read cached response handle.".to_string(),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "handle_read_failed",
                        "retryable": true,
                        "retry_instruction": "Fix the local project cache/filesystem issue, then re-run the original MCP tool to regenerate the full response and a fresh handle."
                    })),
                );
            }
            _ => {}
        }
    }
    if let TraceDecayError::ProjectRoute {
        reason_code,
        retryable: false,
        detail,
    } = error
    {
        return JsonRpcResponse::error_with_data(
            id,
            ErrorCode::InvalidParams,
            detail.clone(),
            Some(json!({
                "tool": tool_name,
                "reason_code": reason_code,
                "retryable": false,
                "detail": detail,
            })),
        );
    }
    if let TraceDecayError::Config { message } = error {
        // Handler-authored argument and lookup failures follow two message
        // conventions; surface them as typed invalid-params data instead of
        // an untyped internal error.
        let reason_code = if message.starts_with("missing required parameter") {
            Some("missing_required_parameter")
        } else if message.contains("not found") {
            Some("not_found")
        } else {
            None
        };
        if let Some(reason_code) = reason_code {
            return JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InvalidParams,
                message.clone(),
                Some(json!({
                    "tool": tool_name,
                    "reason_code": reason_code,
                    "retryable": false,
                    "detail": message,
                })),
            );
        }
    }

    let cli_name = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
    JsonRpcResponse::error_with_data(
        id,
        ErrorCode::InternalError,
        format!("tool execution failed: {error}"),
        Some(json!({
            "tool": tool_name,
            "cli_fallback": format!(
                "This tool is also available from the shell: `tracedecay tool {cli_name} ...` \
                 (`tracedecay tool {cli_name} --help` for parameters). If MCP calls keep \
                 failing or timing out, fall back to that CLI instead of querying \
                 .tracedecay databases directly."
            ),
        })),
    )
}

fn hardcoded_internal_error_response(id: &Value, detail: &str) -> String {
    let id_json = serde_json::to_string(id).unwrap_or_else(|_| "null".to_string());
    let detail_json = serde_json::to_string(detail)
        .unwrap_or_else(|_| "\"response serialization failed\"".to_string());
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id_json},\"error\":{{\"code\":-32603,\"message\":\"failed to serialize JSON-RPC response\",\"data\":{{\"reason_code\":\"response_serialization_failed\",\"detail\":{detail_json}}}}}}}"
    )
}

pub(crate) fn serialize_response_line(resp: &JsonRpcResponse) -> String {
    match serde_json::to_string(resp) {
        Ok(line) => line,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize JSON-RPC response");
            let fallback = JsonRpcResponse::error_with_data(
                resp.id.clone(),
                ErrorCode::InternalError,
                "failed to serialize JSON-RPC response".to_string(),
                Some(json!({
                    "reason_code": "response_serialization_failed",
                    "detail": e.to_string(),
                })),
            );
            serde_json::to_string(&fallback).unwrap_or_else(|fallback_err| {
                hardcoded_internal_error_response(&resp.id, &fallback_err.to_string())
            })
        }
    }
}

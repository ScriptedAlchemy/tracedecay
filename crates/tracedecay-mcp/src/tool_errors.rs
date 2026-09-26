//! Semantic tool-failure classification and JSON-RPC error-response mapping.

use serde_json::{Value, json};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_sessions::admission::HostAdmissionStatus;

use crate::response_handles::RESPONSE_RETRIEVE_TOOL;
use crate::tools::ToolResult;
use crate::transport::{ErrorCode, JsonRpcResponse};

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

/// Projects a hook-runtime error onto the structured JSON-RPC data object.
///
/// The status is whatever the admission authority reported, carried through
/// the error rather than re-derived here. Failures raised without an
/// authority behind them report the application-level default.
#[must_use]
pub fn structured_hook_error_data(error: &TraceDecayError) -> Option<Value> {
    let (reason_code, retryable, detail) = error.hook_runtime_context()?;
    let status = error
        .hook_runtime_status()
        .and_then(HostAdmissionStatus::from_wire)
        .unwrap_or(HostAdmissionStatus::Degraded);
    Some(json!({
        "tool": "tracedecay_hook_runtime",
        "status": status,
        "reason_code": reason_code,
        "retryable": retryable,
        "detail": detail,
    }))
}

/// Whether an MCP tool result should be classified as a semantic failure for
/// analytics/`isError` purposes.
///
/// Handlers that build results structurally (e.g. edit tools, whose result
/// struct carries a `success: bool`) call
/// [`ToolResult::with_semantic_error`] to record the outcome directly, that
/// marker is authoritative and wins over the rendered text. Handlers that
/// have not been migrated to set the marker leave it `None`, and this falls
/// back to the pre-existing text-based heuristic (`value_has_semantic_error`)
/// that sniffs the rendered response text for JSON failure shapes or known
/// plain-text failure prefixes.
#[must_use]
pub fn tool_result_has_semantic_error(result: &ToolResult) -> bool {
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
/// known to be a semantic failure, it does not itself re-check that.
#[must_use]
pub fn semantic_failure_reason(result: &ToolResult) -> Option<String> {
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

pub fn mark_semantic_tool_error(result: &mut ToolResult) {
    if !tool_result_has_semantic_error(result) {
        return;
    }
    if let Some(obj) = result.value.as_object_mut() {
        obj.insert("isError".to_string(), json!(true));
    }
}

/// Canonical wire problem kind for reason codes minted inside MCP dispatch
/// and application-surface layers. The boundary owns this translation so
/// clients (and the catalog sweep) can read a truthful `kind` alongside the
/// machine `code` instead of inferring from prose.
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
        "application_surface_invalid_request" => Some("invalid_request"),
        "application_surface_not_found_or_not_authorized" => Some("denied"),
        _ => None,
    }
}

/// Map response-handle failures onto actionable JSON-RPC errors at the MCP
/// boundary so clients can distinguish bad input from cache/runtime problems.
#[must_use]
pub fn tool_error_response(id: Value, tool_name: &str, error: &TraceDecayError) -> JsonRpcResponse {
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
            format!(
                "tool project route failed: reason_code={reason_code} retryable={retryable}: {detail}"
            ),
            Some(data),
        );
    }
    if tool_name == "tracedecay_hook_runtime"
        && let Some(data) = structured_hook_error_data(error)
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
    // A refused persisted shape is a terminal typed state with one legal
    // action (reset); it travels with its authority so the client can render
    // the exact reset command instead of an anonymous internal error.
    if let Some((authority, reason)) = reset_required_context(error) {
        let remedy = reset_required_remedy(&authority, None);
        return JsonRpcResponse::error_with_data(
            id,
            ErrorCode::InternalError,
            format!("{error}\n\n{remedy}"),
            Some(json!({
                "tool": tool_name,
                "kind": "reset_required",
                "retryable": false,
                "authority": authority,
                "reason": reason,
                "remedy": remedy,
            })),
        );
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

/// Authorities whose refused shape is one project's store. Everything else a
/// typed reset names is profile-scoped state.
const PROJECT_STORE_AUTHORITIES: [&str; 2] = ["project store", "graph store"];

fn is_project_store_authority(authority: &str) -> bool {
    PROJECT_STORE_AUTHORITIES.contains(&authority)
}

/// Authorities whose refused shape was written into an agent host's files.
/// No profile reset reaches it; its reset deletes exactly the block or
/// package the refusal reason names.
const HOST_ARTIFACT_AUTHORITIES: [&str; 2] =
    ["managed skill prompt index", "materialized skill package"];

fn is_host_artifact_authority(authority: &str) -> bool {
    HOST_ARTIFACT_AUTHORITIES.contains(&authority)
}

fn project_root_argument(project_root: Option<&std::path::Path>) -> String {
    project_root.map_or_else(
        || "<project-root>".to_string(),
        |root| shell_words::quote(&root.to_string_lossy()).into_owned(),
    )
}

/// The one exact command that performs the legal `reset` action for a typed
/// reset refusal. Refused shapes are never migrated or backed up: the reset
/// deletes the authority's old data and the next open creates the shape this
/// binary writes. `project_root` scopes a project-store reset to the refused
/// project when the caller knows it. Single line, so it fits a bounded
/// problem diagnostic.
pub fn reset_required_command(authority: &str, project_root: Option<&std::path::Path>) -> String {
    if is_project_store_authority(authority) {
        format!(
            "tracedecay storage reset-project-store --project-root {} --yes",
            project_root_argument(project_root)
        )
    } else if is_host_artifact_authority(authority) {
        "delete the block or package directory named in the refusal".to_string()
    } else {
        "tracedecay wipe --all --yes".to_string()
    }
}

/// Operator rendering of [`reset_required_command`]: the refused authority,
/// what the reset deletes, the command, and the re-initialization that
/// follows. `tracedecay update` performs the profile reset itself after
/// refreshing the binary and daemon.
pub fn reset_required_remedy(authority: &str, project_root: Option<&std::path::Path>) -> String {
    let command = reset_required_command(authority, project_root);
    if is_project_store_authority(authority) {
        return format!(
            "refused authority: {authority}\n\
             this binary does not open or migrate that shape; reset it (its old data is \
             deleted, nothing is backed up):\n  \
             {command}\n\
             then re-run `tracedecay init {}`",
            project_root_argument(project_root)
        );
    }
    if is_host_artifact_authority(authority) {
        return format!(
            "refused authority: {authority}\n\
             this binary does not adopt or migrate that host file; reset it (its old \
             content is deleted, nothing is backed up):\n  \
             {command}\n\
             the next managed-skill export writes the current shape"
        );
    }
    format!(
        "refused authority: {authority}\n\
         this binary does not open or migrate that shape; reset it (its old data is deleted, \
         nothing is backed up):\n  \
         tracedecay update              refreshes the binary and daemon, then resets every \
         refused profile authority\n  \
         {command}    resets the complete profile database state now\n\
         then re-run `tracedecay init <project-root>` for each project"
    )
}

/// The refused authority and its reason for both typed reset states: the
/// generic persisted-shape refusal and the LCM profile schema refusal.
fn reset_required_context(error: &TraceDecayError) -> Option<(String, String)> {
    match error {
        TraceDecayError::ResetRequired { authority, reason } => {
            Some((authority.clone(), reason.clone()))
        }
        TraceDecayError::ProfileResetRequired { component, .. } => {
            Some((format!("{component} profile schema"), error.to_string()))
        }
        _ => None,
    }
}

fn hardcoded_internal_error_response(id: &Value, detail: &str) -> String {
    let id_json = serde_json::to_string(id).unwrap_or_else(|_| "null".to_string());
    let detail_json = serde_json::to_string(detail)
        .unwrap_or_else(|_| "\"response serialization failed\"".to_string());
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id_json},\"error\":{{\"code\":-32603,\"message\":\"failed to serialize JSON-RPC response\",\"data\":{{\"reason_code\":\"response_serialization_failed\",\"detail\":{detail_json}}}}}}}"
    )
}

#[must_use]
pub fn serialize_response_line(resp: &JsonRpcResponse) -> String {
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::errors::TraceDecayError;

    use super::tool_error_response;

    #[test]
    fn reset_required_travels_with_its_authority_and_reason() {
        let response = tool_error_response(
            json!(3),
            "tracedecay_project_list",
            &TraceDecayError::reset_required(
                "session temporal",
                "persisted session temporal schema is the published v3 shape",
            ),
        );
        let wire = serde_json::to_value(response).expect("JSON-RPC wire response");

        assert_eq!(wire["error"]["data"]["kind"], "reset_required");
        assert_eq!(wire["error"]["data"]["retryable"], false);
        assert_eq!(wire["error"]["data"]["authority"], "session temporal");
        assert_eq!(
            wire["error"]["data"]["reason"],
            "persisted session temporal schema is the published v3 shape"
        );
        let remedy = wire["error"]["data"]["remedy"]
            .as_str()
            .expect("the refusal names its reset command");
        assert!(remedy.contains("tracedecay wipe --all --yes"), "{remedy}");
        assert!(
            wire["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("tracedecay wipe --all --yes")),
            "the human-readable message must carry the reset command too: {wire}"
        );
        assert!(
            wire["error"]["data"].get("cli_fallback").is_none(),
            "a refused shape is not a transport failure to retry from the shell"
        );
    }

    #[test]
    fn reset_remedy_scopes_project_stores_and_quotes_their_roots() {
        let project = super::reset_required_remedy(
            "project store",
            Some(std::path::Path::new("/repo/it's an example")),
        );
        let command = project
            .split_once("\n  tracedecay storage")
            .map(|(_, tail)| format!("tracedecay storage{}", tail.split_once('\n').unwrap().0))
            .expect("project-store reset command");
        assert_eq!(
            shell_words::split(&command).unwrap(),
            [
                "tracedecay",
                "storage",
                "reset-project-store",
                "--project-root",
                "/repo/it's an example",
                "--yes",
            ]
        );
        assert!(!project.contains("wipe --all"), "{project}");

        let profile = super::reset_required_remedy("session temporal", None);
        assert!(profile.contains("refused authority: session temporal"));
        assert!(profile.contains("\n  tracedecay update "), "{profile}");
        assert!(
            profile.contains("\n  tracedecay wipe --all --yes"),
            "{profile}"
        );
        assert!(!profile.contains("reset-project-store"), "{profile}");

        let host = super::reset_required_remedy("managed skill prompt index", None);
        assert!(host.contains("refused authority: managed skill prompt index"));
        assert!(
            host.contains("\n  delete the block or package directory named in the refusal\n"),
            "{host}"
        );
        assert!(!host.contains("wipe --all"), "{host}");
    }

    #[test]
    fn application_surface_invalid_request_keeps_its_typed_wire_kind() {
        let response = tool_error_response(
            json!(7),
            "tracedecay_configuration_get",
            &TraceDecayError::project_route(
                "application_surface_invalid_request",
                false,
                "configuration request rejected by the application surface",
            ),
        );
        let wire = serde_json::to_value(response).expect("JSON-RPC wire response");

        assert_eq!(wire["error"]["code"], -32602);
        assert_eq!(wire["error"]["data"]["kind"], "invalid_request");
        assert_eq!(
            wire["error"]["data"]["code"],
            "application_surface_invalid_request"
        );
    }
}

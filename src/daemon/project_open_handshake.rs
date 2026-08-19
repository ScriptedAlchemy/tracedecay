//! Opening a project for one handshake, and how open failures are reported.
//!
//! Classifies the failure modes that first-touch bootstrap may repair -
//! never-enrolled identity, a missing index, a read-only store - apart from
//! genuine conflicts, and renders the client-visible refusal.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

pub(super) async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<crate::tracedecay::TraceDecay> {
    let open_options = handshake.open_options();
    let registry_database = store_administration.registered_profile_database().await?;
    let (store_layout, first_touch) =
        match crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_path,
            &open_options,
            registry_database.as_ref(),
        )
        .await
        {
            Ok(layout) => (layout, false),
            // A brand-new project has no enrollment marker or registry match,
            // so identity resolution fails closed. When the client
            // explicitly asked to initialize (first-touch `tracedecay init`),
            // mint a fresh path-derived identity and let the missing-index
            // fallback below bootstrap it.
            Err(err) if handshake.allow_init && is_unregistered_identity_error(&err) => (
                crate::tracedecay::TraceDecay::resolve_first_touch_configuration_layout_with_adoption(
                    project_path,
                    &open_options,
                    registry_database.as_ref(),
                    &handshake.moved_store_adoption,
                )
                .await?,
                true,
            ),
            Err(err) if is_unregistered_identity_error(&err) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "no TraceDecay index found at '{}'; run 'tracedecay init' first",
                        project_path.display()
                    ),
                });
            }
            Err(err) => return Err(err),
        };
    let project_id =
        store_layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "registered project open requires an authoritative project identity"
                    .to_owned(),
            })?;
    // First-touch enrollment: persist the minted identity in the `.git/`
    // repository identity marker so a subsequent open resolves the same
    // identity before the registry row lands. A non-git root persists
    // nothing — its identity is deterministic from the canonical path and
    // the registry registration below is its durable home. TraceDecay never
    // creates files inside a project's working tree.
    if first_touch {
        crate::storage::write_repository_identity_marker(project_path, project_id)?;
    }
    let configuration_database = store_administration
        .registered_project_session_database(project_path, &store_layout)
        .await?;
    let runtime_registry = store_administration.registered_runtime_registry().await?;
    // The retired relational graph health/index lane is never spent on the
    // admission path. Opening establishes the exact registered configuration
    // and durable store authority; project composition schedules the maintained
    // bounded code-index owner after publication.
    let open_result = crate::tracedecay::TraceDecay::open_with_registered_configuration(
        project_path,
        open_options.clone(),
        store_layout.clone(),
        configuration_database.clone(),
        registry_database.clone(),
        Arc::clone(&runtime_registry),
    )
    .await;
    match open_result {
        Ok(cg) => Ok(cg),
        Err(open_err) if is_readonly_database_error(&open_err) => {
            match crate::tracedecay::TraceDecay::open_read_only_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
            {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    Ok(cg)
                }
                Err(_) => Err(open_err),
            }
        }
        Err(open_err) if handshake.allow_init && is_missing_index_error(&open_err) => {
            // First-touch bootstrap creates the final registered store and
            // exact configuration authority only. The bounded code-index
            // activation owner performs indexing after admission, so opening a
            // project never waits for a repository scan or rebuild.
            crate::tracedecay::TraceDecay::init_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
        }
        Err(open_err) => Err(open_err),
    }
}

/// Whether `err` is the specific fail-closed error raised when identity
/// resolution finds no enrollment marker or registry match for a project.
fn is_unregistered_identity_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains(
                "registered configuration layout requires an enrolled or registry-resolved project identity"
            )
    )
}

pub(super) fn is_missing_index_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains("no TraceDecay index found")
                || message.contains("no TraceDecay database found")
    )
}

fn is_readonly_database_error(err: &TraceDecayError) -> bool {
    if !err.is_database_error() {
        return false;
    }
    match err {
        TraceDecayError::Database { message, .. } => {
            message.to_ascii_lowercase().contains("readonly database")
        }
        _ => false,
    }
}

pub(super) async fn write_project_open_error(
    transport: &mut impl McpTransport,
    request_line: &str,
    connection_scope: &str,
    error: &TraceDecayError,
) -> Result<()> {
    let request = serde_json::from_str::<JsonRpcRequest>(request_line).ok();
    let response = request
        .as_ref()
        .and_then(|request| tool_call_open_refusal_response(request, connection_scope, error))
        .unwrap_or_else(|| {
            let id = request
                .and_then(|request| request.id)
                .unwrap_or(serde_json::Value::Null);
            project_open_error_response(id, error)
        });
    write_json_rpc_response(transport, &response).await
}

/// A `tools/call` refused at project open still answers on the MCP tool
/// surface when the refusal is an admitted application terminal.
///
/// Reset-required is the store's own typed answer for the exact operation the
/// caller named. Reporting it as a JSON-RPC internal error hid the one legal
/// action (`reset`) from MCP clients while CLI and HTTP callers of the same
/// operation received the canonical problem envelope. Non-application tools
/// and every other project-open failure keep the raw refusal shape.
fn tool_call_open_refusal_response(
    request: &JsonRpcRequest,
    connection_scope: &str,
    error: &TraceDecayError,
) -> Option<JsonRpcResponse> {
    if !matches!(classify_mcp_method(&request.method), McpMethod::ToolsCall) {
        return None;
    }
    let TraceDecayError::ResetRequired { authority, reason } = error else {
        return None;
    };
    let id = request.id.clone()?;
    let tool_name = request.params.as_ref()?.get("name")?.as_str()?;
    let request_id =
        crate::request_identity::mcp_connection_request_id(&id, connection_scope)?;
    let envelope = crate::application_surface::mcp_project_open_reset_refusal(
        tool_name, request_id, authority, reason,
    )?;
    let text = serde_json::to_string(&envelope).ok()?;
    let problem = serde_json::to_value(envelope.problem.as_ref()).ok()?;
    Some(JsonRpcResponse::success(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": true,
            "problem": problem,
        }),
    ))
}

pub(super) fn project_open_error_response(
    id: serde_json::Value,
    error: &TraceDecayError,
) -> JsonRpcResponse {
    match error {
        TraceDecayError::Config { message }
            if message.contains(PROJECT_OPEN_FAILURE_RETRY_HINT) =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_route_open_backoff",
                    "retryable": true,
                    "retry_after_ms": PROJECT_OPEN_FAILURE_RETRY_BACKOFF.as_millis() as u64,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project open task capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_open_task_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_TRACKED_PROJECT_OPEN_TASKS,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project server capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_server_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_CACHED_PROJECT_SERVERS,
                })),
            )
        }
        TraceDecayError::ResetRequired { authority, reason } => JsonRpcResponse::error_with_data(
            id,
            ErrorCode::InternalError,
            error.to_string(),
            Some(json!({
                "kind": "reset_required",
                "retryable": false,
                "authority": authority,
                "reason": reason,
            })),
        ),
        _ => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_required_tools_call_answers_with_the_canonical_problem_envelope() {
        let request: JsonRpcRequest = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "tracedecay_storage_status", "arguments": {} },
        }))
        .expect("canonical tools/call request");
        let error =
            TraceDecayError::reset_required("project store", "schema v26 is incompatible");

        let response = tool_call_open_refusal_response(&request, "connection.test", &error)
            .expect("an application tools/call refusal must answer on the tool surface");

        assert!(response.error.is_none(), "the refusal is a tool result");
        let result = response.result.expect("tool result payload");
        assert_eq!(result["isError"], serde_json::json!(true));
        assert_eq!(result["problem"]["kind"], "reset_required");
        assert_eq!(
            result["problem"]["legal_actions"],
            serde_json::json!(["reset"])
        );
        let text = result["content"][0]["text"]
            .as_str()
            .expect("rendered envelope text");
        let envelope: serde_json::Value =
            serde_json::from_str(text).expect("machine-readable envelope");
        assert_eq!(envelope["problem"]["kind"], "reset_required");
        assert_eq!(envelope["problem"]["legal_actions"], serde_json::json!(["reset"]));
        assert!(
            envelope["problem"]["diagnostic"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("schema v26 is incompatible")),
            "the refusal must carry the store's own reason: {envelope}"
        );
    }

    #[test]
    fn non_application_tools_and_other_failures_keep_the_raw_refusal_shape() {
        let reset = TraceDecayError::reset_required("project store", "incompatible shape");
        let unknown_tool: JsonRpcRequest = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "tracedecay_not_an_application_tool", "arguments": {} },
        }))
        .expect("tools/call request");
        assert!(
            tool_call_open_refusal_response(&unknown_tool, "connection.test", &reset).is_none(),
            "a tool without a mounted application binding keeps the raw refusal"
        );

        let initialize: JsonRpcRequest = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {},
        }))
        .expect("initialize request");
        assert!(
            tool_call_open_refusal_response(&initialize, "connection.test", &reset).is_none(),
            "protocol bootstrap requests keep the raw refusal"
        );

        let storage_status: JsonRpcRequest = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "tracedecay_storage_status", "arguments": {} },
        }))
        .expect("tools/call request");
        let config = TraceDecayError::Config {
            message: "unrelated open failure".to_owned(),
        };
        assert!(
            tool_call_open_refusal_response(&storage_status, "connection.test", &config).is_none(),
            "non-terminal open failures keep the raw refusal"
        );
    }

    #[test]
    fn reset_required_project_open_is_serialized_as_a_non_retryable_typed_failure() {
        let response = project_open_error_response(
            serde_json::json!(41),
            &TraceDecayError::reset_required("graph store", "schema v26 is incompatible"),
        );
        let data = response
            .error
            .expect("project-open refusal")
            .data
            .expect("typed reset-required data");

        assert_eq!(data["kind"], "reset_required");
        assert_eq!(data["retryable"], false);
        assert_eq!(data["authority"], "graph store");
        assert_eq!(data["reason"], "schema v26 is incompatible");
    }
}

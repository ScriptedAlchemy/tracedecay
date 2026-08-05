//! Projectless client handling: tool calls served without a mounted project
//! (user-scoped LCM, message search, dashboard, doctor, version).
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed. `use super::*` re-exposes every name the
//! parent `daemon` module had in scope so the moved code resolves unchanged.

use std::collections::VecDeque;
use std::sync::Arc;

use serde_json::json;

use crate::client_identity::DaemonClientIdentity;
use crate::errors::Result;
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};

use super::*;

pub(super) async fn serve_projectless_client(
    transport: &mut impl McpTransport,
    client_identity: &DaemonClientIdentity,
    lifecycle: &DaemonLifecycle,
    store_administration: &StoreAdministration,
) -> Result<()> {
    const MAX_PIPELINED_REQUESTS: usize = 64;
    let mut pending = VecDeque::new();
    let mut queued_cancellations = Vec::new();
    let mut request_sequence = 0_u64;
    loop {
        let line = match pending.pop_front() {
            Some(line) => Some(line),
            None => tokio::select! {
                result = read_line_handling_wire_oversized(transport) => result?,
                () = lifecycle.wait_for_draining() => break,
            },
        };
        let Some(line) = line else {
            break;
        };
        let Some(_activity) = lifecycle.try_enter() else {
            break;
        };
        let request = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => request,
            Err(error) => {
                write_json_rpc_response(
                    transport,
                    &JsonRpcResponse::error(
                        json!(null),
                        ErrorCode::ParseError,
                        format!("Parse error: {error}"),
                    ),
                )
                .await?;
                continue;
            }
        };
        request_sequence = request_sequence.checked_add(1).ok_or_else(|| {
            crate::errors::TraceDecayError::Config {
                message: "projectless cancellation sequence exhausted".to_owned(),
            }
        })?;
        let cancellation = if request.id.is_some() {
            Some(
                tracedecay_application::CancellationSignal::active(format!(
                    "cancellation.projectless.{request_sequence}"
                ))
                .map_err(|error| crate::errors::TraceDecayError::Config {
                    message: format!("projectless cancellation admission failed: {error}"),
                })?,
            )
        } else {
            None
        };
        if let Some(request_id) = request.id.as_ref()
            && let Some(index) = queued_cancellations
                .iter()
                .position(|queued| queued == request_id)
        {
            queued_cancellations.swap_remove(index);
            if let Some(cancellation) = cancellation.as_ref() {
                cancellation.cancel(tracedecay_application::clock::now_micros());
            }
        }
        let response = projectless_response(
            &request,
            client_identity,
            store_administration,
            cancellation.clone(),
        );
        tokio::pin!(response);
        let response = loop {
            tokio::select! {
                response = &mut response => break response,
                result = read_line_handling_wire_oversized(transport) => {
                    match result {
                        Ok(Some(line)) => {
                            if let Some(cancelled_id) = cancellation_request_id(&line) {
                                if request.id.as_ref() == Some(&cancelled_id) {
                                    if let Some(cancellation) = cancellation.as_ref() {
                                        cancellation.cancel(
                                            tracedecay_application::clock::now_micros(),
                                        );
                                    }
                                } else if pending_contains_request(&pending, &cancelled_id)
                                    && !queued_cancellations.contains(&cancelled_id)
                                {
                                    queued_cancellations.push(cancelled_id);
                                }
                            } else if pending.len() < MAX_PIPELINED_REQUESTS {
                                pending.push_back(line);
                            } else {
                                let response = projectless_overload_response(&line);
                                if let Some(response) = response {
                                    write_json_rpc_response(transport, &response).await?;
                                }
                            }
                        }
                        Ok(None) => {
                            if let Some(cancellation) = cancellation.as_ref() {
                                cancellation.cancel(tracedecay_application::clock::now_micros());
                            }
                            return Ok(());
                        }
                        Err(error) => {
                            if let Some(cancellation) = cancellation.as_ref() {
                                cancellation.cancel(tracedecay_application::clock::now_micros());
                            }
                            return Err(error);
                        }
                    }
                }
                () = lifecycle.wait_for_draining() => {
                    if let Some(cancellation) = cancellation.as_ref() {
                        cancellation.cancel(tracedecay_application::clock::now_micros());
                    }
                    return Ok(());
                }
            }
        };
        if let Some(response) = response {
            write_json_rpc_response(transport, &response).await?;
        }
        if !lifecycle.accepting() {
            break;
        }
    }
    Ok(())
}

async fn projectless_response(
    request: &crate::mcp::JsonRpcRequest,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Option<crate::mcp::JsonRpcResponse> {
    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {
                        "listChanged": true
                    }
                },
                "serverInfo": {
                    "name": "tracedecay",
                    "version": crate::version::build_version()
                }
            }),
        )),
        "tools/call" => Some(
            projectless_tools_call_response_with_cancellation(
                id,
                request.params.as_ref(),
                client_identity,
                store_administration,
                cancellation,
            )
            .await,
        ),
        "ping" | "logging/setLevel" => Some(JsonRpcResponse::success(id, json!({}))),
        _ => Some(JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("Method not found: {}", request.method),
        )),
    }
}

pub(super) async fn projectless_tools_call_response(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    projectless_tools_call_response_with_cancellation(
        id,
        params,
        client_identity,
        store_administration,
        None,
    )
    .await
}

async fn projectless_tools_call_response_with_cancellation(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> crate::mcp::JsonRpcResponse {
    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };
    if tool_name == "tracedecay_admin_project" {
        #[derive(serde::Deserialize)]
        #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
        enum ProjectlessAdminProjectAction {
            AutomationReconcile {
                scope: crate::dashboard::AutomationReconcileScope,
            },
        }

        let request = match serde_json::from_value::<ProjectlessAdminProjectAction>(arguments) {
            Ok(request) => request,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    ErrorCode::InvalidParams,
                    format!("invalid projectless tracedecay_admin_project arguments: {error}"),
                );
            }
        };
        let ProjectlessAdminProjectAction::AutomationReconcile { scope } = request;
        if scope != crate::dashboard::AutomationReconcileScope::Profile {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "project-scoped automation reconciliation requires a project path".to_string(),
            );
        }
        let outcomes = match store_administration
            .reconcile_cached_automation_for_profile(&client_identity.profile_root)
            .await
        {
            Ok(outcomes) => outcomes,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let report = crate::dashboard::ProfileAutomationReconcileReport {
            scope,
            cached_owners: outcomes.len(),
            outcomes,
            uncached_projects:
                crate::dashboard::UncachedProjectReconcileOutcome::DeferredUntilProjectStartup,
        };
        return JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string())
                }]
            }),
        );
    }
    if tool_name == "tracedecay_hook_runtime" {
        let global_db = match store_administration.registered_profile_database().await {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let session_runtime_registry =
            match store_administration.registered_runtime_registry().await {
                Ok(registry) => registry,
                Err(error) => {
                    return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
                }
            };
        let user_session_db = match store_administration
            .registered_profile_session_database()
            .await
        {
            Ok(database) => database,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let profile_identity = match store_administration.profile_identity() {
            Ok(identity) => identity,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let host_admission_state = match store_administration
            .host_admission_broker(&user_session_db)
            .await
        {
            Ok(state) => state,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let host_admission_broker = match &host_admission_state {
            branch_admin::HostAdmissionBrokerState::Available(broker) => Ok(broker),
            branch_admin::HostAdmissionBrokerState::Unavailable(outcome) => Err(*outcome),
        };
        let refresh_wake = store_administration
            .session_temporal_refresh_schedulers()
            .ensure_profile(
                user_session_db.db_path().to_path_buf(),
                Arc::clone(&user_session_db),
            )
            .await;
        return match crate::mcp::tools::handle_projectless_hook_runtime(
            arguments,
            &client_identity.profile_root,
            session_runtime_registry,
            global_db.as_ref(),
            crate::mcp::tools::SessionAuthorities::new(None, Some(&user_session_db))
                .with_profile_identity(Some(profile_identity))
                .with_registered_databases(None, Some(&user_session_db)),
            host_admission_broker,
        )
        .await
        {
            Ok(result) => {
                refresh_wake.wake();
                projectless_tool_result_response(id, result)
            }
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    if tool_name == "tracedecay_admin_cli" {
        let global_db = match store_administration.registered_profile_database().await {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let profile_identity = match store_administration.profile_identity() {
            Ok(identity) => identity,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let profile_sessions = match store_administration
            .registered_profile_session_database()
            .await
        {
            Ok(database) => database,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let accounting_authority = crate::global_db::global_accounting_enabled()
            .then(|| {
                crate::daemon::accounting_authority::DaemonAccountingAuthority::new(
                    crate::daemon::accounting_authority::DaemonAccountingOwners {
                        profile_id: profile_identity.profile_id().clone(),
                        accounting: Arc::clone(&global_db),
                        profile_root: profile_identity.profile_root().to_path_buf(),
                        transcript_source_home: daemon_transcript_source_home(
                            profile_identity.profile_root(),
                        ),
                        project_id: None,
                        project_root: None,
                        graph: None,
                        project_sessions: None,
                        profile_sessions: Some(Arc::clone(&profile_sessions)),
                    },
                )
            })
            .flatten();
        let request_id = crate::mcp::server::application_surface_request_id(
            &id,
            profile_identity.profile_id().as_str(),
        )
        .and_then(|request_id| tracedecay_application::RequestId::new(request_id).ok());
        let now = tracedecay_application::clock::now_micros();
        let deadline = tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
            now.0.saturating_add(600_000_000),
        ))
        .ok();
        return match crate::mcp::tools::handle_projectless_admin_cli(
            arguments,
            &global_db,
            crate::global_db::global_accounting_enabled().then_some(global_db.as_ref()),
            crate::mcp::tools::AccountingAdapterControls {
                authority: accounting_authority.as_ref().map(|authority| {
                    authority as &dyn tracedecay_application::AccountingAuthorityPort
                }),
                request_id,
                deadline,
                cancellation,
            },
            &client_identity.profile_root,
        )
        .await
        {
            Ok(result) => projectless_tool_result_response(id, result),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    if tool_name.starts_with("tracedecay_lcm_") || tool_name == "tracedecay_message_search" {
        return projectless_user_lcm_tools_call_response(
            id,
            tool_name,
            arguments,
            client_identity,
            store_administration,
        )
        .await;
    }
    if matches!(
        tool_name,
        "tracedecay_fact_store" | "tracedecay_fact_feedback" | "tracedecay_memory_status"
    ) {
        if arguments
            .get("memory_scope")
            .and_then(serde_json::Value::as_str)
            != Some("user")
        {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "projectless memory dispatch requires memory_scope=user".to_string(),
            );
        }
        let runtime_registry = match store_administration.retained_runtime_registry().await {
            Ok(registry) => registry,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        return match crate::mcp::tools::handle_user_memory_tool(
            tool_name,
            arguments,
            runtime_registry.as_ref(),
            &client_identity.profile_root,
        )
        .await
        {
            Ok(result) => projectless_tool_result_response(id, result),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    JsonRpcResponse::error(
        id,
        ErrorCode::InternalError,
        format!("{tool_name} requires an initialized code project"),
    )
}

async fn projectless_user_lcm_tools_call_response(
    id: serde_json::Value,
    tool_name: &str,
    arguments: serde_json::Value,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    if arguments
        .get("storage_scope")
        .and_then(serde_json::Value::as_str)
        != Some("user")
    {
        return JsonRpcResponse::error(
            id,
            ErrorCode::InvalidParams,
            "projectless LCM dispatch requires storage_scope=user".to_string(),
        );
    }
    if let Err(error) =
        await_user_profile_host_admission_replay_for_identity(store_administration, client_identity)
            .await
    {
        return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    }
    let user_session_db = match store_administration
        .registered_profile_session_database()
        .await
    {
        Ok(database) => database,
        Err(error) => {
            return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
        }
    };
    let profile_identity = match store_administration.profile_identity() {
        Ok(identity) => identity,
        Err(error) => {
            return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
        }
    };
    let refresh_wake = store_administration
        .session_temporal_refresh_schedulers()
        .ensure_profile(
            user_session_db.db_path().to_path_buf(),
            Arc::clone(&user_session_db),
        )
        .await;
    let retrieval_service = crate::mcp::server::DaemonSessionRetrievalRoot::profile()
        .and_then(|root| root.with_profile_runtime_shard(profile_identity))
        .and_then(|root| {
            crate::mcp::server::DaemonSessionRetrievalService::new_registered(
                Arc::clone(&user_session_db),
                Arc::clone(&user_session_db),
                root,
                Some(refresh_wake.clone()),
            )
        })
        .map(|service| {
            Arc::new(service) as Arc<dyn crate::mcp::tools::SessionRetrievalServicePort>
        });
    let result = crate::mcp::tools::handle_user_lcm_tool_with_retained_authority(
        tool_name,
        arguments.clone(),
        &client_identity.profile_root,
        &user_session_db,
        retrieval_service.as_deref(),
    )
    .await;
    match result {
        Ok(result) => {
            if tool_name == "tracedecay_lcm_preflight"
                && arguments
                    .get("transcript_projection")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            {
                let _ = refresh_wake
                    .wake_and_wait_until_idle(std::time::Duration::from_secs(5))
                    .await;
            } else if matches!(
                tool_name,
                "tracedecay_lcm_preflight"
                    | "tracedecay_lcm_compress"
                    | "tracedecay_lcm_session_boundary"
            ) {
                refresh_wake.wake();
            }
            projectless_tool_result_response(id, result)
        }
        Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}

fn projectless_tool_result_response(
    id: serde_json::Value,
    mut result: crate::mcp::tools::ToolResult,
) -> JsonRpcResponse {
    crate::mcp::server::mark_semantic_tool_error(&mut result);
    JsonRpcResponse::success(id, result.value)
}

fn cancellation_request_id(line: &str) -> Option<serde_json::Value> {
    let Ok(notification) = serde_json::from_str::<serde_json::Value>(line) else {
        return None;
    };
    (notification
        .get("method")
        .and_then(serde_json::Value::as_str)
        == Some("notifications/cancelled"))
    .then(|| {
        notification
            .get("params")
            .and_then(|params| params.get("requestId"))
            .cloned()
    })
    .flatten()
}

fn pending_contains_request(pending: &VecDeque<String>, request_id: &serde_json::Value) -> bool {
    pending.iter().any(|line| {
        serde_json::from_str::<JsonRpcRequest>(line)
            .ok()
            .and_then(|request| request.id)
            .as_ref()
            == Some(request_id)
    })
}

fn projectless_overload_response(line: &str) -> Option<JsonRpcResponse> {
    let request = serde_json::from_str::<JsonRpcRequest>(line).ok()?;
    let id = request.id?;
    Some(JsonRpcResponse::error_with_data(
        id,
        ErrorCode::InternalError,
        "projectless request queue is full".to_owned(),
        Some(json!({
            "reason_code": "projectless_request_queue_full",
            "retryable": true,
        })),
    ))
}

pub(super) fn projectless_tool_call(
    params: Option<&serde_json::Value>,
) -> std::result::Result<(&str, serde_json::Value), &'static str> {
    let Some(params) = params else {
        return Err("missing params for tools/call");
    };
    let Some(tool_name) = params.get("name").and_then(|v| v.as_str()) else {
        return Err("missing 'name' in tools/call params");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Ok((tool_name, arguments))
}

pub(super) fn projectless_user_session_request(request_line: &str) -> bool {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(request_line.trim()) else {
        return false;
    };
    if request.method != "tools/call" {
        return false;
    }
    let Ok((tool_name, arguments)) = projectless_tool_call(request.params.as_ref()) else {
        return false;
    };
    (tool_name.starts_with("tracedecay_lcm_") || tool_name == "tracedecay_message_search")
        && arguments
            .get("storage_scope")
            .and_then(serde_json::Value::as_str)
            == Some("user")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::json;

    use super::{cancellation_request_id, pending_contains_request, projectless_overload_response};

    #[test]
    fn queued_request_cancellation_is_retained_by_exact_request_id() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": "queued-request",
            "method": "tools/call",
            "params": {"name": "tracedecay_admin_cli", "arguments": {}},
        })
        .to_string();
        let cancellation = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": "queued-request"},
        })
        .to_string();
        let pending = VecDeque::from([request]);

        let request_id = cancellation_request_id(&cancellation).expect("cancellation request id");
        assert_eq!(request_id, json!("queued-request"));
        assert!(pending_contains_request(&pending, &request_id));
    }

    #[test]
    fn saturated_queue_rejection_is_typed_and_retryable() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {"name": "tracedecay_status", "arguments": {}},
        })
        .to_string();

        let response = projectless_overload_response(&request).expect("overload response");
        assert_eq!(response.id, json!(42));
        let data = response
            .error
            .expect("error")
            .data
            .expect("structured error data");
        assert_eq!(data["reason_code"], "projectless_request_queue_full");
        assert_eq!(data["retryable"], true);
    }
}

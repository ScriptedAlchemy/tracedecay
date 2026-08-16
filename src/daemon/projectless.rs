//! Projectless client handling: tool calls served without a mounted project
//! (user-scoped LCM, message search, dashboard, doctor, version).
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed. `use super::*` re-exposes every name the
//! parent `daemon` module had in scope so the moved code resolves unchanged.

use std::sync::Arc;

use serde_json::json;

use crate::client_identity::DaemonClientIdentity;
use crate::errors::Result;
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};

use super::*;

/// Authenticated durable identity pinned once for a projectless connection.
/// Request grants are issued only after the adapter supplies exact controls.
struct ProjectlessConnectionStateV1 {
    client_identity: DaemonClientIdentity,
    profile_authority: crate::daemon::retained_owner::ProfileRetainedConnectionAuthorityV1,
}

fn admit_projectless_connection(
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> Result<ProjectlessConnectionStateV1> {
    let profile_identity = store_administration.profile_identity()?;
    if profile_identity.profile_root() != client_identity.profile_root {
        return Err(TraceDecayError::Config {
            message: "projectless connection profile does not match its authenticated identity"
                .to_owned(),
        });
    }
    let profile_session_root =
        crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::profile()
            .and_then(|root| root.with_profile_runtime_shard(profile_identity))
            .ok_or_else(|| TraceDecayError::Config {
                message: "projectless profile session authority is unavailable".to_owned(),
            })?;
    let profile_authority = crate::daemon::retained_owner::profile_retained_connection_authority(
        profile_identity,
        profile_session_root.identity(),
    )?;
    Ok(ProjectlessConnectionStateV1 {
        client_identity: client_identity.clone(),
        profile_authority,
    })
}

pub(super) async fn serve_projectless_client(
    transport: &mut impl McpTransport,
    client_identity: &DaemonClientIdentity,
    lifecycle: &DaemonLifecycle,
    store_administration: &StoreAdministration,
) -> Result<()> {
    let connection = admit_projectless_connection(client_identity, store_administration)?;
    loop {
        let line = tokio::select! {
            result = read_line_handling_wire_oversized(transport) => result?,
            () = lifecycle.wait_for_draining() => break,
        };
        let Some(line) = line else {
            break;
        };
        let Some(_activity) = lifecycle.try_enter() else {
            break;
        };
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => projectless_response(&request, &connection, store_administration).await,
            Err(e) => Some(JsonRpcResponse::error(
                json!(null),
                ErrorCode::ParseError,
                format!("Parse error: {e}"),
            )),
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
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
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
            projectless_tools_call_response_with_connection(
                id,
                request.params.as_ref(),
                connection,
                store_administration,
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

#[cfg(test)]
pub(super) async fn projectless_tools_call_response(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    let connection = match admit_projectless_connection(client_identity, store_administration) {
        Ok(connection) => connection,
        Err(error) => {
            return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
        }
    };
    projectless_tools_call_response_with_connection(id, params, &connection, store_administration)
        .await
}

async fn projectless_tools_call_response_with_connection(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };
    if let Err(error) = store_administration.ensure_account_active().await {
        return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    }
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
            .reconcile_cached_automation_for_profile(&connection.client_identity.profile_root)
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
        let host_admission_broker = match store_administration
            .host_admission_broker(&user_session_db)
            .await
        {
            Ok(broker) => broker,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let host_admission_broker = Ok(&host_admission_broker);
        let refresh_wake = store_administration
            .session_temporal_refresh_schedulers()
            .ensure_profile(
                user_session_db.db_path().to_path_buf(),
                user_session_db.clone(),
            )
            .await;
        return match crate::mcp::tools::handle_projectless_hook_runtime(
            arguments.clone(),
            &connection.client_identity.profile_root,
            session_runtime_registry,
            global_db.as_ref(),
            crate::mcp::tools::SessionAuthorities::new(None, Some(&user_session_db))
                .with_profile_identity(Some(profile_identity))
                .with_registered_databases(None, Some(&user_session_db)),
            host_admission_broker,
        )
        .await
        {
            Ok(result) if crate::mcp::server::tool_result_has_semantic_error(&result) => {
                JsonRpcResponse::success(id, result.value)
            }
            Ok(result) => match crate::mcp::server::join_required_live_transcript_refresh(
                tool_name,
                &arguments,
                false,
                None,
                Some(&refresh_wake),
            )
            .await
            {
                Ok(crate::mcp::server::LiveTranscriptRefreshJoin::PublicationJoined) => {
                    JsonRpcResponse::success(id, result.value)
                }
                Ok(crate::mcp::server::LiveTranscriptRefreshJoin::NotRequired) => {
                    refresh_wake.wake();
                    JsonRpcResponse::success(id, result.value)
                }
                Err(error) => crate::mcp::server::tool_error_response(id, tool_name, &error),
            },
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
        let accounting_db = match store_administration.registered_profile_database().await {
            Ok(database) => database,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        return match crate::mcp::tools::handle_projectless_admin_cli(
            arguments,
            &global_db,
            crate::global_db::global_accounting_enabled().then_some(accounting_db.as_ref()),
            &connection.client_identity.profile_root,
        )
        .await
        {
            Ok(result) => JsonRpcResponse::success(id, result.value),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    if let Some(operation) = crate::mcp::tools::retained_mcp_operation(tool_name, &arguments) {
        return projectless_profile_retained_response(
            id,
            tool_name,
            operation,
            arguments,
            connection,
            store_administration,
        )
        .await;
    }
    JsonRpcResponse::error(
        id,
        ErrorCode::InternalError,
        format!("{tool_name} requires an initialized code project"),
    )
}

async fn projectless_profile_retained_response(
    id: serde_json::Value,
    tool_name: &str,
    operation: tracedecay_application::RetainedSurfaceOperation,
    arguments: serde_json::Value,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    let is_lcm =
        tool_name.starts_with("tracedecay_lcm_") || tool_name == "tracedecay_message_search";
    let requested_scope = if is_lcm {
        arguments
            .get("storage_scope")
            .and_then(serde_json::Value::as_str)
    } else {
        arguments
            .get("memory_scope")
            .and_then(serde_json::Value::as_str)
    };
    if requested_scope != Some("user") {
        return JsonRpcResponse::error(
            id,
            ErrorCode::InvalidParams,
            "projectless retained dispatch requires an explicit user scope".to_string(),
        );
    }
    if is_lcm
        && let Err(error) = await_user_profile_host_admission_replay_for_identity(
            store_administration,
            &connection.client_identity,
        )
        .await
    {
        return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    }
    let runtime_registry = match store_administration.retained_runtime_registry().await {
        Ok(registry) => registry,
        Err(error) => {
            return crate::mcp::server::tool_error_response(id, tool_name, &error);
        }
    };
    let result = crate::mcp::tools::execute_profile_retained_mcp_tool(
        operation,
        tool_name,
        arguments,
        runtime_registry.as_ref(),
        &connection.profile_authority,
        None,
        None,
        None,
        None,
        None,
    )
    .await;
    match result {
        Ok(result) => JsonRpcResponse::success(id, result.value),
        Err(error) => crate::mcp::server::tool_error_response(id, tool_name, &error),
    }
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

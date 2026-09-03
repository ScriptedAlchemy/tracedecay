//! Projectless client handling: tool calls served without a mounted project
//! (user-scoped LCM, message search, dashboard, doctor, version).

use serde_json::json;

use tracedecay_daemon_protocol::DaemonClientIdentity;
use tracedecay_domain::errors::Result;
use tracedecay_mcp::{
    ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport, tool_error_response,
    tool_result_has_semantic_error,
};
use tracedecay_session_runtime::session_retrieval::DaemonSessionRetrievalRoot;
use tracedecay_sessions::runtime::user_sessions_db_path;
use tracedecay_store::StoreShardIdV1;

use super::*;

type ProjectlessPhaseFutureV1<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[inline(never)]
fn boxed_projectless_phase<'a, T>(
    future: impl std::future::Future<Output = T> + Send + 'a,
) -> ProjectlessPhaseFutureV1<'a, T>
where
    T: Send + 'a,
{
    Box::pin(future)
}

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
    let shard = StoreShardIdV1::profile_sessions(
        profile_identity.brain_id().clone(),
        profile_identity.profile_id().clone(),
    );
    let serving_db = user_sessions_db_path(profile_identity.profile_root());
    let serving = crate::daemon::retained_owner::profile_session_retrieval_serving_identity(
        profile_identity,
        &shard,
        &serving_db,
    )
    .ok_or_else(|| TraceDecayError::Config {
        message: "projectless profile session identity is unavailable".to_owned(),
    })?;
    let profile_session_root =
        DaemonSessionRetrievalRoot::profile(serving).ok_or_else(|| TraceDecayError::Config {
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
    transport: &mut (impl McpTransport + Send),
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
            Ok(request) => {
                boxed_projectless_phase(projectless_response(
                    &request,
                    &connection,
                    store_administration,
                ))
                .await
            }
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
    request: &tracedecay_mcp::JsonRpcRequest,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> Option<tracedecay_mcp::JsonRpcResponse> {
    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => Some(match crate::version::build_version() {
            Ok(version) => JsonRpcResponse::success(
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
                        "version": version
                    }
                }),
            ),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        }),
        "tools/call" => Some(
            boxed_projectless_phase(projectless_tools_call_response_with_connection(
                id,
                request.params.as_ref(),
                connection,
                store_administration,
            ))
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
) -> tracedecay_mcp::JsonRpcResponse {
    let connection = match admit_projectless_connection(client_identity, store_administration) {
        Ok(connection) => connection,
        Err(error) => {
            return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
        }
    };
    projectless_tools_call_response_with_connection(id, params, &connection, store_administration)
        .await
}

#[hotpath::measure(label = "mcp.tools_call.projectless", future = true)]
async fn projectless_tools_call_response_with_connection(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> tracedecay_mcp::JsonRpcResponse {
    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };
    #[cfg(feature = "hotpath")]
    {
        let hotpath_tool_name = if matches!(
            tool_name,
            "tracedecay_admin_project" | "tracedecay_hook_runtime" | "tracedecay_admin_cli"
        )
            || tracedecay_application::RetainedSurfaceOperation::from_tool_name(tool_name).is_some()
        {
            tool_name
        } else {
            "unknown"
        };
        hotpath::val!("mcp.tool.name").set(&hotpath_tool_name);
    }
    if let Err(error) = boxed_projectless_phase(store_administration.ensure_account_active()).await
    {
        return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    }
    // Keep unrelated tool families out of one generated poll frame. Some handlers
    // retain large typed futures, and combining them here can exhaust a Tokio
    // worker stack before the selected handler is polled.
    let response =
        match tool_name {
            "tracedecay_admin_project" => boxed_projectless_phase(
                projectless_admin_project_response(id, arguments, connection, store_administration),
            ),
            "tracedecay_hook_runtime" => boxed_projectless_phase(
                projectless_hook_runtime_response(id, arguments, connection, store_administration),
            ),
            "tracedecay_admin_cli" => boxed_projectless_phase(projectless_admin_cli_response(
                id,
                arguments,
                connection,
                store_administration,
            )),
            _ => {
                if let Some(operation) =
                    crate::mcp::tools::retained_mcp_operation(tool_name, &arguments)
                {
                    boxed_projectless_phase(projectless_profile_retained_response(
                        id,
                        tool_name,
                        operation,
                        arguments,
                        connection,
                        store_administration,
                    ))
                } else {
                    return JsonRpcResponse::error(
                        id,
                        ErrorCode::InternalError,
                        format!("{tool_name} requires an initialized code project"),
                    );
                }
            }
        };
    response.await
}

async fn projectless_admin_project_response(
    id: serde_json::Value,
    arguments: serde_json::Value,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> tracedecay_mcp::JsonRpcResponse {
    #[derive(serde::Deserialize)]
    #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
    enum ProjectlessAdminProjectAction {
        AutomationReconcile {
            scope: tracedecay_dashboard_api::AutomationReconcileScope,
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
    if scope != tracedecay_dashboard_api::AutomationReconcileScope::Profile {
        return JsonRpcResponse::error(
            id,
            ErrorCode::InvalidParams,
            "project-scoped automation reconciliation requires a project path".to_string(),
        );
    }
    let outcomes = match boxed_projectless_phase(
        store_administration
            .reconcile_cached_automation_for_profile(&connection.client_identity.profile_root),
    )
    .await
    {
        Ok(outcomes) => outcomes,
        Err(error) => {
            return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
        }
    };
    let report = tracedecay_dashboard_api::ProfileAutomationReconcileReport {
        scope,
        cached_owners: outcomes.len(),
        outcomes,
        uncached_projects:
            tracedecay_dashboard_api::UncachedProjectReconcileOutcome::DeferredUntilProjectStartup,
    };
    JsonRpcResponse::success(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string())
            }]
        }),
    )
}

async fn projectless_hook_runtime_response(
    id: serde_json::Value,
    arguments: serde_json::Value,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> tracedecay_mcp::JsonRpcResponse {
    let global_db =
        match boxed_projectless_phase(store_administration.registered_profile_database()).await {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
    let session_runtime_registry =
        match boxed_projectless_phase(store_administration.registered_runtime_registry()).await {
            Ok(registry) => registry,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
    let user_session_db =
        match boxed_projectless_phase(store_administration.registered_profile_session_database())
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
    let host_admission_broker =
        match boxed_projectless_phase(store_administration.host_admission_broker(&user_session_db))
            .await
        {
            Ok(broker) => broker,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
    let host_admission_broker = Ok(&host_admission_broker);
    let refresh_wake = boxed_projectless_phase(
        store_administration
            .session_temporal_refresh_schedulers()
            .ensure_profile(
                user_session_db.db_path().to_path_buf(),
                user_session_db.clone(),
            ),
    )
    .await;
    match boxed_projectless_phase(crate::mcp::tools::handle_projectless_hook_runtime(
        arguments.clone(),
        &connection.client_identity.profile_root,
        session_runtime_registry,
        global_db.as_ref(),
        crate::mcp::tools::SessionAuthorities::new(None, Some(&user_session_db))
            .with_profile_identity(Some(std::sync::Arc::new(profile_identity.clone())))
            .with_registered_databases(None, Some(&user_session_db)),
        host_admission_broker,
    ))
    .await
    {
        Ok(result) if tool_result_has_semantic_error(&result) => {
            JsonRpcResponse::success(id, result.value)
        }
        Ok(result) => match boxed_projectless_phase(
            crate::mcp::server::join_required_live_transcript_refresh(
                "tracedecay_hook_runtime",
                &arguments,
                false,
                None,
                Some(&refresh_wake),
            ),
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
            Err(error) => tool_error_response(id, "tracedecay_hook_runtime", &error),
        },
        Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}

async fn projectless_admin_cli_response(
    id: serde_json::Value,
    arguments: serde_json::Value,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> tracedecay_mcp::JsonRpcResponse {
    let global_db =
        match boxed_projectless_phase(store_administration.registered_profile_database()).await {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
    let accounting_db =
        match boxed_projectless_phase(store_administration.registered_profile_database()).await {
            Ok(database) => database,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
    match boxed_projectless_phase(crate::mcp::tools::handle_projectless_admin_cli(
        arguments,
        &global_db,
        tracedecay_global_db::global_accounting_enabled().then_some(accounting_db.as_ref()),
        &connection.client_identity.profile_root,
    ))
    .await
    {
        Ok(result) => JsonRpcResponse::success(id, result.value),
        Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}

#[hotpath::measure(label = "daemon.project.projectless_retained", future = true)]
async fn projectless_profile_retained_response(
    id: serde_json::Value,
    tool_name: &str,
    operation: tracedecay_application::RetainedSurfaceOperation,
    arguments: serde_json::Value,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> tracedecay_mcp::JsonRpcResponse {
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
        && let Err(error) =
            boxed_projectless_phase(await_user_profile_host_admission_replay_for_identity(
                store_administration,
                &connection.client_identity,
            ))
            .await
    {
        return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    }
    let runtime_registry =
        match boxed_projectless_phase(store_administration.registered_runtime_registry()).await {
            Ok(registry) => registry,
            Err(error) => {
                return tool_error_response(id, tool_name, &error);
            }
        };
    let result = boxed_projectless_phase(crate::mcp::tools::execute_profile_retained_mcp_tool(
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
    ))
    .await;
    match result {
        Ok(result) => JsonRpcResponse::success(id, result.value),
        Err(error) => tool_error_response(id, tool_name, &error),
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

pub(super) fn projectless_user_session_request(request: Option<&JsonRpcRequest>) -> bool {
    let Some(request) = request else {
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

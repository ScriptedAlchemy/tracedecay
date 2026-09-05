//! Projectless client handling: tool calls served without a mounted project
//! (user-scoped LCM, message search, dashboard, doctor, version).

use serde_json::json;

use tracedecay_daemon_identity::authority;
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

/// Two profile roots name the same profile when they resolve to the same
/// physical directory.
///
/// The authority side is always canonical: `profile_identity::load_or_create`
/// runs `canonical_identity_path` before it pins the record. The client side
/// carries whatever path the host process derived from its own environment,
/// which is never canonicalized on the wire. A byte comparison therefore
/// refuses a connection that is in fact addressing the very same directory
/// whenever any component of the client's profile root is a symlink — the
/// default on macOS, where the per-user temporary root and anything under
/// `/var` resolve through `/var -> /private/var`. Project routing already
/// canonicalizes this exact field before it compares
/// (`project_routing::resolve_project_route`); projectless admission must
/// agree with it or the same client is admitted for projects and refused for
/// user-scoped tools.
fn profile_roots_match(authority_root: &Path, client_root: &Path) -> bool {
    if authority_root == client_root {
        return true;
    }
    match (
        authority::canonical_identity_path(authority_root),
        authority::canonical_identity_path(client_root),
    ) {
        (Ok(authority_root), Ok(client_root)) => authority_root == client_root,
        // A root that cannot be resolved stays refused: admission is
        // fail-closed, never fail-open.
        _ => false,
    }
}

fn admit_projectless_connection(
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> Result<ProjectlessConnectionStateV1> {
    let profile_identity = store_administration.profile_identity()?;
    if !profile_roots_match(
        profile_identity.profile_root(),
        &client_identity.profile_root,
    ) {
        return Err(TraceDecayError::Config {
            message: "projectless connection profile does not match its authenticated identity"
                .to_owned(),
        });
    }
    let pinned_profile_root = profile_identity.profile_root().to_path_buf();
    let shard = StoreShardIdV1::profile_sessions(
        profile_identity.brain_id().clone(),
        profile_identity.profile_id().clone(),
    );
    let serving_db = user_sessions_db_path(&pinned_profile_root);
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
        client_identity: DaemonClientIdentity::new(
            pinned_profile_root.clone(),
            pinned_profile_root.join("global.db"),
        ),
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

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod projectless_admission_tests {
    use super::*;

    /// Build a private profile root plus a symlinked spelling of the same
    /// directory. On macOS the runner's own `TMPDIR` is already reached
    /// through `/var -> /private/var`, so a client's profile root and the
    /// pinned authority differ by exactly this much on every connection; on
    /// Linux the symlink has to be made explicitly to reproduce it.
    fn linked_profile_root(temp: &std::path::Path) -> (PathBuf, PathBuf) {
        let real_root = temp.join("real").join(".tracedecay");
        std::fs::create_dir_all(&real_root).expect("create profile root");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&real_root, std::fs::Permissions::from_mode(0o700))
                .expect("restrict profile root");
        }
        let link = temp.join("linked");
        std::os::unix::fs::symlink(temp.join("real"), &link).expect("link profile parent");
        (real_root, link.join(".tracedecay"))
    }

    #[test]
    fn a_symlinked_client_profile_root_is_the_same_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (real_root, linked_root) = linked_profile_root(temp.path());
        assert_ne!(real_root, linked_root, "the two spellings must differ");

        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&real_root)
            .expect("pin profile identity");
        let administration = StoreAdministration::default().with_profile_identity(identity);
        let client = DaemonClientIdentity::new(linked_root.clone(), linked_root.join("global.db"));

        admit_projectless_connection(&client, &administration)
            .expect("a symlinked spelling of the pinned profile root must be admitted");
    }

    #[test]
    fn an_unrelated_client_profile_root_stays_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (real_root, _linked_root) = linked_profile_root(temp.path());
        let foreign_root = temp.path().join("foreign").join(".tracedecay");
        std::fs::create_dir_all(&foreign_root).expect("create foreign root");

        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&real_root)
            .expect("pin profile identity");
        let administration = StoreAdministration::default().with_profile_identity(identity);
        let client =
            DaemonClientIdentity::new(foreign_root.clone(), foreign_root.join("global.db"));

        let Err(error) = admit_projectless_connection(&client, &administration) else {
            panic!("an unrelated profile root must stay refused")
        };
        assert!(
            error
                .to_string()
                .contains("projectless connection profile does not match"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn an_unresolvable_client_profile_root_stays_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (real_root, _linked_root) = linked_profile_root(temp.path());
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&real_root)
            .expect("pin profile identity");
        let administration = StoreAdministration::default().with_profile_identity(identity);
        let missing = temp.path().join("never-created").join(".tracedecay");
        let client = DaemonClientIdentity::new(missing.clone(), missing.join("global.db"));

        assert!(
            admit_projectless_connection(&client, &administration).is_err(),
            "a profile root that resolves to nothing must stay refused"
        );
    }

    #[tokio::test]
    async fn retargeted_client_profile_root_keeps_hermes_receipt_under_pinned_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (real_root, linked_root) = linked_profile_root(temp.path());
        let foreign_root = temp.path().join("foreign").join(".tracedecay");
        std::fs::create_dir_all(&foreign_root).expect("create foreign profile root");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&foreign_root, std::fs::Permissions::from_mode(0o700))
                .expect("restrict foreign profile root");
        }
        crate::product_runtime::register_fixture_product_runtime();
        crate::host_admission::ensure_process_background_cpu_authority()
            .expect("install fixture worker authority");
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&real_root)
            .expect("pin profile identity");
        let administration = StoreAdministration::default().with_profile_identity(identity);
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &real_root,
            1,
            "projectless-retargeted-hermes-test",
        )
        .expect("enter fixture database scope");
        let client = DaemonClientIdentity::new(linked_root.clone(), linked_root.join("global.db"));
        let connection = admit_projectless_connection(&client, &administration)
            .expect("admit symlinked client profile");

        let linked_parent = linked_root.parent().expect("linked profile parent");
        std::fs::remove_file(linked_parent).expect("remove original profile symlink");
        std::os::unix::fs::symlink(
            foreign_root.parent().expect("foreign profile parent"),
            linked_parent,
        )
        .expect("retarget profile symlink");

        let response = projectless_hook_runtime_response(
            json!(1),
            json!({
                "action": "hermes_receipt",
                "event": {
                    "agent": "hermes",
                    "event": "turnCompleted",
                    "route": { "session_id": "pinned-hermes-session" },
                    "receipt": {
                        "status": "success",
                        "transcript_watermark": "pinned-hermes-watermark"
                    }
                }
            }),
            &connection,
            &administration,
        )
        .await;

        let pinned_automation_root =
            tracedecay_automation_runtime::automation::runner::user_automation_root(&real_root);
        let foreign_automation_root =
            tracedecay_automation_runtime::automation::runner::user_automation_root(&foreign_root);
        let pinned_receipt_exists = pinned_automation_root.join("host_receipts.json").is_file();
        let foreign_receipt_exists = foreign_automation_root.join("host_receipts.json").exists();
        administration.shutdown_host_admission_replay().await;

        assert!(
            response.error.is_none(),
            "Hermes receipt failed: {response:?}"
        );
        assert!(
            pinned_receipt_exists,
            "durable Hermes receipt must remain under the admitted profile"
        );
        assert!(
            !foreign_receipt_exists,
            "retargeting the client symlink must never redirect durable receipt writes"
        );
    }

    #[tokio::test]
    async fn removed_client_profile_symlink_keeps_retained_codex_path_pinned() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (real_root, linked_root) = linked_profile_root(temp.path());
        crate::product_runtime::register_fixture_product_runtime();
        crate::host_admission::ensure_process_background_cpu_authority()
            .expect("install fixture worker authority");
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&real_root)
            .expect("pin profile identity");
        let administration = StoreAdministration::default().with_profile_identity(identity);
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &real_root,
            1,
            "projectless-removed-codex-test",
        )
        .expect("enter fixture database scope");
        let client = DaemonClientIdentity::new(linked_root.clone(), linked_root.join("global.db"));
        let connection = admit_projectless_connection(&client, &administration)
            .expect("admit symlinked client profile");
        let runtime_registry = administration
            .registered_runtime_registry()
            .await
            .expect("open profile runtime registry");

        std::fs::remove_file(linked_root.parent().expect("linked profile parent"))
            .expect("remove client profile symlink after admission");
        let retained_profile_root = connection.client_identity.profile_root.clone();
        let retained_global_db_path = connection.client_identity.global_db_path.clone();

        let response = projectless_hook_runtime_response(
            json!(2),
            json!({
                "action": "codex_stop",
                "session_id": "pinned-codex-session"
            }),
            &connection,
            &administration,
        )
        .await;
        let terminal_shutdown = runtime_registry.shutdown_terminal_tasks().await;
        administration.shutdown_host_admission_replay().await;

        assert_eq!(retained_profile_root, real_root);
        assert_eq!(
            retained_global_db_path,
            retained_profile_root.join("global.db")
        );
        assert!(
            response.error.is_none(),
            "retained Codex stop failed after client symlink removal: {response:?}"
        );
        terminal_shutdown.expect("join retained Codex task");
    }
}

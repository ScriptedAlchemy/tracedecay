//! Projectless client handling: tool calls served without a mounted project
//! (user-scoped LCM, message search, dashboard, doctor, version).

use serde_json::json;

use tracedecay_daemon_identity::authority;
use tracedecay_daemon_protocol::DaemonClientIdentity;
use tracedecay_domain::errors::Result;
use tracedecay_mcp::tool_errors::structure_tool_problem;
use tracedecay_mcp::tools::catalog_discovery::{
    catalog_discovery_tools_list_payload, default_catalog_discovery_authority,
};
use tracedecay_mcp::{
    ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport, ToolRegistryMode,
    explore_call_budget, project_catalog_discovery_scope, tool_error_response,
};

use super::*;
use tracedecay_daemon_service::shutdown::DaemonLifecycle;

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
struct ProjectlessConnectionStateV1 {
    client_identity: DaemonClientIdentity,
    active_project_root: Option<PathBuf>,
}

/// Two profile roots name the same profile when they resolve to the same
/// physical directory.
///
/// The authority side is always canonical: `profile_identity::load_or_create`
/// runs `canonical_identity_path` before it pins the record. The client side
/// carries whatever path the host process derived from its own environment,
/// which is never canonicalized on the wire. A byte comparison therefore
/// refuses a connection that is in fact addressing the very same directory
/// whenever any component of the client's profile root is a symlink, the
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
    Ok(ProjectlessConnectionStateV1 {
        client_identity: DaemonClientIdentity::new(
            pinned_profile_root.clone(),
            pinned_profile_root.join("global.db"),
        ),
        active_project_root: None,
    })
}

/// `active_project_root` is the handshake's project, used only to mark that
/// project active in registry reads; it never mounts or opens the project.
pub(super) async fn serve_projectless_client(
    transport: &mut (impl McpTransport + Send),
    client_identity: &DaemonClientIdentity,
    active_project_root: Option<PathBuf>,
    timings_enabled: bool,
    lifecycle: &DaemonLifecycle,
    store_administration: &StoreAdministration,
) -> Result<()> {
    let mut connection = admit_projectless_connection(client_identity, store_administration)?;
    connection.active_project_root = active_project_root;
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
        let response = match JsonRpcRequest::decode(&line) {
            Ok(request) => {
                boxed_projectless_phase(projectless_response(
                    &request,
                    &connection,
                    timings_enabled,
                    store_administration,
                ))
                .await
            }
            Err(error) => Some(error.into_response()),
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
    timings_enabled: bool,
    store_administration: &StoreAdministration,
) -> Option<tracedecay_mcp::JsonRpcResponse> {
    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => {
            let mut response = match tracedecay_project::version::build_version() {
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
                Err(error) => {
                    JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string())
                }
            };
            boxed_projectless_phase(attach_reset_required_stores(
                &mut response,
                store_administration,
            ))
            .await;
            Some(response)
        }
        "tools/list" => Some(projectless_tools_list_response(id)),
        "tools/call" => {
            let started = timings_enabled.then(std::time::Instant::now);
            let mut response =
                boxed_projectless_phase(projectless_tools_call_response_with_connection(
                    id,
                    request.params.as_ref(),
                    connection,
                    store_administration,
                ))
                .await;
            if let Some(result) = response.result.as_mut() {
                structure_tool_problem(result);
            }
            attach_projectless_tool_timing(
                &mut response,
                started.map(|started| started.elapsed().as_micros() as u64),
            );
            Some(response)
        }
        "ping" => Some(JsonRpcResponse::success(id, json!({}))),
        _ => Some(JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("Method not found: {}", request.method),
        )),
    }
}

/// Whether projectless dispatch can serve this tool without a mounted project.
///
/// Discovery and call admission share this predicate so `tools/list` never
/// advertises a name that still answers "requires an initialized code project".
fn projectless_tool_is_discoverable(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_admin_cli"
            | "tracedecay_project_list"
            | "tracedecay_project_search"
            | "tracedecay_project_context"
    ) || tracedecay_contracts::RetainedSurfaceOperation::from_tool_name(tool_name).is_some()
}

/// Projectless `tools/list`: the host-available catalog, reduced to tools the
/// projectless dispatcher can actually call. An empty or uncomposable catalog
/// is a typed error, never a successful empty listing.
fn projectless_tools_list_payload() -> std::result::Result<serde_json::Value, String> {
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .map_err(|error| format!("invalid MCP discovery profile: {error}"))?;
    let authority = default_catalog_discovery_authority()
        .map_err(|error| format!("MCP catalog discovery unavailable: {error}"))?;
    let mut payload = catalog_discovery_tools_list_payload(
        None,
        explore_call_budget(0),
        &profile_id,
        &authority,
        &project_catalog_discovery_scope(),
        ToolRegistryMode::HostAvailable,
    )
    .map_err(|error| format!("MCP catalog discovery unavailable: {error}"))?;
    let tools = payload
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "MCP catalog discovery unavailable".to_owned())?;
    tools.retain(|tool| {
        tool.get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(projectless_tool_is_discoverable)
    });
    if tools.is_empty() {
        return Err("MCP projectless catalog discovery produced no tools".to_owned());
    }
    Ok(payload)
}

fn projectless_tools_list_response(id: serde_json::Value) -> JsonRpcResponse {
    match projectless_tools_list_payload() {
        Ok(payload) => JsonRpcResponse::success(id, payload),
        Err(message) => JsonRpcResponse::error(id, ErrorCode::InternalError, message),
    }
}

fn attach_projectless_tool_timing(response: &mut JsonRpcResponse, duration_us: Option<u64>) {
    let Some(duration_us) = duration_us else {
        return;
    };
    let Some(result) = response
        .result
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let meta = result
        .entry("_meta")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if meta.is_null() {
        *meta = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(meta) = meta.as_object_mut() {
        meta.entry("duration_us")
            .or_insert_with(|| serde_json::json!(duration_us));
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
    // Call admission is the discovery predicate, widened by the requests the
    // daemon's profile owner answers: anything else is refused here before
    // any account or store work.
    let no_arguments = serde_json::Map::new();
    let profile_owner_operation =
        tracedecay_tool_catalog::ApplicationSurfaceOperation::from_tool_name(tool_name).filter(
            |operation| {
                operation.is_profile_owner_request(arguments.as_object().unwrap_or(&no_arguments))
            },
        );
    let discoverable =
        profile_owner_operation.is_some() || projectless_tool_is_discoverable(tool_name);
    #[cfg(feature = "hotpath")]
    {
        let hotpath_tool_name = if discoverable { tool_name } else { "unknown" };
        hotpath::val!("mcp.tool.name").set(&hotpath_tool_name);
    }
    if !discoverable {
        return requires_project_error(id, tool_name);
    }
    if let Err(error) = boxed_projectless_phase(store_administration.ensure_account_active()).await
    {
        if error.store_reset_required("profile authority").is_some() {
            return tool_error_response(id, tool_name, &error);
        }
        return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    }
    // Keep unrelated tool families out of one generated poll frame. Some handlers
    // retain large typed futures, and combining them here can exhaust a Tokio
    // worker stack before the selected handler is polled.
    if let Some(operation) = profile_owner_operation {
        return boxed_projectless_phase(projectless_profile_owner_response(
            id,
            operation,
            arguments,
            connection,
            store_administration,
        ))
        .await;
    }
    let response = match tool_name {
        "tracedecay_admin_cli" => boxed_projectless_phase(projectless_admin_cli_response(
            id,
            arguments,
            connection,
            store_administration,
        )),
        _ => {
            // `projectless_tool_is_discoverable` admitted the name above,
            // so any remaining tool is a retained profile operation.
            let Some(operation) =
                tracedecay_contracts::RetainedSurfaceOperation::from_tool_name(tool_name)
            else {
                return requires_project_error(id, tool_name);
            };
            boxed_projectless_phase(projectless_profile_retained_response(
                id,
                tool_name,
                operation,
                arguments,
                connection,
                store_administration,
            ))
        }
    };
    response.await
}

fn requires_project_error(id: serde_json::Value, tool_name: &str) -> JsonRpcResponse {
    tool_error_response(
        id,
        tool_name,
        &TraceDecayError::project_route(
            PROJECT_REQUIRED_REASON_CODE,
            false,
            format!(
                "{tool_name} requires an initialized code project; run it inside an \
                 initialized project or pass --project <path>"
            ),
        ),
    )
}

/// Profile-owner requests go to the daemon's profile owner, the one path
/// every connection's profile-owner request takes. Only the handshake's
/// project, if any, is marked active.
async fn projectless_profile_owner_response(
    id: serde_json::Value,
    operation: tracedecay_tool_catalog::ApplicationSurfaceOperation,
    arguments: serde_json::Value,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> tracedecay_mcp::JsonRpcResponse {
    let tool_name = operation.mcp_tool_name();
    let executor = profile_executor(connection, store_administration);
    let result = match boxed_projectless_phase(crate::mcp::tools::execute_graph_tool_surface(
        tracedecay_tool_catalog::BindingSurface::Mcp,
        operation,
        arguments.clone(),
        Some(&executor),
        None,
        None,
        None,
    ))
    .await
    {
        Ok(Ok(completion)) => {
            tracedecay_mcp::handlers::graph_tool::render_graph_tool(None, &arguments, completion)
        }
        Ok(Err(refusal)) => refusal.render(None, &arguments),
        Err(error) => Err(error),
    };
    match result {
        Ok(mut result) => {
            tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
            JsonRpcResponse::success(id, result.value)
        }
        Err(error) => tool_error_response(id, tool_name, &error),
    }
}

fn profile_executor(
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> super::profile_retained::ProfileExecutor {
    super::profile_retained::ProfileExecutor {
        store_administration: store_administration.clone(),
        active_project_root: connection.active_project_root.clone(),
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
    match boxed_projectless_phase(
        tracedecay_mcp::handlers::admin_cli::handle_projectless_admin_cli(
            arguments,
            &global_db,
            tracedecay_global_db::global_accounting_enabled().then_some(accounting_db.as_ref()),
            &connection.client_identity.profile_root,
        ),
    )
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
    operation: tracedecay_contracts::RetainedSurfaceOperation,
    arguments: serde_json::Value,
    connection: &ProjectlessConnectionStateV1,
    store_administration: &StoreAdministration,
) -> tracedecay_mcp::JsonRpcResponse {
    match crate::mcp::tools::retained_tool_target(operation, &arguments) {
        Ok(tracedecay_contracts::InvocationTarget::Profile) => {}
        Ok(_) => {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "projectless retained dispatch requires an explicit user scope".to_string(),
            );
        }
        Err(error) => return tool_error_response(id, tool_name, &error),
    }
    let Some(application) =
        tracedecay_tool_catalog::ApplicationSurfaceOperation::from_tool_name(tool_name)
    else {
        return requires_project_error(id, tool_name);
    };
    let executor = profile_executor(connection, store_administration);
    let result = boxed_projectless_phase(crate::mcp::tools::run_retained_surface_tool(
        None,
        tracedecay_tool_catalog::BindingSurface::Mcp,
        application,
        arguments,
        Some(&executor),
        None,
        None,
        None,
    ))
    .await;
    match result {
        Ok(mut result) => {
            tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
            JsonRpcResponse::success(id, result.value)
        }
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

/// Whether a first request is served by the projectless dispatcher even when
/// the handshake names a project: profile-session reads and profile registry
/// reads never depend on that project's open or warm-up.
pub(super) fn projectless_first_request(request: Option<&JsonRpcRequest>) -> bool {
    let Some(request) = request else {
        return false;
    };
    if request.method != "tools/call" {
        return false;
    }
    let Ok((tool_name, arguments)) = projectless_tool_call(request.params.as_ref()) else {
        return false;
    };
    use tracedecay_contracts::RetainedSurfaceOperation as Op;
    // Profile session and LCM reads never wait on the handshake's project;
    // profile memory stays on the project connection.
    matches!(
        tool_name,
        "tracedecay_project_list" | "tracedecay_project_search" | "tracedecay_project_context"
    ) || matches!(
        Op::from_tool_name(tool_name),
        Some(
            operation @ (Op::LcmStatus
                | Op::LcmDoctor
                | Op::LcmLoadSession
                | Op::LcmGrep
                | Op::LcmDescribe
                | Op::LcmExpand
                | Op::LcmExpandQuery
                | Op::MessageSearch
                | Op::SessionRefreshBegin
                | Op::SessionRefreshStatus
                | Op::SessionRefreshCancel)
        ) if crate::mcp::tools::retained_tool_target(operation, &arguments)
            .is_ok_and(|target| target == tracedecay_contracts::InvocationTarget::Profile)
    )
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
        // The retained profile root is whatever admission canonicalized, so the
        // fixture's own base has to be canonical before anything is joined onto
        // it; on macOS `tempfile` hands back the `/var` spelling of
        // `/private/var` and every expectation built from it names a path the
        // daemon never pinned.
        let temp = tracedecay_runtime_core::lifecycle_lease::canonical_or_original(temp);
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
        tracedecay_project::product_runtime::register_fixture_product_runtime();
        tracedecay_project::test_support::host_admission::ensure_process_background_cpu_authority()
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

        let response = projectless_tools_call_response_with_connection(
            json!(1),
            Some(&json!({
                "name": "tracedecay_hook_runtime",
                "arguments": {
                    "action": "hermes_receipt",
                    "event": {
                        "agent": "hermes",
                        "event": "turnCompleted",
                        "route": { "session_id": "pinned-hermes-session" },
                        "receipt": {
                            "status": "success",
                            "transcript_watermark": "pinned-hermes-watermark"
                        }
                    },
                    "format": "json",
                },
            })),
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

        let result = response.result.expect("Hermes receipt result");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                result["content"][0]["text"]
                    .as_str()
                    .expect("Hermes receipt text")
            )
            .expect("Hermes receipt JSON"),
            json!({"action": "hermes_receipt", "status": "recorded"}),
            "{result}"
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
}

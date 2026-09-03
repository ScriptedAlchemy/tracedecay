//! Static MCP bootstrap handled before any project database is opened.
//!
//! `daemon_bootstrap_response` answers initialize, notifications and the other
//! project-independent calls so a client can hand shake without paying for a
//! project admission.

use std::sync::OnceLock;

use tracedecay_mcp::ToolDefinition;

use super::*;

static WARMING_BOOTSTRAP_TOOLS: OnceLock<Vec<ToolDefinition>> = OnceLock::new();

#[hotpath::measure(label = "daemon.bootstrap.warming_catalog")]
fn warming_bootstrap_tool_definitions() -> Result<Vec<ToolDefinition>> {
    if let Some(definitions) = WARMING_BOOTSTRAP_TOOLS.get() {
        return Ok(definitions.clone());
    }
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("MCP bootstrap profile is invalid: {error}"),
    })?;
    let authority =
        default_catalog_discovery_authority().map_err(|error| TraceDecayError::Config {
            message: format!("MCP bootstrap catalog authority is unavailable: {error}"),
        })?;
    let definitions = get_catalog_filtered_tool_definitions_with_warming_budget(
        explore_call_budget(0),
        &profile_id,
        &authority,
        &project_catalog_discovery_scope(),
        ToolRegistryMode::HostAvailable,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("MCP bootstrap catalog is unavailable: {error}"),
    })?;
    if WARMING_BOOTSTRAP_TOOLS.set(definitions.clone()).is_ok() {
        return Ok(definitions);
    }
    WARMING_BOOTSTRAP_TOOLS
        .get()
        .cloned()
        .ok_or_else(|| TraceDecayError::Config {
            message: "MCP bootstrap catalog cache lost a concurrent publication".to_owned(),
        })
}

pub(super) fn prewarm_daemon_bootstrap_catalog() -> Result<()> {
    warming_bootstrap_tool_definitions().map(|_| ())
}

#[hotpath::measure(label = "daemon.bootstrap.initialize_route", future = true)]
pub(super) async fn apply_daemon_initialize_route(
    handshake: &mut DaemonHandshake,
    first_request: &AuthenticatedFirstRequest,
    store_administration: &StoreAdministration,
) -> Result<Option<InitializeRouteMetadata>> {
    apply_daemon_initialize_route_inner(handshake, first_request, store_administration).await
}

fn apply_daemon_initialize_route_inner<'a>(
    handshake: &'a mut DaemonHandshake,
    first_request: &'a AuthenticatedFirstRequest,
    store_administration: &'a StoreAdministration,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<InitializeRouteMetadata>>> + Send + 'a>,
> {
    // Erase the deeply nested future before it reaches the measured wrapper
    // so every profiling feature can compute its layout.
    Box::pin(async move {
        if !handshake.allow_initialize_root_routing {
            return Ok(None);
        }
        let Some(request) = first_request.parsed() else {
            return Ok(None);
        };
        if request.method != "initialize" {
            return Ok(None);
        }
        let registry = store_administration.registered_profile_database().await?;
        let Some(route) =
            resolve_daemon_initialize_route(request.params.as_ref(), Some(&registry)).await?
        else {
            return Ok(None);
        };
        if handshake.project_path.as_deref() != Some(route.project_path.as_path()) {
            handshake.scope_prefix = None;
        }
        handshake.project_path = Some(route.project_path.clone());
        handshake.allow_init = route.allow_init;
        Ok(Some(route))
    })
}

pub(super) fn attach_initialize_route_metadata(
    response: &mut JsonRpcResponse,
    route: &InitializeRouteMetadata,
) {
    let Some(result) = response.result.as_mut() else {
        return;
    };
    result["_meta"]["tracedecayInitializeRoute"] = json!(route);
}

/// Returns `None` for project-dependent requests, `Some(None)` for handled
/// notifications, and `Some(Some(response))` for static MCP bootstrap calls.
pub(super) fn daemon_bootstrap_response(
    request: &JsonRpcRequest,
    route: Option<&InitializeRouteMetadata>,
    project_node_count: Option<u64>,
) -> Option<Option<JsonRpcResponse>> {
    match classify_mcp_method(&request.method) {
        McpMethod::Initialize => Some(request.id.clone().map(|id| {
            let mut response = match initialize_result(SERVER_INSTRUCTIONS) {
                Ok(result) => JsonRpcResponse::success(id, result),
                Err(error) => {
                    return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
                }
            };
            if let Some(route) = route {
                attach_initialize_route_metadata(&mut response, route);
            }
            response
        })),
        McpMethod::InitializedAck => Some(None),
        McpMethod::ToolsList => Some(request.id.clone().map(|id| match project_node_count {
            None => match warming_bootstrap_tool_definitions() {
                Ok(tools) => JsonRpcResponse::success(id, json!({ "tools": tools })),
                Err(_) => JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    "MCP catalog discovery unavailable".to_owned(),
                ),
            },
            Some(node_count) => {
                let budget = explore_call_budget(node_count);
                let profile_id = tracedecay_tool_catalog::ProfileId::new(
                    tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
                );
                let authority = default_catalog_discovery_authority();
                match (profile_id, authority) {
                    (Ok(profile_id), Ok(authority)) => {
                        let definitions = hotpath::measure_block!(
                            "daemon.bootstrap.catalog",
                            get_catalog_filtered_tool_definitions_with_budget(
                                node_count,
                                budget,
                                &profile_id,
                                &authority,
                                &project_catalog_discovery_scope(),
                                ToolRegistryMode::HostAvailable
                            )
                        );
                        match definitions {
                            Ok(tools) => JsonRpcResponse::success(id, json!({ "tools": tools })),
                            Err(_) => JsonRpcResponse::error(
                                id,
                                ErrorCode::InternalError,
                                "MCP catalog discovery unavailable".to_owned(),
                            ),
                        }
                    }
                    _ => JsonRpcResponse::error(
                        id,
                        ErrorCode::InternalError,
                        "MCP catalog discovery unavailable".to_owned(),
                    ),
                }
            }
        })),
        _ => None,
    }
}

#[hotpath::measure(label = "daemon.bootstrap.project_node_count", future = true)]
pub(super) async fn cached_project_node_count(
    store_administration: &StoreAdministration,
    handshake: &DaemonHandshake,
) -> Option<u64> {
    cached_project_node_count_inner(store_administration, handshake).await
}

fn cached_project_node_count_inner<'a>(
    store_administration: &'a StoreAdministration,
    handshake: &'a DaemonHandshake,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<u64>> + Send + 'a>> {
    // Erase the deeply nested future before it reaches the measured wrapper
    // so every profiling feature can compute its layout.
    Box::pin(async move {
        let project_path = handshake.project_path.as_ref()?;
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake).ok()?;
        let _server = {
            let servers = store_administration.project_servers().lock().await;
            servers
                .get_route(&route)
                .map(|(_, server)| Arc::clone(server))
        }?;
        ensure_registered_project_route(
            store_administration,
            &canonical_project_path,
            handshake.allow_init,
        )
        .await
        .ok()?;
        // Bootstrap routing does not receive the daemon invocation state's
        // retained code-index scheduler. A mounted project alone cannot prove a
        // current generation or node count, so catalog discovery stays in its
        // explicit warming state instead of reading the retired SQLite graph.
        None
    })
}

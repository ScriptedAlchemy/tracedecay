//! Per-connection serving: one accepted daemon client, start to finish.
//!
//! Covers the authenticated Unix socket path, the routed rmcp bridge, and the
//! portable broker path. Each entry point owns framing, project-owner routing,
//! and connection teardown for exactly one client.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

#[cfg(all(unix, test))]
pub(super) async fn serve_socket_client(
    stream: tokio::net::UnixStream,
    engine: DaemonEngine,
) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        BrokerStream::Unix(stream),
        engine,
        None,
        DaemonClientAdmissionClass::General,
    ))
    .await
}

#[cfg(unix)]
#[allow(dead_code)] // in-flight authenticated socket serving — staged
pub(super) async fn serve_authenticated_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
) -> Result<()> {
    Box::pin(serve_authenticated_socket_client_with_class(
        stream,
        engine,
        auth_token,
        DaemonClientAdmissionClass::General,
    ))
    .await
}

#[cfg(unix)]
pub(super) async fn serve_authenticated_socket_client_with_class(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        stream,
        engine,
        Some(auth_token),
        admission_class,
    ))
    .await
}

pub(super) async fn serve_routed_rmcp_connection(
    server: Arc<crate::mcp::McpServer>,
    transport: BrokerStreamTransport,
    first_request_line: String,
    pending_lines: impl IntoIterator<Item = String>,
    initialize_route: Option<InitializeRouteMetadata>,
    timings_enabled: bool,
    lifecycle: &DaemonLifecycle,
) -> Result<()> {
    let initialize_response_decorator = initialize_route.map(|route| {
        Arc::new(move |response: &mut JsonRpcResponse| {
            attach_initialize_route_metadata(response, &route);
        }) as RmcpInitializeResponseDecorator
    });
    let mut transport =
        transport.with_project_response_lifecycle(server.project_server_response_lifecycle());
    transport.push_replay(first_request_line)?;
    for line in pending_lines {
        transport.push_replay(line)?;
    }
    let adapter =
        RmcpConnectionAdapter::new(server, timings_enabled, initialize_response_decorator)
            .map_err(|error| TraceDecayError::Config {
                message: format!("MCP connection identity unavailable: {error}"),
            })?;
    let running = adapter
        .serve(transport)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("rmcp server initialization failed: {error}"),
        })?;
    let cancellation = running.cancellation_token();
    let waiting = running.waiting();
    tokio::pin!(waiting);
    let result = tokio::select! {
        result = &mut waiting => result,
        () = lifecycle.wait_for_draining() => {
            cancellation.cancel();
            waiting.await
        }
    };
    result.map_err(|error| TraceDecayError::Config {
        message: format!("rmcp server task failed: {error}"),
    })?;
    Ok(())
}

fn is_mcp_initialize_request(line: &str) -> bool {
    serde_json::from_str::<JsonRpcRequest>(line.trim())
        .is_ok_and(|request| request.method == "initialize")
}

const MAX_PENDING_PROJECT_OPEN_LINES: usize = 64;

pub(super) async fn await_project_owner_or_disconnect<T>(
    transport: &mut impl McpTransport,
    open: impl std::future::Future<Output = Result<T>>,
) -> Result<Option<(T, VecDeque<String>)>> {
    tokio::pin!(open);
    let mut pending_lines = VecDeque::new();
    loop {
        tokio::select! {
            result = &mut open => return result.map(|owner| Some((owner, pending_lines))),
            incoming = transport.read_line() => {
                let Some(line) = incoming? else {
                    // EOF closes only the client's request half. It may still
                    // be reading the response, as one-shot CLI clients do.
                    // Finish the already bounded owner lookup and let the
                    // subsequent write prove whether the peer fully left.
                    return open
                        .await
                        .map(|owner| Some((owner, pending_lines)));
                };
                if pending_lines.len() >= MAX_PENDING_PROJECT_OPEN_LINES {
                    return Err(TraceDecayError::Config {
                        message: "daemon client pipelined too many requests while the project owner was opening"
                            .to_owned(),
                    });
                }
                pending_lines.push_back(line);
            }
        }
    }
}

#[cfg(unix)]
async fn serve_broker_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: Option<String>,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    if let Some(expected_token) = auth_token.as_deref() {
        let preface_line = tokio::select! {
            result = read_line_handling_wire_oversized(&mut transport) => result?,
            () = engine.lifecycle.wait_for_draining() => return Ok(()),
        };
        let Some(preface_line) = preface_line else {
            return Ok(());
        };
        let preface =
            DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            })?;
        if !preface.authenticate(expected_token) {
            return Err(TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            });
        }
    }
    let line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(line) = line else {
        return Ok(());
    };
    let Some(setup_activity) = engine.lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&line)?;
    let store_administration =
        bind_authenticated_profile_identity(&mut handshake, &engine.store_administration).await?;
    let mut engine = engine;
    engine.store_administration = store_administration;
    let first_request_line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(first_request_line) = first_request_line else {
        return Ok(());
    };
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match engine
            .per_client_admission
            .try_admit_request(&handshake, &first_request_line)
        {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    let Some(setup_activity) = serve_core_doctor_runtime_request(
        &mut transport,
        &handshake,
        &engine.store_administration,
        setup_activity,
        &first_request_line,
        || async {
            Ok(engine
                .cached_project_server(&handshake)
                .await?
                .is_some_and(|server| server.doctor_report_ready()))
        },
    )
    .await?
    else {
        return Ok(());
    };
    engine.log_client_version_skew(&handshake).await;
    ensure_user_profile_host_admission_replay_for_identity(
        &engine.store_administration,
        &handshake.client_identity,
    )
    .await?;
    // Resolve initialize roots only after authentication and inside daemon
    // authority. The proxy process never opens the registry database.
    let initialize_route = apply_daemon_initialize_route(
        &mut handshake,
        &first_request_line,
        &engine.store_administration,
    )
    .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => engine.execute_branch_admin(&handshake, action).await,
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response = match await_project_owner_or_disconnect(
            &mut transport,
            engine.project_server_for_request(&handshake, ProjectServerRequirement::Core),
        )
        .await
        {
            Ok(Some(_)) => {
                branch_add_response(&engine.store_administration, &handshake, &request).await
            }
            Ok(None) => return Ok(()),
            Err(error) => JsonRpcResponse::error(
                request.id.clone(),
                ErrorCode::InternalError,
                error.to_string(),
            ),
        };
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Some(invocation) = parse_daemon_invocation_request(&first_request_line) {
        let mut invocation = invocation;
        let mut owned_lsp_sessions = HashMap::new();
        let result = async {
            loop {
                let session_transition = invocation
                    .as_ref()
                    .ok()
                    .and_then(invocation_lsp_session_transition);
                let response = match invocation {
                    Ok(request) => execute_daemon_invocation(&engine, &handshake, request).await,
                    Err(response) => response,
                };
                update_connection_lsp_sessions(
                    &mut owned_lsp_sessions,
                    session_transition.as_ref(),
                    &response,
                );
                write_daemon_invocation_response(&mut transport, &response).await?;
                let next_line = tokio::select! {
                    result = read_line_handling_wire_oversized(&mut transport) => result?,
                    () = engine.lifecycle.wait_for_draining() => return Ok(()),
                };
                let Some(next_line) = next_line else {
                    return Ok(());
                };
                let Some(next_invocation) = parse_daemon_invocation_request(&next_line) else {
                    return Ok(());
                };
                invocation = next_invocation;
            }
        }
        .await;
        cleanup_connection_lsp_sessions(&engine.invocation, owned_lsp_sessions).await;
        return result;
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let initialized_project_server_ready =
            matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                && handshake.project_path.is_some()
                && engine.cached_project_server(&handshake).await?.is_some();
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&engine.store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if !initialized_project_server_ready
            && let Some(mut response) =
                daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            let project_open_error = if handshake.project_path.is_some()
                && matches!(
                    classify_mcp_method(&request.method),
                    McpMethod::Initialize | McpMethod::ToolsList
                ) {
                match engine.cached_project_open_failure(&handshake).await {
                    Ok(Some(failure)) => Some(failure.to_error()),
                    Ok(None)
                        if matches!(
                            classify_mcp_method(&request.method),
                            McpMethod::Initialize
                        ) =>
                    {
                        Box::pin(
                            engine
                                .schedule_project_server_warmup(handshake.clone(), request.clone()),
                        )
                        .await
                        .err()
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            if let Some(error) = project_open_error {
                response = request
                    .id
                    .clone()
                    .map(|id| project_open_error_response(id, &error));
            }
            // Keep catalog-refresh bookkeeping consistent with the regular MCP
            // server path: initialize and tools/list mark this catalog current.
            if let Some(key) = engine
                .claim_catalog_refresh(&handshake, &first_request_line)
                .await
                && let Err(error) = write_tool_list_changed_notification(&mut transport).await
            {
                engine.release_catalog_refresh(key).await;
                return Err(error);
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let user_session_request = projectless_user_session_request(&first_request_line);
    let mut pending_project_open_lines = VecDeque::new();
    let server = if handshake.project_path.is_some() && !user_session_request {
        match await_project_owner_or_disconnect(
            &mut transport,
            engine.project_server_for_request(
                &handshake,
                project_server_requirement(&first_request_line),
            ),
        )
        .await
        {
            Ok(Some((server, pending_lines))) => {
                pending_project_open_lines = pending_lines;
                Some(server)
            }
            Ok(None) => {
                drop(setup_activity);
                return Ok(());
            }
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    drop(setup_activity);
    if !engine.lifecycle.accepting() {
        return Ok(());
    }

    // The stdio proxy creates one daemon connection per request. The request
    // was peeked above so initialize-root routing happens before project open.
    if let Some(key) = engine
        .claim_catalog_refresh(&handshake, &first_request_line)
        .await
        && let Err(error) = write_tool_list_changed_notification(&mut transport).await
    {
        engine.release_catalog_refresh(key).await;
        return Err(error);
    }
    if let Some(server) = server {
        if is_mcp_initialize_request(&first_request_line) {
            #[cfg(test)]
            tests::record_mcp_route(&handshake.client_instance_id, tests::ObservedMcpRoute::Rmcp);
            serve_routed_rmcp_connection(
                server,
                transport,
                first_request_line,
                pending_project_open_lines,
                initialize_route,
                handshake.timings,
                &engine.lifecycle,
            )
            .await?;
        } else {
            #[cfg(test)]
            tests::record_mcp_route(
                &handshake.client_instance_id,
                tests::ObservedMcpRoute::Legacy,
            );
            let mut transport = ReplayTransport::new(transport);
            transport.push_replay(first_request_line)?;
            for line in pending_project_open_lines {
                transport.push_replay(line)?;
            }
            Box::pin(server.run_daemon_connection_with_timings(
                &mut transport,
                handshake.timings,
                &engine.lifecycle,
            ))
            .await?;
        }
    } else {
        let mut transport = ReplayTransport::new(transport);
        transport.push_replay(first_request_line)?;
        for line in pending_project_open_lines {
            transport.push_replay(line)?;
        }
        serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            &engine.lifecycle,
            &engine.store_administration,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) async fn serve_windows_broker_client(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    Box::pin(serve_windows_broker_client_with_class(
        stream,
        auth_token,
        lifecycle,
        store_administration,
        project_open_gates,
        DaemonPerClientAdmission::default(),
        DaemonClientAdmissionClass::General,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
}

#[cfg(test)]
// Cohesive per-connection serving context; bundling into a params struct would churn every caller.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_windows_broker_client_with_class(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    per_client_admission: DaemonPerClientAdmission,
    admission_class: DaemonClientAdmissionClass,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    Box::pin(serve_windows_broker_client_with_class_and_invocation(
        stream,
        auth_token,
        lifecycle,
        store_administration,
        project_open_gates,
        DaemonInvocationState::default(),
        http_application::DaemonHttpApplicationRegistry::default(),
        per_client_admission,
        admission_class,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
}

#[cfg(any(not(unix), test))]
// The foreground portable broker supplies one daemon-generation invocation state.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_windows_broker_client_with_class_and_invocation(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    per_client_admission: DaemonPerClientAdmission,
    admission_class: DaemonClientAdmissionClass,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    let Some(preface_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let preface =
        DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        })?;
    if !preface.authenticate(auth_token) {
        return Err(TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        });
    }
    let Some(handshake_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let Some(setup_activity) = lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&handshake_line)?;
    let Some(first_request_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    if let Some(response) = daemon_shutdown_response(&first_request_line) {
        lifecycle.begin_draining();
        write_json_rpc_response(&mut transport, &response).await?;
        drop(setup_activity);
        return Ok(());
    }
    let store_administration =
        bind_authenticated_profile_identity(&mut handshake, &store_administration).await?;
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match per_client_admission.try_admit_request(&handshake, &first_request_line) {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    let Some(setup_activity) = serve_core_doctor_runtime_request(
        &mut transport,
        &handshake,
        &store_administration,
        setup_activity,
        &first_request_line,
        || async {
            let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
            Ok(portable_cached_project_server(
                &store_administration,
                &canonical_project_path,
                &handshake,
                ProjectServerRequirement::Core,
            )
            .await?
            .is_some_and(|server| server.doctor_report_ready()))
        },
    )
    .await?
    else {
        return Ok(());
    };
    ensure_user_profile_host_admission_replay_for_identity(
        &store_administration,
        &handshake.client_identity,
    )
    .await?;
    let initialize_route =
        apply_daemon_initialize_route(&mut handshake, &first_request_line, &store_administration)
            .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => {
                store_administration
                    .execute_branch_admin_for_handshake(&handshake, action)
                    .await
            }
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response = match await_project_owner_or_disconnect(
            &mut transport,
            portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry.clone(),
                &handshake,
                ProjectServerRequirement::Core,
                #[cfg(test)]
                project_open_attempts.clone(),
            ),
        )
        .await
        {
            Ok(Some(_)) => branch_add_response(&store_administration, &handshake, &request).await,
            Ok(None) => return Ok(()),
            Err(error) => JsonRpcResponse::error(
                request.id.clone(),
                ErrorCode::InternalError,
                error.to_string(),
            ),
        };
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Some(invocation_request) = parse_daemon_invocation_request(&first_request_line) {
        let mut invocation_request = invocation_request;
        let mut owned_lsp_sessions = HashMap::new();
        let result = async {
            loop {
                let session_transition = invocation_request
                    .as_ref()
                    .ok()
                    .and_then(invocation_lsp_session_transition);
                let response = match invocation_request {
                    Ok(request) => {
                        execute_portable_daemon_invocation(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            &handshake,
                            &invocation,
                            http_application_registry.clone(),
                            request,
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        )
                        .await
                    }
                    Err(response) => response,
                };
                update_connection_lsp_sessions(
                    &mut owned_lsp_sessions,
                    session_transition.as_ref(),
                    &response,
                );
                write_daemon_invocation_response(&mut transport, &response).await?;
                let next_line = tokio::select! {
                    result = read_line_handling_wire_oversized(&mut transport) => result?,
                    () = lifecycle.wait_for_draining() => return Ok(()),
                };
                let Some(next_line) = next_line else {
                    return Ok(());
                };
                let Some(next_invocation) = parse_daemon_invocation_request(&next_line) else {
                    return Ok(());
                };
                invocation_request = next_invocation;
            }
        }
        .await;
        cleanup_connection_lsp_sessions(&invocation, owned_lsp_sessions).await;
        return result;
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let initialized_project_server_ready =
            if matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                && handshake.project_path.is_some()
            {
                let (project_path, _) = project_route_for_handshake(&handshake)?;
                portable_cached_project_server(
                    &store_administration,
                    &project_path,
                    &handshake,
                    ProjectServerRequirement::Core,
                )
                .await?
                .is_some()
            } else {
                false
            };
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if !initialized_project_server_ready
            && let Some(mut response) =
                daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            let project_open_error = if handshake.project_path.is_some()
                && matches!(
                    classify_mcp_method(&request.method),
                    McpMethod::Initialize | McpMethod::ToolsList
                ) {
                match portable_cached_project_open_failure(project_open_gates.as_ref(), &handshake)
                    .await
                {
                    Ok(Some(failure)) => Some(failure.to_error()),
                    Ok(None)
                        if matches!(
                            classify_mcp_method(&request.method),
                            McpMethod::Initialize
                        ) =>
                    {
                        Box::pin(schedule_portable_project_server_warmup(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            invocation.clone(),
                            http_application_registry.clone(),
                            handshake.clone(),
                            request.clone(),
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        ))
                        .await
                        .err()
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            if let Some(error) = project_open_error {
                response = request
                    .id
                    .clone()
                    .map(|id| project_open_error_response(id, &error));
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let user_session_request = projectless_user_session_request(&first_request_line);
    if handshake.project_path.is_some() && !user_session_request {
        let server = match await_project_owner_or_disconnect(
            &mut transport,
            portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry,
                &handshake,
                project_server_requirement(&first_request_line),
                #[cfg(test)]
                project_open_attempts.clone(),
            ),
        )
        .await
        {
            Ok(Some(server)) => server,
            Ok(None) => {
                drop(setup_activity);
                return Ok(());
            }
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        };
        drop(setup_activity);
        let (server, pending_lines) = server;
        if is_mcp_initialize_request(&first_request_line) {
            #[cfg(test)]
            tests::record_mcp_route(&handshake.client_instance_id, tests::ObservedMcpRoute::Rmcp);
            serve_routed_rmcp_connection(
                server,
                transport,
                first_request_line,
                pending_lines,
                initialize_route,
                handshake.timings,
                lifecycle,
            )
            .await?;
        } else {
            #[cfg(test)]
            tests::record_mcp_route(
                &handshake.client_instance_id,
                tests::ObservedMcpRoute::Legacy,
            );
            let mut transport = ReplayTransport::new(transport);
            transport.push_replay(first_request_line)?;
            for line in pending_lines {
                transport.push_replay(line)?;
            }
            Box::pin(server.run_daemon_connection_with_timings(
                &mut transport,
                handshake.timings,
                lifecycle,
            ))
            .await?;
        }
    } else {
        drop(setup_activity);
        let mut transport = ReplayTransport::new(transport);
        transport.push_replay(first_request_line)?;
        Box::pin(serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            lifecycle,
            &store_administration,
        ))
        .await?;
    }
    Ok(())
}

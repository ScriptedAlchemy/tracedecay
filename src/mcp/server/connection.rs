//! Connection lifecycle: the JSON-RPC read/write loop, shutdown
//! policy, and daemon-owned host-admission replay driving.

use super::*;

const MAX_PENDING_CANCELLABLE_REQUEST_LINES: usize = 64;

fn queued_cancellable_request_key(
    pending_lines: &VecDeque<String>,
    request_id: &Value,
    connection_scope: &str,
) -> Option<String> {
    let expected = application_surface_request_id(request_id, connection_scope)?;
    pending_lines
        .iter()
        .any(|line| {
            let Ok(request) = serde_json::from_str::<JsonRpcRequest>(line.trim()) else {
                return false;
            };
            request.method == "tools/call"
                && request
                    .params
                    .as_ref()
                    .and_then(|params| params.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(super::requests::tool_supports_live_cancellation)
                && request
                    .id
                    .as_ref()
                    .and_then(|id| application_surface_request_id(id, connection_scope))
                    .as_ref()
                    == Some(&expected)
        })
        .then_some(expected)
}

impl McpServer {
    async fn write_response_line_or_revoke(
        &self,
        transport: &mut impl crate::mcp::transport::McpTransport,
        output: &str,
        revocable: bool,
    ) -> std::io::Result<bool> {
        let write = async {
            transport.write_line(output).await?;
            transport.flush().await
        };
        if !revocable {
            return write.await.map(|()| true);
        }
        tokio::select! {
            biased;
            () = self.project_server_lifecycle.response_revoked().cancelled() => Ok(false),
            result = write => result.map(|()| true),
        }
    }

    async fn handle_cancellable_application_request(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        connection: &mut ConnectionRouteState,
        transport: &mut impl crate::mcp::transport::McpTransport,
        pending_lines: &mut VecDeque<String>,
        pending_cancellations: &mut HashSet<String>,
        mut shutdown_requested: std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
    ) -> Result<(Option<JsonRpcResponse>, bool)> {
        let connection_scope = connection.memory_request_scope().to_owned();
        let pre_cancelled = request
            .id
            .as_ref()
            .and_then(|id| application_surface_request_id(id, &connection_scope))
            .is_some_and(|key| pending_cancellations.remove(&key));
        let handling = Box::pin(self.handle_request_for_connection(
            request,
            timings_enabled,
            connection,
            pre_cancelled,
        ));
        tokio::pin!(handling);
        // One-shot clients (the CLI and the stdio proxy) shut down their write
        // half once the request is on the wire, so end-of-input means "no more
        // requests", not "peer is gone". Stop watching for cancellations and
        // keep serving the in-flight response. Cancel only on actual peer loss
        // (read/write I/O failure) or explicit shutdown/cancel paths.
        let mut peer_close_check: Option<
            std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>,
        > = None;
        loop {
            if let Some(peer_close_check) = peer_close_check.as_mut() {
                tokio::select! {
                    response = &mut handling => return Ok((response, false)),
                    () = &mut shutdown_requested => {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(id, &connection_scope);
                        }
                        return Ok((None, true));
                    }
                    () = peer_close_check => {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(id, &connection_scope);
                        }
                        return Ok((None, true));
                    }
                }
            }
            tokio::select! {
                response = &mut handling => return Ok((response, false)),
                () = &mut shutdown_requested => {
                    if let Some(id) = request.id.as_ref() {
                        let _ = self.cancel_application_surface_request(id, &connection_scope);
                    }
                    return Ok((None, true));
                }
                incoming = transport.read_line() => {
                    let line = match incoming {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            peer_close_check = Some(Box::pin(
                                transport.peer_fully_closed_after_eof(),
                            ));
                            continue;
                        }
                        Err(error) => {
                            if let Some(id) = request.id.as_ref() {
                                let _ = self.cancel_application_surface_request(id, &connection_scope);
                            }
                            return Err(error.into());
                        }
                    };
                    let parsed = serde_json::from_str::<JsonRpcRequest>(line.trim());
                    if let Ok(notification) = &parsed
                        && matches!(
                            classify_mcp_method(&notification.method),
                            McpMethod::Cancelled
                        )
                    {
                        if let Some(id) = notification
                            .params
                            .as_ref()
                            .and_then(|params| params.get("requestId"))
                            && !self.cancel_application_surface_request(id, &connection_scope)
                                && pending_cancellations.len()
                                    < MAX_PENDING_CANCELLABLE_REQUEST_LINES
                                && let Some(key) = queued_cancellable_request_key(
                                    pending_lines,
                                    id,
                                    &connection_scope,
                                )
                            {
                                pending_cancellations.insert(key);
                            }
                        continue;
                    }
                    if pending_lines.len() >= MAX_PENDING_CANCELLABLE_REQUEST_LINES {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(id, &connection_scope);
                        }
                        return Ok((None, true));
                    }
                    pending_lines.push_back(line);
                }
            }
        }
    }

    /// Runs a non-live-cancellable request while still observing connection
    /// teardown.  A request-side EOF is only a half-close until the transport
    /// reports the peer's write side closed; this keeps one-shot CLI responses
    /// intact while dropping abandoned handlers and their admission permits.
    async fn handle_non_cancellable_application_request(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        connection: &mut ConnectionRouteState,
        transport: &mut impl crate::mcp::transport::McpTransport,
        pending_lines: &mut VecDeque<String>,
        mut shutdown_requested: std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
    ) -> Result<(Option<JsonRpcResponse>, bool)> {
        let connection_scope = connection.memory_request_scope().to_owned();
        let handling = Box::pin(self.handle_request_for_connection(
            request,
            timings_enabled,
            connection,
            false,
        ));
        tokio::pin!(handling);
        let mut peer_close_check: Option<
            std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>,
        > = None;
        loop {
            if let Some(peer_close_check) = peer_close_check.as_mut() {
                tokio::select! {
                    response = &mut handling => return Ok((response, false)),
                    () = &mut shutdown_requested => {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(
                                id,
                                &connection_scope,
                            );
                        }
                        return Ok((None, true));
                    }
                    () = peer_close_check => {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(
                                id,
                                &connection_scope,
                            );
                        }
                        return Ok((None, true));
                    }
                }
            }
            tokio::select! {
                response = &mut handling => return Ok((response, false)),
                () = &mut shutdown_requested => {
                    if let Some(id) = request.id.as_ref() {
                        let _ = self.cancel_application_surface_request(
                            id,
                            &connection_scope,
                        );
                    }
                    return Ok((None, true));
                }
                incoming = transport.read_line() => {
                    let line = match incoming {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            peer_close_check = Some(Box::pin(
                                transport.peer_fully_closed_after_eof(),
                            ));
                            continue;
                        }
                        Err(error) => {
                            if let Some(id) = request.id.as_ref() {
                                let _ = self.cancel_application_surface_request(
                                    id,
                                    &connection_scope,
                                );
                            }
                            return Err(error.into());
                        }
                    };
                    if pending_lines.len() >= MAX_PENDING_CANCELLABLE_REQUEST_LINES {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(
                                id,
                                &connection_scope,
                            );
                        }
                        return Ok((None, true));
                    }
                    pending_lines.push_back(line);
                }
            }
        }
    }

    /// Process a single raw JSON-RPC line and write the response.
    /// Used to replay a peeked `initialize` message that was consumed before
    /// the server's main loop started.
    pub async fn handle_and_write(
        &self,
        line: &str,
        transport: &mut impl crate::mcp::transport::McpTransport,
    ) -> Result<()> {
        let parsed: std::result::Result<crate::mcp::transport::JsonRpcRequest, _> =
            serde_json::from_str(line);
        let response = match parsed {
            Ok(request) => Box::pin(self.handle_request(&request)).await,
            Err(e) => Some(crate::mcp::transport::JsonRpcResponse::error(
                Value::Null,
                crate::mcp::transport::ErrorCode::ParseError,
                format!("failed to parse JSON-RPC request: {e}"),
            )),
        };
        if let Some(resp) = response {
            let mut json_str = serialize_response_line(&resp);
            json_str.push('\n');
            transport.write_line(&json_str).await?;
            transport.flush().await?;
        }
        Ok(())
    }

    /// Runs the server, reading JSON-RPC requests from stdin and writing
    /// responses to stdout. Runs until stdin is closed or a shutdown signal
    /// (SIGINT/SIGTERM) is received, then performs graceful cleanup.
    pub async fn run(
        &self,
        transport: &mut impl crate::mcp::transport::McpTransport,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, true, true, None, None)
            .await
    }

    /// Runs one client connection without shutting down the server when that
    /// connection closes. Daemon-owned servers use this so the engine remains
    /// shared across independent clients.
    pub async fn run_connection(
        &self,
        transport: &mut impl crate::mcp::transport::McpTransport,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, None, None)
            .await
    }

    /// Runs one daemon client connection using connection-local timing
    /// settings. The shared server's default timing flag remains unchanged.
    pub async fn run_connection_with_timings(
        &self,
        transport: &mut impl crate::mcp::transport::McpTransport,
        timings_enabled: bool,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, Some(timings_enabled), None)
            .await
    }

    pub(crate) async fn run_daemon_connection_with_timings(
        &self,
        transport: &mut impl crate::mcp::transport::McpTransport,
        timings_enabled: bool,
        lifecycle: &crate::daemon::DaemonLifecycle,
    ) -> Result<()> {
        self.run_with_shutdown_policy(
            transport,
            false,
            false,
            Some(timings_enabled),
            Some(lifecycle),
        )
        .await
    }

    pub(crate) async fn run_with_shutdown_policy(
        &self,
        transport: &mut impl crate::mcp::transport::McpTransport,
        shutdown_on_exit: bool,
        listen_for_process_signals: bool,
        timings_override: Option<bool>,
        request_lifecycle: Option<&crate::daemon::DaemonLifecycle>,
    ) -> Result<()> {
        // Register the SIGTERM listener once before entering the loop so
        // there is no window between iterations where a SIGTERM is delivered
        // but no handler is installed (which would cause silent loss of the
        // signal and skip the shutdown() flush).
        #[cfg(unix)]
        #[allow(clippy::expect_used)]
        let mut sigterm = listen_for_process_signals.then(|| {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler")
        });

        let mut connection_route =
            self.new_connection_route_state()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("MCP connection identity unavailable: {error}"),
                })?;
        let mut pending_lines = VecDeque::new();
        let mut pending_cancellations = HashSet::new();

        'connection: loop {
            let line: String = if let Some(line) = pending_lines.pop_front() {
                line
            } else {
                let read = {
                    #[cfg(unix)]
                    {
                        if let Some(sigterm) = sigterm.as_mut() {
                            tokio::select! {
                                result = transport.read_line() => result,
                                _ = tokio::signal::ctrl_c() => break,
                                _ = sigterm.recv() => break,
                            }
                        } else if let Some(lifecycle) = request_lifecycle {
                            tokio::select! {
                                result = transport.read_line() => result,
                                () = lifecycle.wait_for_draining() => break,
                            }
                        } else {
                            transport.read_line().await
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        if listen_for_process_signals {
                            tokio::select! {
                                result = transport.read_line() => result,
                                _ = tokio::signal::ctrl_c() => break,
                            }
                        } else {
                            transport.read_line().await
                        }
                    }
                };
                match read {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        if is_wire_oversized_io_error(&e) {
                            let _ = write_wire_oversized_rejection(transport, &e).await;
                            break;
                        }
                        self.shutdown_if(shutdown_on_exit).await;
                        return Err(e.into());
                    }
                }
            };

            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let parsed: std::result::Result<JsonRpcRequest, _> = serde_json::from_str(&line);
            let revocable_tool_call = parsed.as_ref().ok().and_then(|request| {
                (request.method == "tools/call").then_some(())?;
                let id = request.id.clone()?;
                let tool_name = request.params.as_ref()?.get("name")?.as_str()?.to_owned();
                Some((id, tool_name))
            });
            let project_tool_call = parsed
                .as_ref()
                .is_ok_and(|request| request.method == "tools/call")
                && self.project_server_live.is_some();
            let project_request_guard = if project_tool_call {
                Some(self.project_server_lifecycle.response_gate().read().await)
            } else {
                None
            };
            let project_request_admitted = project_request_guard.is_none()
                || !self
                    .project_server_lifecycle
                    .response_revoked()
                    .is_cancelled();
            let request_activity =
                request_lifecycle.and_then(crate::daemon::DaemonLifecycle::try_enter);
            let rejecting_for_drain = request_lifecycle.is_some() && request_activity.is_none();
            let mut peer_closed = false;

            let response = if rejecting_for_drain {
                parsed.as_ref().ok().and_then(|request| {
                    request.id.clone().map(|id| {
                        JsonRpcResponse::error(
                            id,
                            ErrorCode::InternalError,
                            "TraceDecay daemon is draining for upgrade; retry the request"
                                .to_string(),
                        )
                    })
                })
            } else if !project_request_admitted {
                revocable_tool_call.as_ref().map(|(id, tool_name)| {
                    JsonRpcResponse::error_with_data(
                        id.clone(),
                        ErrorCode::InternalError,
                        "tool project route failed: project server was retired".to_owned(),
                        Some(serde_json::json!({
                            "tool": tool_name,
                            "reason_code": "project_server_retired",
                            "retryable": true,
                            "detail": "the retained project server was replaced or revoked; retry against the current owner",
                        })),
                    )
                })
            } else {
                match parsed {
                    Ok(request) => {
                        if matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                            && self.initialize_root_routing_enabled.load(Ordering::Relaxed)
                        {
                            connection_route
                                .observe_initialize(
                                    request.params.as_ref(),
                                    self.registry_db.as_deref(),
                                )
                                .await;
                        }
                        let cancellable_tool_call = request.method == "tools/call"
                            && request
                                .params
                                .as_ref()
                                .and_then(|params| params.get("name"))
                                .and_then(Value::as_str)
                                .is_some_and(super::requests::tool_supports_live_cancellation);
                        if cancellable_tool_call {
                            let external_shutdown_requested = async {
                                if listen_for_process_signals {
                                    #[cfg(unix)]
                                    {
                                        if let Some(sigterm) = sigterm.as_mut() {
                                            tokio::select! {
                                                _ = tokio::signal::ctrl_c() => {}
                                                _ = sigterm.recv() => {}
                                            }
                                        } else {
                                            let _ = tokio::signal::ctrl_c().await;
                                        }
                                    }
                                    #[cfg(not(unix))]
                                    {
                                        let _ = tokio::signal::ctrl_c().await;
                                    }
                                } else if let Some(lifecycle) = request_lifecycle {
                                    lifecycle.wait_for_draining().await;
                                } else {
                                    std::future::pending::<()>().await;
                                }
                            };
                            tokio::pin!(external_shutdown_requested);
                            let shutdown_requested = async {
                                tokio::select! {
                                    () = &mut external_shutdown_requested => {}
                                    () = self.project_server_lifecycle.request_abort().cancelled() => {}
                                }
                            };
                            tokio::pin!(shutdown_requested);
                            let (response, closed) = self
                                .handle_cancellable_application_request(
                                    &request,
                                    timings_override.unwrap_or_else(|| self.timings_enabled()),
                                    &mut connection_route,
                                    transport,
                                    &mut pending_lines,
                                    &mut pending_cancellations,
                                    shutdown_requested.as_mut(),
                                )
                                .await?;
                            peer_closed = closed;
                            response
                        } else {
                            let external_shutdown_requested = async {
                                if listen_for_process_signals {
                                    #[cfg(unix)]
                                    {
                                        if let Some(sigterm) = sigterm.as_mut() {
                                            tokio::select! {
                                                _ = tokio::signal::ctrl_c() => {}
                                                _ = sigterm.recv() => {}
                                            }
                                        } else {
                                            let _ = tokio::signal::ctrl_c().await;
                                        }
                                    }
                                    #[cfg(not(unix))]
                                    {
                                        let _ = tokio::signal::ctrl_c().await;
                                    }
                                } else if let Some(lifecycle) = request_lifecycle {
                                    lifecycle.wait_for_draining().await;
                                } else {
                                    std::future::pending::<()>().await;
                                }
                            };
                            tokio::pin!(external_shutdown_requested);
                            let shutdown_requested = async {
                                tokio::select! {
                                    () = &mut external_shutdown_requested => {}
                                    () = self.project_server_lifecycle.request_abort().cancelled() => {}
                                }
                            };
                            tokio::pin!(shutdown_requested);
                            let (response, closed) = self
                                .handle_non_cancellable_application_request(
                                    &request,
                                    timings_override.unwrap_or_else(|| self.timings_enabled()),
                                    &mut connection_route,
                                    transport,
                                    &mut pending_lines,
                                    shutdown_requested.as_mut(),
                                )
                                .await?;
                            peer_closed = closed;
                            response
                        }
                    }
                    Err(e) => Some(JsonRpcResponse::error(
                        Value::Null,
                        ErrorCode::ParseError,
                        format!("failed to parse JSON-RPC request: {e}"),
                    )),
                }
            };

            if peer_closed {
                drop(project_request_guard);
                drop(request_activity);
                break;
            }

            let revocable_response = project_tool_call && !project_request_admitted;

            // Drain and write any pending notifications (e.g., version warnings).
            {
                let notifications: Vec<Value> =
                    crate::mcp::server::requests::recover_lock(&self.pending_notifications)
                        .drain(..)
                        .collect();
                for notification in notifications {
                    if let Ok(s) = serde_json::to_string(&notification) {
                        match self
                            .write_response_line_or_revoke(
                                transport,
                                &format!("{s}\n"),
                                revocable_response,
                            )
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => break 'connection,
                            Err(error) => {
                                if let Some((id, _)) = &revocable_tool_call {
                                    let _ = self.cancel_application_surface_request(
                                        id,
                                        connection_route.memory_request_scope(),
                                    );
                                }
                                self.shutdown_if(shutdown_on_exit).await;
                                return Err(error.into());
                            }
                        }
                    }
                }
            }

            if let Some(resp) = response {
                let json_line = serialize_response_line(&resp);
                let output = format!("{json_line}\n");
                match self
                    .write_response_line_or_revoke(transport, &output, revocable_response)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => break 'connection,
                    Err(error) => {
                        tracing::error!(error = %error, "failed to write MCP response");
                        if let Some((id, _)) = &revocable_tool_call {
                            let _ = self.cancel_application_surface_request(
                                id,
                                connection_route.memory_request_scope(),
                            );
                        }
                        self.shutdown_if(shutdown_on_exit).await;
                        return Err(error.into());
                    }
                }
            }
            drop(project_request_guard);
            drop(request_activity);
            if rejecting_for_drain
                || request_lifecycle.is_some_and(|lifecycle| !lifecycle.accepting())
            {
                break;
            }
        }

        self.shutdown_if(shutdown_on_exit).await;
        Ok(())
    }

    pub(crate) async fn shutdown_if(&self, enabled: bool) {
        if enabled {
            self.shutdown().await;
        }
    }

    /// Persists the tokens-saved counter, flushes pending tokens to the
    /// worldwide counter, checkpoints the WAL, and logs a session summary.
    ///
    /// Idempotent — safe to call multiple times. `run` invokes it once when
    /// its main loop exits; callers (e.g. `main.rs`, tests) may invoke it
    /// explicitly afterwards without re-running the persistence logic.
    pub async fn shutdown(&self) {
        // Idempotency guard: only run the persistence path once.
        if self.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }

        self.shutdown_background_tasks().await;

        let uptime = self.stats.started_at.elapsed();
        let tool_calls = self.stats.tool_calls.load(Ordering::Relaxed);
        let tokens_saved = self.tokens_saved.load(Ordering::Relaxed);

        let cg = self.cg_snapshot().await;
        // Persist final tokens-saved value
        if let Err(e) = cg.set_tokens_saved(tokens_saved).await {
            tracing::warn!(error = %e, "failed to persist tokens saved during shutdown");
        }

        if let Some(ref gdb) = self.accounting_db {
            gdb.upsert(cg.project_root(), tokens_saved).await;
            gdb.checkpoint().await;
        } else if let Some(ref gdb) = self.global_db {
            gdb.upsert(cg.project_root(), tokens_saved).await;
            gdb.checkpoint().await;
        }

        // Flush remaining delta to worldwide counter (what periodic flushes missed)
        let last_flushed = self.last_flushed_tokens.load(Ordering::Relaxed);
        if (self.accounting_db.is_some() || self.global_db.is_some()) && tokens_saved > last_flushed
        {
            let delta = tokens_saved - last_flushed;
            let mut config = crate::user_config::UserConfig::load();
            config.pending_upload += delta;
            if config.upload_enabled
                && let Some(_total) = crate::cloud::flush_pending(config.pending_upload)
            {
                config.pending_upload = 0;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                config.last_upload_at = now;
            }
            if let Err(err) = config.save() {
                tracing::warn!(error = %err, "could not save upload config during shutdown");
            }
        }

        // Checkpoint WAL to merge it into the main database file
        if let Err(e) = cg.checkpoint().await {
            tracing::warn!(error = %e, "failed to checkpoint WAL during shutdown");
        }

        tracing::info!(
            tool_calls,
            tokens_saved,
            uptime_secs = uptime.as_secs(),
            "MCP server shutdown complete"
        );
    }

    pub(crate) async fn shutdown_background_tasks(&self) {
        if let Some(worker) = self.project_host_admission_replay.lock().await.take() {
            worker.shutdown().await;
        }
        // Same ordering as before the state machine landed: the index-sync
        // task is aborted and joined first, then the ingest is cancelled,
        // joined, and the machine marked cancelled.
        self.shutdown_startup_catch_up_sync().await;
        self.shutdown_startup_transcript_ingest().await;
    }

    pub(crate) async fn replay_host_admission(
        &self,
        target_seq: Option<u64>,
    ) -> HostAdmissionOutcome {
        const MAX_RECORDS_PER_PASS: usize = 64;

        let Some(broker) = self.host_admission_broker.as_ref() else {
            return HostAdmissionOutcome::retained_unavailable("spool_unavailable");
        };
        let replay = match broker.begin_replay().await {
            Ok(replay) => replay,
            Err(outcome) => return outcome,
        };
        let mut attempted = HashSet::new();
        let mut blocked_sources = HashSet::new();
        let mut retained_leases = Vec::new();
        let mut non_committed_outcome = None;
        let mut target_outcome = None;
        let mut terminal_outcome = None;
        for _ in 0..MAX_RECORDS_PER_PASS {
            let record = match replay.lease_next().await {
                Ok(Some(record)) => record,
                Ok(None) => break,
                Err(outcome) => {
                    terminal_outcome = Some(outcome);
                    break;
                }
            };
            if blocked_sources.contains(&record.source) {
                retained_leases.push(record.seq);
                continue;
            }
            if !attempted.insert(record.seq) {
                let outcome = HostAdmissionOutcome::spool_ack_conflict();
                blocked_sources.insert(record.source);
                retained_leases.push(record.seq);
                non_committed_outcome.get_or_insert(outcome);
                if target_seq == Some(record.seq) {
                    target_outcome = Some(outcome);
                }
                continue;
            }
            let plan = match hook_events::decode_durable_hook_event_plan(&record.payload) {
                Ok(plan) => plan,
                Err(hook_events::DurableHookEventDecodeError::UnsupportedVersion) => {
                    let outcome = HostAdmissionOutcome::durable_payload_unsupported_version();
                    blocked_sources.insert(record.source);
                    retained_leases.push(record.seq);
                    non_committed_outcome.get_or_insert(outcome);
                    if target_seq == Some(record.seq) {
                        target_outcome = Some(outcome);
                    }
                    continue;
                }
                Err(hook_events::DurableHookEventDecodeError::Malformed) => {
                    let outcome = HostAdmissionOutcome::durable_payload_malformed();
                    match replay
                        .quarantine(record.seq, TerminalReason::MalformedPayload)
                        .await
                    {
                        Ok(_) => {
                            non_committed_outcome.get_or_insert(outcome);
                            if target_seq == Some(record.seq) {
                                target_outcome = Some(outcome);
                            }
                        }
                        Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                            blocked_sources.insert(record.source);
                            retained_leases.push(record.seq);
                            non_committed_outcome.get_or_insert(failure);
                            if target_seq == Some(record.seq) {
                                target_outcome = Some(failure);
                            }
                        }
                        Err(failure) => {
                            terminal_outcome = Some(failure);
                            break;
                        }
                    }
                    continue;
                }
            };
            let cg = self.reopen_if_branch_drifted().await;
            let root = cg.project_root().to_path_buf();
            let canonical_outcome = Box::pin(self.run_hook_event_plan(cg, &root, plan)).await;
            let outcome = if canonical_outcome.reason_code == Some("stale_branch_authorization")
                && !canonical_outcome.retryable
            {
                match replay
                    .quarantine(record.seq, TerminalReason::StaleBranchAuthorization)
                    .await
                {
                    Ok(_) => {
                        non_committed_outcome.get_or_insert(canonical_outcome);
                        canonical_outcome
                    }
                    Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                        blocked_sources.insert(record.source);
                        retained_leases.push(record.seq);
                        non_committed_outcome.get_or_insert(failure);
                        failure
                    }
                    Err(failure) => {
                        terminal_outcome = Some(failure);
                        break;
                    }
                }
            } else if matches!(
                canonical_outcome.status,
                HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
            ) {
                match replay.commit(record.seq).await {
                    Ok(_) => canonical_outcome,
                    Err(outcome) => {
                        terminal_outcome = Some(outcome);
                        break;
                    }
                }
            } else {
                blocked_sources.insert(record.source);
                retained_leases.push(record.seq);
                non_committed_outcome.get_or_insert(canonical_outcome);
                canonical_outcome
            };
            if target_seq == Some(record.seq) {
                target_outcome = Some(outcome);
            }
        }
        for seq in retained_leases.into_iter().rev() {
            if let Err(outcome) = replay.defer(seq).await {
                return outcome;
            }
        }
        terminal_outcome
            .or(target_outcome)
            .or(non_committed_outcome)
            .unwrap_or_else(HostAdmissionOutcome::accepted_for_replay)
    }

    pub(crate) fn report_host_admission_outcome(outcome: HostAdmissionOutcome) {
        if outcome.status.is_replay_progress() {
            return;
        }
        tracing::warn!(
            reason_code = outcome.reason_code.unwrap_or("host_admission_unavailable"),
            "host admission did not make replay progress"
        );
    }

    #[cfg(test)]
    pub(crate) async fn wait_project_host_admission_replay_idle(&self, timeout: Duration) -> bool {
        let worker = self
            .project_host_admission_replay
            .lock()
            .await
            .as_ref()
            .map(|task| Arc::clone(task.worker()));
        match worker {
            Some(worker) => worker.wait_idle(timeout).await,
            None => true,
        }
    }

    #[cfg(test)]
    pub(crate) async fn project_host_admission_replay_pass_count(&self) -> usize {
        let guard = self.project_host_admission_replay.lock().await;
        guard.as_ref().map_or(
            0,
            project_host_admission_replay::ProjectHostAdmissionReplayTask::pass_count,
        )
    }

    #[cfg(test)]
    pub(crate) async fn project_host_admission_replay_backoff_count(&self) -> usize {
        let guard = self.project_host_admission_replay.lock().await;
        guard.as_ref().map_or(
            0,
            project_host_admission_replay::ProjectHostAdmissionReplayTask::backoff_count,
        )
    }
}

#[cfg(test)]
mod cancellable_queue_tests {
    use super::*;

    #[test]
    fn queued_request_cancellation_is_type_preserving() {
        let pending = VecDeque::from([
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": "1",
                "method": "tools/call",
                "params": {"name": "tracedecay_search", "arguments": {"query": "queued"}},
            })
            .to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "tracedecay_git_status", "arguments": {}},
            })
            .to_string(),
        ]);

        assert!(
            queued_cancellable_request_key(&pending, &serde_json::json!("1"), "connection")
                .is_some()
        );
        assert!(
            queued_cancellable_request_key(&pending, &serde_json::json!(1), "connection").is_none()
        );
        assert!(
            queued_cancellable_request_key(&pending, &serde_json::json!(2), "connection").is_some()
        );
    }
}

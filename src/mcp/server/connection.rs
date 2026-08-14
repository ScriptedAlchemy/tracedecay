//! Connection lifecycle: the JSON-RPC read/write loop, shutdown
//! policy, and daemon-owned host-admission replay driving.

use super::*;

mod response_delivery;

const MAX_PENDING_CANCELLABLE_REQUEST_LINES: usize = 64;

pub(in crate::mcp::server) struct McpShutdownCompletion {
    state: Arc<McpShutdownState>,
}

#[derive(Default)]
struct McpShutdownState {
    running: AtomicBool,
    terminal: std::sync::Mutex<Option<crate::daemon::ShutdownStatus>>,
    coordinator_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    changed: tokio::sync::Notify,
}

struct McpShutdownCoordinatorCompletion(Arc<McpShutdownState>);

impl Drop for McpShutdownCoordinatorCompletion {
    fn drop(&mut self) {
        self.0.changed.notify_waiters();
    }
}

impl Default for McpShutdownCompletion {
    fn default() -> Self {
        Self {
            state: Arc::new(McpShutdownState::default()),
        }
    }
}

impl McpShutdownCompletion {
    async fn coordinate_until<Work>(
        &self,
        deadline: tokio::time::Instant,
        work: Work,
    ) -> crate::daemon::ShutdownStatus
    where
        Work: std::future::Future<Output = crate::daemon::ShutdownStatus> + Send + 'static,
    {
        let mut work = Some(work);
        loop {
            self.join_finished_coordinator().await;
            if !self.state.running.load(Ordering::Acquire) {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
            }
            if let Some(status) = self.terminal_status() {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }

            let mut coordinator_task = self.state.coordinator_task.lock().await;
            if coordinator_task.is_some() {
                let running = self.state.running.load(Ordering::Acquire);
                drop(coordinator_task);
                if running {
                    return self.wait_for_terminal_status_until(deadline).await;
                }
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                continue;
            }
            if self
                .state
                .running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                drop(coordinator_task);
                return self.wait_for_terminal_status_until(deadline).await;
            }
            if let Some(status) = self.terminal_status() {
                self.state.running.store(false, Ordering::Release);
                drop(coordinator_task);
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }

            let Some(work) = work.take() else {
                self.state.finish(crate::daemon::ShutdownStatus::Failed(
                    "MCP shutdown coordinator lost its work future".to_owned(),
                ));
                drop(coordinator_task);
                return crate::daemon::ShutdownStatus::Failed(
                    "MCP shutdown coordinator lost its work future".to_owned(),
                );
            };
            let state = Arc::clone(&self.state);
            let task = tokio::spawn(async move {
                let _completion = McpShutdownCoordinatorCompletion(Arc::clone(&state));
                let runner = tokio::spawn(work);
                let status = match runner.await {
                    Ok(status) => status,
                    Err(error) => crate::daemon::ShutdownStatus::Failed(error.to_string()),
                };
                state.finish(status);
            });
            *coordinator_task = Some(task);
            drop(coordinator_task);
            return self.wait_for_terminal_status_until(deadline).await;
        }
    }

    async fn join_finished_coordinator(&self) {
        let result = {
            let mut coordinator_task = self.state.coordinator_task.lock().await;
            let Some(task) = coordinator_task.as_mut() else {
                return;
            };
            if !task.is_finished() {
                return;
            }
            let result = task.await;
            coordinator_task.take();
            result
        };
        if let Err(error) = result {
            tracing::error!(%error, "MCP shutdown coordinator task failed after receipt");
            self.state
                .finish(crate::daemon::ShutdownStatus::Failed(error.to_string()));
        }
    }

    async fn wait_for_finished_coordinator(&self) {
        loop {
            let notified = self.state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let finished = self
                .state
                .coordinator_task
                .lock()
                .await
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished);
            if finished {
                return;
            }
            notified.as_mut().await;
        }
    }

    fn terminal_status(&self) -> Option<crate::daemon::ShutdownStatus> {
        self.state
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn wait_for_terminal_status_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        loop {
            if let Some(status) = self.terminal_status() {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }
            if !self.state.running.load(Ordering::Acquire) {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return crate::daemon::ShutdownStatus::TimedOut;
            }
            let notified = self.state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(status) = self.terminal_status() {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }
            if !self.state.running.load(Ordering::Acquire) {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return crate::daemon::ShutdownStatus::TimedOut;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return crate::daemon::ShutdownStatus::TimedOut;
            }
        }
    }
}

impl McpShutdownState {
    fn finish(&self, status: crate::daemon::ShutdownStatus) {
        if status != crate::daemon::ShutdownStatus::TimedOut {
            *self
                .terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
        }
        self.running.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }
}

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

fn current_cancellable_request_key(
    request: &JsonRpcRequest,
    request_id: &Value,
    connection_scope: &str,
) -> Option<String> {
    let current = request
        .id
        .as_ref()
        .and_then(|id| application_surface_request_id(id, connection_scope))?;
    let cancelled = application_surface_request_id(request_id, connection_scope)?;
    (current == cancelled).then_some(current)
}

impl McpServer {
    async fn write_response_line_or_revoke(
        &self,
        transport: &mut impl crate::mcp::transport::McpTransport,
        output: &str,
        response_revoked: Option<&tracedecay_usecases::context::CancellationToken>,
    ) -> std::io::Result<bool> {
        let write = async {
            transport.write_line(output).await?;
            transport.flush().await
        };
        let Some(response_revoked) = response_revoked else {
            return write.await.map(|()| true);
        };
        tokio::select! {
            biased;
            () = response_revoked.cancelled() => Ok(false),
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
        let mut current_cancellation: Option<Value> = None;
        // One-shot clients (the CLI and the stdio proxy) shut down their write
        // half once the request is on the wire, so end-of-input means "no more
        // requests", not "peer is gone". Stop watching for cancellations and
        // keep serving the in-flight response. Cancel only on actual peer loss
        // (read/write I/O failure) or explicit shutdown/cancel paths.
        let mut peer_close_check: Option<
            std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>,
        > = None;
        loop {
            let cancellation_id = current_cancellation.clone();
            let wait_for_current_cancellation_registration = async {
                let Some(cancellation_id) = cancellation_id.as_ref() else {
                    std::future::pending::<()>().await;
                    return;
                };
                while !self.cancel_application_surface_request(cancellation_id, &connection_scope) {
                    tokio::task::yield_now().await;
                }
            };
            tokio::pin!(wait_for_current_cancellation_registration);
            if let Some(peer_close_check) = peer_close_check.as_mut() {
                tokio::select! {
                    biased;
                    () = &mut shutdown_requested => {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(id, &connection_scope);
                        }
                        return Ok((None, true));
                    }
                    () = &mut wait_for_current_cancellation_registration => {
                        current_cancellation = None;
                    }
                    response = &mut handling => return Ok((response, false)),
                    () = peer_close_check => {
                        if let Some(id) = request.id.as_ref() {
                            let _ = self.cancel_application_surface_request(id, &connection_scope);
                        }
                        return Ok((None, true));
                    }
                }
            }
            tokio::select! {
                biased;
                () = &mut shutdown_requested => {
                    if let Some(id) = request.id.as_ref() {
                        let _ = self.cancel_application_surface_request(id, &connection_scope);
                    }
                    return Ok((None, true));
                }
                () = &mut wait_for_current_cancellation_registration => {
                    current_cancellation = None;
                }
                response = &mut handling => return Ok((response, false)),
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
                        {
                            if current_cancellable_request_key(
                                request,
                                id,
                                &connection_scope,
                            )
                            .is_some()
                            {
                                current_cancellation = Some(id.clone());
                            } else if pending_cancellations.len()
                                    < MAX_PENDING_CANCELLABLE_REQUEST_LINES
                                && let Some(key) = queued_cancellable_request_key(
                                    pending_lines,
                                    id,
                                    &connection_scope,
                                )
                            {
                                pending_cancellations.insert(key);
                            }
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

    /// Runs the server, reading JSON-RPC requests from stdin and writing
    /// responses to stdout. Runs until stdin is closed or a shutdown signal
    /// (SIGINT/SIGTERM) is received, then performs graceful cleanup.
    pub async fn run(
        self: &Arc<Self>,
        transport: &mut impl crate::mcp::transport::McpTransport,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, true, true, None, None)
            .await
    }

    /// Runs one client connection without shutting down the server when that
    /// connection closes. Daemon-owned servers use this so the engine remains
    /// shared across independent clients.
    pub async fn run_connection(
        self: &Arc<Self>,
        transport: &mut impl crate::mcp::transport::McpTransport,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, None, None)
            .await
    }

    /// Runs one daemon client connection using connection-local timing
    /// settings. The shared server's default timing flag remains unchanged.
    pub async fn run_connection_with_timings(
        self: &Arc<Self>,
        transport: &mut impl crate::mcp::transport::McpTransport,
        timings_enabled: bool,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, Some(timings_enabled), None)
            .await
    }

    pub(crate) async fn run_daemon_connection_with_timings(
        self: &Arc<Self>,
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
        self: &Arc<Self>,
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

        let mut connection_route = self.new_connection_route_state()?;
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
            } else {
                match parsed {
                    Ok(request) => {
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
                            let shutdown_requested = external_shutdown_requested;
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
                            let shutdown_requested = external_shutdown_requested;
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

            let selected_response_lease = connection_route.take_selected_response_lease();

            if peer_closed {
                drop(request_activity);
                break;
            }

            let response_revoked = selected_response_lease
                .as_ref()
                .map(crate::mcp::server::routing::SelectedProjectResponseLease::revoked);

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
                                response_revoked,
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
                    .write_response_line_or_revoke(transport, &output, response_revoked)
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
            drop(selected_response_lease);
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

    pub(crate) async fn shutdown_if(self: &Arc<Self>, enabled: bool) {
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
    pub async fn shutdown(self: &Arc<Self>) {
        let deadline = tokio::time::Instant::now() + crate::daemon::DAEMON_SHUTDOWN_DEADLINE;
        let status = self.shutdown_until(deadline).await;
        if !status.is_clean() {
            tracing::warn!(?status, "MCP server shutdown did not complete cleanly");
        }
    }

    pub(crate) async fn shutdown_until(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        self.shutdown
            .coordinate_until(deadline, Arc::clone(self).run_shutdown(deadline))
            .await
    }

    async fn run_shutdown(
        self: Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        let mut failures = self.shutdown_background_tasks_until(deadline).await;

        let uptime = self.stats.started_at.elapsed();
        let tool_calls = self.stats.tool_calls.load(Ordering::Relaxed);
        let tokens_saved = self.tokens_saved.load(Ordering::Relaxed);

        let cg = self.cg_snapshot().await;
        // Persist final tokens-saved value
        if let Err(e) = cg.set_tokens_saved(tokens_saved).await {
            tracing::warn!(error = %e, "failed to persist tokens saved during shutdown");
            failures.push(format!("persist tokens saved: {e}"));
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
            match self.canonical_upload_enabled().await {
                Ok(upload_enabled) => {
                    let mut config = crate::user_config::UserConfig::load();
                    config.pending_upload += delta;
                    if upload_enabled
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
                Err(error) => failures.push(format!(
                    "worldwide counter upload configuration unavailable: {error}"
                )),
            }
        }

        // Checkpoint WAL to merge it into the main database file
        if let Err(e) = cg.checkpoint().await {
            tracing::warn!(error = %e, "failed to checkpoint WAL during shutdown");
            failures.push(format!("code graph checkpoint: {e}"));
        }

        if failures.is_empty() {
            tracing::info!(
                tool_calls,
                tokens_saved,
                uptime_secs = uptime.as_secs(),
                "MCP server shutdown complete"
            );
            crate::daemon::ShutdownStatus::Clean
        } else {
            crate::daemon::ShutdownStatus::Failed(failures.join("; "))
        }
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub(crate) async fn shutdown_background_tasks(&self) {
        let failures = self
            .shutdown_background_tasks_until(
                tokio::time::Instant::now() + crate::daemon::DAEMON_SHUTDOWN_DEADLINE,
            )
            .await;
        if !failures.is_empty() {
            tracing::warn!(
                failures = failures.join("; "),
                "MCP background shutdown did not complete cleanly"
            );
        }
    }

    async fn shutdown_background_tasks_until(
        &self,
        _deadline: tokio::time::Instant,
    ) -> Vec<String> {
        let mut failures = Vec::new();
        // The hosted dashboard is daemon-process state (`DASHBOARD_MANAGER`),
        // not project-server state. `tracedecay_dashboard` starts against the
        // Core server; the later Core→Full remount retires that server. Tearing
        // the listener down here made a fresh bind report a URL that died as
        // soon as full capability published.
        failures.extend(self.background_tasks.shutdown().await);
        self.dispatch_authority.shutdown().await;
        if let Some(worker) = self.project_host_admission_replay.lock().await.take() {
            worker.shutdown().await;
        }
        self.shutdown_startup_catch_up_sync().await;
        failures
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

    struct ObservedTransport {
        inner: crate::mcp::transport::ChannelTransport,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl crate::mcp::transport::McpTransport for ObservedTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            let line = self.inner.read_line().await?;
            if line.is_some() {
                self.reads.fetch_add(1, Ordering::Release);
            }
            Ok(line)
        }

        async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.inner.write_line(line).await
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush().await
        }

        fn peer_fully_closed_after_eof(
            &self,
        ) -> impl std::future::Future<Output = ()> + Send + 'static {
            self.inner.peer_fully_closed_after_eof()
        }
    }

    async fn wait_for_transport_reads(reads: &std::sync::atomic::AtomicUsize, expected: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while reads.load(Ordering::Acquire) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transport did not consume the expected request lines");
    }

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

    #[tokio::test]
    async fn cancellation_during_route_resolution_reaches_selected_live_target() {
        let isolation = tempfile::TempDir::new().expect("route cancellation isolation");
        let active_root = isolation.path().join("active");
        let target_root = isolation.path().join("target");
        for root in [&active_root, &target_root] {
            std::fs::create_dir_all(root.join("src")).expect("fixture source directory");
            std::fs::write(root.join("src/lib.rs"), "pub fn route_fixture() {}\n")
                .expect("fixture source");
            super::super::writer_test_support::git(root, &["init", "-q", "-b", "main"]);
            super::super::writer_test_support::git(
                root,
                &["config", "user.email", "route@test.invalid"],
            );
            super::super::writer_test_support::git(root, &["config", "user.name", "Route Test"]);
            super::super::writer_test_support::git(root, &["add", "."]);
            super::super::writer_test_support::git(root, &["commit", "-q", "-m", "fixture"]);
        }
        let harness = crate::daemon::ProductionProjectCompositionHarnessV1::open(
            isolation.path(),
            [active_root.clone(), target_root.clone()],
        )
        .await
        .expect("production route composition");
        let mounted_active = harness.server(&active_root).expect("mounted active server");
        let target = harness.server(&target_root).expect("mounted target server");
        let target_project_id = target
            .cg_snapshot()
            .await
            .store_layout()
            .identity
            .project_id
            .clone()
            .expect("target project identity");

        let route_entered = Arc::new(tokio::sync::Notify::new());
        let release_route = Arc::new(tokio::sync::Notify::new());
        let resolver_target = Arc::clone(&target);
        let resolver_entered = Arc::clone(&route_entered);
        let resolver_release = Arc::clone(&release_route);
        let resolver: super::super::RetainedProjectServerResolver = Arc::new(move |_request| {
            let target = Arc::clone(&resolver_target);
            let entered = Arc::clone(&resolver_entered);
            let release = Arc::clone(&resolver_release);
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Ok(Some(target))
            })
        });
        let context = super::super::McpServerConstructionContext::direct(
            mounted_active.cg_snapshot().await,
            None,
        )
        .with_direct_databases(
            mounted_active.global_db.clone(),
            mounted_active.registry_db.clone(),
            mounted_active.session_db.clone(),
            mounted_active.user_session_db.clone(),
        )
        .with_retained_project_server_resolver(resolver);
        let caller = super::super::McpServer::new_with_context(context).await;
        let (inner_transport, sender, mut responses) =
            crate::mcp::transport::ChannelTransport::new();
        let transport_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut transport = ObservedTransport {
            inner: inner_transport,
            reads: Arc::clone(&transport_reads),
        };
        let serving = tokio::spawn({
            let caller = Arc::clone(&caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 41,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_fact_store_search",
                        "arguments": {
                            "query": "route cancellation",
                            "project_selector": {"project_id": target_project_id}
                        }
                    }
                })
                .to_string(),
            )
            .expect("send selected request");
        route_entered.notified().await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": 41, "reason": "route still resolving"}
                })
                .to_string(),
            )
            .expect("cancel selected request during route resolution");
        wait_for_transport_reads(&transport_reads, 2).await;

        // The connection's original server may retire while the route is
        // unresolved. It must not reject or authorize the selected target.
        caller.project_server_response_lifecycle().revoke();
        release_route.notify_one();

        let response_line =
            tokio::time::timeout(std::time::Duration::from_secs(5), responses.recv())
                .await
                .expect("selected cancellation response timeout")
                .expect("selected cancellation response");
        let response: Value =
            serde_json::from_str(response_line.trim()).expect("selected cancellation JSON");
        assert_eq!(response["id"], serde_json::json!(41));
        assert_eq!(
            response["error"]["data"]["reason_code"],
            serde_json::json!("tool_dispatch_cancelled"),
            "cancellation captured before registration must reach selected target: {response}"
        );
        assert_eq!(
            caller.stats.total_requests.load(Ordering::Relaxed),
            0,
            "caller must not account a request owned by the selected target"
        );
        assert_eq!(
            target.stats.total_requests.load(Ordering::Relaxed),
            1,
            "selected target must own request/error accounting"
        );
        assert_eq!(target.stats.errors.load(Ordering::Relaxed), 1);

        drop(sender);
        tokio::time::timeout(std::time::Duration::from_secs(5), serving)
            .await
            .expect("selected cancellation connection close timeout")
            .expect("join selected cancellation connection")
            .expect("serve selected cancellation connection");
        harness.shutdown().await;
    }
}

#[cfg(test)]
mod shutdown_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    struct RetainedShutdownOwner(Arc<AtomicBool>);

    impl Drop for RetainedShutdownOwner {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn cancelled_shutdown_waiter_does_not_cancel_owned_work() {
        let completion = Arc::new(McpShutdownCompletion::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, released) = tokio::sync::oneshot::channel();

        let first_completion = Arc::clone(&completion);
        let first_attempts = Arc::clone(&attempts);
        let first_entered = Arc::clone(&entered);
        let first = tokio::spawn(async move {
            first_completion
                .coordinate_until(
                    tokio::time::Instant::now() + Duration::from_secs(5),
                    async move {
                        first_attempts.fetch_add(1, Ordering::AcqRel);
                        first_entered.notify_one();
                        let _ = released.await;
                        crate::daemon::ShutdownStatus::Clean
                    },
                )
                .await
        });
        entered.notified().await;
        first.abort();
        assert!(
            first
                .await
                .expect_err("cancel first shutdown waiter")
                .is_cancelled()
        );

        release.send(()).expect("release retained shutdown work");
        let retry_attempts = Arc::clone(&attempts);
        let retry = completion
            .coordinate_until(
                tokio::time::Instant::now() + Duration::from_secs(1),
                async move {
                    retry_attempts.fetch_add(1, Ordering::AcqRel);
                    panic!("retry must await the retained shutdown work");
                },
            )
            .await;

        assert_eq!(retry, crate::daemon::ShutdownStatus::Clean);
        assert_eq!(attempts.load(Ordering::Acquire), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_shutdown_retains_work_until_retry_observes_terminal_status() {
        let completion = Arc::new(McpShutdownCompletion::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let owner_dropped = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, released) = tokio::sync::oneshot::channel();

        let first_completion = Arc::clone(&completion);
        let first_attempts = Arc::clone(&attempts);
        let first_entered = Arc::clone(&entered);
        let first_owner_dropped = Arc::clone(&owner_dropped);
        let first = tokio::spawn(async move {
            first_completion
                .coordinate_until(
                    tokio::time::Instant::now() + Duration::from_secs(1),
                    async move {
                        let _owner = RetainedShutdownOwner(first_owner_dropped);
                        first_attempts.fetch_add(1, Ordering::AcqRel);
                        first_entered.notify_one();
                        let _ = released.await;
                        crate::daemon::ShutdownStatus::Clean
                    },
                )
                .await
        });
        entered.notified().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            first.await.expect("first timed-out shutdown"),
            crate::daemon::ShutdownStatus::TimedOut
        );
        assert!(
            !owner_dropped.load(Ordering::Acquire),
            "the timed-out attempt must retain its owner for a retry"
        );

        let retry_completion = Arc::clone(&completion);
        let retry_attempts = Arc::clone(&attempts);
        let retry = tokio::spawn(async move {
            retry_completion
                .coordinate_until(
                    tokio::time::Instant::now() + Duration::from_secs(1),
                    async move {
                        retry_attempts.fetch_add(1, Ordering::AcqRel);
                        panic!("retry must await the retained shutdown owner");
                    },
                )
                .await
        });
        release.send(()).expect("release retained shutdown owner");

        assert_eq!(
            retry.await.expect("retry shutdown"),
            crate::daemon::ShutdownStatus::Clean
        );
        assert_eq!(attempts.load(Ordering::Acquire), 1);
        assert!(owner_dropped.load(Ordering::Acquire));
    }
}

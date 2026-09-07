//! Connection lifecycle: the JSON-RPC read/write loop, shutdown
//! policy, and daemon-owned host-admission replay driving.

use super::*;
use tracedecay_mcp::serialize_response_line;

#[cfg(any(test, feature = "test-transport"))]
mod response_delivery;

const MAX_PENDING_CANCELLABLE_REQUEST_LINES: usize = 64;
pub(super) const MAX_CONCURRENT_CONNECTION_READS: usize =
    crate::daemon::MAX_CONCURRENT_REQUESTS_PER_DAEMON_CLIENT;

#[hotpath::measure(label = "mcp.server.connection.read", future = true)]
async fn read_connection_line(
    transport: &mut impl tracedecay_mcp::transport::McpTransport,
) -> std::io::Result<Option<String>> {
    transport.read_line().await
}

#[hotpath::measure(label = "mcp.server.connection.inflight_read", future = true)]
async fn read_inflight_connection_line(
    transport: &mut impl tracedecay_mcp::transport::McpTransport,
) -> std::io::Result<Option<String>> {
    transport.read_line().await
}

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
    #[hotpath::skip]
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

    #[hotpath::skip]
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

    #[hotpath::skip]
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

    #[hotpath::skip]
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

/// One buffered request line plus the identity a queued cancellation can
/// target, extracted once at enqueue so each cancellation notification does
/// not re-parse every pending line.
struct QueuedRequestLine {
    line: String,
    request_id: Option<Value>,
    independent_read: bool,
    /// `Some(id)` only when the line is a `tools/call` for a
    /// live-cancellable tool — the only lines a queued cancellation matches.
    cancellable_request_id: Option<Value>,
    queued_at: std::time::Instant,
    _depth: PendingRequestGaugeGuard,
}

struct PendingRequestGaugeGuard {
    bytes: usize,
    #[cfg(test)]
    observer: Option<Arc<std::sync::atomic::AtomicIsize>>,
}

impl PendingRequestGaugeGuard {
    fn enter(bytes: usize) -> Self {
        hotpath::gauge!("mcp.server.request.queue_depth").inc(1_u64);
        hotpath::gauge!("mcp.server.request.queue_bytes").inc(bytes as u64);
        Self {
            bytes,
            #[cfg(test)]
            observer: None,
        }
    }

    #[cfg(test)]
    fn enter_observed(bytes: usize, observer: Arc<std::sync::atomic::AtomicIsize>) -> Self {
        let mut guard = Self::enter(bytes);
        observer.fetch_add(1, Ordering::AcqRel);
        guard.observer = Some(observer);
        guard
    }
}

impl Drop for PendingRequestGaugeGuard {
    fn drop(&mut self) {
        hotpath::gauge!("mcp.server.request.queue_depth").dec(1_u64);
        hotpath::gauge!("mcp.server.request.queue_bytes").dec(self.bytes as u64);
        #[cfg(test)]
        if let Some(observer) = self.observer.as_ref() {
            observer.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl QueuedRequestLine {
    fn new(line: String) -> Self {
        let parsed = hotpath::measure_block!(
            "mcp.server.connection.queued_decode",
            serde_json::from_str::<JsonRpcRequest>(line.trim())
        );
        let request = parsed.as_ref().ok();
        let request_id = request.and_then(|request| request.id.clone());
        let independent_read = request.is_some_and(request_is_independent_read);
        let cancellable_request_id = request.and_then(cancellable_queued_request_id);
        let depth = PendingRequestGaugeGuard::enter(line.len());
        Self {
            line,
            request_id,
            independent_read,
            cancellable_request_id,
            queued_at: std::time::Instant::now(),
            _depth: depth,
        }
    }

    fn from_parsed(line: String, request: Option<&JsonRpcRequest>) -> Self {
        let request_id = request.and_then(|request| request.id.clone());
        let independent_read = request.is_some_and(request_is_independent_read);
        let cancellable_request_id = request.and_then(cancellable_queued_request_id);
        let depth = PendingRequestGaugeGuard::enter(line.len());
        Self {
            line,
            request_id,
            independent_read,
            cancellable_request_id,
            queued_at: std::time::Instant::now(),
            _depth: depth,
        }
    }

    #[cfg(test)]
    fn new_observed(line: String, observer: Arc<std::sync::atomic::AtomicIsize>) -> Self {
        let depth = PendingRequestGaugeGuard::enter_observed(line.len(), observer);
        Self {
            line,
            request_id: None,
            independent_read: false,
            cancellable_request_id: None,
            queued_at: std::time::Instant::now(),
            _depth: depth,
        }
    }

    fn into_line(self) -> String {
        hotpath::gauge!("mcp.server.request.queue_wait_us")
            .set(self.queued_at.elapsed().as_micros() as u64);
        self.line
    }
}

fn cancellable_queued_request_id(request: &JsonRpcRequest) -> Option<Value> {
    let cancellable = request.method == "tools/call"
        && request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .is_some_and(super::requests::tool_supports_live_cancellation);
    if !cancellable {
        return None;
    }
    request.id.clone()
}

fn queued_cancellable_request_key(
    pending_lines: &VecDeque<QueuedRequestLine>,
    request_id: &Value,
    connection_scope: &str,
) -> Option<String> {
    let expected = application_surface_request_id(request_id, connection_scope)?;
    pending_lines
        .iter()
        .filter_map(|queued| queued.cancellable_request_id.as_ref())
        .any(|id| application_surface_request_id(id, connection_scope).as_ref() == Some(&expected))
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

#[hotpath::measure(label = "mcp.server.connection.classify")]
pub(super) fn request_is_independent_read(request: &JsonRpcRequest) -> bool {
    super::dispatch_envelope::dispatch_is_independent_read(
        classify_mcp_method(&request.method),
        request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str),
    )
}

struct ConcurrentReadCompletion {
    request_key: Option<String>,
    _request_activity: Option<tracedecay_mcp::McpRequestActivity>,
    revocable_tool_call: Option<(Value, String)>,
    response: Option<JsonRpcResponse>,
    selected_response_lease: Option<crate::mcp::server::routing::SelectedProjectResponseLease>,
    connection_scope: String,
    connection_closed: bool,
}

enum ConnectionLoopEvent {
    Queued(String),
    Incoming(std::io::Result<Option<String>>),
    Completed(Box<Option<std::result::Result<ConcurrentReadCompletion, tokio::task::JoinError>>>),
    Shutdown,
    PeerClosed,
}

#[hotpath::measure(label = "mcp.server.connection.read_dispatch", future = true)]
async fn dispatch_independent_read(
    server: Arc<McpServer>,
    request: JsonRpcRequest,
    timings_enabled: bool,
    mut connection: ConnectionRouteState,
    request_activity: Option<tracedecay_mcp::McpRequestActivity>,
    cancellation: tracedecay_session_memory::context::CancellationToken,
    connection_shutdown: tracedecay_session_memory::context::CancellationToken,
) -> ConcurrentReadCompletion {
    let connection_scope = connection.memory_request_scope().to_owned();
    let request_key = request
        .id
        .as_ref()
        .and_then(|id| application_surface_request_id(id, &connection_scope));
    let revocable_tool_call = request.id.clone().and_then(|id| {
        (request.method == "tools/call").then_some(())?;
        let tool_name = request.params.as_ref()?.get("name")?.as_str()?.to_owned();
        Some((id, tool_name))
    });
    let (response, connection_closed) = {
        let handling = Box::pin(server.handle_request_for_connection(
            &request,
            timings_enabled,
            &mut connection,
            cancellation.is_cancelled(),
        ));
        tokio::pin!(handling);
        let mut cancellation_waiting_for_registration = false;
        loop {
            let waiting_for_registration = cancellation_waiting_for_registration;
            let wait_for_cancellation_registration = async {
                if !waiting_for_registration {
                    std::future::pending::<()>().await;
                    return;
                }
                loop {
                    let registered = server
                        .dispatch_authority
                        .cancellation_registered()
                        .notified();
                    tokio::pin!(registered);
                    registered.as_mut().enable();
                    if let Some(id) = request.id.as_ref()
                        && server.cancel_application_surface_request(id, &connection_scope)
                    {
                        return;
                    }
                    registered.await;
                }
            };
            tokio::pin!(wait_for_cancellation_registration);
            tokio::select! {
                biased;
                () = connection_shutdown.cancelled() => {
                    if let Some(id) = request.id.as_ref() {
                        let _ = server.cancel_application_surface_request(id, &connection_scope);
                    }
                    break (None, true);
                }
                response = &mut handling => break (response, false),
                () = &mut wait_for_cancellation_registration => {
                    cancellation_waiting_for_registration = false;
                }
                () = cancellation.cancelled(), if !cancellation_waiting_for_registration => {
                    cancellation_waiting_for_registration = request
                        .id
                        .as_ref()
                        .is_some_and(|id| {
                            !server.cancel_application_surface_request(id, &connection_scope)
                        });
                }
            }
        }
    };
    let selected_response_lease = connection.take_selected_response_lease();
    ConcurrentReadCompletion {
        request_key,
        _request_activity: request_activity,
        revocable_tool_call,
        response,
        selected_response_lease,
        connection_scope,
        connection_closed,
    }
}

struct ConnectionResponseWriter;

impl ConnectionResponseWriter {
    async fn write(
        server: &McpServer,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        completion: &mut ConcurrentReadCompletion,
    ) -> std::io::Result<bool> {
        let response_revoked = completion
            .selected_response_lease
            .as_ref()
            .map(crate::mcp::server::routing::SelectedProjectResponseLease::revoked);
        let notifications: Vec<Value> =
            crate::mcp::server::requests::recover_lock(&server.pending_notifications)
                .drain(..)
                .collect();
        for notification in notifications {
            if let Ok(serialized) = hotpath::measure_block!(
                "mcp.server.notification.serialize",
                serde_json::to_string(&notification)
            ) && !server
                .write_response_line_or_revoke(
                    transport,
                    &format!("{serialized}\n"),
                    response_revoked,
                )
                .await?
            {
                return Ok(false);
            }
        }
        let Some(response) = completion.response.as_ref() else {
            return Ok(true);
        };
        let json_line = hotpath::measure_block!(
            "mcp.server.response.serialize",
            serialize_response_line(response)
        );
        server
            .write_response_line_or_revoke(transport, &format!("{json_line}\n"), response_revoked)
            .await
    }
}

async fn wait_for_peer_close(
    peer_close: &mut Option<
        std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>,
    >,
) {
    match peer_close {
        Some(peer_close) => peer_close.await,
        None => std::future::pending().await,
    }
}

impl McpServer {
    #[hotpath::measure(label = "mcp.server.write", future = true)]
    async fn write_response_line_or_revoke(
        &self,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        output: &str,
        response_revoked: Option<&tracedecay_session_memory::context::CancellationToken>,
    ) -> std::io::Result<bool> {
        hotpath::gauge!("mcp.server.response.bytes").set(output.len());
        let write = async {
            hotpath::future!(
                transport.write_line(output),
                label = "mcp.server.response.write"
            )
            .await?;
            hotpath::future!(transport.flush(), label = "mcp.server.response.flush").await
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

    #[hotpath::measure(label = "mcp.server.request_cancellable", future = true)]
    async fn handle_cancellable_application_request(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        connection: &mut ConnectionRouteState,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        pending_lines: &mut VecDeque<QueuedRequestLine>,
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
                loop {
                    // Register interest *before* re-probing so a registration
                    // between the probe and the await cannot be missed.
                    let registered = self.dispatch_authority.cancellation_registered().notified();
                    tokio::pin!(registered);
                    registered.as_mut().enable();
                    if self.cancel_application_surface_request(cancellation_id, &connection_scope) {
                        return;
                    }
                    registered.await;
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
                incoming = read_inflight_connection_line(transport) => {
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
                    let parsed = hotpath::measure_block!(
                        "mcp.server.connection.inflight_decode",
                        serde_json::from_str::<JsonRpcRequest>(line.trim())
                    );
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
                    pending_lines.push_back(QueuedRequestLine::from_parsed(
                        line,
                        parsed.as_ref().ok(),
                    ));
                }
            }
        }
    }

    /// Runs a non-live-cancellable request while still observing connection
    /// teardown.  A request-side EOF is only a half-close until the transport
    /// reports the peer's write side closed; this keeps one-shot CLI responses
    /// intact while dropping abandoned handlers and their admission permits.
    #[hotpath::measure(label = "mcp.server.request_non_cancellable", future = true)]
    async fn handle_non_cancellable_application_request(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        connection: &mut ConnectionRouteState,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        pending_lines: &mut VecDeque<QueuedRequestLine>,
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
                incoming = read_inflight_connection_line(transport) => {
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
                    pending_lines.push_back(QueuedRequestLine::new(line));
                }
            }
        }
    }

    /// Runs the server, reading JSON-RPC requests from stdin and writing
    /// responses to stdout. Runs until stdin is closed or a shutdown signal
    /// (SIGINT/SIGTERM) is received, then performs graceful cleanup.
    #[hotpath::skip]
    pub async fn run(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, true, true, None, None)
            .await
    }

    /// Runs one client connection without shutting down the server when that
    /// connection closes. Production daemon connections go through
    /// [`Self::run_daemon_connection_with_timings`]; this is the in-process
    /// test-transport harness entry for the same connection loop.
    #[cfg(any(test, feature = "test-transport"))]
    #[hotpath::skip]
    pub async fn run_connection(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, None, None)
            .await
    }

    #[hotpath::skip]
    pub(crate) async fn run_daemon_connection_with_timings(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        timings_enabled: bool,
        lifecycle: &dyn tracedecay_mcp::McpConnectionLifecyclePort,
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

    #[hotpath::measure(label = "mcp.server.connection", future = true)]
    pub(crate) async fn run_with_shutdown_policy(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        shutdown_on_exit: bool,
        listen_for_process_signals: bool,
        timings_override: Option<bool>,
        request_lifecycle: Option<&dyn tracedecay_mcp::McpConnectionLifecyclePort>,
    ) -> Result<()> {
        Box::pin(self.run_connection_loop(
            transport,
            shutdown_on_exit,
            listen_for_process_signals,
            timings_override,
            request_lifecycle,
        ))
        .await
    }

    #[hotpath::measure(label = "mcp.server.connection.loop", future = true)]
    async fn run_connection_loop(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        shutdown_on_exit: bool,
        listen_for_process_signals: bool,
        timings_override: Option<bool>,
        request_lifecycle: Option<&dyn tracedecay_mcp::McpConnectionLifecyclePort>,
    ) -> Result<()> {
        let mut connection_route = self.new_connection_route_state()?;
        let mut pending_lines: VecDeque<QueuedRequestLine> = VecDeque::new();
        let mut pending_cancellations = HashSet::new();
        let mut active_reads: tokio::task::JoinSet<ConcurrentReadCompletion> =
            tokio::task::JoinSet::new();
        let mut active_cancellations: HashMap<
            String,
            tracedecay_session_memory::context::CancellationToken,
        > = HashMap::new();
        let connection_shutdown = tracedecay_session_memory::context::CancellationToken::new();
        let mut input_closed = false;
        let mut peer_close_check: Option<
            std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>,
        > = None;
        let timings_enabled = timings_override.unwrap_or_else(|| self.timings_enabled());

        // Install the process listeners once. This same fused future is polled
        // by idle reads, active read batches, and effect barriers, so shutdown
        // cannot land in an iteration gap.
        let external_shutdown_requested = async {
            if listen_for_process_signals {
                #[cfg(unix)]
                {
                    #[allow(clippy::expect_used)]
                    let mut sigterm =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("failed to register SIGTERM handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = sigterm.recv() => {}
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

        'connection: loop {
            if input_closed && pending_lines.is_empty() && active_reads.is_empty() {
                break;
            }

            let queued_ready = pending_lines.front().is_some_and(|queued| {
                if active_reads.is_empty() {
                    return true;
                }
                if !queued.independent_read || active_reads.len() >= MAX_CONCURRENT_CONNECTION_READS
                {
                    return false;
                }
                queued
                    .request_id
                    .as_ref()
                    .and_then(|id| {
                        application_surface_request_id(id, connection_route.memory_request_scope())
                    })
                    .is_none_or(|key| !active_cancellations.contains_key(&key))
            });

            let line_from_queue = queued_ready;
            let event = if queued_ready {
                let Some(queued) = pending_lines.pop_front() else {
                    continue;
                };
                ConnectionLoopEvent::Queued(queued.into_line())
            } else if active_reads.is_empty() {
                let incoming = read_connection_line(transport);
                tokio::pin!(incoming);
                tokio::select! {
                    biased;
                    () = &mut external_shutdown_requested => ConnectionLoopEvent::Shutdown,
                    () = wait_for_peer_close(&mut peer_close_check), if input_closed =>
                        ConnectionLoopEvent::PeerClosed,
                    result = &mut incoming, if !input_closed =>
                        ConnectionLoopEvent::Incoming(result),
                }
            } else {
                let can_read_more =
                    !input_closed && pending_lines.len() < MAX_PENDING_CANCELLABLE_REQUEST_LINES;
                let incoming = read_connection_line(transport);
                tokio::pin!(incoming);
                tokio::select! {
                    biased;
                    () = &mut external_shutdown_requested => ConnectionLoopEvent::Shutdown,
                    () = wait_for_peer_close(&mut peer_close_check), if input_closed =>
                        ConnectionLoopEvent::PeerClosed,
                    result = active_reads.join_next() => {
                        ConnectionLoopEvent::Completed(Box::new(result))
                    },
                    result = &mut incoming, if can_read_more =>
                        ConnectionLoopEvent::Incoming(result),
                }
            };

            let line = match event {
                ConnectionLoopEvent::Queued(line) => Some(line),
                ConnectionLoopEvent::Incoming(Ok(Some(line))) => Some(line),
                ConnectionLoopEvent::Incoming(Ok(None)) => {
                    input_closed = true;
                    peer_close_check = Some(Box::pin(transport.peer_fully_closed_after_eof()));
                    None
                }
                ConnectionLoopEvent::Incoming(Err(error)) => {
                    connection_shutdown.cancel();
                    while active_reads.join_next().await.is_some() {}
                    if is_wire_oversized_io_error(&error) {
                        let _ = write_wire_oversized_rejection(transport, &error).await;
                        break;
                    }
                    self.shutdown_if(shutdown_on_exit).await;
                    return Err(error.into());
                }
                ConnectionLoopEvent::Completed(completed) => {
                    let Some(completed) = *completed else {
                        continue;
                    };
                    let mut completion = completed.map_err(|error| TraceDecayError::Config {
                        message: format!("MCP concurrent read task failed: {error}"),
                    })?;
                    if let Some(request_key) = completion.request_key.as_ref() {
                        active_cancellations.remove(request_key);
                    }
                    if completion.connection_closed {
                        connection_shutdown.cancel();
                        while active_reads.join_next().await.is_some() {}
                        break;
                    }
                    match ConnectionResponseWriter::write(self, transport, &mut completion).await {
                        Ok(true) => {}
                        Ok(false) => {
                            connection_shutdown.cancel();
                            while active_reads.join_next().await.is_some() {}
                            break;
                        }
                        Err(error) => {
                            tracing::error!(error = %error, "failed to write MCP response");
                            if let Some((id, _)) = &completion.revocable_tool_call {
                                let _ = self.cancel_application_surface_request(
                                    id,
                                    &completion.connection_scope,
                                );
                            }
                            connection_shutdown.cancel();
                            while active_reads.join_next().await.is_some() {}
                            self.shutdown_if(shutdown_on_exit).await;
                            return Err(error.into());
                        }
                    }
                    drop(completion);
                    if request_lifecycle.is_some_and(|lifecycle| !lifecycle.accepting()) {
                        connection_shutdown.cancel();
                        while active_reads.join_next().await.is_some() {}
                        break;
                    }
                    None
                }
                ConnectionLoopEvent::Shutdown | ConnectionLoopEvent::PeerClosed => {
                    connection_shutdown.cancel();
                    while active_reads.join_next().await.is_some() {}
                    break;
                }
            };

            let Some(line) = line else {
                continue;
            };

            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let parsed: std::result::Result<JsonRpcRequest, _> = hotpath::measure_block!(
                "mcp.server.connection.decode",
                serde_json::from_str(&line)
            );
            if let Ok(notification) = &parsed
                && matches!(
                    classify_mcp_method(&notification.method),
                    McpMethod::Cancelled
                )
                && let Some(id) = notification
                    .params
                    .as_ref()
                    .and_then(|params| params.get("requestId"))
            {
                let connection_scope = connection_route.memory_request_scope();
                if !self.cancel_application_surface_request(id, connection_scope)
                    && let Some(key) = application_surface_request_id(id, connection_scope)
                {
                    if let Some(cancellation) = active_cancellations.get(&key) {
                        cancellation.cancel();
                    } else if pending_cancellations.len() < MAX_PENDING_CANCELLABLE_REQUEST_LINES
                        && queued_cancellable_request_key(&pending_lines, id, connection_scope)
                            .is_some()
                    {
                        pending_cancellations.insert(key);
                    }
                }
                continue;
            }

            if let Ok(request) = &parsed
                && request_is_independent_read(request)
            {
                let request_key = request.id.as_ref().and_then(|id| {
                    application_surface_request_id(id, connection_route.memory_request_scope())
                });
                let duplicate_in_flight = request_key
                    .as_ref()
                    .is_some_and(|key| active_cancellations.contains_key(key));
                if !duplicate_in_flight
                    && active_reads.len() < MAX_CONCURRENT_CONNECTION_READS
                    && (line_from_queue || pending_lines.is_empty())
                {
                    let request_activity = request_lifecycle
                        .and_then(tracedecay_mcp::McpConnectionLifecyclePort::try_enter);
                    if request_lifecycle.is_some() && request_activity.is_none() {
                        let mut completion = ConcurrentReadCompletion {
                            request_key,
                            _request_activity: request_activity,
                            revocable_tool_call: None,
                            response: request.id.clone().map(|id| {
                                JsonRpcResponse::error(
                                    id,
                                    ErrorCode::InternalError,
                                    "TraceDecay daemon is draining for upgrade; retry the request"
                                        .to_string(),
                                )
                            }),
                            selected_response_lease: None,
                            connection_scope: connection_route.memory_request_scope().to_owned(),
                            connection_closed: false,
                        };
                        ConnectionResponseWriter::write(self, transport, &mut completion).await?;
                        break;
                    }
                    let cancellation = tracedecay_session_memory::context::CancellationToken::new();
                    if let Some(request_key) = request_key.as_ref() {
                        if pending_cancellations.remove(request_key) {
                            cancellation.cancel();
                        }
                        active_cancellations.insert(request_key.clone(), cancellation.clone());
                    }
                    let admission = crate::daemon::current_connection_admission();
                    active_reads.spawn(crate::daemon::in_connection_admission(
                        admission,
                        dispatch_independent_read(
                            Arc::clone(self),
                            request.clone(),
                            timings_enabled,
                            connection_route.fork_for_connection_owned_read(),
                            request_activity,
                            cancellation,
                            connection_shutdown.clone(),
                        ),
                    ));
                    continue;
                }
            }

            if !active_reads.is_empty() {
                if pending_lines.len() >= MAX_PENDING_CANCELLABLE_REQUEST_LINES {
                    connection_shutdown.cancel();
                    while active_reads.join_next().await.is_some() {}
                    break;
                }
                pending_lines.push_back(QueuedRequestLine::from_parsed(line, parsed.as_ref().ok()));
                continue;
            }

            let revocable_tool_call = parsed.as_ref().ok().and_then(|request| {
                (request.method == "tools/call").then_some(())?;
                let id = request.id.clone()?;
                let tool_name = request.params.as_ref()?.get("name")?.as_str()?.to_owned();
                Some((id, tool_name))
            });
            let request_activity =
                request_lifecycle.and_then(tracedecay_mcp::McpConnectionLifecyclePort::try_enter);
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
                            let (response, closed) = self
                                .handle_cancellable_application_request(
                                    &request,
                                    timings_enabled,
                                    &mut connection_route,
                                    transport,
                                    &mut pending_lines,
                                    &mut pending_cancellations,
                                    external_shutdown_requested.as_mut(),
                                )
                                .await?;
                            peer_closed = closed;
                            response
                        } else {
                            let (response, closed) = self
                                .handle_non_cancellable_application_request(
                                    &request,
                                    timings_enabled,
                                    &mut connection_route,
                                    transport,
                                    &mut pending_lines,
                                    external_shutdown_requested.as_mut(),
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
            let mut completion = ConcurrentReadCompletion {
                request_key: None,
                _request_activity: request_activity,
                revocable_tool_call,
                response,
                selected_response_lease,
                connection_scope: connection_route.memory_request_scope().to_owned(),
                connection_closed: false,
            };
            match ConnectionResponseWriter::write(self, transport, &mut completion).await {
                Ok(true) => {}
                Ok(false) => break 'connection,
                Err(error) => {
                    tracing::error!(error = %error, "failed to write MCP response");
                    if let Some((id, _)) = &completion.revocable_tool_call {
                        let _ = self
                            .cancel_application_surface_request(id, &completion.connection_scope);
                    }
                    self.shutdown_if(shutdown_on_exit).await;
                    return Err(error.into());
                }
            }
            drop(completion);
            if rejecting_for_drain
                || request_lifecycle.is_some_and(|lifecycle| !lifecycle.accepting())
            {
                break;
            }
        }

        self.shutdown_if(shutdown_on_exit).await;
        Ok(())
    }

    #[hotpath::skip]
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
    #[hotpath::skip]
    pub async fn shutdown(self: &Arc<Self>) {
        let deadline =
            tokio::time::Instant::now() + tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE;
        let status = self.shutdown_until(deadline).await;
        if !status.is_clean() {
            tracing::warn!(?status, "MCP server shutdown did not complete cleanly");
        }
    }

    #[hotpath::measure(label = "mcp.server.shutdown", future = true)]
    pub(crate) async fn shutdown_until(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        self.shutdown
            .coordinate_until(deadline, Arc::clone(self).run_shutdown(deadline))
            .await
    }

    #[hotpath::skip]
    async fn run_shutdown(
        self: Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        let mut failures = self.shutdown_background_tasks_until(deadline).await;

        let uptime = self.stats.started_at.elapsed();
        let tool_calls = self.stats.tool_calls.load(Ordering::Relaxed);
        let tokens_saved = self
            .tokens_saved
            .as_ref()
            .map(|tokens| tokens.load(Ordering::Relaxed));

        let cg = self.cg_snapshot().await;
        if let Some(tokens_saved) = tokens_saved {
            if let Err(e) = cg.set_tokens_saved(tokens_saved).await {
                tracing::warn!(error = %e, "failed to persist tokens saved during shutdown");
                failures.push(format!("persist tokens saved: {e}"));
            }

            // A failed global-ledger flush joins the shutdown failure report
            // beside the local persistence failures instead of vanishing.
            if let Some(gdb) = self.accounting_db.as_ref().or(self.global_db.as_ref()) {
                if let Err(error) = gdb
                    .try_upsert_project_tokens(cg.project_root(), tokens_saved)
                    .await
                {
                    tracing::warn!(error = %error, "failed to flush tokens saved to the global ledger during shutdown");
                    failures.push(format!("flush global ledger tokens saved: {error}"));
                }
                gdb.checkpoint().await;
            }

            // Flush remaining delta to worldwide counter (what periodic flushes missed).
            if let Some(last_flushed_tokens) = self.last_flushed_tokens.as_ref() {
                let last_flushed = last_flushed_tokens.load(Ordering::Relaxed);
                if (self.accounting_db.is_some() || self.global_db.is_some())
                    && tokens_saved > last_flushed
                {
                    let delta = tokens_saved - last_flushed;
                    match self.canonical_upload_enabled().await {
                        Ok(upload_enabled) => {
                            let mut config =
                                tracedecay_session_memory::user_config::UserConfig::load();
                            config.pending_upload += delta;
                            if upload_enabled
                                && let Some(_total) =
                                    crate::cloud::flush_pending(config.pending_upload)
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
                ?tokens_saved,
                uptime_secs = uptime.as_secs(),
                "MCP server shutdown complete"
            );
            crate::daemon::ShutdownStatus::Clean
        } else {
            crate::daemon::ShutdownStatus::Failed(failures.join("; "))
        }
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[hotpath::skip]
    pub(crate) async fn shutdown_background_tasks(&self) {
        let failures = self
            .shutdown_background_tasks_until(
                tokio::time::Instant::now() + tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE,
            )
            .await;
        if !failures.is_empty() {
            tracing::warn!(
                failures = failures.join("; "),
                "MCP background shutdown did not complete cleanly"
            );
        }
    }

    #[hotpath::skip]
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

    #[hotpath::skip]
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
                non_committed_outcome.get_or_insert(outcome.clone());
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
                    non_committed_outcome.get_or_insert(outcome.clone());
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
                            non_committed_outcome.get_or_insert(outcome.clone());
                            if target_seq == Some(record.seq) {
                                target_outcome = Some(outcome);
                            }
                        }
                        Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                            blocked_sources.insert(record.source);
                            retained_leases.push(record.seq);
                            non_committed_outcome.get_or_insert(failure.clone());
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
                        non_committed_outcome.get_or_insert(canonical_outcome.clone());
                        canonical_outcome
                    }
                    Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                        blocked_sources.insert(record.source);
                        retained_leases.push(record.seq);
                        non_committed_outcome.get_or_insert(failure.clone());
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
                non_committed_outcome.get_or_insert(canonical_outcome.clone());
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

    pub(crate) fn report_host_admission_outcome(outcome: &HostAdmissionOutcome) {
        if outcome.status.is_replay_progress() {
            return;
        }
        tracing::warn!(
            reason_code = outcome.reason_code.unwrap_or("host_admission_unavailable"),
            "host admission did not make replay progress"
        );
    }

    #[cfg(test)]
    #[hotpath::skip]
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
    #[hotpath::skip]
    pub(crate) async fn project_host_admission_replay_pass_count(&self) -> usize {
        let guard = self.project_host_admission_replay.lock().await;
        guard.as_ref().map_or(
            0,
            project_host_admission_replay::ProjectHostAdmissionReplayTask::pass_count,
        )
    }

    #[cfg(test)]
    #[hotpath::skip]
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

    static DELAYED_ROUTE_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct DelayedRouteFixture {
        _fixture_guard: tokio::sync::MutexGuard<'static, ()>,
        _isolation: tempfile::TempDir,
        harness: crate::daemon::ProductionProjectCompositionHarnessV1,
        caller: Arc<McpServer>,
        target_project_id: String,
        route_started: Arc<std::sync::atomic::AtomicUsize>,
        route_release: Arc<tokio::sync::Semaphore>,
    }

    impl DelayedRouteFixture {
        async fn new() -> Self {
            let fixture_guard = DELAYED_ROUTE_FIXTURE_LOCK.lock().await;
            crate::product_runtime::register_fixture_product_runtime();
            let isolation = tempfile::TempDir::new().expect("route concurrency isolation");
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
                super::super::writer_test_support::git(
                    root,
                    &["config", "user.name", "Route Test"],
                );
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
            let route_started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let route_release = Arc::new(tokio::sync::Semaphore::new(0));
            let resolver_target = Arc::clone(&target);
            let resolver_started = Arc::clone(&route_started);
            let resolver_release = Arc::clone(&route_release);
            let resolver: super::super::RetainedProjectServerResolver =
                super::super::install_retained_project_server_resolver(move |_request| {
                    let target = Arc::clone(&resolver_target);
                    let started = Arc::clone(&resolver_started);
                    let release = Arc::clone(&resolver_release);
                    Box::pin(async move {
                        started.fetch_add(1, Ordering::AcqRel);
                        let permit = release.acquire().await.map_err(|error| {
                            tracedecay_domain::errors::TraceDecayError::Config {
                                message: format!("route concurrency gate closed: {error}"),
                            }
                        })?;
                        permit.forget();
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
            Self {
                _fixture_guard: fixture_guard,
                _isolation: isolation,
                harness,
                caller,
                target_project_id,
                route_started,
                route_release,
            }
        }

        async fn wait_for_routes(&self, expected: usize) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.route_started.load(Ordering::Acquire) < expected {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("selected requests did not enter route resolution");
        }
    }

    struct ObservedTransport {
        inner: tracedecay_mcp::transport::ChannelTransport,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Clone, Default)]
    struct TestConnectionLifecycle {
        accepting: Arc<AtomicBool>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        draining: Arc<tokio::sync::Notify>,
    }

    struct TestRequestActivity(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for TestRequestActivity {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    impl TestConnectionLifecycle {
        fn accepting() -> Self {
            Self {
                accepting: Arc::new(AtomicBool::new(true)),
                ..Self::default()
            }
        }

        fn begin_draining(&self) {
            self.accepting.store(false, Ordering::Release);
            self.draining.notify_waiters();
        }
    }

    impl tracedecay_mcp::McpConnectionLifecyclePort for TestConnectionLifecycle {
        fn accepting(&self) -> bool {
            self.accepting.load(Ordering::Acquire)
        }

        fn try_enter(&self) -> Option<tracedecay_mcp::McpRequestActivity> {
            if !self.accepting() {
                return None;
            }
            self.active.fetch_add(1, Ordering::AcqRel);
            if self.accepting() {
                return Some(tracedecay_mcp::McpRequestActivity::retain(
                    TestRequestActivity(Arc::clone(&self.active)),
                ));
            }
            self.active.fetch_sub(1, Ordering::AcqRel);
            None
        }

        fn wait_for_draining(&self) -> tracedecay_mcp::McpLifecycleDrainFuture<'_> {
            Box::pin(async move {
                while self.accepting() {
                    self.draining.notified().await;
                }
            })
        }
    }

    impl tracedecay_mcp::transport::McpTransport for ObservedTransport {
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

    async fn receive_response(
        responses: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let line = responses.recv().await.expect("connection response");
                let value: Value =
                    serde_json::from_str(line.trim()).expect("connection response JSON");
                if value.get("id").is_some() {
                    return value;
                }
            }
        })
        .await
        .expect("connection response timeout")
    }

    #[tokio::test]
    async fn independent_reads_complete_out_of_order_with_exact_ids() {
        let fixture = DelayedRouteFixture::new().await;
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "slow-read",
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send delayed selected read");
        fixture.wait_for_routes(1).await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send independent status read");

        let fast = receive_response(&mut responses).await;
        assert_eq!(
            fast["id"],
            serde_json::json!(2),
            "the independent status read must not wait behind route resolution: {fast}"
        );

        fixture.route_release.add_permits(1);
        let slow = receive_response(&mut responses).await;
        assert_eq!(
            slow["id"],
            serde_json::json!("slow-read"),
            "the delayed response must preserve the string request id: {slow}"
        );

        drop(sender);
        serving
            .await
            .expect("join concurrent connection")
            .expect("serve concurrent connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn ordinary_connection_read_has_one_connection_task_owner() {
        let fixture = DelayedRouteFixture::new().await;
        let registry = fixture.caller.dispatch_authority.registry();
        let retained_before = registry.retained_spawn_count_for_test();
        let connection_owned_before = registry.connection_owned_count_for_test();
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send ordinary connection read");
        assert_eq!(receive_response(&mut responses).await["id"], json!(3));

        assert_eq!(
            registry.retained_spawn_count_for_test(),
            retained_before,
            "the connection's active-read task must be the sole task owner"
        );
        assert_eq!(
            registry.connection_owned_count_for_test(),
            connection_owned_before + 1,
            "one inline registry lease must cover the ordinary read"
        );

        drop(sender);
        serving
            .await
            .expect("join ordinary read connection")
            .expect("serve ordinary read connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn effect_request_is_a_barrier_for_reads_on_both_sides() {
        let fixture = DelayedRouteFixture::new().await;
        let (inner_transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let transport_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut transport = ObservedTransport {
            inner: inner_transport,
            reads: Arc::clone(&transport_reads),
        };
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send read before effect");
        fixture.wait_for_routes(1).await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_fact_store_add",
                        "arguments": {
                            "content": "effect barrier fixture",
                            "category": "project",
                            "trust": 0.9,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("send effect barrier");
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 12,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send read after effect");
        wait_for_transport_reads(&transport_reads, 3).await;
        assert_eq!(
            fixture.route_started.load(Ordering::Acquire),
            1,
            "the effect must not begin before the preceding read settles"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), responses.recv())
                .await
                .is_err(),
            "no request behind the blocked read/effect barrier may answer"
        );

        fixture.route_release.add_permits(1);
        assert_eq!(receive_response(&mut responses).await["id"], json!(10));
        assert_eq!(
            receive_response(&mut responses).await["id"],
            json!(11),
            "the effect must settle after the preceding read"
        );
        assert_eq!(
            receive_response(&mut responses).await["id"],
            json!(12),
            "the later read must not overtake the effect"
        );

        drop(sender);
        serving
            .await
            .expect("join effect barrier connection")
            .expect("serve effect barrier connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn independent_read_work_is_bounded_by_daemon_per_client_admission() {
        let fixture = DelayedRouteFixture::new().await;
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        for id in 1..=MAX_CONCURRENT_CONNECTION_READS + 1 {
            sender
                .send(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "tools/call",
                        "params": {
                            "name": "tracedecay_grep",
                            "arguments": {
                                "pattern": "route_fixture",
                                "fixed_strings": true,
                                "project_selector": {
                                    "project_id": fixture.target_project_id.clone()
                                },
                                "format": "json"
                            }
                        }
                    })
                    .to_string(),
                )
                .expect("send bounded read");
        }

        fixture
            .wait_for_routes(MAX_CONCURRENT_CONNECTION_READS)
            .await;
        tokio::task::yield_now().await;
        assert_eq!(
            fixture.route_started.load(Ordering::Acquire),
            MAX_CONCURRENT_CONNECTION_READS,
            "one connection must derive its active-read cap from daemon per-client admission"
        );

        fixture.route_release.add_permits(1);
        let _ = receive_response(&mut responses).await;
        fixture
            .wait_for_routes(MAX_CONCURRENT_CONNECTION_READS + 1)
            .await;
        fixture
            .route_release
            .add_permits(MAX_CONCURRENT_CONNECTION_READS);

        let mut response_ids = HashSet::new();
        while response_ids.len() < MAX_CONCURRENT_CONNECTION_READS {
            response_ids.insert(receive_response(&mut responses).await["id"].clone());
        }
        assert_eq!(
            response_ids.len(),
            MAX_CONCURRENT_CONNECTION_READS,
            "every admitted and backpressured read must receive one response"
        );

        drop(sender);
        serving
            .await
            .expect("join bounded connection")
            .expect("serve bounded connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn notification_is_an_ordering_barrier_for_later_reads() {
        let fixture = DelayedRouteFixture::new().await;
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 20,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send read before notification");
        fixture.wait_for_routes(1).await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                })
                .to_string(),
            )
            .expect("send ordering notification");
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 21,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send read after notification");

        assert!(
            tokio::time::timeout(Duration::from_millis(50), responses.recv())
                .await
                .is_err(),
            "the later read must not overtake an ordered notification"
        );
        fixture.route_release.add_permits(1);
        assert_eq!(receive_response(&mut responses).await["id"], json!(20));
        assert_eq!(receive_response(&mut responses).await["id"], json!(21));

        drop(sender);
        serving
            .await
            .expect("join notification barrier connection")
            .expect("serve notification barrier connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn daemon_drain_cancels_and_joins_concurrent_reads() {
        let fixture = DelayedRouteFixture::new().await;
        let lifecycle = TestConnectionLifecycle::accepting();
        let (mut transport, sender, _responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            let lifecycle = lifecycle.clone();
            async move {
                caller
                    .run_with_shutdown_policy(&mut transport, false, false, None, Some(&lifecycle))
                    .await
            }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 30,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send read held across drain");
        fixture.wait_for_routes(1).await;
        assert_eq!(lifecycle.active.load(Ordering::Acquire), 1);

        lifecycle.begin_draining();
        tokio::time::timeout(Duration::from_secs(5), serving)
            .await
            .expect("draining connection did not join active reads")
            .expect("join draining connection")
            .expect("serve draining connection");
        assert_eq!(
            lifecycle.active.load(Ordering::Acquire),
            0,
            "shutdown drain must release every admitted request activity"
        );

        drop(sender);
        fixture.harness.shutdown().await;
    }

    #[test]
    fn queued_request_cancellation_is_type_preserving() {
        let pending: VecDeque<QueuedRequestLine> = [
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
        ]
        .into_iter()
        .map(QueuedRequestLine::new)
        .collect();

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

    #[test]
    fn queued_request_depth_is_released_on_dequeue_and_connection_drop() {
        let queued = Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let mut pending = VecDeque::new();
        pending.push_back(QueuedRequestLine::new_observed(
            "first".to_owned(),
            Arc::clone(&queued),
        ));
        pending.push_back(QueuedRequestLine::new_observed(
            "second".to_owned(),
            Arc::clone(&queued),
        ));
        assert_eq!(queued.load(Ordering::Acquire), 2);

        let first = pending.pop_front().expect("first queued line").into_line();
        assert_eq!(first, "first");
        assert_eq!(queued.load(Ordering::Acquire), 1);

        drop(pending);
        assert_eq!(queued.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn cancellation_during_route_resolution_reaches_selected_live_target() {
        let _fixture_guard = DELAYED_ROUTE_FIXTURE_LOCK.lock().await;
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
        let resolver: super::super::RetainedProjectServerResolver =
            super::super::install_retained_project_server_resolver(move |_request| {
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
            tracedecay_mcp::transport::ChannelTransport::new();
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
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {"project_id": target_project_id},
                            "format": "json"
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

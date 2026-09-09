//! Transport-owned JSON-RPC connection scheduling and delivery.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::Ordering;

use serde_json::Value;

use crate::lifecycle::{McpConnectionLifecyclePort, McpRequestActivity};
use crate::transport::{McpTransport, write_wire_oversized_rejection};
use crate::{ErrorCode, JsonRpcRequest, JsonRpcResponse, serialize_response_line};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_framing::is_wire_oversized_io_error;

use super::{McpDispatchRequest, McpMethod, classify_mcp_method, dispatch_is_independent_read};

const MAX_PENDING_CANCELLABLE_REQUEST_LINES: usize = 64;

#[hotpath::measure(label = "mcp.server.connection.read", future = true)]
async fn read_connection_line(
    transport: &mut impl McpTransport,
) -> std::io::Result<Option<String>> {
    transport.read_line().await
}

#[hotpath::measure(label = "mcp.server.connection.inflight_read", future = true)]
async fn read_inflight_connection_line(
    transport: &mut impl McpTransport,
) -> std::io::Result<Option<String>> {
    transport.read_line().await
}

/// Selected-project response authority retained through transport delivery.
pub trait McpResponseLease: Send + 'static {
    fn revoked(&self) -> &tracedecay_session_memory::context::CancellationToken;
}

/// Per-connection routing state owned by the production request context.
pub trait McpConnectionState: Send + Sync + 'static {
    type ResponseLease: McpResponseLease;

    fn memory_request_scope(&self) -> &str;
    #[must_use]
    fn fork_for_independent_read(&self) -> Self;
    #[must_use]
    fn fork_for_connection_owned_read(&self) -> Self;
    fn take_selected_response_lease(&mut self) -> Option<Self::ResponseLease>;
}

/// The one production context required by the portable connection scheduler.
pub trait McpConnectionContext: Send + Sync + 'static {
    type Connection: McpConnectionState;

    fn new_connection(&self) -> Result<Self::Connection>;
    fn timings_enabled(&self) -> bool;
    fn build_version(&self) -> Result<&'static str>;
    fn max_concurrent_reads(&self) -> usize;
    fn tool_is_read_only(&self, tool_name: &str) -> bool;
    fn tool_supports_live_cancellation(&self, tool_name: &str) -> bool;
    fn dispatch<'a>(
        &'a self,
        request: McpDispatchRequest<'a>,
        timings_enabled: bool,
        connection: &'a mut Self::Connection,
        pre_cancelled: bool,
    ) -> Pin<Box<dyn Future<Output = Option<JsonRpcResponse>> + Send + 'a>>;
    fn cancel_request(&self, id: &Value, connection_scope: &str) -> bool;
    fn cancellation_registered(&self) -> &tokio::sync::Notify;
    fn take_pending_notifications(&self) -> Vec<Value>;
    fn run_in_connection_admission<'a, T, F>(
        &'a self,
        future: F,
    ) -> Pin<Box<dyn Future<Output = T> + Send + 'a>>
    where
        T: Send + 'a,
        F: Future<Output = T> + Send + 'a;
    fn shutdown(self: Arc<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// A portable connection scheduler over one real production context.
pub struct McpConnectionServer<C> {
    context: Arc<C>,
}

impl<C> McpConnectionServer<C>
where
    C: McpConnectionContext,
{
    pub fn new(context: Arc<C>) -> Arc<Self> {
        Arc::new(Self { context })
    }

    fn new_connection_route_state(&self) -> Result<C::Connection> {
        self.context.new_connection()
    }

    fn timings_enabled(&self) -> bool {
        self.context.timings_enabled()
    }

    fn cancel_application_surface_request(&self, id: &Value, connection_scope: &str) -> bool {
        self.context.cancel_request(id, connection_scope)
    }

    async fn handle_request_for_connection<'a>(
        &'a self,
        request: &'a JsonRpcRequest,
        timings_enabled: bool,
        connection: &'a mut C::Connection,
        pre_cancelled: bool,
    ) -> Option<JsonRpcResponse> {
        self.context
            .dispatch(
                McpDispatchRequest::from_legacy(request),
                timings_enabled,
                connection,
                pre_cancelled,
            )
            .await
    }

    async fn shutdown_if(self: &Arc<Self>, enabled: bool) {
        if enabled {
            Arc::clone(&self.context).shutdown().await;
        }
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
    fn new<C>(line: String, context: &C) -> Self
    where
        C: McpConnectionContext,
    {
        let parsed = hotpath::measure_block!(
            "mcp.server.connection.queued_decode",
            JsonRpcRequest::decode(line.trim())
        );
        let request = parsed.as_ref().ok();
        let request_id = request.and_then(|request| request.id.clone());
        let independent_read =
            request.is_some_and(|request| request_is_independent_read(request, context));
        let cancellable_request_id =
            request.and_then(|request| cancellable_queued_request_id(request, context));
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

    fn from_parsed<C>(line: String, request: Option<&JsonRpcRequest>, context: &C) -> Self
    where
        C: McpConnectionContext,
    {
        let request_id = request.and_then(|request| request.id.clone());
        let independent_read =
            request.is_some_and(|request| request_is_independent_read(request, context));
        let cancellable_request_id =
            request.and_then(|request| cancellable_queued_request_id(request, context));
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

fn cancellable_queued_request_id<C>(request: &JsonRpcRequest, context: &C) -> Option<Value>
where
    C: McpConnectionContext,
{
    let cancellable = request.method == "tools/call"
        && request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .is_some_and(|tool_name| context.tool_supports_live_cancellation(tool_name));
    if !cancellable {
        return None;
    }
    request.id.clone()
}

fn application_surface_request_id(id: &Value, connection_scope: &str) -> Option<String> {
    tracedecay_contracts::request_identity::mcp_connection_request_id(id, connection_scope)
        .map(|request_id| request_id.as_str().to_owned())
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
fn request_is_independent_read<C>(request: &JsonRpcRequest, context: &C) -> bool
where
    C: McpConnectionContext,
{
    dispatch_is_independent_read(
        classify_mcp_method(&request.method),
        request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str),
        |tool_name| context.tool_is_read_only(tool_name),
    )
}

struct ConcurrentReadCompletion<L> {
    request_key: Option<String>,
    _request_activity: Option<McpRequestActivity>,
    revocable_tool_call: Option<(Value, String)>,
    response: Option<JsonRpcResponse>,
    selected_response_lease: Option<L>,
    connection_scope: String,
    connection_closed: bool,
}

enum ConnectionLoopEvent<L> {
    Queued(String),
    Incoming(std::io::Result<Option<String>>),
    Completed(
        Box<Option<std::result::Result<ConcurrentReadCompletion<L>, tokio::task::JoinError>>>,
    ),
    Shutdown,
    PeerClosed,
}

#[hotpath::measure(label = "mcp.server.connection.read_dispatch", future = true)]
async fn dispatch_independent_read<C>(
    server: Arc<McpConnectionServer<C>>,
    request: JsonRpcRequest,
    timings_enabled: bool,
    mut connection: C::Connection,
    request_activity: Option<McpRequestActivity>,
    cancellation: tracedecay_session_memory::context::CancellationToken,
    connection_shutdown: tracedecay_session_memory::context::CancellationToken,
) -> ConcurrentReadCompletion<<C::Connection as McpConnectionState>::ResponseLease>
where
    C: McpConnectionContext,
{
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
                    let registered = server.context.cancellation_registered().notified();
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
    async fn write<C>(
        server: &McpConnectionServer<C>,
        transport: &mut impl McpTransport,
        completion: &mut ConcurrentReadCompletion<
            <C::Connection as McpConnectionState>::ResponseLease,
        >,
    ) -> std::io::Result<bool>
    where
        C: McpConnectionContext,
    {
        let response_revoked = completion
            .selected_response_lease
            .as_ref()
            .map(McpResponseLease::revoked);
        let notifications = server.context.take_pending_notifications();
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

impl<C> McpConnectionServer<C>
where
    C: McpConnectionContext,
{
    #[hotpath::measure(label = "mcp.server.write", future = true)]
    async fn write_response_line_or_revoke(
        &self,
        transport: &mut impl McpTransport,
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

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "mcp.server.request_cancellable", future = true)]
    async fn handle_cancellable_application_request(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        connection: &mut C::Connection,
        transport: &mut impl McpTransport,
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
                    let registered = self.context.cancellation_registered().notified();
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
                        JsonRpcRequest::decode(line.trim())
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
                        self.context.as_ref(),
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
        connection: &mut C::Connection,
        transport: &mut impl McpTransport,
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
                    pending_lines.push_back(QueuedRequestLine::new(line, self.context.as_ref()));
                }
            }
        }
    }

    /// Runs the server, reading JSON-RPC requests from stdin and writing
    /// responses to stdout. Runs until stdin is closed or a shutdown signal
    /// (SIGINT/SIGTERM) is received, then performs graceful cleanup.
    #[hotpath::skip]
    pub async fn run(self: &Arc<Self>, transport: &mut impl McpTransport) -> Result<()> {
        self.run_with_shutdown_policy(transport, true, true, None, None)
            .await
    }

    /// Runs one client connection without shutting down the server when that
    /// connection closes. Production daemon connections go through
    /// [`Self::run_daemon_connection_with_timings`]; direct servers and
    /// transport harnesses use this same connection loop without process
    /// shutdown.
    #[hotpath::skip]
    pub async fn run_connection(self: &Arc<Self>, transport: &mut impl McpTransport) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, None, None)
            .await
    }

    #[hotpath::skip]
    pub async fn run_daemon_connection_with_timings(
        self: &Arc<Self>,
        transport: &mut impl McpTransport,
        timings_enabled: bool,
        lifecycle: &dyn McpConnectionLifecyclePort,
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
    pub async fn run_with_shutdown_policy(
        self: &Arc<Self>,
        transport: &mut impl McpTransport,
        shutdown_on_exit: bool,
        listen_for_process_signals: bool,
        timings_override: Option<bool>,
        request_lifecycle: Option<&dyn McpConnectionLifecyclePort>,
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
        transport: &mut impl McpTransport,
        shutdown_on_exit: bool,
        listen_for_process_signals: bool,
        timings_override: Option<bool>,
        request_lifecycle: Option<&dyn McpConnectionLifecyclePort>,
    ) -> Result<()> {
        let mut connection_route = self.new_connection_route_state()?;
        let mut pending_lines: VecDeque<QueuedRequestLine> = VecDeque::new();
        let mut pending_cancellations = HashSet::new();
        let mut active_reads: tokio::task::JoinSet<
            ConcurrentReadCompletion<<C::Connection as McpConnectionState>::ResponseLease>,
        > = tokio::task::JoinSet::new();
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
                if !queued.independent_read
                    || active_reads.len() >= self.context.max_concurrent_reads()
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
                ConnectionLoopEvent::Queued(line)
                | ConnectionLoopEvent::Incoming(Ok(Some(line))) => Some(line),
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

            let parsed = hotpath::measure_block!(
                "mcp.server.connection.decode",
                JsonRpcRequest::decode(&line)
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
                && request_is_independent_read(request, self.context.as_ref())
            {
                let request_key = request.id.as_ref().and_then(|id| {
                    application_surface_request_id(id, connection_route.memory_request_scope())
                });
                let duplicate_in_flight = request_key
                    .as_ref()
                    .is_some_and(|key| active_cancellations.contains_key(key));
                if !duplicate_in_flight
                    && active_reads.len() < self.context.max_concurrent_reads()
                    && (line_from_queue || pending_lines.is_empty())
                {
                    let request_activity =
                        request_lifecycle.and_then(McpConnectionLifecyclePort::try_enter);
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
                    let context = Arc::clone(&self.context);
                    let dispatch = dispatch_independent_read(
                        Arc::clone(self),
                        request.clone(),
                        timings_enabled,
                        connection_route.fork_for_connection_owned_read(),
                        request_activity,
                        cancellation,
                        connection_shutdown.clone(),
                    );
                    active_reads
                        .spawn(async move { context.run_in_connection_admission(dispatch).await });
                    continue;
                }
            }

            if !active_reads.is_empty() {
                if pending_lines.len() >= MAX_PENDING_CANCELLABLE_REQUEST_LINES {
                    connection_shutdown.cancel();
                    while active_reads.join_next().await.is_some() {}
                    break;
                }
                pending_lines.push_back(QueuedRequestLine::from_parsed(
                    line,
                    parsed.as_ref().ok(),
                    self.context.as_ref(),
                ));
                continue;
            }

            let revocable_tool_call = parsed.as_ref().ok().and_then(|request| {
                (request.method == "tools/call").then_some(())?;
                let id = request.id.clone()?;
                let tool_name = request.params.as_ref()?.get("name")?.as_str()?.to_owned();
                Some((id, tool_name))
            });
            let request_activity =
                request_lifecycle.and_then(McpConnectionLifecyclePort::try_enter);
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
                                .is_some_and(|tool_name| {
                                    self.context.tool_supports_live_cancellation(tool_name)
                                });
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
                    Err(error) => Some(error.into_response()),
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use super::QueuedRequestLine;

    #[test]
    fn queued_request_depth_is_released_on_dequeue_and_connection_drop() {
        let queued = Arc::new(AtomicIsize::new(0));
        let mut pending = std::collections::VecDeque::new();
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
}

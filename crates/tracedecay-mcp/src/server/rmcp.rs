//! `rmcp` 3.x adapter for the authenticated `TraceDecay` MCP surface.
//!
//! The daemon owns authentication, bounded framing, project selection, and
//! replacement/retirement. Once that boundary selected a project server, this
//! adapter delegates standard MCP requests to the existing catalog and handler
//! authority through `rmcp`'s typed server callbacks.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CustomNotification, ErrorCode,
    ErrorData, Implementation, InitializeRequestParams, InitializeResult, ListResourcesResult,
    ListToolsResult, MetaObject, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, ServerCapabilities, ServerInfo,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{RwLock, Semaphore};

use crate::transport::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

use super::{
    McpConnectionContext, McpConnectionState, McpDispatchParams, McpDispatchRequest,
    McpResponseLease, dispatch_is_independent_read,
};

/// Per-RMCP-connection handoff from handler completion to the transport write.
///
/// A selected project response owns a read lease from its exact target server.
/// `rmcp` separates handler completion from response serialization, so the
/// lease must cross that gap keyed by the JSON-RPC request id. The transport
/// removes it exactly once when it sends or suppresses the response.
pub struct RmcpSelectedProjectResponseAuthority<L> {
    leases: Arc<std::sync::Mutex<std::collections::HashMap<String, L>>>,
}

impl<L> Clone for RmcpSelectedProjectResponseAuthority<L> {
    fn clone(&self) -> Self {
        Self {
            leases: Arc::clone(&self.leases),
        }
    }
}

impl<L> Default for RmcpSelectedProjectResponseAuthority<L> {
    fn default() -> Self {
        Self {
            leases: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

impl<L> RmcpSelectedProjectResponseAuthority<L> {
    fn request_key(id: &Value) -> tracedecay_domain::errors::Result<String> {
        if id.is_null() {
            return Err(tracedecay_domain::errors::TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                "selected RMCP response has no request identity",
            ));
        }
        serde_json::to_string(id).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                format!("selected RMCP response identity is invalid: {error}"),
            )
        })
    }

    pub fn retain(&self, id: &Value, lease: L) -> tracedecay_domain::errors::Result<()> {
        let key = Self::request_key(id)?;
        let mut leases = self.leases.lock().map_err(|_| {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                "selected RMCP response authority is poisoned during handler handoff",
            )
        })?;
        if leases.contains_key(&key) {
            return Err(tracedecay_domain::errors::TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                "selected RMCP response identity is already awaiting transport delivery",
            ));
        }
        leases.insert(key, lease);
        Ok(())
    }

    pub fn take(&self, id: Option<&Value>) -> tracedecay_domain::errors::Result<Option<L>> {
        let Some(id) = id else {
            return Ok(None);
        };
        // JSON-RPC error responses may legitimately carry `id: null` when no
        // request identity could be recovered. They cannot correspond to a
        // retained selected-project lease, so leave them deliverable through
        // the ordinary connection lifecycle rather than fabricating a route
        // authority failure.
        if id.is_null() {
            return Ok(None);
        }
        let key = Self::request_key(id)?;
        self.leases
            .lock()
            .map_err(|_| {
                tracedecay_domain::errors::TraceDecayError::project_route(
                    "project_route_unavailable",
                    true,
                    "selected RMCP response authority is poisoned during transport delivery",
                )
            })
            .map(|mut leases| leases.remove(&key))
    }
}

/// Allows daemon routing to enrich the legacy `initialize` response without
/// coupling this MCP module to daemon route types.
pub type RmcpInitializeResponseDecorator =
    Arc<dyn Fn(&mut JsonRpcResponse) + Send + Sync + 'static>;

/// Connection-local Work-delivery ledger input for the RMCP transport.
///
/// The RMCP request handler finishes before the transport writes its response.
/// Keeping the pending attempt with the transport makes the write-and-flush
/// boundary the only place allowed to offer a delivery settlement.
#[derive(Clone)]
pub struct RmcpWorkDeliverySettlement {
    recorder:
        Option<Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>>,
    connection_scope: String,
}

impl RmcpWorkDeliverySettlement {
    pub fn new(
        recorder: Option<
            Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>,
        >,
        connection_scope: String,
    ) -> Self {
        Self {
            recorder,
            connection_scope,
        }
    }

    pub fn attempt_for_request(
        &self,
        request: &Value,
    ) -> Option<tracedecay_domain::DeliverySettlementAttemptV1> {
        self.recorder.as_ref()?;
        (request.get("method").and_then(Value::as_str) == Some("tools/call")).then_some(())?;
        let request_id = request.get("id")?;
        let tool_name = request
            .get("params")
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)?;
        let operation_key = tool_name.strip_prefix("tracedecay_work_")?;
        tracedecay_api::WorkOperation::ALL
            .into_iter()
            .find(|operation| operation.operation_key() == operation_key)?;
        let identity = tracedecay_domain::canonical_sha256(&(
            "tracedecay.mcp-work-delivery.v1",
            &self.connection_scope,
            tool_name,
            request_id,
        ))
        .ok()?;
        let channel = tracedecay_domain::canonical_sha256(&(
            "tracedecay.mcp-delivery-channel.v1",
            &self.connection_scope,
        ))
        .ok()?;
        let identity = identity.as_str().trim_start_matches("sha256:");
        let channel = channel.as_str().trim_start_matches("sha256:");
        let observed_at = tracedecay_contracts::clock::now_micros();
        Some(tracedecay_domain::DeliverySettlementAttemptV1 {
            owner_event_id: format!("work:mcp-response:{identity}"),
            event_class: tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
            channel: tracedecay_domain::DeliveryChannelIdentityV1 {
                surface: tracedecay_domain::DeliverySurfaceFamilyV1::Mcp,
                channel_ref: format!("mcp:connection:{channel}"),
            },
            work_attempt: None,
            eligible: 1,
            valid_at: observed_at,
            attempted_at: observed_at,
        })
    }

    pub fn settle(
        &self,
        attempt: tracedecay_domain::DeliverySettlementAttemptV1,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        let settlement = tracedecay_domain::DeliverySettlementV1 {
            settled_at: std::cmp::max(
                attempt.attempted_at,
                tracedecay_contracts::clock::now_micros(),
            ),
            attempt,
            outcome,
            drop_reason,
        };
        match recorder.try_record(settlement) {
            Ok(tracedecay_application::observability::DeliverySettlementRecordOutcomeV1::Enqueued) => {}
            Ok(tracedecay_application::observability::DeliverySettlementRecordOutcomeV1::DroppedAtCapacity) => {
                tracing::warn!("RMCP Work delivery settlement was dropped at recorder capacity");
            }
            Err(error) => tracing::warn!(%error, "RMCP Work delivery settlement was refused"),
        }
    }
}

pub async fn await_dispatch_with_cancellation<F, C, N>(
    handling: F,
    cancellation: N,
    mut cancel_registered_request: C,
) -> Option<F::Output>
where
    F: std::future::Future,
    C: FnMut() -> bool,
    N: std::future::Future<Output = ()>,
{
    tokio::pin!(handling);
    tokio::pin!(cancellation);
    tokio::select! {
        response = &mut handling => Some(response),
        () = &mut cancellation => {
            if cancel_registered_request() {
                Some(handling.await)
            } else {
                None
            }
        }
    }
}

/// Per-connection `rmcp` server facade over the existing `TraceDecay` request
/// authority.
pub struct RmcpConnectionAdapter<C>
where
    C: McpConnectionContext,
{
    context: Arc<C>,
    connection: RwLock<C::Connection>,
    request_admission: Semaphore,
    memory_request_scope: String,
    timings_enabled: bool,
    selected_project_responses:
        RmcpSelectedProjectResponseAuthority<<C::Connection as McpConnectionState>::ResponseLease>,
    work_delivery_settlement: RmcpWorkDeliverySettlement,
    initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
    /// Resolved from the registered product runtime at construction, because
    /// `ServerHandler::get_info` is infallible and must not fabricate one.
    build_version: &'static str,
}

struct RmcpQueueDepthGuard;

impl RmcpQueueDepthGuard {
    fn enter() -> Self {
        hotpath::gauge!("mcp.server.rmcp.queue_depth").inc(1_u64);
        Self
    }
}

impl Drop for RmcpQueueDepthGuard {
    fn drop(&mut self) {
        hotpath::gauge!("mcp.server.rmcp.queue_depth").dec(1_u64);
    }
}

impl<C> RmcpConnectionAdapter<C>
where
    C: McpConnectionContext,
{
    pub fn new(
        context: Arc<C>,
        timings_enabled: bool,
        initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
        delivery_settlement_recorder: Option<
            Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>,
        >,
    ) -> tracedecay_domain::errors::Result<Self> {
        let connection = context.new_connection()?;
        let memory_request_scope = connection.memory_request_scope().to_owned();
        let build_version = context.build_version()?;
        let max_concurrent_reads = context.max_concurrent_reads();
        Ok(Self {
            context,
            connection: RwLock::new(connection),
            request_admission: Semaphore::new(max_concurrent_reads),
            work_delivery_settlement: RmcpWorkDeliverySettlement::new(
                delivery_settlement_recorder,
                memory_request_scope.clone(),
            ),
            memory_request_scope,
            timings_enabled,
            selected_project_responses: RmcpSelectedProjectResponseAuthority::default(),
            initialize_response_decorator,
            build_version,
        })
    }

    pub fn work_delivery_settlement(&self) -> RmcpWorkDeliverySettlement {
        self.work_delivery_settlement.clone()
    }

    pub fn selected_project_responses(
        &self,
    ) -> RmcpSelectedProjectResponseAuthority<<C::Connection as McpConnectionState>::ResponseLease>
    {
        self.selected_project_responses.clone()
    }

    #[hotpath::measure(label = "mcp.server.rmcp.dispatch_total", future = true)]
    async fn dispatch(
        &self,
        context: RequestContext<RoleServer>,
        method: &'static str,
        params: McpDispatchParams<'_>,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let queued_at = std::time::Instant::now();
        let queued = RmcpQueueDepthGuard::enter();
        let request_permit = self.acquire_request_permit().await?;
        drop(queued);
        hotpath::gauge!("mcp.server.rmcp.queue_wait_us")
            .set(queued_at.elapsed().as_micros() as u64);
        // Heap-allocate the admission + dispatch composition: rmcp's generated
        // `handle_request` polls every handler-method future inline, and the
        // combined resident frame overflows the worker stack in perf-profile
        // layouts when this mega-future is embedded by value.
        let result = self
            .context
            .run_in_connection_admission(self.dispatch_admitted(context, method, params))
            .await;
        drop(request_permit);
        result
    }

    #[hotpath::measure(label = "mcp.server.rmcp.queue_wait", future = true)]
    async fn acquire_request_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ErrorData> {
        self.request_admission
            .acquire()
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }

    #[hotpath::measure(label = "mcp.server.rmcp.dispatch", future = true)]
    async fn dispatch_admitted(
        &self,
        context: RequestContext<RoleServer>,
        method: &'static str,
        params: McpDispatchParams<'_>,
    ) -> Result<JsonRpcResponse, ErrorData> {
        // The wire identity is the one value internal dispatch genuinely keys
        // on (cancellation identity, response leases, delivery settlement), so
        // it is converted once, infallibly, and never re-derived.
        let id = context.id.into_json_value();
        let request_cancellation = context.ct;
        let request = McpDispatchRequest::typed(id.clone(), method, params);
        if dispatch_is_independent_read(request.method_class(), request.tool_name(), |tool_name| {
            self.context.tool_is_read_only(tool_name)
        }) {
            let ordering_guard = self.connection.read().await;
            let mut request_connection = ordering_guard.fork_for_independent_read();
            let result = self
                .dispatch_request_with_connection(
                    request,
                    id,
                    request_cancellation,
                    &mut request_connection,
                )
                .await;
            drop(ordering_guard);
            return result;
        }
        let mut connection = self.connection.write().await;
        self.dispatch_request_with_connection(request, id, request_cancellation, &mut connection)
            .await
    }

    #[hotpath::measure(label = "mcp.server.rmcp.dispatch_request", future = true)]
    async fn dispatch_request_with_connection(
        &self,
        request: McpDispatchRequest<'_>,
        id: Value,
        request_cancellation: tokio_util::sync::CancellationToken,
        connection: &mut C::Connection,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let pre_cancelled = request_cancellation.is_cancelled();
        // The legacy MCP route already erases this shared dispatch authority
        // before awaiting it. Keep the typed RMCP route at the same ownership
        // boundary: the cancellation combinator otherwise stores the complete
        // catalog-dispatch future inline in rmcp's generated request future.
        let handling =
            self.context
                .dispatch(request, self.timings_enabled, connection, pre_cancelled);
        let response = if pre_cancelled {
            Some(handling.await)
        } else {
            await_dispatch_with_cancellation(handling, request_cancellation.cancelled(), || {
                self.context.cancel_request(&id, &self.memory_request_scope)
            })
            .await
        }
        .ok_or_else(|| {
            ErrorData::new(
                ErrorCode(-32800),
                "MCP request cancelled",
                Some(json!({"reason_code": "request_cancelled"})),
            )
        })?
        .ok_or_else(|| ErrorData::internal_error("MCP request did not produce a response", None))?;
        let selected_response_lease = connection.take_selected_response_lease();
        if selected_response_lease
            .as_ref()
            .is_some_and(|lease| lease.revoked().is_cancelled())
        {
            return Err(project_server_retired_error());
        }
        if let Some(selected_response_lease) = selected_response_lease {
            self.selected_project_responses
                .retain(&id, selected_response_lease)
                .map_err(|error| {
                    ErrorData::internal_error(
                        error.to_string(),
                        Some(json!({
                            "reason_code": "project_route_unavailable",
                            "retryable": true,
                        })),
                    )
                })?;
        }
        Ok(response)
    }

    #[hotpath::measure(label = "mcp.server.rmcp.notification", future = true)]
    async fn dispatch_notification(&self, method: String, params: Option<Value>) {
        // Custom notifications (hook events, cancellations) have no typed
        // `rmcp` DTO: their params arrive as JSON and stay JSON.
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: None,
            method,
            params,
        };
        let mut connection = self.connection.write().await;
        let _ = self
            .context
            .dispatch(
                McpDispatchRequest::from_legacy(&request),
                self.timings_enabled,
                &mut connection,
                false,
            )
            .await;
    }

    fn cancel_request(&self, request_id: Option<rmcp::model::RequestId>) -> bool {
        request_id
            .map(rmcp::model::RequestId::into_json_value)
            .is_some_and(|request_id| {
                self.context
                    .cancel_request(&request_id, &self.memory_request_scope)
            })
    }

    /// Serve one connection with the handshake guard installed.
    ///
    /// This deliberately shadows [`rmcp::ServiceExt::serve`]: `rmcp` moves the
    /// transport into its initialization state machine, so a caller can no
    /// longer reach the wire once that machine fails. Installing the guard
    /// here is what keeps every call site — daemon routing, benchmarks,
    /// tests — on the same typed-refusal behavior.
    pub async fn serve<T>(
        self,
        transport: T,
    ) -> std::result::Result<
        rmcp::service::RunningService<RoleServer, Self>,
        rmcp::service::ServerInitializeError,
    >
    where
        T: rmcp::transport::Transport<RoleServer> + Send + 'static,
    {
        rmcp::service::serve_server(
            self,
            GuardedHandshakeTransport {
                inner: transport,
                handshake_settled: false,
            },
        )
        .await
    }
}

pub fn rmcp_response_result<T: DeserializeOwned>(
    response: JsonRpcResponse,
) -> Result<T, ErrorData> {
    match (response.result, response.error) {
        (Some(result), None) => serde_json::from_value(result)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None)),
        (_, Some(error)) => Err(rmcp_error(error)),
        _ => Err(ErrorData::internal_error(
            "TraceDecay MCP handler returned neither result nor error",
            None,
        )),
    }
}

/// The typed refusal for an `initialize` whose params `rmcp` could not decode.
const MALFORMED_INITIALIZE_MESSAGE: &str = "initialize params are missing or malformed: \
     protocolVersion, capabilities and clientInfo are required";

/// Answers a malformed `initialize` with a typed JSON-RPC error frame and
/// leaves the connection able to accept a corrected handshake.
///
/// `rmcp` demotes an `initialize` whose params do not deserialize into
/// `InitializeRequestParams` to a `CustomRequest`, and its pre-initialize state
/// machine then fails with `ExpectedInitializeRequest` *without writing
/// anything to the wire* (rmcp 3.1.1, `service/server.rs`). Daemon routing maps
/// that failure to a config error and drops the socket, so the client only sees
/// "the daemon closed the connection after the request was sent but before
/// returning a matching response" — a transport mystery for what is a
/// definitive protocol answer, exactly like the unparseable-handshake and
/// rejected-auth refusals the daemon already writes before closing.
struct GuardedHandshakeTransport<T> {
    inner: T,
    /// Set once a request that ends `rmcp`'s pre-initialize loop is forwarded.
    /// After that the guard is inert: a later stray `initialize` is an ordinary
    /// request the adapter answers with a typed error of its own.
    handshake_settled: bool,
}

impl<T> rmcp::transport::Transport<RoleServer> for GuardedHandshakeTransport<T>
where
    T: rmcp::transport::Transport<RoleServer> + Send + 'static,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<(), Self::Error>> + Send + 'static
    {
        self.inner.send(item)
    }

    fn receive(
        &mut self,
    ) -> impl std::future::Future<Output = Option<rmcp::service::RxJsonRpcMessage<RoleServer>>> + Send
    {
        async move {
            loop {
                let message = self.inner.receive().await?;
                if self.handshake_settled {
                    return Some(message);
                }
                let rmcp::model::ClientJsonRpcMessage::Request(request) = &message else {
                    return Some(message);
                };
                let malformed_initialize = request.request.method() == "initialize"
                    && !matches!(
                        request.request,
                        rmcp::model::ClientRequest::InitializeRequest(_)
                    );
                if !malformed_initialize {
                    // `rmcp` answers a pre-initialize ping in place and keeps
                    // waiting; any other request ends its handshake loop.
                    self.handshake_settled =
                        !matches!(request.request, rmcp::model::ClientRequest::PingRequest(_));
                    return Some(message);
                }
                let refusal = rmcp::model::ServerJsonRpcMessage::error(
                    ErrorData::invalid_params(MALFORMED_INITIALIZE_MESSAGE, None),
                    Some(request.id.clone()),
                );
                if self.inner.send(refusal).await.is_err() {
                    return None;
                }
            }
        }
    }

    fn close(
        &mut self,
    ) -> impl std::future::Future<Output = std::result::Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

impl<C> ServerHandler for RmcpConnectionAdapter<C>
where
    C: McpConnectionContext,
{
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("tracedecay", self.build_version))
    }

    #[hotpath::skip]
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let mut response = self
            .dispatch(
                context,
                "initialize",
                McpDispatchParams::Initialize(&request),
            )
            .await?;
        if let Some(decorate) = &self.initialize_response_decorator {
            decorate(&mut response);
        }
        rmcp_response_result(response)
    }

    #[hotpath::skip]
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        rmcp_response_result(
            self.dispatch(context, "tools/list", McpDispatchParams::TypedEmpty)
                .await?,
        )
    }

    #[hotpath::skip]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let started =
            (self.timings_enabled || self.context.timings_enabled()).then(std::time::Instant::now);
        let mut result = rmcp_response_result::<CallToolResult>(
            self.dispatch(context, "tools/call", McpDispatchParams::ToolsCall(request))
                .await?,
        )?;
        if let Some(started) = started {
            result
                .meta
                .get_or_insert_with(MetaObject::new)
                .0
                .entry("duration_us".to_owned())
                .or_insert_with(|| json!(started.elapsed().as_micros() as u64));
        }
        Ok(result.into())
    }

    #[hotpath::skip]
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        rmcp_response_result(
            self.dispatch(context, "resources/list", McpDispatchParams::TypedEmpty)
                .await?,
        )
    }

    #[hotpath::skip]
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        rmcp_response_result::<ReadResourceResult>(
            self.dispatch(
                context,
                "resources/read",
                McpDispatchParams::ResourcesRead(&request),
            )
            .await?,
        )
        .map(Into::into)
    }

    #[hotpath::skip]
    async fn on_cancelled(
        &self,
        notification: rmcp::model::CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        let _ = self.cancel_request(notification.request_id);
    }

    #[hotpath::skip]
    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _context: NotificationContext<RoleServer>,
    ) {
        self.dispatch_notification(notification.method, notification.params)
            .await;
    }
}

fn rmcp_error(error: JsonRpcError) -> ErrorData {
    ErrorData::new(ErrorCode(error.code), error.message, error.data)
}

pub fn project_server_retired_error() -> ErrorData {
    ErrorData::internal_error(
        "tool project route failed: project server was retired",
        Some(json!({
            "reason_code": "project_server_retired",
            "retryable": true,
            "detail": "the retained project server was replaced or revoked; retry against the current owner",
        })),
    )
}

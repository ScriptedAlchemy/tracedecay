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
    ListToolsResult, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{RwLock, Semaphore};

use tracedecay_mcp::transport::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

use super::dispatch_envelope::{
    McpDispatchParams, McpDispatchRequest, dispatch_is_independent_read,
};
use super::{ConnectionRouteState, McpServer};

/// Per-RMCP-connection handoff from handler completion to the transport write.
///
/// A selected project response owns a read lease from its exact target server.
/// `rmcp` separates handler completion from response serialization, so the
/// lease must cross that gap keyed by the JSON-RPC request id. The transport
/// removes it exactly once when it sends or suppresses the response.
#[derive(Clone, Default)]
pub(crate) struct RmcpSelectedProjectResponseAuthority {
    leases: Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, super::routing::SelectedProjectResponseLease>,
        >,
    >,
}

impl RmcpSelectedProjectResponseAuthority {
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

    pub(crate) fn retain(
        &self,
        id: &Value,
        lease: super::routing::SelectedProjectResponseLease,
    ) -> tracedecay_domain::errors::Result<()> {
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

    pub(crate) fn take(
        &self,
        id: Option<&Value>,
    ) -> tracedecay_domain::errors::Result<Option<super::routing::SelectedProjectResponseLease>>
    {
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
pub(crate) type RmcpInitializeResponseDecorator =
    Arc<dyn Fn(&mut JsonRpcResponse) + Send + Sync + 'static>;

/// Connection-local Work-delivery ledger input for the RMCP transport.
///
/// The RMCP request handler finishes before the transport writes its response.
/// Keeping the pending attempt with the transport makes the write-and-flush
/// boundary the only place allowed to offer a delivery settlement.
#[derive(Clone)]
pub(crate) struct RmcpWorkDeliverySettlement {
    recorder: Option<Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>>,
    connection_scope: String,
}

impl RmcpWorkDeliverySettlement {
    pub(crate) fn new(
        recorder: Option<
            Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>,
        >,
        connection_scope: String,
    ) -> Self {
        Self {
            recorder,
            connection_scope,
        }
    }

    pub(crate) fn attempt_for_request(
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
        crate::mcp::tools::binding::work_operation_for_tool(tool_name)?;
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
        let observed_at = tracedecay_application::clock::now_micros();
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

    pub(crate) fn settle(
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
                tracedecay_application::clock::now_micros(),
            ),
            attempt,
            outcome,
            drop_reason,
        };
        match recorder.try_record(settlement) {
            Ok(tracedecay_usecases::observability::DeliverySettlementRecordOutcomeV1::Enqueued) => {}
            Ok(tracedecay_usecases::observability::DeliverySettlementRecordOutcomeV1::DroppedAtCapacity) => {
                tracing::warn!("RMCP Work delivery settlement was dropped at recorder capacity");
            }
            Err(error) => tracing::warn!(%error, "RMCP Work delivery settlement was refused"),
        }
    }
}

async fn await_dispatch_with_cancellation<F, C, N>(
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
pub(crate) struct RmcpConnectionAdapter {
    server: Arc<McpServer>,
    connection: RwLock<ConnectionRouteState>,
    request_admission: Semaphore,
    memory_request_scope: String,
    timings_enabled: bool,
    selected_project_responses: RmcpSelectedProjectResponseAuthority,
    initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
    /// The accepted connection's admission slot, captured on the connection task.
    ///
    /// `rmcp` runs the request loop on a task it spawns, which does not inherit
    /// the connection's task-local, so each dispatch re-enters this scope. That
    /// is what lets a tool call parked on a generation decode hand its admission
    /// slot back instead of starving tools that need no generation at all.
    admission: Option<Arc<crate::daemon::ParkableConnectionAdmission>>,
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

impl RmcpConnectionAdapter {
    pub(crate) fn new(
        server: Arc<McpServer>,
        timings_enabled: bool,
        initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
    ) -> tracedecay_domain::errors::Result<Self> {
        let connection = server.new_connection_route_state()?;
        let memory_request_scope = connection.memory_request_scope().to_owned();
        Ok(Self {
            server,
            connection: RwLock::new(connection),
            request_admission: Semaphore::new(super::connection::MAX_CONCURRENT_CONNECTION_READS),
            memory_request_scope,
            timings_enabled,
            selected_project_responses: RmcpSelectedProjectResponseAuthority::default(),
            initialize_response_decorator,
            admission: crate::daemon::current_connection_admission(),
            build_version: crate::version::build_version()?,
        })
    }

    pub(crate) fn work_delivery_settlement(&self) -> RmcpWorkDeliverySettlement {
        RmcpWorkDeliverySettlement::new(
            self.server.delivery_settlement_recorder.clone(),
            self.memory_request_scope.clone(),
        )
    }

    pub(crate) fn selected_project_responses(&self) -> RmcpSelectedProjectResponseAuthority {
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
        let result = Box::pin(crate::daemon::in_connection_admission(
            self.admission.clone(),
            self.dispatch_admitted(context, method, params),
        ))
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
        if dispatch_is_independent_read(request.method_class(), request.tool_name()) {
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
        connection: &mut ConnectionRouteState,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let pre_cancelled = request_cancellation.is_cancelled();
        // The legacy MCP route already erases this shared dispatch authority
        // before awaiting it. Keep the typed RMCP route at the same ownership
        // boundary: the cancellation combinator otherwise stores the complete
        // catalog-dispatch future inline in rmcp's generated request future.
        let handling = Box::pin(self.server.dispatch_envelope(
            request,
            self.timings_enabled,
            connection,
            pre_cancelled,
        ));
        let response = if pre_cancelled {
            Some(handling.await)
        } else {
            await_dispatch_with_cancellation(handling, request_cancellation.cancelled(), || {
                self.server
                    .cancel_application_surface_request(&id, &self.memory_request_scope)
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
            .is_some_and(crate::mcp::server::routing::SelectedProjectResponseLease::is_revoked)
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
            .server
            .handle_request_for_connection(&request, self.timings_enabled, &mut connection, false)
            .await;
    }

    fn cancel_request(&self, request_id: Option<rmcp::model::RequestId>) -> bool {
        request_id
            .map(rmcp::model::RequestId::into_json_value)
            .is_some_and(|request_id| {
                self.server
                    .cancel_application_surface_request(&request_id, &self.memory_request_scope)
            })
    }

    fn response_result<T: DeserializeOwned>(response: JsonRpcResponse) -> Result<T, ErrorData> {
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
}

impl ServerHandler for RmcpConnectionAdapter {
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
        Self::response_result(response)
    }

    #[hotpath::skip]
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Self::response_result(
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
        Self::response_result::<CallToolResult>(
            self.dispatch(context, "tools/call", McpDispatchParams::ToolsCall(request))
                .await?,
        )
        .map(Into::into)
    }

    #[hotpath::skip]
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Self::response_result(
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
        Self::response_result::<ReadResourceResult>(
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

fn project_server_retired_error() -> ErrorData {
    ErrorData::internal_error(
        "tool project route failed: project server was retired",
        Some(json!({
            "reason_code": "project_server_retired",
            "retryable": true,
            "detail": "the retained project server was replaced or revoked; retry against the current owner",
        })),
    )
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ReadResourceRequestParams,
    };
    use rmcp::service::ServiceRole;
    use rmcp::transport::{IntoTransport, Transport};
    use rmcp::{RoleClient, ServiceExt};
    use serde::Serialize;
    use serde_json::json;

    use super::*;

    struct RecordingTransport<R, T>
    where
        R: ServiceRole,
    {
        inner: T,
        messages: Arc<std::sync::Mutex<Vec<Value>>>,
        _role: std::marker::PhantomData<R>,
    }

    impl<R, T> RecordingTransport<R, T>
    where
        R: ServiceRole,
    {
        fn new(inner: T, messages: Arc<std::sync::Mutex<Vec<Value>>>) -> Self {
            Self {
                inner,
                messages,
                _role: std::marker::PhantomData,
            }
        }
    }

    impl<R, T> Transport<R> for RecordingTransport<R, T>
    where
        R: ServiceRole,
        T: Transport<R> + 'static,
        rmcp::service::TxJsonRpcMessage<R>: Serialize,
    {
        type Error = T::Error;

        fn name() -> Cow<'static, str> {
            "rmcp-wire-recording".into()
        }

        fn send(
            &mut self,
            item: rmcp::service::TxJsonRpcMessage<R>,
        ) -> impl std::future::Future<Output = std::result::Result<(), Self::Error>> + Send + 'static
        {
            let encoded = serde_json::to_value(&item).expect("record RMCP wire message");
            self.messages
                .lock()
                .expect("RMCP wire recording lock")
                .push(encoded);
            self.inner.send(item)
        }

        fn receive(
            &mut self,
        ) -> impl std::future::Future<Output = Option<rmcp::service::RxJsonRpcMessage<R>>> + Send
        {
            self.inner.receive()
        }

        fn close(
            &mut self,
        ) -> impl std::future::Future<Output = std::result::Result<(), Self::Error>> + Send
        {
            self.inner.close()
        }
    }

    struct RmcpWireFixture {
        client: rmcp::service::RunningService<RoleClient, ()>,
        server: Arc<McpServer>,
        client_messages: Arc<std::sync::Mutex<Vec<Value>>>,
        server_messages: Arc<std::sync::Mutex<Vec<Value>>>,
        serving: tokio::task::JoinHandle<()>,
        _repo: tempfile::TempDir,
        _authority: crate::mcp::server::writer_test_support::WriterTestFixtureAuthority,
    }

    impl RmcpWireFixture {
        async fn start() -> Self {
            crate::product_runtime::register_fixture_product_runtime();
            let (cg, repo, authority) =
                crate::mcp::server::writer_test_support::init_indexed_repo().await;
            let context =
                crate::mcp::server::writer_test_support::registered_context(cg, &authority);
            let server = McpServer::new_with_registered_test_context(context, Vec::new())
                .await
                .expect("registered RMCP wire server");
            let adapter =
                RmcpConnectionAdapter::new(Arc::clone(&server), false, Some(Arc::new(|response| {
                    response.result.as_mut().expect("initialize result")["_meta"]
                        ["tracedecayInitializeRoute"] = json!({
                            "projectPath": "/wire/oracle",
                            "allowInit": false,
                        });
                })))
                .expect("RMCP adapter");
            let (server_io, client_io) = tokio::io::duplex(2 * 1024 * 1024);
            let server_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
            let server_transport = RecordingTransport::<RoleServer, _>::new(
                IntoTransport::<RoleServer, _, _>::into_transport(server_io),
                Arc::clone(&server_messages),
            );
            let serving = tokio::spawn(async move {
                let running = adapter
                    .serve(server_transport)
                    .await
                    .expect("serve RMCP adapter");
                running.waiting().await.expect("RMCP adapter task");
            });
            let client_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
            let client_transport = RecordingTransport::<RoleClient, _>::new(
                IntoTransport::<RoleClient, _, _>::into_transport(client_io),
                Arc::clone(&client_messages),
            );
            let client = ().serve(client_transport).await.expect("initialize RMCP client");
            Self {
                client,
                server,
                client_messages,
                server_messages,
                serving,
                _repo: repo,
                _authority: authority,
            }
        }

        fn last_request(&self) -> JsonRpcRequest {
            let response_id = self.last_response()["id"].clone();
            let messages = self
                .client_messages
                .lock()
                .expect("client wire recording lock");
            let request = messages
                .iter()
                .rev()
                .find(|message| message.get("id") == Some(&response_id))
                .expect("recorded client request for response");
            serde_json::from_value(request.clone()).expect("legacy request shape")
        }

        fn last_response(&self) -> Value {
            self.server_messages
                .lock()
                .expect("server wire recording lock")
                .last()
                .expect("recorded server response")
                .clone()
        }

        async fn assert_last_response_matches_legacy(&self, decorate_initialize: bool) {
            let request = self.last_request();
            let mut expected = self
                .server
                .handle_request(&request)
                .await
                .expect("legacy response");
            if decorate_initialize {
                expected.result.as_mut().expect("legacy initialize result")["_meta"]["tracedecayInitializeRoute"] = json!({
                    "projectPath": "/wire/oracle",
                    "allowInit": false,
                });
            }
            assert_eq!(
                self.last_response(),
                serde_json::to_value(expected).expect("serialize legacy response"),
            );
        }

        async fn shutdown(mut self) {
            self.client.close().await.expect("close RMCP client");
            self.serving.await.expect("join RMCP server");
            self.server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn rmcp_wire_matrix_matches_legacy_initialize_tools_and_resources() {
        let fixture = RmcpWireFixture::start().await;
        let initialize_id = fixture.last_response()["id"].clone();
        let mut initialize_result =
            crate::mcp::server::initialize_result(crate::mcp::server::SERVER_INSTRUCTIONS)
                .expect("initialize oracle");
        initialize_result["protocolVersion"] = json!("2025-11-25");
        initialize_result["_meta"]["tracedecayInitializeRoute"] = json!({
            "projectPath": "/wire/oracle",
            "allowInit": false,
        });
        assert_eq!(
            fixture.last_response(),
            serde_json::to_value(JsonRpcResponse::success(initialize_id, initialize_result))
                .expect("serialize initialize oracle"),
            "rmcp negotiates the client protocol version while preserving the legacy payload",
        );
        assert_eq!(
            fixture.last_response()["result"]["_meta"]["tracedecayInitializeRoute"],
            json!({"projectPath": "/wire/oracle", "allowInit": false}),
            "rmcp InitializeResult must preserve daemon-selected route metadata",
        );

        fixture
            .client
            .list_tools(None)
            .await
            .expect("RMCP tools/list");
        fixture.assert_last_response_matches_legacy(false).await;

        fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_status").with_arguments(
                    json!({"admission_only": true, "format": "json"})
                        .as_object()
                        .cloned()
                        .expect("object arguments"),
                ),
            )
            .await
            .expect("RMCP tools/call success");
        assert_eq!(
            fixture.last_response()["result"]["content"][0]["type"],
            json!("text"),
        );
        assert!(
            fixture.last_response()["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "RMCP success must preserve the real handler's text content",
        );

        let handler_error = fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_not_a_tool")
                    .with_arguments(serde_json::Map::new()),
            )
            .await
            .expect_err("unknown tool must be a JSON-RPC error");
        fixture.assert_last_response_matches_legacy(false).await;
        assert_eq!(
            fixture.last_response()["error"]["code"],
            json!(-32603),
            "handler error code is a host-visible protocol contract",
        );
        assert!(
            handler_error.to_string().contains("unknown tool"),
            "typed rmcp client must receive the handler error",
        );

        fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_changelog").with_arguments(
                    json!({"from_ref": "missing-rmcp-oracle-ref", "to_ref": "HEAD"})
                        .as_object()
                        .cloned()
                        .expect("object arguments"),
                ),
            )
            .await
            .expect("semantic refusal stays a completed tool result");
        assert_eq!(
            fixture.last_response()["result"]["isError"],
            json!(true),
            "typed refusals must remain successful JSON-RPC responses with isError=true",
        );

        fixture
            .client
            .list_resources(None)
            .await
            .expect("RMCP resources/list");
        fixture.assert_last_response_matches_legacy(false).await;

        fixture
            .client
            .read_resource(ReadResourceRequestParams::new("tracedecay://schema"))
            .await
            .expect("RMCP resources/read");
        fixture.assert_last_response_matches_legacy(false).await;

        let unknown_resource = fixture
            .client
            .read_resource(ReadResourceRequestParams::new(
                "tracedecay://not-a-resource",
            ))
            .await
            .expect_err("an unknown resource URI must be a JSON-RPC error");
        fixture.assert_last_response_matches_legacy(false).await;
        assert_eq!(
            fixture.last_response()["error"]["code"],
            json!(-32602),
            "the typed resources/read refusal keeps the legacy invalid-params code",
        );
        assert!(
            unknown_resource
                .to_string()
                .contains("unknown resource URI: tracedecay://not-a-resource"),
            "the typed rmcp client must receive the handler's own refusal text",
        );

        for index in 0..8 {
            fixture
                .client
                .call_tool(
                    CallToolRequestParams::new("tracedecay_fact_store_add").with_arguments(
                        json!({
                            "content": format!(
                                "RMCP_WIRE_ORACLE_{index:02}: {}",
                                "large response remains retrievable ".repeat(180),
                            ),
                            "category": "project",
                            "trust": 0.9,
                            "format": "json",
                        })
                        .as_object()
                        .cloned()
                        .expect("object arguments"),
                    ),
                )
                .await
                .expect("seed large RMCP tools/call");
        }
        fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_fact_store_list").with_arguments(
                    json!({
                        "category": "project",
                        "min_trust": 0.0,
                        "limit": 200,
                        "format": "json",
                    })
                    .as_object()
                    .cloned()
                    .expect("object arguments"),
                ),
            )
            .await
            .expect("large RMCP tools/call");
        let large_response = fixture.last_response();
        let large_text = large_response["result"]["content"][0]["text"]
            .as_str()
            .expect("large response text");
        let large_envelope: Value =
            serde_json::from_str(large_text).expect("large response truncation envelope");
        assert_eq!(large_envelope["truncated"], json!(true));
        assert!(
            large_envelope["original_chars"]
                .as_u64()
                .unwrap_or_default()
                >= 15_000,
            "large response must cross the production response budget",
        );
        assert!(
            large_envelope["handle"]
                .as_str()
                .is_some_and(|handle| handle.starts_with("rh_")),
            "large response must retain a typed retrieval handle",
        );
        fixture.shutdown().await;
    }

    /// The legacy raw JSON-RPC transport must stay byte-for-byte what it was
    /// before the typed envelope: the envelope is an internal representation,
    /// never a wire change. These are the shapes a host actually parses —
    /// method refusals, param refusals, the trivial ack, and a resource body —
    /// pinned as exact serialized frames rather than as structural matches.
    #[tokio::test]
    async fn legacy_json_rpc_wire_frames_are_unchanged_by_the_typed_envelope() {
        crate::product_runtime::register_fixture_product_runtime();
        let (cg, _repo, authority) =
            crate::mcp::server::writer_test_support::init_indexed_repo().await;
        let context = crate::mcp::server::writer_test_support::registered_context(cg, &authority);
        let server = McpServer::new_with_registered_test_context(context, Vec::new())
            .await
            .expect("registered legacy wire server");

        for (request_line, expected) in [
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#,
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found: nope"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{}}"#,
                r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"missing 'uri' in resources/read params"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"tracedecay://nope"}}"#,
                r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"unknown resource URI: tracedecay://nope"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":4,"method":"tools/call"}"#,
                r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"missing params for tools/call"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32602,"message":"missing 'name' in tools/call params"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"ack","method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":"ack","result":{}}"#,
            ),
        ] {
            let request: JsonRpcRequest =
                serde_json::from_str(request_line).expect("legacy request line");
            let response = server
                .handle_request(&request)
                .await
                .expect("legacy response");
            assert_eq!(
                serde_json::to_string(&response).expect("serialize legacy response"),
                expected,
                "legacy wire frame changed for {request_line}",
            );
        }

        // A resource body is too large to pin whole; its frame *shape* — field
        // order included — is the part hosts depend on.
        let schema_request: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"tracedecay://schema"}}"#,
        )
        .expect("legacy request line");
        let schema = serde_json::to_string(
            &server
                .handle_request(&schema_request)
                .await
                .expect("legacy response"),
        )
        .expect("serialize legacy response");
        assert!(
            schema.starts_with(
                r#"{"jsonrpc":"2.0","id":6,"result":{"contents":[{"mimeType":"text/markdown","text":"#
            ) && schema.ends_with(r#","uri":"tracedecay://schema"}]}}"#),
            "legacy resources/read frame shape changed: {schema}",
        );

        // Notifications stay responseless on the legacy transport.
        for notification in [
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
        ] {
            let request: JsonRpcRequest =
                serde_json::from_str(notification).expect("legacy notification line");
            assert!(
                server.handle_request(&request).await.is_none(),
                "legacy notification produced a response: {notification}",
            );
        }
        server.shutdown().await;
    }

    #[tokio::test]
    async fn rmcp_cancellation_uses_the_connection_scoped_application_identity() {
        crate::product_runtime::register_fixture_product_runtime();
        let (cg, _repo, authority) =
            crate::mcp::server::writer_test_support::init_indexed_repo().await;
        let context = crate::mcp::server::writer_test_support::registered_context(cg, &authority);
        let server = McpServer::new_with_registered_test_context(context, Vec::new())
            .await
            .expect("registered cancellation server");
        let adapter =
            RmcpConnectionAdapter::new(Arc::clone(&server), false, None).expect("RMCP adapter");
        let wire_id = json!("rmcp-cancellation-oracle");
        let application_id =
            super::super::application_surface_request_id(&wire_id, &adapter.memory_request_scope)
                .expect("connection-scoped application request id");
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancellation.rmcp-wire-oracle")
                .expect("cancellation signal");
        server
            .dispatch_authority
            .register_cancellation(application_id, cancellation);

        assert!(
            adapter.cancel_request(Some(rmcp::model::RequestId::String(Arc::from(
                "rmcp-cancellation-oracle"
            ),)))
        );
        assert!(
            !adapter.cancel_request(Some(rmcp::model::RequestId::String(Arc::from(
                "different-id",
            )))),
            "a cancellation from the same connection must not alias another wire id",
        );
        server.shutdown().await;
    }

    #[test]
    fn rmcp_selected_project_retirement_error_is_stable() {
        let error = project_server_retired_error();
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            error.message,
            "tool project route failed: project server was retired",
        );
        assert_eq!(
            error.data,
            Some(json!({
                "reason_code": "project_server_retired",
                "retryable": true,
                "detail": "the retained project server was replaced or revoked; retry against the current owner",
            })),
        );
    }

    #[test]
    fn response_conversion_preserves_tool_content_and_rpc_errors() {
        let complete: CallToolResponse =
            RmcpConnectionAdapter::response_result::<CallToolResult>(JsonRpcResponse::success(
                json!(7),
                json!({"content": [{"type": "text", "text": "ok"}]}),
            ))
            .map(Into::into)
            .expect("tool response");
        let CallToolResponse::Complete(CallToolResult { content, .. }) = complete else {
            panic!("ordinary TraceDecay tool responses must stay complete");
        };
        assert_eq!(
            content[0].as_text().map(|text| text.text.as_str()),
            Some("ok")
        );

        let error = RmcpConnectionAdapter::response_result::<ListToolsResult>(
            JsonRpcResponse::error_with_data(
                json!("request"),
                tracedecay_mcp::transport::ErrorCode::InvalidParams,
                "invalid arguments".to_owned(),
                Some(json!({"reason": "missing_query"})),
            ),
        )
        .expect_err("error response");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(error.message, "invalid arguments");
        assert_eq!(error.data, Some(json!({"reason": "missing_query"})));
    }

    #[test]
    fn adapter_accepts_the_legacy_initialize_response_shape() {
        crate::product_runtime::register_fixture_product_runtime();
        let initialized: InitializeResult =
            RmcpConnectionAdapter::response_result(JsonRpcResponse::success(
                json!(1),
                crate::mcp::server::initialize_result("TraceDecay instructions")
                    .expect("fixture product runtime registered"),
            ))
            .expect("rmcp must preserve legacy MCP initialization compatibility");

        assert_eq!(
            serde_json::to_value(&initialized).expect("serialize initialized response")["protocolVersion"],
            json!("2024-11-05")
        );
        assert!(initialized.capabilities.tools.is_some());
        assert!(initialized.capabilities.resources.is_some());
    }

    #[tokio::test]
    async fn cancellation_stops_dispatch_before_live_request_registration() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let cancel_attempts = Arc::clone(&attempts);

        let result = await_dispatch_with_cancellation(
            std::future::pending::<()>(),
            std::future::ready(()),
            move || {
                cancel_attempts.fetch_add(1, Ordering::SeqCst);
                false
            },
        )
        .await;

        assert_eq!(result, None);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "an unregistered request has no admitted work to poll for cancellation"
        );
    }
}

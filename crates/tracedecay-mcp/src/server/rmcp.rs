//! `rmcp` 3.x model adapter for the authenticated `TraceDecay` MCP surface.
//!
//! The daemon owns authentication, bounded framing, project selection, and
//! replacement/retirement. Once that boundary selected a project server, this
//! adapter answers standard MCP requests, decoded into `rmcp`'s typed model,
//! from the existing catalog and handler authority. The session loop that
//! feeds it lives in [`super::session`].

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientRequest,
    CompleteResult, CustomRequest, CustomResult, DiscoverResult, EmptyResult, ErrorCode, ErrorData,
    Implementation, InitializeRequestParams, InitializeResult, MetaObject, ProtocolVersion,
    ReadResourceResult, RequestMetaObject, ServerCapabilities, ServerConfig, ServerResult,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{RwLock, Semaphore};

use crate::transport::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

use super::protocol::negotiate_protocol_version;
use super::session::{McpRequestContext, SessionState, missing_request_context_error};
use super::{
    McpConnectionContext, McpConnectionState, McpDispatchParams, McpDispatchRequest,
    McpResponseLease, dispatch_is_independent_read, speaks_2026_07_28, stamp_2026_07_28_result,
};

/// Per-RMCP-connection handoff from handler completion to the transport write.
///
/// A selected project response owns a read lease from its exact target server.
/// Handler completion and response serialization are separate steps, so the
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

/// Allows daemon routing to enrich the `initialize` response without
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

/// Race a dispatch future against the transport cancellation token.
///
/// When cancel wins and `cancel_registered_request` finds a live registration,
/// the sticky/worker path owns settlement, so this keeps awaiting `handling`.
/// When cancel wins and the request is not registered yet, pass
/// `cancellation_registered` so this waits for route resolution to register
/// the request instead of dropping `handling` mid-route.
/// Only a cancel that can never register (no notify channel) abandons.
pub async fn await_dispatch_with_cancellation<F, C, N>(
    handling: F,
    cancellation: N,
    mut cancel_registered_request: C,
    cancellation_registered: Option<&tokio::sync::Notify>,
) -> Option<F::Output>
where
    F: std::future::Future,
    C: FnMut() -> bool,
    N: std::future::Future<Output = ()>,
{
    tokio::pin!(handling);
    tokio::pin!(cancellation);
    tokio::select! {
        biased;
        response = &mut handling => return Some(response),
        () = &mut cancellation => {}
    }
    if cancel_registered_request() {
        return Some(handling.await);
    }
    let notify = cancellation_registered?;
    // Cancel raced route resolution: keep polling handling while waiting for
    // prepare_dispatch_control to register.
    loop {
        let registered = notify.notified();
        tokio::pin!(registered);
        registered.as_mut().enable();
        if cancel_registered_request() {
            return Some(handling.await);
        }
        tokio::select! {
            biased;
            response = &mut handling => return Some(response),
            () = &mut registered => {
                if cancel_registered_request() {
                    return Some(handling.await);
                }
            }
        }
    }
}

/// Per-connection MCP request handler over the existing `TraceDecay` request
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
    /// `server_config` is infallible and must not fabricate one.
    build_version: &'static str,
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

    #[tracing::instrument(name = "mcp.server.rmcp.dispatch_total", level = "trace", skip_all)]
    async fn dispatch(
        &self,
        context: McpRequestContext,
        method: &str,
        params: McpDispatchParams<'_>,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let on_2026_07_28 = speaks_2026_07_28(context.protocol_version().as_ref());
        let request_permit = self.acquire_request_permit().await?;
        // Heap-allocate the admission + dispatch composition so the combined
        // resident frame never sits inline in the session's request future;
        // it overflows the worker stack in perf-profile layouts.
        let result = Box::pin(self.dispatch_admitted(context, method, params)).await;
        drop(request_permit);
        result.map(|mut response| {
            if on_2026_07_28 && let Some(result) = response.result.as_mut() {
                stamp_2026_07_28_result(method, result);
            }
            response
        })
    }

    #[tracing::instrument(name = "mcp.server.rmcp.queue_wait", level = "trace", skip_all)]
    async fn acquire_request_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ErrorData> {
        self.request_admission
            .acquire()
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }

    #[tracing::instrument(name = "mcp.server.rmcp.dispatch", level = "trace", skip_all)]
    async fn dispatch_admitted(
        &self,
        context: McpRequestContext,
        method: &str,
        params: McpDispatchParams<'_>,
    ) -> Result<JsonRpcResponse, ErrorData> {
        // The wire identity is the one value internal dispatch genuinely keys
        // on (cancellation identity, response leases, delivery settlement), so
        // it is converted once, infallibly, and never re-derived.
        let id = context.id.into_json_value();
        let request_cancellation = context.cancellation;
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

    #[tracing::instrument(name = "mcp.server.rmcp.dispatch_request", level = "trace", skip_all)]
    async fn dispatch_request_with_connection(
        &self,
        request: McpDispatchRequest<'_>,
        id: Value,
        request_cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
        connection: &mut C::Connection,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let pre_cancelled = request_cancellation.is_cancelled();
        let dispatch_cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
        if pre_cancelled {
            dispatch_cancellation.cancel();
        }
        // Erase the shared dispatch authority before awaiting it: the
        // cancellation combinator otherwise stores the complete
        // catalog-dispatch future inline in the session's request future.
        let handling = self.context.dispatch(
            request,
            self.timings_enabled,
            connection,
            dispatch_cancellation.clone(),
        );
        let response = if pre_cancelled {
            Some(handling.await)
        } else {
            await_dispatch_with_cancellation(
                handling,
                request_cancellation.cancelled(),
                || {
                    dispatch_cancellation.cancel();
                    // Live registration delivers the cancel to the admitted
                    // worker. A miss here is usually "not yet registered"
                    // during route resolution; the notify channel below waits
                    // for prepare_dispatch_control instead of dropping the
                    // sticky sample / selected-target settlement path.
                    self.context.cancel_request(&id, &self.memory_request_scope)
                },
                Some(self.context.cancellation_registered()),
            )
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

    #[tracing::instrument(name = "mcp.server.rmcp.notification", level = "trace", skip_all)]
    pub(crate) async fn dispatch_notification(&self, method: String, params: Option<Value>) {
        // Custom notifications (hook events) have no typed `rmcp` DTO: their
        // params arrive as JSON and stay JSON.
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
                McpDispatchRequest::raw(&request),
                self.timings_enabled,
                &mut connection,
                tracedecay_runtime_core::cancellation::CancellationToken::new(),
            )
            .await;
    }

    pub(crate) fn cancel_request(&self, request_id: Option<rmcp::model::RequestId>) -> bool {
        request_id
            .map(rmcp::model::RequestId::into_json_value)
            .is_some_and(|request_id| {
                self.context
                    .cancel_request(&request_id, &self.memory_request_scope)
            })
    }

    /// Serves one broker connection until the peer leaves or `shutdown`
    /// resolves; see [`super::session`].
    pub async fn serve<S>(
        &self,
        transport: crate::BrokerStreamTransport,
        shutdown: S,
    ) -> tracedecay_domain::errors::Result<()>
    where
        S: Future<Output = ()> + Send,
    {
        super::session::serve(self, transport, shutdown).await
    }
}

/// Whether a daemon connection's first request opens an `rmcp` session: an
/// `initialize`, or a request carrying the SEP-2575 per-request client context
/// `rmcp` serves without one.
pub fn opens_rmcp_session(request: &JsonRpcRequest) -> bool {
    request.method == "initialize"
        || (request.id.is_some()
            && request
                .params
                .as_ref()
                .and_then(|params| params.get("_meta"))
                .and_then(Value::as_object)
                .is_some_and(|meta| {
                    RequestMetaObject::DRAFT_REQUIRED_KEYS
                        .iter()
                        .all(|key| meta.contains_key(*key))
                }))
}

/// Attaches this client's SEP-2575 per-request context to `params._meta` so a
/// request sent on its own daemon connection needs no `initialize` session.
///
/// Notifications, `initialize`, and non-object params or `_meta` are left
/// unchanged; `_meta` entries the caller already set win. Returns whether the
/// request changed.
pub fn attach_stateless_request_context(request: &mut JsonRpcRequest) -> bool {
    if request.id.is_none() || request.method == "initialize" {
        return false;
    }
    let Some(params) = request
        .params
        .get_or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
    else {
        return false;
    };
    let Some(meta) = params
        .entry("_meta")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
    else {
        return false;
    };
    let mut context = RequestMetaObject::new();
    // `LATEST` (2026-07-28) has no initialize handshake, and stamping it would add
    // resultType/ttlMs/cacheScope that strict 2025-11-25 clients reject.
    context.set_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE);
    context.set_client_capabilities(ClientCapabilities::default());
    let mut attached = false;
    for (key, value) in context.0.0 {
        if let serde_json::map::Entry::Vacant(entry) = meta.entry(key) {
            entry.insert(value);
            attached = true;
        }
    }
    attached
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

impl<C> RmcpConnectionAdapter<C>
where
    C: McpConnectionContext,
{
    fn server_config(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("tracedecay", self.build_version))
    }

    /// Answers one request under `rmcp` 3.5's server request policy
    /// (`handler/server.rs`), narrowed to the methods this server implements.
    pub(crate) async fn handle_request(
        &self,
        request: ClientRequest,
        context: McpRequestContext,
    ) -> Result<ServerResult, ErrorData> {
        // MCP requires an empty result to a ping at any time; hosts use it as
        // a liveness probe, so no lifecycle rule below may refuse one.
        if matches!(request, ClientRequest::PingRequest(_)) {
            return Ok(ServerResult::EmptyResult(EmptyResult {}));
        }
        let protocol_version = context.protocol_version();
        let on_2026_07_28 = speaks_2026_07_28(protocol_version.as_ref());
        let requested_version = context.meta.protocol_version();
        let inline_negotiation = !matches!(request, ClientRequest::InitializeRequest(_));
        if inline_negotiation
            && let Some(requested) = requested_version.as_ref()
            && !ProtocolVersion::KNOWN_VERSIONS.contains(requested)
        {
            return Err(ErrorData::unsupported_protocol_version(
                requested.clone(),
                ProtocolVersion::KNOWN_VERSIONS,
            ));
        }
        // Per-request context is required only of a request that itself uses
        // the inline lifecycle; a session opened by `initialize` may omit it.
        let requires_request_context = inline_negotiation
            && (matches!(request, ClientRequest::DiscoverRequest(_))
                || matches!(context.session, SessionState::Inline)
                || speaks_2026_07_28(requested_version.as_ref()));
        if requires_request_context {
            let missing = context
                .meta
                .missing_required_keys(&ProtocolVersion::V_2026_07_28);
            if !missing.is_empty() {
                return Err(missing_request_context_error(&missing));
            }
        }
        let result = match request {
            ClientRequest::InitializeRequest(request) => self
                .initialize(request.params, context)
                .await
                .map(ServerResult::InitializeResult),
            ClientRequest::DiscoverRequest(_) => Ok(ServerResult::DiscoverResult(
                DiscoverResult::from_server_info(
                    ProtocolVersion::KNOWN_VERSIONS.to_vec(),
                    self.server_config(),
                ),
            )),
            ClientRequest::CompleteRequest(_) => {
                Ok(ServerResult::CompleteResult(CompleteResult::default()))
            }
            ClientRequest::ListToolsRequest(_) => rmcp_response_result(
                self.dispatch(context, "tools/list", McpDispatchParams::TypedEmpty)
                    .await?,
            )
            .map(ServerResult::ListToolsResult),
            ClientRequest::CallToolRequest(request) => self
                .call_tool(request.params, context)
                .await
                .map(ServerResult::from),
            ClientRequest::ListResourcesRequest(_) => rmcp_response_result(
                self.dispatch(context, "resources/list", McpDispatchParams::TypedEmpty)
                    .await?,
            )
            .map(ServerResult::ListResourcesResult),
            ClientRequest::ListResourceTemplatesRequest(_) => empty_list_result(
                &context,
                "resources/templates/list",
                json!({ "resourceTemplates": [] }),
            )
            .map(ServerResult::ListResourceTemplatesResult),
            ClientRequest::ListPromptsRequest(_) => {
                empty_list_result(&context, "prompts/list", json!({ "prompts": [] }))
                    .map(ServerResult::ListPromptsResult)
            }
            ClientRequest::ReadResourceRequest(request) => {
                rmcp_response_result::<ReadResourceResult>(
                    self.dispatch(
                        context,
                        "resources/read",
                        McpDispatchParams::ResourcesRead(&request.params),
                    )
                    .await?,
                )
                .map(ServerResult::ReadResourceResult)
            }
            ClientRequest::CustomRequest(request) => self
                .custom_request(request, context)
                .await
                .map(ServerResult::CustomResult),
            // Prompts, logging, subscriptions, and tasks are not offered.
            request => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                request.method().to_owned(),
                None,
            )),
        };
        result
            .map(|mut result| {
                // Peers before 2026-07-28 keep the wire shape without
                // `resultType: "complete"`.
                if !on_2026_07_28 {
                    result.strip_result_type_for_legacy_peer();
                }
                result
            })
            .map_err(|mut error| {
                // SEP-2164: from 2026-07-28 a missing resource is invalid params.
                if on_2026_07_28 && error.code == ErrorCode::RESOURCE_NOT_FOUND {
                    error.code = ErrorCode::INVALID_PARAMS;
                }
                error
            })
    }

    pub(crate) async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: McpRequestContext,
    ) -> Result<InitializeResult, ErrorData> {
        let protocol_version = negotiate_protocol_version(request.protocol_version.as_str());
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
        let mut result = rmcp_response_result::<InitializeResult>(response)?;
        result.protocol_version = protocol_version;
        Ok(result)
    }

    async fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        context: McpRequestContext,
    ) -> Result<CallToolResponse, ErrorData> {
        // The session lifts the wire `params._meta` into the request context;
        // the caller deadline is read from the typed params.
        if request.meta.is_none() && !context.meta.is_empty() {
            request.meta = Some(context.meta.clone());
        }
        let started =
            (self.timings_enabled || self.context.timings_enabled()).then(std::time::Instant::now);
        let response = self
            .dispatch(context, "tools/call", McpDispatchParams::ToolsCall(request))
            .await?;
        let mut result = rmcp_response_result::<CallToolResult>(response)?;
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

    /// A hook event arrives as a stateless request on its own daemon
    /// connection; it is dispatched exactly like the notification form.
    /// Any other custom request is an unknown method or a known one whose
    /// params did not fit its typed DTO; the dispatch authority answers it
    /// (method not found, invalid params) and accounts for it.
    async fn custom_request(
        &self,
        request: CustomRequest,
        context: McpRequestContext,
    ) -> Result<CustomResult, ErrorData> {
        if request.method == tracedecay_hooks::core_events::HOOK_EVENT_METHOD {
            self.dispatch_notification(request.method, request.params)
                .await;
            return Ok(CustomResult::new(json!({})));
        }
        // The session lifts `params._meta` into the request context; the
        // object it leaves empty is a request that carried no params of its
        // own (the stdio proxy creates `params` only to hold that context).
        let params = request
            .params
            .as_ref()
            .filter(|params| params.as_object().is_none_or(|object| !object.is_empty()));
        rmcp_response_result(
            self.dispatch(context, &request.method, McpDispatchParams::Raw(params))
                .await?,
        )
        .map(CustomResult::new)
    }
}

/// A list this server always answers empty, shaped for the request's revision.
fn empty_list_result<T: DeserializeOwned>(
    context: &McpRequestContext,
    method: &str,
    mut result: Value,
) -> Result<T, ErrorData> {
    if speaks_2026_07_28(context.protocol_version().as_ref()) {
        stamp_2026_07_28_result(method, &mut result);
    }
    serde_json::from_value(result)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))
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

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests run on an ambient Tokio runtime"
)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::await_dispatch_with_cancellation;

    #[tokio::test]
    async fn cancellation_stops_dispatch_when_registration_can_never_arrive() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let cancel_attempts = Arc::clone(&attempts);

        let result = await_dispatch_with_cancellation(
            std::future::pending::<()>(),
            std::future::ready(()),
            move || {
                cancel_attempts.fetch_add(1, Ordering::SeqCst);
                false
            },
            None,
        )
        .await;

        assert_eq!(result, None);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "without a registration channel, an unregistered cancel abandons dispatch"
        );
    }

    #[tokio::test]
    async fn rmcp_cancel_during_route_resolution_preserves_dispatch_until_registration() {
        // Cancel can win while route resolution still awaits, before
        // prepare_dispatch_control registers. RMCP waits on
        // cancellation_registered so sticky sampling and the selected target
        // still settle.
        let registration = Arc::new(tokio::sync::Notify::new());
        let registered = Arc::new(AtomicBool::new(false));
        let cancel_attempts = Arc::new(AtomicUsize::new(0));
        let handling_registered = Arc::clone(&registered);
        let handling_notify = Arc::clone(&registration);
        let cancel_registered = Arc::clone(&registered);
        let cancel_hits = Arc::clone(&cancel_attempts);

        let handling = async move {
            tokio::task::yield_now().await;
            handling_registered.store(true, Ordering::SeqCst);
            handling_notify.notify_waiters();
            "tool_dispatch_cancelled"
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            await_dispatch_with_cancellation(
                handling,
                std::future::ready(()),
                move || {
                    cancel_hits.fetch_add(1, Ordering::SeqCst);
                    cancel_registered.load(Ordering::SeqCst)
                },
                Some(registration.as_ref()),
            ),
        )
        .await
        .expect("RMCP mid-route cancel must not hang waiting for registration");

        assert_eq!(
            result,
            Some("tool_dispatch_cancelled"),
            "cancel before registration must preserve dispatch through the sticky sample"
        );
        assert!(
            cancel_attempts.load(Ordering::SeqCst) >= 2,
            "cancel must retry after registration, not abandon on the first miss"
        );
        assert!(
            registered.load(Ordering::SeqCst),
            "registration must complete so the selected target can settle"
        );
    }
}

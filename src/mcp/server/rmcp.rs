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
use tokio::sync::Mutex;

use crate::mcp::transport::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

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
    fn request_key(id: &Value) -> crate::errors::Result<String> {
        if id.is_null() {
            return Err(crate::errors::TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                "selected RMCP response has no request identity",
            ));
        }
        serde_json::to_string(id).map_err(|error| {
            crate::errors::TraceDecayError::project_route(
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
    ) -> crate::errors::Result<()> {
        let key = Self::request_key(id)?;
        let mut leases = self.leases.lock().map_err(|_| {
            crate::errors::TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                "selected RMCP response authority is poisoned during handler handoff",
            )
        })?;
        if leases.contains_key(&key) {
            return Err(crate::errors::TraceDecayError::project_route(
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
    ) -> crate::errors::Result<Option<super::routing::SelectedProjectResponseLease>> {
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
                crate::errors::TraceDecayError::project_route(
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
) -> F::Output
where
    F: std::future::Future,
    C: FnMut() -> bool,
    N: std::future::Future<Output = ()>,
{
    tokio::pin!(handling);
    tokio::pin!(cancellation);
    tokio::select! {
        response = &mut handling => response,
        () = &mut cancellation => {
            while !cancel_registered_request() {
                tokio::select! {
                    response = &mut handling => return response,
                    () = tokio::task::yield_now() => {}
                }
            }
            handling.await
        }
    }
}

/// Per-connection `rmcp` server facade over the existing `TraceDecay` request
/// authority.
pub(crate) struct RmcpConnectionAdapter {
    server: Arc<McpServer>,
    connection: Mutex<ConnectionRouteState>,
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
}

impl RmcpConnectionAdapter {
    pub(crate) fn new(
        server: Arc<McpServer>,
        timings_enabled: bool,
        initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
    ) -> crate::errors::Result<Self> {
        let connection = server.new_connection_route_state()?;
        let memory_request_scope = connection.memory_request_scope().to_owned();
        Ok(Self {
            server,
            connection: Mutex::new(connection),
            memory_request_scope,
            timings_enabled,
            selected_project_responses: RmcpSelectedProjectResponseAuthority::default(),
            initialize_response_decorator,
            admission: crate::daemon::current_connection_admission(),
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

    async fn dispatch(
        &self,
        context: RequestContext<RoleServer>,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, ErrorData> {
        crate::daemon::in_connection_admission(
            self.admission.clone(),
            self.dispatch_admitted(context, method, params),
        )
        .await
    }

    async fn dispatch_admitted(
        &self,
        context: RequestContext<RoleServer>,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let request_id = context.id;
        let request_cancellation = context.ct;
        let id = serde_json::to_value(request_id)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(id.clone()),
            method: method.to_owned(),
            params,
        };
        let mut connection = self.connection.lock().await;
        let pre_cancelled = request_cancellation.is_cancelled();
        let response = if pre_cancelled {
            self.server
                .handle_request_for_connection(
                    &request,
                    self.timings_enabled,
                    &mut connection,
                    true,
                )
                .await
        } else {
            await_dispatch_with_cancellation(
                self.server.handle_request_for_connection(
                    &request,
                    self.timings_enabled,
                    &mut connection,
                    false,
                ),
                request_cancellation.cancelled(),
                || {
                    self.server
                        .cancel_application_surface_request(&id, &self.memory_request_scope)
                },
            )
            .await
        }
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

    async fn dispatch_notification(&self, method: String, params: Option<Value>) {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: None,
            method,
            params,
        };
        let mut connection = self.connection.lock().await;
        let _ = self
            .server
            .handle_request_for_connection(&request, self.timings_enabled, &mut connection, false)
            .await;
    }

    fn cancel_request(&self, request_id: Option<rmcp::model::RequestId>) -> bool {
        request_id
            .and_then(|request_id| serde_json::to_value(request_id).ok())
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
        .with_server_info(Implementation::new(
            "tracedecay",
            crate::version::build_version(),
        ))
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let params = serde_json::to_value(request)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let mut response = self.dispatch(context, "initialize", Some(params)).await?;
        if let Some(decorate) = &self.initialize_response_decorator {
            decorate(&mut response);
        }
        Self::response_result(response)
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Self::response_result(self.dispatch(context, "tools/list", None).await?)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let params = serde_json::to_value(request)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        Self::response_result::<CallToolResult>(
            self.dispatch(context, "tools/call", Some(params)).await?,
        )
        .map(Into::into)
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Self::response_result(self.dispatch(context, "resources/list", None).await?)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let params = serde_json::to_value(request)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        Self::response_result::<ReadResourceResult>(
            self.dispatch(context, "resources/read", Some(params))
                .await?,
        )
        .map(Into::into)
    }

    async fn on_cancelled(
        &self,
        notification: rmcp::model::CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        let _ = self.cancel_request(notification.request_id);
    }

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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rmcp::model::{CallToolResponse, CallToolResult};
    use serde_json::json;
    use tokio::sync::Notify;

    use super::*;

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
                crate::mcp::transport::ErrorCode::InvalidParams,
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
        let initialized: InitializeResult =
            RmcpConnectionAdapter::response_result(JsonRpcResponse::success(
                json!(1),
                crate::mcp::server::initialize_result("TraceDecay instructions"),
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
    async fn cancellation_retries_until_the_live_request_registers() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let registered = Arc::new(Notify::new());
        let handling_registered = Arc::clone(&registered);
        let cancel_attempts = Arc::clone(&attempts);
        let cancel_registered = Arc::clone(&registered);

        let result = await_dispatch_with_cancellation(
            async move {
                handling_registered.notified().await;
                "cancelled"
            },
            std::future::ready(()),
            move || {
                if cancel_attempts.fetch_add(1, Ordering::SeqCst) == 2 {
                    cancel_registered.notify_one();
                    true
                } else {
                    false
                }
            },
        )
        .await;

        assert_eq!(result, "cancelled");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "cancellation must retry until the request registration is visible"
        );
    }
}

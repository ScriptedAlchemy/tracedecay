//! `rmcp` 3.x adapter for the authenticated `TraceDecay` MCP surface.
//!
//! The daemon owns authentication, bounded framing, project selection, and
//! replacement/retirement. Once that boundary selected a project server, this
//! adapter delegates standard MCP requests to the existing catalog and handler
//! authority through `rmcp`'s typed server callbacks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CustomNotification, CustomRequest,
    CustomResult, ErrorCode, ErrorData, Implementation, InitializeRequestParams, InitializeResult,
    ListResourcesResult, ListToolsResult, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, ServerCapabilities, ServerInfo,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::mcp::transport::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

use super::{
    ConnectionRouteState, McpRequestStart, McpServer, McpToolDispatchControl, McpToolDispatchStage,
};

/// Allows daemon routing to enrich the legacy `initialize` response without
/// coupling this MCP module to daemon route types.
pub(crate) type RmcpInitializeResponseDecorator =
    Arc<dyn Fn(&mut JsonRpcResponse) + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub(crate) struct RmcpRequestIngressRegistry {
    entries: Arc<StdMutex<HashMap<String, RmcpRequestIngress>>>,
}

#[derive(Clone)]
struct RmcpRequestIngress {
    started: McpRequestStart,
    response_deadline_at: Option<tokio::time::Instant>,
    externally_cancellable: bool,
    cancellation_response_sent: bool,
    dispatch_control: Option<McpToolDispatchControl>,
    method: Arc<str>,
    tool_name: Option<Arc<str>>,
}

impl RmcpRequestIngressRegistry {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, RmcpRequestIngress>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn request_key(id: &Value) -> Option<String> {
        (!id.is_null())
            .then(|| serde_json::to_string(id).ok())
            .flatten()
    }

    pub(crate) fn record(
        &self,
        id: &Value,
        method: &str,
        params: Option<&Value>,
        started: McpRequestStart,
    ) {
        let Some(request_key) = Self::request_key(id) else {
            return;
        };
        let lifecycle_policy = (method == "tools/call")
            .then(|| params?.get("name")?.as_str())
            .flatten()
            .and_then(|tool_name| {
                crate::mcp::tools::dispatch::lifecycle_policy_for_tool(tool_name)
                    .ok()
                    .flatten()
            });
        let response_deadline_at =
            lifecycle_policy.and_then(|policy| started.runtime_deadline(policy.maximum_duration()));
        self.entries().insert(
            request_key,
            RmcpRequestIngress {
                started,
                response_deadline_at,
                externally_cancellable: lifecycle_policy
                    .is_some_and(|policy| policy.externally_cancellable()),
                cancellation_response_sent: false,
                dispatch_control: None,
                method: Arc::from(method),
                tool_name: params
                    .and_then(|params| params.get("name"))
                    .and_then(Value::as_str)
                    .map(Arc::from),
            },
        );
    }

    fn started(&self, id: &Value) -> Option<McpRequestStart> {
        let request_key = Self::request_key(id)?;
        self.entries().get(&request_key).map(|entry| entry.started)
    }

    pub(crate) fn response_deadline(&self, request_key: &str) -> Option<tokio::time::Instant> {
        self.entries()
            .get(request_key)
            .and_then(|entry| entry.response_deadline_at)
    }

    pub(crate) fn finish(&self, request_key: &str) {
        self.entries().remove(request_key);
    }

    fn observe_dispatch_control(&self, id: &Value, control: McpToolDispatchControl) {
        let Some(request_key) = Self::request_key(id) else {
            return;
        };
        if let Some(ingress) = self.entries().get_mut(&request_key) {
            if ingress.cancellation_response_sent {
                control.cancel_from_transport();
            }
            ingress.dispatch_control = Some(control);
        }
    }

    pub(crate) fn cancelled_response(
        &self,
        id: &Value,
    ) -> Option<(JsonRpcResponse, Option<tokio::time::Instant>)> {
        let request_key = Self::request_key(id)?;
        let mut entries = self.entries();
        let ingress = entries.get_mut(&request_key)?;
        if !ingress.externally_cancellable || ingress.cancellation_response_sent {
            return None;
        }
        ingress.cancellation_response_sent = true;
        let ingress = ingress.clone();
        drop(entries);
        if let Some(control) = ingress.dispatch_control.as_ref() {
            control.cancel_from_transport();
        }
        let response = if ingress.method.as_ref() == "tools/call" {
            super::finish_transport_cancelled_tool_call_response(
                id.clone(),
                ingress.tool_name.as_deref().unwrap_or("<unresolved>"),
                ingress.started,
                ingress.dispatch_control.as_ref(),
            )
        } else {
            JsonRpcResponse::error_with_data(
                id.clone(),
                crate::mcp::transport::ErrorCode::RequestCancelled,
                "MCP request cancelled".to_owned(),
                Some(json!({
                    "reason_code": "request_cancelled",
                    "retryable": true,
                })),
            )
        };
        Some((response, ingress.response_deadline_at))
    }
}

/// Per-connection `rmcp` server facade over the existing `TraceDecay` request
/// authority.
pub(crate) struct RmcpConnectionAdapter {
    server: Arc<McpServer>,
    connection: Mutex<ConnectionRouteState>,
    memory_request_scope: String,
    timings_enabled: bool,
    initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
    request_ingress: RmcpRequestIngressRegistry,
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
        request_ingress: RmcpRequestIngressRegistry,
    ) -> Result<Self, crate::request_identity::RequestIdentityError> {
        let connection = server.new_connection_route_state()?;
        let memory_request_scope = connection.memory_request_scope().to_owned();
        Ok(Self {
            server,
            connection: Mutex::new(connection),
            memory_request_scope,
            timings_enabled,
            initialize_response_decorator,
            request_ingress,
            admission: crate::daemon::current_connection_admission(),
        })
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
        let id = serde_json::to_value(&request_id)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let started = self
            .request_ingress
            .started(&id)
            .unwrap_or_else(McpRequestStart::now);
        let tool_name = (method == "tools/call")
            .then(|| {
                params
                    .as_ref()
                    .and_then(|params| params.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
        let dispatch_control = match tool_name.as_deref() {
            Some(tool_name) => Some(
                self.server
                    .admit_tool_request(&id, tool_name, &self.memory_request_scope, started)
                    .map_err(|error| {
                        typed_tool_error(id.clone(), tool_name, &error, started, None)
                    })?,
            ),
            None => None,
        };
        if let Some(control) = dispatch_control.as_ref() {
            self.request_ingress
                .observe_dispatch_control(&id, control.clone());
        }
        if request_cancellation.is_cancelled() {
            let _ = self.cancel_request(Some(request_id));
        }
        let project_tool_call = method == "tools/call" && self.server.project_server_live.is_some();
        let _response_guard = if project_tool_call {
            let response_gate = self.server.project_server_lifecycle.response_gate();
            match dispatch_control.as_ref() {
                Some(control) => Some(
                    control
                        .run_value(McpToolDispatchStage::ProjectGate, response_gate.read())
                        .await
                        .map_err(|error| {
                            typed_tool_error(
                                id.clone(),
                                tool_name.as_deref().unwrap_or("<unresolved>"),
                                &error,
                                started,
                                dispatch_control.as_ref(),
                            )
                        })?,
                ),
                None => Some(response_gate.read().await),
            }
        } else {
            None
        };
        if project_tool_call
            && self
                .server
                .project_server_lifecycle
                .response_revoked()
                .is_cancelled()
        {
            return Err(project_server_retired_error(
                id.clone(),
                tool_name.as_deref().unwrap_or("<unresolved>"),
                started,
                dispatch_control.as_ref(),
            ));
        }
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(id.clone()),
            method: method.to_owned(),
            params,
        };
        let mut connection = match dispatch_control.as_ref() {
            Some(control) => control
                .run_value(McpToolDispatchStage::QueueAdmission, self.connection.lock())
                .await
                .map_err(|error| {
                    typed_tool_error(
                        id.clone(),
                        tool_name.as_deref().unwrap_or("<unresolved>"),
                        &error,
                        started,
                        dispatch_control.as_ref(),
                    )
                })?,
            None => self.connection.lock().await,
        };
        let handling = self.server.handle_request_for_connection(
            &request,
            self.timings_enabled,
            &mut connection,
            dispatch_control.clone(),
            started,
        );
        tokio::pin!(handling);
        let response = tokio::select! {
            response = &mut handling => response,
            () = request_cancellation.cancelled() => {
                let _ = self.server.cancel_application_surface_request(
                    &id,
                    &self.memory_request_scope,
                );
                handling.await
            }
        }
        .ok_or_else(|| {
            let response = JsonRpcResponse::error(
                id.clone(),
                crate::mcp::transport::ErrorCode::InternalError,
                "MCP request did not produce a response".to_owned(),
            );
            let response = match tool_name.as_deref() {
                Some(_) => super::request_receipts::finish_tool_call_response(
                    response,
                    &super::request_receipts::McpToolCallTiming::new(started),
                    dispatch_control.as_ref(),
                    None,
                ),
                None => response,
            };
            response_error(response)
        })?;
        if project_tool_call
            && self
                .server
                .project_server_lifecycle
                .response_revoked()
                .is_cancelled()
        {
            return Err(project_server_retired_error(
                id,
                tool_name.as_deref().unwrap_or("<unresolved>"),
                started,
                dispatch_control.as_ref(),
            ));
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
            .handle_request_for_connection(
                &request,
                self.timings_enabled,
                &mut connection,
                None,
                McpRequestStart::now(),
            )
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

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        let CustomRequest { method, params, .. } = request;
        let response = self.dispatch(context, &method, params).await?;
        Self::response_result::<Value>(response).map(CustomResult::new)
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

fn typed_tool_error(
    id: Value,
    tool_name: &str,
    error: &crate::errors::TraceDecayError,
    started: McpRequestStart,
    control: Option<&McpToolDispatchControl>,
) -> ErrorData {
    response_error(super::request_receipts::finish_tool_error_response(
        id, tool_name, error, started, control,
    ))
}

fn response_error(response: JsonRpcResponse) -> ErrorData {
    match response.error {
        Some(error) => rmcp_error(error),
        None => ErrorData::internal_error(
            "TraceDecay MCP tool error response omitted its error payload",
            None,
        ),
    }
}

fn project_server_retired_error(
    id: Value,
    tool_name: &str,
    started: McpRequestStart,
    control: Option<&McpToolDispatchControl>,
) -> ErrorData {
    let response = JsonRpcResponse::error_with_data(
        id,
        crate::mcp::transport::ErrorCode::InternalError,
        "tool project route failed: project server was retired".to_owned(),
        Some(json!({
            "tool": tool_name,
            "reason_code": "project_server_retired",
            "retryable": true,
            "detail": "the retained project server was replaced or revoked; retry against the current owner",
        })),
    );
    response_error(super::request_receipts::finish_tool_call_response(
        response,
        &super::request_receipts::McpToolCallTiming::new(started),
        control,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use rmcp::model::{CallToolResponse, CallToolResult};
    use serde_json::json;

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

    #[test]
    fn typed_rmcp_admission_errors_include_the_canonical_receipt() {
        let error = crate::errors::TraceDecayError::mcp_tool_dispatch(
            "tool_dispatch_duplicate_request_id",
            "queue_admission",
            false,
            "duplicate active request id",
        );
        let error = typed_tool_error(
            json!(7),
            "tracedecay_search",
            &error,
            McpRequestStart::now(),
            None,
        );
        let receipt = error
            .data
            .and_then(|data| {
                data.get(super::super::request_receipts::EXECUTION_RECEIPT_KEY)
                    .cloned()
            })
            .expect("canonical execution receipt");
        assert_eq!(receipt["terminal"], "denied");
        assert_eq!(receipt["worker_settlement"], "not_started");
    }

    #[test]
    fn non_cancellable_ingress_keeps_response_ownership_on_cancel_notification() {
        let ingress = RmcpRequestIngressRegistry::default();
        let id = json!(17);
        ingress.record(
            &id,
            "tools/call",
            Some(&json!({
                "name": "tracedecay_diagnostics",
                "arguments": {},
            })),
            McpRequestStart::now(),
        );

        assert!(
            ingress.cancelled_response(&id).is_none(),
            "a catalog NotCancellable operation must not emit a false cancellation terminal"
        );
        assert!(
            ingress
                .response_deadline(&RmcpRequestIngressRegistry::request_key(&id).unwrap())
                .is_some(),
            "ignored cancellation must leave the real response owner registered"
        );
    }

    #[test]
    fn cancellable_ingress_cancels_the_admitted_request_signal() {
        let ingress = RmcpRequestIngressRegistry::default();
        let id = json!(18);
        let params = json!({
            "name": "tracedecay_search",
            "arguments": {"query": "cancel"},
        });
        let started = McpRequestStart::now();
        ingress.record(&id, "tools/call", Some(&params), started);
        let request_key = RmcpRequestIngressRegistry::request_key(&id).unwrap();
        let policy = crate::mcp::tools::dispatch::lifecycle_policy_for_tool("tracedecay_search")
            .unwrap()
            .unwrap();
        let control = super::super::request_lifecycle::McpRequestRegistry::new()
            .admit(&request_key, "tracedecay_search", started, policy)
            .unwrap();
        ingress.observe_dispatch_control(&id, control.clone());

        let (response, _) = ingress
            .cancelled_response(&id)
            .expect("cancellable ingress response");
        assert!(
            control.is_cancelled(),
            "transport cancellation must cancel the admitted application signal"
        );
        let error = response.error.expect("cancellation error");
        assert_eq!(
            error.code,
            crate::mcp::transport::ErrorCode::RequestCancelled.as_i32()
        );
        let receipt = error
            .data
            .and_then(|data| {
                data.get(super::super::request_receipts::EXECUTION_RECEIPT_KEY)
                    .cloned()
            })
            .expect("cancellation receipt");
        assert_eq!(receipt["terminal"], "cancelled");
        assert_eq!(receipt["worker_settlement"], "not_started");
    }

    #[test]
    fn cancellation_before_control_observation_cancels_control_at_admission() {
        let ingress = RmcpRequestIngressRegistry::default();
        let id = json!(20);
        let params = json!({
            "name": "tracedecay_search",
            "arguments": {"query": "cancel-before-admission"},
        });
        let started = McpRequestStart::now();
        ingress.record(&id, "tools/call", Some(&params), started);
        ingress
            .cancelled_response(&id)
            .expect("cancellable ingress response");

        let request_key = RmcpRequestIngressRegistry::request_key(&id).unwrap();
        let policy = crate::mcp::tools::dispatch::lifecycle_policy_for_tool("tracedecay_search")
            .unwrap()
            .unwrap();
        let control = super::super::request_lifecycle::McpRequestRegistry::new()
            .admit(&request_key, "tracedecay_search", started, policy)
            .unwrap();
        ingress.observe_dispatch_control(&id, control.clone());

        assert!(
            control.is_cancelled(),
            "the ingress cancellation tombstone must cancel a control admitted after notification"
        );
        assert!(
            ingress.cancelled_response(&id).is_none(),
            "the client receives exactly one cancellation response"
        );
    }

    #[tokio::test]
    async fn in_flight_worker_cancellation_receipt_has_real_reconciliation_identity() {
        let ingress = RmcpRequestIngressRegistry::default();
        let id = json!(19);
        let params = json!({
            "name": "tracedecay_search",
            "arguments": {"query": "worker"},
        });
        let started = McpRequestStart::now();
        ingress.record(&id, "tools/call", Some(&params), started);
        let request_key = RmcpRequestIngressRegistry::request_key(&id).unwrap();
        let policy = crate::mcp::tools::dispatch::lifecycle_policy_for_tool("tracedecay_search")
            .unwrap()
            .unwrap();
        let control = super::super::request_lifecycle::McpRequestRegistry::new()
            .admit(&request_key, "tracedecay_search", started, policy)
            .unwrap();
        ingress.observe_dispatch_control(&id, control.clone());

        let reservation = control
            .reserve_join_required_worker(McpToolDispatchStage::Handler)
            .unwrap();
        let worker =
            tokio::spawn(async { std::future::pending::<crate::errors::Result<()>>().await });
        let worker_control = control.clone();
        let dispatch = tokio::spawn(async move {
            worker_control
                .run_owned_join_required(McpToolDispatchStage::Handler, reservation, worker)
                .await
        });
        tokio::task::yield_now().await;

        let (response, _) = ingress
            .cancelled_response(&id)
            .expect("cancellable ingress response");
        let receipt = response
            .error
            .and_then(|error| error.data)
            .and_then(|data| {
                data.get(super::super::request_receipts::EXECUTION_RECEIPT_KEY)
                    .cloned()
            })
            .expect("cancellation receipt");
        assert_eq!(receipt["worker_settlement"], "indeterminate");
        assert!(
            receipt["worker_reconciliation"]["id"]
                .as_u64()
                .is_some_and(|id| id > 0),
            "an indeterminate worker must name its real reaper record: {receipt}"
        );
        assert_eq!(receipt["worker_reconciliation"]["status"], "pending");

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), dispatch)
            .await
            .expect("cancelled dispatch cleanup")
            .expect("dispatch task");
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn queued_rmcp_start_is_expired_at_callback_admission() {
        let ingress = RmcpRequestIngressRegistry::default();
        let id = json!(41);
        let started = McpRequestStart::now();
        let params = json!({
            "name": "tracedecay_search",
            "arguments": {"query": "queued"}
        });
        ingress.record(&id, "tools/call", Some(&params), started);
        let policy = crate::mcp::tools::dispatch::lifecycle_policy_for_tool("tracedecay_search")
            .expect("catalog policy")
            .expect("search policy");
        tokio::time::advance(policy.maximum_duration()).await;

        let error = match super::super::request_lifecycle::McpRequestRegistry::new().admit(
            "rmcp-queued",
            "tracedecay_search",
            ingress.started(&id).expect("wire ingress start"),
            policy,
        ) {
            Ok(_) => panic!("queued request must not receive a fresh callback deadline"),
            Err(error) => error,
        };
        assert_eq!(
            error.mcp_tool_dispatch_context().map(|context| context.0),
            Some("tool_dispatch_deadline_exceeded")
        );
    }
}

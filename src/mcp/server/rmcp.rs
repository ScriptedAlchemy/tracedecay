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

use super::{ConnectionRouteState, McpRequestStart, McpServer, McpToolDispatchStage};

/// Allows daemon routing to enrich the legacy `initialize` response without
/// coupling this MCP module to daemon route types.
pub(crate) type RmcpInitializeResponseDecorator =
    Arc<dyn Fn(&mut JsonRpcResponse) + Send + Sync + 'static>;

/// Per-connection `rmcp` server facade over the existing `TraceDecay` request
/// authority.
pub(crate) struct RmcpConnectionAdapter {
    server: Arc<McpServer>,
    connection: Mutex<ConnectionRouteState>,
    memory_request_scope: String,
    timings_enabled: bool,
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
    ) -> Result<Self, crate::request_identity::RequestIdentityError> {
        let connection = server.new_connection_route_state()?;
        let memory_request_scope = connection.memory_request_scope().to_owned();
        Ok(Self {
            server,
            connection: Mutex::new(connection),
            memory_request_scope,
            timings_enabled,
            initialize_response_decorator,
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
        let started = McpRequestStart::now();
        let request_id = context.id;
        let request_cancellation = context.ct;
        let id = serde_json::to_value(&request_id)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
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
                    .map_err(|error| typed_tool_error(id.clone(), tool_name, &error))?,
            ),
            None => None,
        };
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
            return Err(project_server_retired_error());
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
                    )
                })?,
            None => self.connection.lock().await,
        };
        let handling = self.server.handle_request_for_connection(
            &request,
            self.timings_enabled,
            &mut connection,
            dispatch_control,
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
        .ok_or_else(|| ErrorData::internal_error("MCP request did not produce a response", None))?;
        if project_tool_call
            && self
                .server
                .project_server_lifecycle
                .response_revoked()
                .is_cancelled()
        {
            return Err(project_server_retired_error());
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
) -> ErrorData {
    match super::tool_errors::tool_error_response(id, tool_name, error).error {
        Some(error) => rmcp_error(error),
        None => ErrorData::internal_error(
            "TraceDecay MCP tool error response omitted its error payload",
            None,
        ),
    }
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
}

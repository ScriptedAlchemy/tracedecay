//! `rmcp` 3.x adapter for the authenticated `TraceDecay` MCP surface.
//!
//! The daemon owns authentication, bounded framing, project selection, and
//! replacement/retirement. Once that boundary selected a project server, this
//! adapter delegates standard MCP requests to the existing catalog and handler
//! authority through `rmcp`'s typed server callbacks.

use std::sync::Arc;

use rmcp::model::{
    Annotations, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    CustomNotification, ErrorCode, ErrorData, Implementation, InitializeRequestParams,
    InitializeResult, ListResourcesResult, ListToolsResult, MetaObject, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, Role, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde_json::{Value, json};
use tokio::sync::{RwLock, Semaphore};

use tracedecay_mcp::transport::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

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

struct McpRequest {
    method: &'static str,
    params: Option<Value>,
}

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
        request: McpRequest,
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
            self.dispatch_admitted(context, request),
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
        request: McpRequest,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let request_id = context.id;
        let request_cancellation = context.ct;
        let id = request_id.into_json_value();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(id.clone()),
            method: request.method.to_owned(),
            params: request.params,
        };
        if super::connection::request_is_independent_read(&request) {
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
        request: JsonRpcRequest,
        id: Value,
        request_cancellation: tokio_util::sync::CancellationToken,
        connection: &mut ConnectionRouteState,
    ) -> Result<JsonRpcResponse, ErrorData> {
        let pre_cancelled = request_cancellation.is_cancelled();
        let response = if pre_cancelled {
            Some(
                self.server
                    .handle_request_for_connection(&request, self.timings_enabled, connection, true)
                    .await,
            )
        } else {
            await_dispatch_with_cancellation(
                self.server.handle_request_for_connection(
                    &request,
                    self.timings_enabled,
                    connection,
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
}

fn response_value(response: JsonRpcResponse) -> Result<Value, ErrorData> {
    match (response.result, response.error) {
        (Some(result), None) => Ok(result),
        (_, Some(error)) => Err(rmcp_error(error)),
        _ => Err(ErrorData::internal_error(
            "TraceDecay MCP handler returned neither result nor error",
            None,
        )),
    }
}

fn value_object(value: Value, context: &str) -> Result<serde_json::Map<String, Value>, ErrorData> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(ErrorData::internal_error(
            format!("TraceDecay MCP {context} was not an object"),
            None,
        )),
    }
}

fn take_required_string(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<String, ErrorData> {
    object
        .remove(field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| {
            ErrorData::internal_error(
                format!("TraceDecay MCP {context} omitted string field `{field}`"),
                None,
            )
        })
}

fn take_meta(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<Option<MetaObject>, ErrorData> {
    object
        .remove(field)
        .map(|value| value_object(value, context).map(MetaObject))
        .transpose()
}

fn annotations_from_value(value: Value, context: &str) -> Result<Annotations, ErrorData> {
    let mut object = value_object(value, context)?;
    let audience = object
        .remove("audience")
        .map(|value| {
            let Value::Array(values) = value else {
                return Err(ErrorData::internal_error(
                    format!("TraceDecay MCP {context} audience was not an array"),
                    None,
                ));
            };
            values
                .into_iter()
                .map(|value| match value.as_str() {
                    Some("user") => Ok(Role::User),
                    Some("assistant") => Ok(Role::Assistant),
                    _ => Err(ErrorData::internal_error(
                        format!("TraceDecay MCP {context} carried an unknown audience role"),
                        None,
                    )),
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let priority = object
        .remove("priority")
        .map(|value| {
            value.as_f64().map(|priority| priority as f32).ok_or_else(|| {
                ErrorData::internal_error(
                    format!("TraceDecay MCP {context} priority was not numeric"),
                    None,
                )
            })
        })
        .transpose()?;
    let last_modified = object
        .remove("lastModified")
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                ErrorData::internal_error(
                    format!("TraceDecay MCP {context} lastModified was not a string"),
                    None,
                )
            })
        })
        .transpose()?;
    let mut annotations = Annotations::default();
    annotations.audience = audience;
    annotations.priority = priority;
    annotations.last_modified = last_modified;
    Ok(annotations)
}

fn content_block_from_value(value: Value) -> Result<ContentBlock, ErrorData> {
    let mut object = value_object(value, "tool content")?;
    let content_type = take_required_string(&mut object, "type", "tool content")?;
    if content_type != "text" {
        return Err(ErrorData::internal_error(
            format!("TraceDecay MCP emitted unsupported tool content type `{content_type}`"),
            None,
        ));
    }
    let text = take_required_string(&mut object, "text", "tool content")?;
    let meta = take_meta(&mut object, "_meta", "tool content metadata")?;
    let annotations = object
        .remove("annotations")
        .map(|value| annotations_from_value(value, "tool content annotations"))
        .transpose()?;
    let mut content = rmcp::model::TextContent::new(text);
    content.meta = meta;
    content.annotations = annotations;
    Ok(ContentBlock::Text(content))
}

fn call_tool_result(response: JsonRpcResponse) -> Result<CallToolResult, ErrorData> {
    let mut object = value_object(response_value(response)?, "tools/call result")?;
    let content = match object.remove("content") {
        Some(Value::Array(content)) => content
            .into_iter()
            .map(content_block_from_value)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(ErrorData::internal_error(
                "TraceDecay MCP tools/call content was not an array",
                None,
            ));
        }
        None => Vec::new(),
    };
    let is_error = object
        .remove("isError")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                ErrorData::internal_error(
                    "TraceDecay MCP tools/call isError was not boolean",
                    None,
                )
            })
        })
        .transpose()?;
    let structured_content = object.remove("structuredContent");
    let meta = take_meta(&mut object, "_meta", "tools/call metadata")?;
    let mut result = if is_error == Some(true) {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.result_type = None;
    result.structured_content = structured_content;
    result.is_error = is_error;
    result.meta = meta;
    Ok(result)
}

fn tool_annotations_from_value(value: Value) -> Result<ToolAnnotations, ErrorData> {
    let mut object = value_object(value, "tool annotations")?;
    let mut annotations = ToolAnnotations::default();
    annotations.title = object
        .remove("title")
        .and_then(|value| value.as_str().map(str::to_owned));
    annotations.read_only_hint = object.remove("readOnlyHint").and_then(|value| value.as_bool());
    annotations.destructive_hint = object
        .remove("destructiveHint")
        .and_then(|value| value.as_bool());
    annotations.idempotent_hint = object
        .remove("idempotentHint")
        .and_then(|value| value.as_bool());
    annotations.open_world_hint = object
        .remove("openWorldHint")
        .and_then(|value| value.as_bool());
    Ok(annotations)
}

fn tool_from_value(value: Value) -> Result<Tool, ErrorData> {
    let mut object = value_object(value, "tool definition")?;
    let name = take_required_string(&mut object, "name", "tool definition")?;
    let description = take_required_string(&mut object, "description", "tool definition")?;
    let input_schema = object
        .remove("inputSchema")
        .ok_or_else(|| {
            ErrorData::internal_error(
                "TraceDecay MCP tool definition omitted inputSchema",
                None,
            )
        })
        .and_then(|value| value_object(value, "tool input schema"))?;
    let annotations = object
        .remove("annotations")
        .map(tool_annotations_from_value)
        .transpose()?;
    let meta = take_meta(&mut object, "_meta", "tool metadata")?;
    let mut tool = Tool::new(name, description, Arc::new(input_schema));
    tool.annotations = annotations;
    tool.meta = meta;
    Ok(tool)
}

fn list_tools_result(response: JsonRpcResponse) -> Result<ListToolsResult, ErrorData> {
    let mut object = value_object(response_value(response)?, "tools/list result")?;
    let tools = match object.remove("tools") {
        Some(Value::Array(tools)) => tools
            .into_iter()
            .map(tool_from_value)
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(ErrorData::internal_error(
                "TraceDecay MCP tools/list result omitted tools",
                None,
            ));
        }
    };
    let mut result = ListToolsResult::with_all_items(tools);
    result.result_type = None;
    result.meta = take_meta(&mut object, "_meta", "tools/list metadata")?;
    Ok(result)
}

fn initialize_result(response: JsonRpcResponse) -> Result<InitializeResult, ErrorData> {
    let mut object = value_object(response_value(response)?, "initialize result")?;
    let instructions = object
        .remove("instructions")
        .and_then(|value| value.as_str().map(str::to_owned));
    let mut server_info = value_object(
        object.remove("serverInfo").ok_or_else(|| {
            ErrorData::internal_error("TraceDecay MCP initialize omitted serverInfo", None)
        })?,
        "initialize serverInfo",
    )?;
    let name = take_required_string(&mut server_info, "name", "initialize serverInfo")?;
    let version = take_required_string(&mut server_info, "version", "initialize serverInfo")?;
    let meta = take_meta(&mut object, "_meta", "initialize metadata")?;
    let mut capabilities = ServerCapabilities::builder()
        .enable_resources()
        .enable_tools()
        .enable_tool_list_changed()
        .build();
    capabilities.logging = Some(serde_json::Map::new());
    let mut result = InitializeResult::new(capabilities)
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(Implementation::new(name, version));
    result.instructions = instructions;
    result.meta = meta;
    Ok(result)
}

fn resource_from_value(value: Value) -> Result<Resource, ErrorData> {
    let mut object = value_object(value, "resource definition")?;
    let uri = take_required_string(&mut object, "uri", "resource definition")?;
    let name = take_required_string(&mut object, "name", "resource definition")?;
    let mut resource = Resource::new(uri, name);
    resource.description = object
        .remove("description")
        .and_then(|value| value.as_str().map(str::to_owned));
    resource.mime_type = object
        .remove("mimeType")
        .and_then(|value| value.as_str().map(str::to_owned));
    resource.meta = take_meta(&mut object, "_meta", "resource metadata")?;
    Ok(resource)
}

fn list_resources_result(response: JsonRpcResponse) -> Result<ListResourcesResult, ErrorData> {
    let mut object = value_object(response_value(response)?, "resources/list result")?;
    let resources = match object.remove("resources") {
        Some(Value::Array(resources)) => resources
            .into_iter()
            .map(resource_from_value)
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(ErrorData::internal_error(
                "TraceDecay MCP resources/list result omitted resources",
                None,
            ));
        }
    };
    let mut result = ListResourcesResult::with_all_items(resources);
    result.result_type = None;
    result.meta = take_meta(&mut object, "_meta", "resources/list metadata")?;
    Ok(result)
}

fn read_resource_result(response: JsonRpcResponse) -> Result<ReadResourceResult, ErrorData> {
    let mut object = value_object(response_value(response)?, "resources/read result")?;
    let contents = match object.remove("contents") {
        Some(Value::Array(contents)) => contents
            .into_iter()
            .map(|value| {
                let mut content = value_object(value, "resource contents")?;
                let uri = take_required_string(&mut content, "uri", "resource contents")?;
                let text = take_required_string(&mut content, "text", "resource contents")?;
                let mime_type = content
                    .remove("mimeType")
                    .and_then(|value| value.as_str().map(str::to_owned));
                let meta = take_meta(&mut content, "_meta", "resource contents metadata")?;
                Ok(ResourceContents::TextResourceContents {
                    uri,
                    mime_type,
                    text,
                    meta,
                })
            })
            .collect::<Result<Vec<_>, ErrorData>>()?,
        _ => {
            return Err(ErrorData::internal_error(
                "TraceDecay MCP resources/read result omitted contents",
                None,
            ));
        }
    };
    let mut result = ReadResourceResult::new(contents);
    result.result_type = None;
    result.meta = take_meta(&mut object, "_meta", "resources/read metadata")?;
    Ok(result)
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
        let params = json!({
            "clientInfo": {
                "name": request.client_info.name,
                "version": request.client_info.version,
            },
        });
        let mut response = self
            .dispatch(
                context,
                McpRequest {
                    method: "initialize",
                    params: Some(params),
                },
            )
            .await?;
        if let Some(decorate) = &self.initialize_response_decorator {
            decorate(&mut response);
        }
        initialize_result(response)
    }

    #[hotpath::skip]
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        list_tools_result(
            self.dispatch(
                context,
                McpRequest {
                    method: "tools/list",
                    params: None,
                },
            )
            .await?,
        )
    }

    #[hotpath::skip]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let mut params = serde_json::Map::with_capacity(2);
        params.insert("name".to_owned(), Value::String(request.name.into_owned()));
        params.insert(
            "arguments".to_owned(),
            Value::Object(request.arguments.unwrap_or_default()),
        );
        call_tool_result(
            self.dispatch(
                context,
                McpRequest {
                    method: "tools/call",
                    params: Some(Value::Object(params)),
                },
            )
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
        list_resources_result(
            self.dispatch(
                context,
                McpRequest {
                    method: "resources/list",
                    params: None,
                },
            )
            .await?,
        )
    }

    #[hotpath::skip]
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        read_resource_result(
            self.dispatch(
                context,
                McpRequest {
                    method: "resources/read",
                    params: Some(json!({"uri": request.uri})),
                },
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

    #[cfg(feature = "hotpath-alloc")]
    #[global_allocator]
    static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

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

    type RecordedWireMessages = Arc<std::sync::Mutex<Vec<Value>>>;

    async fn connect_rmcp(
        server: Arc<McpServer>,
        initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
    ) -> (
        rmcp::service::RunningService<RoleClient, ()>,
        RecordedWireMessages,
        RecordedWireMessages,
        tokio::task::JoinHandle<()>,
    ) {
        let adapter = RmcpConnectionAdapter::new(server, false, initialize_response_decorator)
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
        (client, client_messages, server_messages, serving)
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
            let initialize_response_decorator = Some(Arc::new(|response: &mut JsonRpcResponse| {
                response.result.as_mut().expect("initialize result")["_meta"]["tracedecayInitializeRoute"] = json!({
                    "projectPath": "/wire/oracle",
                    "allowInit": false,
                });
            })
                as RmcpInitializeResponseDecorator);
            let (client, client_messages, server_messages, serving) =
                connect_rmcp(Arc::clone(&server), initialize_response_decorator).await;
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

    fn percentile(samples: &mut [u64], numerator: usize) -> u64 {
        samples.sort_unstable();
        let index = samples
            .len()
            .saturating_mul(numerator)
            .div_ceil(100)
            .saturating_sub(1);
        samples.get(index).copied().unwrap_or_default()
    }

    fn status_call() -> CallToolRequestParams {
        CallToolRequestParams::new("tracedecay_status").with_arguments(
            json!({"admission_only": true, "format": "json"})
                .as_object()
                .cloned()
                .expect("object arguments"),
        )
    }

    #[test]
    #[ignore = "explicit RMCP latency and allocation benchmark"]
    fn measure_rmcp_dispatch_latency_and_allocations() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_stack_size(16 * 1024 * 1024)
            .enable_all()
            .build()
            .expect("RMCP benchmark runtime");
        runtime.block_on(Box::pin(async {
            let mode = std::env::var("TRACEDECAY_RMCP_BENCH_MODE")
                .unwrap_or_else(|_| "persistent".to_owned());
            let fixture = RmcpWireFixture::start().await;

            if mode == "large" {
                for index in 0..128 {
                    fixture
                        .client
                        .call_tool(
                            CallToolRequestParams::new("tracedecay_fact_store_add").with_arguments(
                                json!({
                                    "content": format!(
                                        "RMCP_LARGE_BENCH_{index:03}: {}",
                                        "one mebibyte response materialization ".repeat(220),
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
                        .expect("seed large benchmark response");
                }
            }

            let hotpath = hotpath::HotpathGuardBuilder::new("rmcp-dispatch-bench").build();
            let mut samples_us = Vec::new();
            let mut original_chars = None;
            match mode.as_str() {
                "persistent" => {
                    for _ in 0..5 {
                        fixture
                            .client
                            .call_tool(status_call())
                            .await
                            .expect("warm persistent RMCP call");
                    }
                    for _ in 0..25 {
                        let started = std::time::Instant::now();
                        fixture
                            .client
                            .call_tool(status_call())
                            .await
                            .expect("persistent RMCP call");
                        samples_us.push(started.elapsed().as_micros() as u64);
                    }
                }
                "large" => {
                    let started = std::time::Instant::now();
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
                        .expect("large RMCP benchmark call");
                    samples_us.push(started.elapsed().as_micros() as u64);
                    let response = fixture.last_response();
                    let text = response["result"]["content"][0]["text"]
                        .as_str()
                        .expect("large benchmark response text");
                    let envelope: Value =
                        serde_json::from_str(text).expect("large benchmark truncation envelope");
                    assert_eq!(envelope["truncated"], json!(true));
                    original_chars = envelope["original_chars"].as_u64();
                    assert!(
                        original_chars.is_some_and(|chars| chars >= 1024 * 1024),
                        "large benchmark must materialize at least one mebibyte before bounding",
                    );
                }
                "churn" => {
                    for _ in 0..25 {
                        let started = std::time::Instant::now();
                        let (mut client, _, _, serving) =
                            connect_rmcp(Arc::clone(&fixture.server), None).await;
                        samples_us.push(started.elapsed().as_micros() as u64);
                        client.close().await.expect("close churn RMCP client");
                        serving.await.expect("join churn RMCP server");
                    }
                }
                other => panic!("unknown TRACEDECAY_RMCP_BENCH_MODE: {other}"),
            }
            drop(hotpath);

            let mut p50_samples = samples_us.clone();
            let mut p95_samples = samples_us.clone();
            println!(
                "{}",
                json!({
                    "mode": mode,
                    "requests": samples_us.len(),
                    "p50_us": percentile(&mut p50_samples, 50),
                    "p95_us": percentile(&mut p95_samples, 95),
                    "original_chars": original_chars,
                }),
            );
            fixture.shutdown().await;
        }));
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
        let complete: CallToolResponse = call_tool_result(JsonRpcResponse::success(
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

        let error = list_tools_result(JsonRpcResponse::error_with_data(
                json!("request"),
                tracedecay_mcp::transport::ErrorCode::InvalidParams,
                "invalid arguments".to_owned(),
                Some(json!({"reason": "missing_query"})),
            ))
        .expect_err("error response");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(error.message, "invalid arguments");
        assert_eq!(error.data, Some(json!({"reason": "missing_query"})));
    }

    #[test]
    fn adapter_accepts_the_legacy_initialize_response_shape() {
        crate::product_runtime::register_fixture_product_runtime();
        let initialized: InitializeResult = initialize_result(JsonRpcResponse::success(
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

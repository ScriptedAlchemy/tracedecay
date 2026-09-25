//! The typed internal dispatch envelope in front of the handler catalog.
//!
//! The `rmcp` adapter's server callbacks are handed already-typed request
//! DTOs; custom notifications and in-process replay arrive as a raw
//! [`JsonRpcRequest`]. The envelope carries the request identity, the method,
//! its classification, and a method-specific payload that is *either* the raw
//! JSON params or the typed DTO, so no typed DTO is re-encoded into a JSON
//! tree just to reach dispatch. Internal dispatch reads the payload only
//! through the accessors below, each of which answers the same question on
//! both representations, so there is exactly one dispatch authority.
//!
//! Response rendering stays at the wire edge: the `rmcp` adapter materializes
//! the handler's [`JsonRpcResponse`] into the typed result DTO its
//! `ServerHandler` signature requires.

use crate::transport::JsonRpcRequest;
use rmcp::model::{CallToolRequestParams, InitializeRequestParams, ReadResourceRequestParams};
use serde_json::Value;

use super::protocol::{McpMethod, classify_mcp_method};

/// The method-specific request payload, in whichever form its transport
/// already owns.
pub enum McpDispatchParams<'a> {
    /// Raw JSON-RPC params, borrowed from a parsed request. This is how
    /// `rmcp` delivers hook-event and cancellation notifications, which are
    /// custom (untyped) methods on that transport.
    Raw(Option<&'a Value>),
    /// Typed `initialize` params from the `rmcp` server callback.
    Initialize(&'a InitializeRequestParams),
    /// Typed `tools/call` params. Owned, so the tool name and the argument map
    /// move into dispatch instead of being re-encoded and cloned back out.
    ToolsCall(CallToolRequestParams),
    /// Typed `resources/read` params.
    ResourcesRead(&'a ReadResourceRequestParams),
    /// A typed request whose params dispatch does not read (`tools/list`,
    /// `resources/list`). Equivalent to `Raw(None)` at every accessor.
    TypedEmpty,
}

/// `tools/call` params handed to tool-call preparation.
///
/// Split out of [`McpDispatchParams`] so preparation can consume the typed
/// payload by value; every other accessor only needs to borrow.
pub enum ToolCallParams<'a> {
    Raw(Option<&'a Value>),
    Typed(CallToolRequestParams),
}

/// One MCP request, independent of the transport that received it.
pub struct McpDispatchRequest<'a> {
    id: Option<Value>,
    method: &'a str,
    method_class: McpMethod,
    params: McpDispatchParams<'a>,
}

impl<'a> McpDispatchRequest<'a> {
    /// Wraps a raw JSON-RPC request whose params stay untyped JSON, such as a
    /// custom notification.
    ///
    /// Nothing is copied but the (small) request identity, which dispatch
    /// already cloned before consuming.
    pub fn raw(request: &'a JsonRpcRequest) -> Self {
        Self::new(
            request.id.clone(),
            &request.method,
            McpDispatchParams::Raw(request.params.as_ref()),
        )
    }

    /// Wraps a typed request from the `rmcp` adapter.
    pub fn typed(id: Value, method: &'a str, params: McpDispatchParams<'a>) -> Self {
        Self::new(Some(id), method, params)
    }

    fn new(id: Option<Value>, method: &'a str, params: McpDispatchParams<'a>) -> Self {
        Self {
            id,
            method,
            method_class: classify_mcp_method(method),
            params,
        }
    }

    /// The request identity, cloned for the handlers that own their response.
    pub fn cloned_id(&self) -> Option<Value> {
        self.id.clone()
    }

    /// The method name, borrowed from the transport's own request rather than
    /// from the envelope, so callers may keep it across
    /// [`Self::into_tool_call`].
    pub fn method(&self) -> &'a str {
        self.method
    }

    pub fn method_class(&self) -> McpMethod {
        self.method_class
    }

    /// Params for the notification methods that are only ever raw.
    ///
    /// Hook events and `notifications/cancelled` arrive as custom notification
    /// methods on both transports, so no typed variant can reach these paths;
    /// a typed payload deliberately reads as "no params" rather than being
    /// materialized into JSON to answer the question.
    pub fn raw_notification_params(&self) -> Option<&Value> {
        match &self.params {
            McpDispatchParams::Raw(params) => *params,
            _ => None,
        }
    }

    /// Params to resolve `initialize` client roots from.
    ///
    /// `rmcp`'s `InitializeRequestParams` has no `roots` field, the roots a
    /// client advertises reach the daemon through its own routing, not through
    /// this DTO, so a typed initialize has never contributed a root here.
    /// Returning `None` preserves that exactly, without building the JSON tree
    /// the previous bridge built only to find no `roots` key in it.
    pub fn initialize_roots_params(&self) -> Option<&Value> {
        match &self.params {
            McpDispatchParams::Raw(params) => *params,
            McpDispatchParams::Initialize(_)
            | McpDispatchParams::ToolsCall(_)
            | McpDispatchParams::ResourcesRead(_)
            | McpDispatchParams::TypedEmpty => None,
        }
    }

    /// The negotiated `clientInfo.name` from an `initialize` request.
    pub fn client_info_name(&self) -> Option<&str> {
        match &self.params {
            McpDispatchParams::Raw(params) => params
                .and_then(|params| params.get("clientInfo"))
                .and_then(|client_info| client_info.get("name"))
                .and_then(Value::as_str),
            McpDispatchParams::Initialize(params) => Some(params.client_info.name.as_str()),
            McpDispatchParams::ToolsCall(_)
            | McpDispatchParams::ResourcesRead(_)
            | McpDispatchParams::TypedEmpty => None,
        }
    }

    /// The `resources/read` target URI.
    pub fn resource_uri(&self) -> Option<&str> {
        match &self.params {
            McpDispatchParams::Raw(params) => params
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str),
            McpDispatchParams::ResourcesRead(params) => Some(params.uri.as_str()),
            McpDispatchParams::Initialize(_)
            | McpDispatchParams::ToolsCall(_)
            | McpDispatchParams::TypedEmpty => None,
        }
    }

    /// The `tools/call` tool name, without consuming the payload.
    ///
    /// Read before dispatch to decide read concurrency, so it must not disturb
    /// the owned typed params that preparation later moves out.
    pub fn tool_name(&self) -> Option<&str> {
        self.params.tool_name()
    }

    /// Consumes the envelope into the `tools/call` payload.
    ///
    /// Only reachable for [`McpMethod::ToolsCall`]; the other variants map to
    /// absent params, which preparation refuses exactly as it refuses a raw
    /// `tools/call` with no params at all.
    pub fn into_tool_call(self) -> ToolCallParams<'a> {
        match self.params {
            McpDispatchParams::Raw(params) => ToolCallParams::Raw(params),
            McpDispatchParams::ToolsCall(params) => ToolCallParams::Typed(params),
            McpDispatchParams::Initialize(_)
            | McpDispatchParams::ResourcesRead(_)
            | McpDispatchParams::TypedEmpty => ToolCallParams::Raw(None),
        }
    }
}

impl McpDispatchParams<'_> {
    fn tool_name(&self) -> Option<&str> {
        match self {
            Self::Raw(params) => params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str),
            Self::ToolsCall(params) => Some(&*params.name),
            Self::Initialize(_) | Self::ResourcesRead(_) | Self::TypedEmpty => None,
        }
    }
}

/// Whether a request may run concurrently with other in-flight reads on its
/// connection.
///
/// One authority for both payload forms: a raw and a typed request classify
/// the same method the same way, so a read that forks connection state in one
/// form can never take the ordered write path in the other.
pub fn dispatch_is_independent_read(
    method: McpMethod,
    tool_name: Option<&str>,
    tool_is_read_only: impl FnOnce(&str) -> bool,
) -> bool {
    match method {
        McpMethod::ToolsCall => tool_name.is_some_and(tool_is_read_only),
        McpMethod::ToolsList
        | McpMethod::ResourcesList
        | McpMethod::ResourcesRead
        | McpMethod::TrivialAck => true,
        McpMethod::Initialize
        | McpMethod::InitializedAck
        | McpMethod::HookEvent
        | McpMethod::Cancelled
        | McpMethod::Unknown => false,
    }
}

pub fn needs_lazy_sync_before_dispatch(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_ast_grep_rewrite"
            | "tracedecay_insert_at"
            | "tracedecay_insert_at_symbol"
            | "tracedecay_move_symbol"
            | "tracedecay_multi_str_replace"
            | "tracedecay_replace_symbol"
            | "tracedecay_str_replace"
    )
}

#[cfg(test)]
mod tests {
    use rmcp::model::{ClientCapabilities, Implementation};
    use serde_json::json;

    use super::*;

    fn raw_request_for(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(json!(1)),
            method: method.to_owned(),
            params: Some(params),
        }
    }

    #[test]
    fn typed_and_raw_initialize_agree_on_every_field_dispatch_reads() {
        let typed_params = InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new("claude-code", "1.2.3"),
        );
        let raw_request = raw_request_for(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "claude-code", "version": "1.2.3"},
            }),
        );

        let raw = McpDispatchRequest::raw(&raw_request);
        let typed = McpDispatchRequest::typed(
            json!(1),
            "initialize",
            McpDispatchParams::Initialize(&typed_params),
        );

        assert_eq!(raw.method_class(), McpMethod::Initialize);
        assert_eq!(typed.method_class(), McpMethod::Initialize);
        assert_eq!(raw.client_info_name(), Some("claude-code"));
        assert_eq!(typed.client_info_name(), Some("claude-code"));
        // `rmcp`'s typed initialize carries no `roots`, exactly as the JSON
        // bridge it replaces carried none.
        assert!(typed.initialize_roots_params().is_none());
    }

    #[test]
    fn typed_tool_call_params_reach_dispatch_without_a_json_bridge() {
        let arguments = json!({"query": "envelope", "limit": 3})
            .as_object()
            .cloned()
            .expect("object arguments");
        let typed = McpDispatchRequest::typed(
            json!("call-1"),
            "tools/call",
            McpDispatchParams::ToolsCall(
                CallToolRequestParams::new("tracedecay_search").with_arguments(arguments.clone()),
            ),
        );
        assert_eq!(typed.method_class(), McpMethod::ToolsCall);
        assert_eq!(typed.tool_name(), Some("tracedecay_search"));

        let ToolCallParams::Typed(params) = typed.into_tool_call() else {
            panic!("a typed tools/call must stay typed through the envelope");
        };
        assert_eq!(&*params.name, "tracedecay_search");
        assert_eq!(params.arguments, Some(arguments));
    }

    #[test]
    fn typed_and_raw_resources_read_agree_on_the_target_uri() {
        let typed_params = ReadResourceRequestParams::new("tracedecay://schema");
        let raw_request = raw_request_for("resources/read", json!({"uri": "tracedecay://schema"}));
        assert_eq!(
            McpDispatchRequest::raw(&raw_request).resource_uri(),
            Some("tracedecay://schema"),
        );
        assert_eq!(
            McpDispatchRequest::typed(
                json!(1),
                "resources/read",
                McpDispatchParams::ResourcesRead(&typed_params),
            )
            .resource_uri(),
            Some("tracedecay://schema"),
        );
    }

    #[test]
    fn a_typed_request_without_params_reads_like_absent_params() {
        let typed =
            McpDispatchRequest::typed(json!(1), "tools/list", McpDispatchParams::TypedEmpty);
        assert_eq!(typed.method_class(), McpMethod::ToolsList);
        assert!(typed.tool_name().is_none());
        assert!(typed.resource_uri().is_none());
        assert!(typed.client_info_name().is_none());
        assert!(typed.initialize_roots_params().is_none());
        assert!(typed.raw_notification_params().is_none());
    }

    #[test]
    fn read_concurrency_is_decided_identically_for_both_transports() {
        let raw_request = raw_request_for(
            "tools/call",
            json!({"name": "tracedecay_search", "arguments": {}}),
        );
        let raw = McpDispatchRequest::raw(&raw_request);
        let typed = McpDispatchRequest::typed(
            json!(1),
            "tools/call",
            McpDispatchParams::ToolsCall(CallToolRequestParams::new("tracedecay_search")),
        );
        assert_eq!(
            dispatch_is_independent_read(raw.method_class(), raw.tool_name(), |name| {
                name == "tracedecay_search"
            }),
            dispatch_is_independent_read(typed.method_class(), typed.tool_name(), |name| {
                name == "tracedecay_search"
            }),
        );

        let raw_write = raw_request_for(
            "tools/call",
            json!({"name": "tracedecay_str_replace", "arguments": {}}),
        );
        let raw_write = McpDispatchRequest::raw(&raw_write);
        let typed_write = McpDispatchRequest::typed(
            json!(1),
            "tools/call",
            McpDispatchParams::ToolsCall(CallToolRequestParams::new("tracedecay_str_replace")),
        );
        assert!(!dispatch_is_independent_read(
            raw_write.method_class(),
            raw_write.tool_name(),
            |_| false,
        ));
        assert!(!dispatch_is_independent_read(
            typed_write.method_class(),
            typed_write.tool_name(),
            |_| false,
        ));
    }
}

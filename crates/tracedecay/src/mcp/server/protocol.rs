use rmcp::model::{
    Implementation, InitializeResult, ListResourcesResult, ProtocolVersion, ReadResourceResult,
    Resource, ServerCapabilities,
};
use serde_json::{Value, json};
use tracedecay_mcp::{ErrorCode, JsonRpcError, JsonRpcResponse, ToolDefinition, ToolResult};

/// Every JSON-RPC method surface the MCP server understands. This is the
/// single source of truth for [`McpServer::handle_request`] dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpMethod {
    Initialize,
    /// `initialized` / `notifications/initialized` — compatibility no-ops.
    InitializedAck,
    ToolsList,
    ToolsCall,
    ResourcesList,
    ResourcesRead,
    /// `ping` / `logging/setLevel` — acknowledged with an empty result.
    TrivialAck,
    /// The daemon's internal hook-event notification.
    HookEvent,
    Cancelled,
    Unknown,
}

pub(crate) enum McpResponseBody {
    Initialize(InitializeResult),
    ToolsList(Vec<ToolDefinition>),
    ToolsCall(ToolResult),
    ResourcesList(ListResourcesResult),
    ResourcesRead(ReadResourceResult),
    Raw(Value),
}

pub(crate) struct McpResponse {
    pub(crate) id: Value,
    pub(crate) result: Option<McpResponseBody>,
    pub(crate) error: Option<JsonRpcError>,
}

impl McpResponse {
    pub(crate) fn success(id: Value, result: Value) -> Self {
        Self {
            id,
            result: Some(McpResponseBody::Raw(result)),
            error: None,
        }
    }

    pub(crate) fn typed(id: Value, result: McpResponseBody) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn error(id: Value, code: ErrorCode, message: String) -> Self {
        Self::error_with_data(id, code, message, None)
    }

    pub(crate) fn error_with_data(
        id: Value,
        code: ErrorCode,
        message: String,
        data: Option<Value>,
    ) -> Self {
        Self {
            id,
            result: None,
            error: Some(JsonRpcError {
                code: code.as_i32(),
                message,
                data,
            }),
        }
    }

    pub(crate) fn from_legacy(response: JsonRpcResponse) -> Self {
        Self {
            id: response.id,
            result: response.result.map(McpResponseBody::Raw),
            error: response.error,
        }
    }

    pub(crate) fn insert_result_meta(&mut self, key: &str, value: Value) {
        match self.result.as_mut() {
            Some(McpResponseBody::Initialize(result)) => {
                result
                    .meta
                    .get_or_insert_with(rmcp::model::MetaObject::new)
                    .0
                    .insert(key.to_owned(), value);
            }
            Some(McpResponseBody::Raw(Value::Object(result))) => {
                let meta = result
                    .entry("_meta")
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                if let Value::Object(meta) = meta {
                    meta.insert(key.to_owned(), value);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn into_legacy(self) -> JsonRpcResponse {
        let Self { id, result, error } = self;
        match (result, error) {
            (Some(result), None) => {
                let result = match result {
                    McpResponseBody::Initialize(result) => serde_json::to_value(result),
                    McpResponseBody::ToolsList(tools) => Ok(json!({"tools": tools})),
                    McpResponseBody::ToolsCall(result) => Ok(result.value),
                    McpResponseBody::ResourcesList(result) => serde_json::to_value(result),
                    McpResponseBody::ResourcesRead(result) => serde_json::to_value(result),
                    McpResponseBody::Raw(result) => Ok(result),
                };
                match result {
                    Ok(result) => JsonRpcResponse::success(id, result),
                    Err(error) => JsonRpcResponse::error(
                        id,
                        ErrorCode::InternalError,
                        format!("failed to render typed MCP response: {error}"),
                    ),
                }
            }
            (None, Some(error)) => JsonRpcResponse {
                jsonrpc: "2.0".to_owned(),
                id,
                result: None,
                error: Some(error),
            },
            _ => JsonRpcResponse::error(
                id,
                ErrorCode::InternalError,
                "TraceDecay MCP handler returned neither result nor error".to_owned(),
            ),
        }
    }
}

impl From<JsonRpcResponse> for McpResponse {
    fn from(response: JsonRpcResponse) -> Self {
        Self::from_legacy(response)
    }
}

pub(crate) fn classify_mcp_method(method: &str) -> McpMethod {
    if method == tracedecay_hooks::core_events::HOOK_EVENT_METHOD {
        return McpMethod::HookEvent;
    }
    match method {
        "initialize" => McpMethod::Initialize,
        "initialized" | "notifications/initialized" => McpMethod::InitializedAck,
        "tools/list" => McpMethod::ToolsList,
        "tools/call" => McpMethod::ToolsCall,
        "resources/list" => McpMethod::ResourcesList,
        "resources/read" => McpMethod::ResourcesRead,
        "notifications/cancelled" => McpMethod::Cancelled,
        "ping" | "logging/setLevel" => McpMethod::TrivialAck,
        _ => McpMethod::Unknown,
    }
}

/// The steering instructions advertised from the `initialize` handshake of a
/// healthy server.
pub(crate) const SERVER_INSTRUCTIONS: &str = concat!(
    "tracedecay is a code-graph MCP server. \
    Start with tracedecay_context for any code exploration task \
    — it returns relevant symbols, relationships, and code \
    snippets for a natural-language query. Use tracedecay_search \
    to find specific symbols by name. Discovery and analysis \
    tools are read-only and safe to call in parallel. Edit \
    and session-memory tools can mutate local project state \
    and declare readOnlyHint=false. \
    Every tool is also available from the shell: ",
    crate::cli_fallback_args_invocation_lit!(),
    " \
    — run `tracedecay tool` to list tools, \
    `tracedecay tool <name> --help` for parameters). If an MCP \
    call errors, times out, or this server disconnects, fall \
    back to that CLI instead of querying .tracedecay databases \
    directly or abandoning tracedecay. \
    When a tool result contains a `tracedecay_metrics:` line, \
    report the savings to the user (e.g. 'TraceDecay\\'d ~N tokens')."
);

/// The `initialize` result payload. Fallible because the advertised server
/// version reads the registered product runtime.
pub(crate) fn initialize_result(
    instructions: &str,
) -> Result<InitializeResult, crate::product_runtime::ProductRuntimeError> {
    let mut capabilities = ServerCapabilities::builder()
        .enable_resources()
        .enable_tools()
        .enable_tool_list_changed()
        .build();
    capabilities.logging = Some(serde_json::Map::new());
    Ok(InitializeResult::new(capabilities)
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(Implementation::new(
            "tracedecay",
            crate::version::build_version()?,
        ))
        .with_instructions(instructions))
}

/// The `resources/list` result payload.
pub(crate) fn resources_list_result() -> ListResourcesResult {
    let resources = [
        (
            "tracedecay://status",
            "Graph Status",
            "Code graph statistics: node/edge/file counts, languages, DB size, and index freshness.",
            "application/json",
        ),
        (
            "tracedecay://files",
            "File List",
            "All indexed project files grouped by directory with symbol counts.",
            "text/plain",
        ),
        (
            "tracedecay://overview",
            "Project Overview",
            "High-level project summary: language distribution, largest modules, and top entry points.",
            "text/plain",
        ),
        (
            "tracedecay://branches",
            "Tracked Branches",
            "List of tracked branches with DB sizes, parent branch, and last sync time. Empty if multi-branch is not active.",
            "application/json",
        ),
        (
            "tracedecay://schema",
            "SQLite Schema",
            "Documentation for the .tracedecay/tracedecay.db schema: tables, columns, indexes, and common query recipes. Use when MCP tools don't cover your query and you need to drop down to raw SQL.",
            "text/markdown",
        ),
    ]
    .into_iter()
    .map(|(uri, name, description, mime_type)| {
        Resource::new(uri, name)
            .with_description(description)
            .with_mime_type(mime_type)
    })
    .collect();
    let mut result = ListResourcesResult::with_all_items(resources);
    result.result_type = None;
    result
}

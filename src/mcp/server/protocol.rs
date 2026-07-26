use serde_json::{Value, json};

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

pub(crate) fn classify_mcp_method(method: &str) -> McpMethod {
    if method == crate::daemon::HOOK_EVENT_METHOD {
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

/// The `initialize` result payload.
pub(crate) fn initialize_result(instructions: &str) -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {
                "listChanged": true
            },
            "resources": {},
            "logging": {}
        },
        "serverInfo": {
            "name": "tracedecay",
            "version": crate::version::build_version()
        },
        "instructions": instructions,
    })
}

/// The `resources/list` result payload.
pub(crate) fn resources_list_result() -> Value {
    json!({
        "resources": [
            {
                "uri": "tracedecay://status",
                "name": "Graph Status",
                "description": "Code graph statistics: node/edge/file counts, languages, DB size, and index freshness.",
                "mimeType": "application/json"
            },
            {
                "uri": "tracedecay://files",
                "name": "File List",
                "description": "All indexed project files grouped by directory with symbol counts.",
                "mimeType": "text/plain"
            },
            {
                "uri": "tracedecay://overview",
                "name": "Project Overview",
                "description": "High-level project summary: language distribution, largest modules, and top entry points.",
                "mimeType": "text/plain"
            },
            {
                "uri": "tracedecay://branches",
                "name": "Tracked Branches",
                "description": "List of tracked branches with DB sizes, parent branch, and last sync time. Empty if multi-branch is not active.",
                "mimeType": "application/json"
            },
            {
                "uri": "tracedecay://schema",
                "name": "SQLite Schema",
                "description": "Documentation for the .tracedecay/tracedecay.db schema: tables, columns, indexes, and common query recipes. Use when MCP tools don't cover your query and you need to drop down to raw SQL.",
                "mimeType": "text/markdown"
            }
        ]
    })
}

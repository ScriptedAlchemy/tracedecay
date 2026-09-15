use serde_json::{Value, json};

/// Every JSON-RPC method surface the MCP server understands. This is the
/// single source of truth for MCP request dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMethod {
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

pub fn classify_mcp_method(method: &str) -> McpMethod {
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

/// The `initialize` result payload for product metadata supplied by the
/// composition root.
pub fn initialize_result(version: &str, instructions: &str) -> Value {
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
            "version": version
        },
        "instructions": instructions,
    })
}

/// The `resources/list` result payload.
pub fn resources_list_result() -> Value {
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

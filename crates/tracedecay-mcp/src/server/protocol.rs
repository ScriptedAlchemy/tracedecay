use rmcp::model::{ProtocolVersion, RequestMetaObject};
use serde::Deserialize;
use serde_json::{Value, json};

/// Every JSON-RPC method surface the MCP server understands. This is the
/// single source of truth for MCP request dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMethod {
    Initialize,
    /// `initialized` / `notifications/initialized`, compatibility no-ops.
    InitializedAck,
    ToolsList,
    ToolsCall,
    ResourcesList,
    ResourcesRead,
    /// `ping`, acknowledged with an empty result.
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
        "ping" => McpMethod::TrivialAck,
        _ => McpMethod::Unknown,
    }
}

/// The `initialize` result payload for product metadata supplied by the
/// composition root, agreeing to the client's `requested_protocol` when this
/// server speaks it and to the newest handshake revision otherwise. Raw
/// answers such as the daemon bootstrap reach hosts without rmcp's own
/// negotiation, so they must agree to the same version rmcp would.
pub fn initialize_result(
    version: &str,
    instructions: &str,
    requested_protocol: Option<&str>,
) -> Value {
    let protocol = ProtocolVersion::known_up_to(&ProtocolVersion::LATEST_WITH_INITIALIZE)
        .iter()
        .find(|known| Some(known.as_str()) == requested_protocol)
        .unwrap_or(&ProtocolVersion::LATEST_WITH_INITIALIZE);
    json!({
        "protocolVersion": protocol,
        "capabilities": {
            "tools": {
                "listChanged": true
            },
            "resources": {}
        },
        "serverInfo": {
            "name": "tracedecay",
            "version": version
        },
        "instructions": instructions,
    })
}

/// The protocol version a request names in its own `params._meta`.
pub fn request_protocol_version(params: Option<&Value>) -> Option<ProtocolVersion> {
    RequestMetaObject::deserialize(params?.get("_meta")?)
        .ok()?
        .protocol_version()
}

/// Whether `version` is MCP `2026-07-28` or newer, the revision whose results
/// carry `resultType` (SEP-2322) and whose cacheable results carry `ttlMs`
/// and `cacheScope` (SEP-2549).
pub fn speaks_2026_07_28(version: Option<&ProtocolVersion>) -> bool {
    version.is_some_and(|version| version.as_str() >= ProtocolVersion::V_2026_07_28.as_str())
}

/// Adds the fields MCP `2026-07-28` requires to a `method` result. Stamp
/// only an answer to a request on that revision: older revisions keep the
/// wire shape that predates these fields, and strict peers reject them.
///
/// Every list and read reflects one session's catalog or one project's
/// state, so a cacheable result is stale at once and private to its caller.
pub fn stamp_2026_07_28_result(method: &str, result: &mut Value) {
    let Some(result) = result.as_object_mut() else {
        return;
    };
    result
        .entry("resultType")
        .or_insert_with(|| json!("complete"));
    if matches!(
        method,
        "tools/list"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
    ) {
        result.entry("ttlMs").or_insert_with(|| json!(0));
        result
            .entry("cacheScope")
            .or_insert_with(|| json!("private"));
    }
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
                "description": "List of tracked branches with parent branch and last sync time. Empty if multi-branch is not active.",
                "mimeType": "application/json"
            },
            {
                "uri": "tracedecay://schema",
                "name": "SQLite Schema",
                "description": "Installed project-store DDL for this binary, generated from the fresh-store shape create_schema admits. Code topology is not in these tables.",
                "mimeType": "text/markdown"
            }
        ]
    })
}

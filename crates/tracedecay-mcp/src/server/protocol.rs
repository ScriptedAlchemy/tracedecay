use std::path::PathBuf;

use rmcp::model::{ProtocolVersion, RequestMetaObject};
use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_lsp::decode_uri_segment;

/// Every JSON-RPC method surface the MCP server understands. This is the
/// single source of truth for MCP request dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMethod {
    Initialize,
    /// `notifications/initialized`, acknowledged without a response.
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
        "notifications/initialized" => McpMethod::InitializedAck,
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
    let protocol = requested_protocol.map_or(
        ProtocolVersion::LATEST_WITH_INITIALIZE,
        negotiate_protocol_version,
    );
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

/// Decodes one `initialize` root URI into the local path it names.
///
/// A root that is not a `file:` URI is taken as a plain path: some hosts put
/// the filesystem path itself in `roots`. The stricter desktop-URI admission
/// policy the LSP gateway applies does not hold here, so the two never share a
/// decoder.
pub fn local_path_from_mcp_root_uri(uri: &str) -> Option<PathBuf> {
    let path = if let Some(rest) = uri.strip_prefix("file://") {
        if let Some(localhost_path) = rest.strip_prefix("localhost/") {
            format!("/{localhost_path}")
        } else if rest == "localhost" {
            "/".to_string()
        } else if rest.starts_with('/') {
            rest.to_string()
        } else {
            return None;
        }
    } else {
        uri.to_string()
    };
    decode_uri_segment(&path)
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .map(strip_windows_drive_slash)
        .map(PathBuf::from)
}

/// A Windows file URI like `file:///C:/work` decodes to `/C:/work`; the
/// leading slash before the drive letter must be dropped to form a usable
/// local path. On other platforms the path is returned unchanged.
#[cfg(windows)]
fn strip_windows_drive_slash(path: String) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        path[1..].to_string()
    } else {
        path
    }
}

#[cfg(not(windows))]
fn strip_windows_drive_slash(path: String) -> String {
    path
}

/// The version `initialize` agrees to: the client's request when it is a
/// known revision that still has an `initialize` handshake, else the newest
/// such revision (`2026-07-28` replaced the handshake with per-request context).
pub(crate) fn negotiate_protocol_version(requested: &str) -> ProtocolVersion {
    ProtocolVersion::known_up_to(&ProtocolVersion::LATEST_WITH_INITIALIZE)
        .iter()
        .find(|known| known.as_str() == requested)
        .cloned()
        .unwrap_or(ProtocolVersion::LATEST_WITH_INITIALIZE)
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

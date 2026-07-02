use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::mcp::transport::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeGlobalDbResolution {
    Found(PathBuf),
    Ambiguous(Vec<String>),
    None,
}

#[derive(Debug, Clone, Copy)]
enum CwdProjectMatch {
    ProjectContainsCwd,
    ProjectUnderCwd,
}

impl CwdProjectMatch {
    fn matches(self, project_path: &Path, cwd: &Path) -> bool {
        match self {
            Self::ProjectContainsCwd => cwd.starts_with(project_path),
            Self::ProjectUnderCwd => project_path.starts_with(cwd),
        }
    }

    fn sort_matches(self, matches: &mut [(usize, String)]) {
        match self {
            Self::ProjectContainsCwd => {
                matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            }
            Self::ProjectUnderCwd => {
                matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            }
        }
    }
}

pub fn global_db_ambiguity_message(paths: &[String]) -> String {
    let mut message =
        "Multiple tracedecay projects found — pass -p <path> to select one:".to_string();
    for path in paths {
        message.push_str("\n  ");
        message.push_str(path);
    }
    message
}

/// Opens an existing project, or tells the user to run `tracedecay init` first.
pub async fn ensure_initialized(project_path: &Path) -> Result<TraceDecay> {
    ensure_initialized_with_options(project_path, TraceDecayOpenOptions::default()).await
}

pub async fn ensure_initialized_with_options(
    project_path: &Path,
    open_options: TraceDecayOpenOptions,
) -> Result<TraceDecay> {
    match TraceDecay::open_with_options(project_path, open_options.clone()).await {
        Ok(cg) => return Ok(cg),
        Err(open_err) => {
            match TraceDecay::open_read_only_with_options(project_path, open_options).await {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    return Ok(cg);
                }
                Err(_) => {
                    if !matches!(open_err, TraceDecayError::Config { .. }) {
                        return Err(open_err);
                    }
                }
            }
        }
    }
    Err(TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}' — run 'tracedecay init' first",
            project_path.display()
        ),
    })
}

fn initialized_project_paths(mut paths: Vec<String>) -> Vec<String> {
    paths.retain(|path| TraceDecay::is_initialized(Path::new(path)));
    paths
}

fn cwd_match_resolution(
    paths: &[String],
    cwd: &Path,
    match_kind: CwdProjectMatch,
) -> Option<ServeGlobalDbResolution> {
    let mut matches: Vec<_> = paths
        .iter()
        .filter_map(|p| {
            let project_path = Path::new(p).canonicalize().ok()?;
            match_kind
                .matches(&project_path, cwd)
                .then(|| (project_path.components().count(), p.clone()))
        })
        .collect();
    match_kind.sort_matches(&mut matches);

    let (depth, _) = matches.first()?;
    if matches.get(1).is_some_and(|next| next.0 == *depth) {
        return Some(ServeGlobalDbResolution::Ambiguous(
            matches.into_iter().map(|(_, p)| p).collect(),
        ));
    }
    Some(ServeGlobalDbResolution::Found(PathBuf::from(
        matches.remove(0).1,
    )))
}

/// Fallback for `serve`: when CWD-based discovery fails, check the global DB
/// for registered projects. When multiple projects exist, pick the best match
/// against cwd: prefer a project that is an ancestor of cwd (cwd is inside the
/// project), then a project that is a descendant of cwd (project is under cwd).
/// Ties at the winning depth are ambiguous and require an explicit path.
pub async fn resolve_serve_from_global_db() -> ServeGlobalDbResolution {
    let Some(gdb) = GlobalDb::open().await else {
        return ServeGlobalDbResolution::None;
    };
    let mut paths = initialized_project_paths(gdb.list_project_paths().await);
    paths.sort();
    if paths.len() == 1 {
        return ServeGlobalDbResolution::Found(PathBuf::from(paths.remove(0)));
    }
    if paths.is_empty() {
        return ServeGlobalDbResolution::None;
    }

    // Multiple projects — try to resolve using cwd.
    let Ok(cwd) = std::env::current_dir() else {
        return ServeGlobalDbResolution::Ambiguous(paths);
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    // Priority 1: cwd is inside a project (project is ancestor of cwd).
    // Pick the deepest ancestor (most specific match).
    if let Some(resolution) =
        cwd_match_resolution(&paths, &cwd, CwdProjectMatch::ProjectContainsCwd)
    {
        return resolution;
    }

    // Priority 2: a project is under cwd (cwd is ancestor of project).
    // Pick the shallowest descendant (closest child).
    if let Some(resolution) = cwd_match_resolution(&paths, &cwd, CwdProjectMatch::ProjectUnderCwd) {
        return resolution;
    }

    // No cwd-based match — the global DB alone cannot disambiguate.
    ServeGlobalDbResolution::Ambiguous(paths)
}

/// Last-resort fallback for `serve`: peek at the first stdin line to read the
/// MCP `initialize` request's `roots` array.  If a root matches a registered
/// project, return its path.  The raw line is stored in `out` so the caller
/// can replay it into the MCP transport (the server still needs to see it).
pub async fn resolve_serve_from_mcp_roots(out: &mut Option<String>) -> Option<std::path::PathBuf> {
    let line = read_first_non_empty_stdin_line().await?;
    *out = Some(line.trim().to_string());

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let roots = parsed.pointer("/params/roots").and_then(|v| v.as_array())?;

    let registered = match GlobalDb::open().await {
        Some(gdb) => initialized_project_paths(gdb.list_project_paths().await),
        None => Vec::new(),
    };

    // Try each root URI — first match wins.
    for root in roots {
        let uri = root.get("uri").and_then(|v| v.as_str()).unwrap_or_default();
        let Some(root_path) = local_path_from_mcp_root_uri(uri) else {
            continue;
        };
        // Exact match: the root IS a registered project.
        if let Some(hit) = registered
            .iter()
            .find(|p| std::path::Path::new(p) == root_path.as_path())
        {
            return Some(std::path::PathBuf::from(hit));
        }
        // Walk up from the root to find the nearest enclosing project.
        if let Some(discovered) = crate::config::discover_project_root(&root_path) {
            return Some(discovered);
        }
    }
    None
}

async fn read_first_non_empty_stdin_line() -> Option<String> {
    use tokio::io::AsyncReadExt;

    let mut stdin = tokio::io::stdin();
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    // Avoid buffering past the first line; the normal stdio transport must still
    // receive any later JSON-RPC messages from stdin.
    loop {
        match stdin.read(&mut byte).await {
            Ok(0) if line.is_empty() => return None,
            Ok(0) => {
                let text = String::from_utf8(line).ok()?;
                return (!text.trim().is_empty()).then_some(text);
            }
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    let text = String::from_utf8(std::mem::take(&mut line)).ok()?;
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
            Err(_) => return None,
        }
    }
}

fn local_path_from_mcp_root_uri(uri: &str) -> Option<std::path::PathBuf> {
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
    percent_decode_path(&path)
        .map(strip_windows_drive_slash)
        .map(std::path::PathBuf::from)
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

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1)?;
            let lo = *bytes.get(i + 2)?;
            decoded.push((hex_value(hi)? << 4) | hex_value(lo)?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Degraded MCP serving (startup project-resolution failures)
// ---------------------------------------------------------------------------

/// Marker line written to stderr when serve enters degraded mode. Grepped by
/// `tracedecay doctor --agent cursor` from Cursor's MCP logs, so keep the
/// wording stable.
pub const DEGRADED_SERVE_STDERR_MARKER: &str =
    "[tracedecay] serve: staying alive in degraded MCP mode";

/// How a degraded serving session ended.
pub enum DegradedServeOutcome {
    /// stdin closed (client disconnected) while still degraded.
    Closed,
    /// Project resolution started succeeding mid-session (e.g. the user ran
    /// `tracedecay init`). The pending request line is the `tools/call` that
    /// triggered the successful retry; the caller must replay it into the
    /// recovered full server. Boxed to keep the enum small next to `Closed`.
    Recovered {
        cg: Box<TraceDecay>,
        pending_line: String,
    },
}

/// Runs a minimal MCP server after startup project resolution failed.
///
/// MCP hosts treat a dead server process as a permanently failed scope:
/// Cursor in particular never retries a failed spawn, so one startup exit
/// (bad `--path`, uninitialized project, ambiguous global fallback) turns
/// every later tool call in that session into "Timed out waiting for
/// connection" until the user toggles the server or reloads the window.
/// Instead of exiting, this loop completes the MCP handshake, lists the real
/// tools, and answers every `tools/call` with an actionable error that names
/// the failure, the fix, and the `tracedecay tool …` CLI fallback.
///
/// Each `tools/call` first retries resolution of `project_path`; when it
/// starts succeeding the loop hands control back so the caller can serve the
/// project for real — no toggle or window reload needed after `tracedecay
/// init`.
pub async fn run_degraded_mcp_server(
    transport: &mut impl McpTransport,
    project_path: &Path,
    startup_error: &TraceDecayError,
) -> Result<DegradedServeOutcome> {
    let notice = degraded_serve_notice(project_path, startup_error);
    loop {
        let line = match transport.read_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(DegradedServeOutcome::Closed),
            Err(e) => return Err(e.into()),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_tool_call_line(trimmed) {
            if let Ok(cg) = ensure_initialized(project_path).await {
                eprintln!(
                    "[tracedecay] serve: project resolution recovered for '{}'; leaving degraded MCP mode",
                    project_path.display()
                );
                return Ok(DegradedServeOutcome::Recovered {
                    cg: Box::new(cg),
                    pending_line: trimmed.to_string(),
                });
            }
        }
        let Some(response) = degraded_response_for_line(trimmed, &notice) else {
            continue;
        };
        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        transport.write_line(&out).await?;
        transport.flush().await?;
    }
}

fn is_tool_call_line(line: &str) -> bool {
    serde_json::from_str::<JsonRpcRequest>(line).is_ok_and(|request| request.method == "tools/call")
}

/// Builds the actionable error text returned from every degraded `tools/call`.
pub fn degraded_serve_notice(project_path: &Path, startup_error: &TraceDecayError) -> String {
    format!(
        "TraceDecay MCP server is running in degraded mode: project resolution failed at startup \
         for '{path}'.\n\
         \n\
         Error: {startup_error}\n\
         \n\
         To fix:\n\
         1. Run `tracedecay init` inside the project (or point the MCP server at an initialized \
         project via `tracedecay serve --path <project>`).\n\
         2. Retry the tool call — this server rechecks the project on every call and recovers \
         automatically once resolution succeeds.\n\
         3. If the MCP client shows connection errors instead of this message, toggle the \
         tracedecay MCP server in Cursor Settings → MCP or reload the window; Cursor does not \
         retry a failed MCP server on its own.\n\
         \n\
         Diagnose with `tracedecay doctor --agent cursor`. Every tool is also available from \
         the shell: `tracedecay tool <name> --key value` (run `tracedecay tool` to list tools) \
         from inside an initialized project.",
        path = project_path.display(),
    )
}

fn degraded_response_for_line(line: &str, notice: &str) -> Option<JsonRpcResponse> {
    let request: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => {
            return Some(JsonRpcResponse::error(
                serde_json::Value::Null,
                ErrorCode::ParseError,
                format!("failed to parse JSON-RPC request: {e}"),
            ));
        }
    };
    let id = request.id?;
    let response = match request.method.as_str() {
        "initialize" => JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {}, "resources": {}, "logging": {} },
                "serverInfo": {
                    "name": "tracedecay",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "instructions": notice,
            }),
        ),
        // Compatibility no-ops: some clients send these with an id.
        "initialized" | "notifications/initialized" => return None,
        "tools/list" => JsonRpcResponse::success(
            id,
            json!({ "tools": crate::mcp::tools::get_tool_definitions() }),
        ),
        "tools/call" => JsonRpcResponse::success(
            id,
            json!({
                "content": [{ "type": "text", "text": notice }],
                "isError": true,
            }),
        ),
        "resources/list" => JsonRpcResponse::success(id, json!({ "resources": [] })),
        "resources/read" => {
            JsonRpcResponse::error(id, ErrorCode::InternalError, notice.to_string())
        }
        "ping" | "logging/setLevel" => JsonRpcResponse::success(id, json!({})),
        other => JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("method not found: {other}"),
        ),
    };
    Some(response)
}

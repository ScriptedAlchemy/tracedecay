use std::io::Write;
use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::mcp::transport::{McpTransport, ReplayTransport, StdioTransport};
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

/// Returns the first plausible unexpanded `${...}` template variable in a
/// `--path` argument (e.g. `${workspaceFolder}`), or `None` when the value
/// contains no template syntax. The brace contents must look like a variable
/// name — a leading ASCII letter followed by word/`.`/`-` characters,
/// optionally with a `:`-introduced modifier such as a default value — so
/// degenerate forms (`${}`, `${ }`, `${a/b}`) and directories that merely
/// contain `$` are not misclassified. A matching value is overwhelmingly more
/// likely to be an unexpanded host template than a real directory name, so
/// callers treat it as "no path given".
pub fn unexpanded_template_variable(path: &str) -> Option<&str> {
    let mut search_from = 0;
    while let Some(offset) = path[search_from..].find("${") {
        let start = search_from + offset;
        let inner_start = start + 2;
        let end = inner_start + path[inner_start..].find('}')?;
        if plausible_template_contents(&path[inner_start..end]) {
            return Some(&path[start..=end]);
        }
        search_from = inner_start;
    }
    None
}

fn plausible_template_contents(contents: &str) -> bool {
    // Variable name, optionally followed by a `:`-introduced modifier
    // (e.g. `${workspaceFolder:-/tmp/fallback}`); only the name is validated.
    let name = contents.split(':').next().unwrap_or_default();
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Filters a `serve --path` CLI argument that the MCP host failed to expand.
///
/// Some hosts pass config template variables like `${workspaceFolder}`
/// through literally instead of expanding them (Cursor's headless
/// agent-session MCP scopes do this; see `cursor-plugin/README.md`). Such a
/// value is discarded with a stderr warning so `serve` can fall back to its
/// no-path discovery chain where possible; callers must use
/// [`ServeGlobalDbMatch::UniqueOnly`] for the global-registry step in that
/// mode because the host's spawn directory says nothing about the intended
/// workspace.
pub fn sanitize_serve_path_arg(path: Option<String>) -> Option<String> {
    let raw = path?;
    let Some(variable) = unexpanded_template_variable(&raw) else {
        return Some(raw);
    };
    // The host may have spawned us with stderr closed; a failed diagnostic
    // write must not take the server down.
    let _ = writeln!(
        std::io::stderr(),
        "warning: --path '{raw}' contains the unexpanded template variable '{variable}' \
         (the MCP host did not expand it); ignoring --path and falling back to project discovery"
    );
    None
}

/// Reports which project the discarded-template fallback settled on and why,
/// so a wrong pick is diagnosable from the host's MCP logs.
pub fn log_serve_project_choice(project: &Path, why: &str) {
    let _ = writeln!(
        std::io::stderr(),
        "tracedecay serve: using project '{}' ({why})",
        project.display()
    );
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

/// How [`resolve_serve_from_global_db`] may pick among multiple registered
/// projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeGlobalDbMatch {
    /// Disambiguate via the cwd heuristic (ancestor/descendant depth ranking).
    /// Appropriate when the user genuinely ran `serve` without a path: cwd is
    /// where they invoked it, so proximity is meaningful.
    CwdHeuristic,
    /// Require exactly one registered project; report every multi-project
    /// registry as ambiguous. Used when an explicit `--path` was discarded as
    /// an unexpanded host template: the host's spawn directory (typically the
    /// user home) says nothing about the intended workspace, so a silent
    /// depth-ranked pick risks serving the wrong project's index.
    UniqueOnly,
}

/// Fallback for `serve`: when CWD-based discovery fails, check the global DB
/// for registered projects. With [`ServeGlobalDbMatch::CwdHeuristic`],
/// multiple projects are ranked against cwd: prefer a project that is an
/// ancestor of cwd (cwd is inside the project), then a project that is a
/// descendant of cwd (project is under cwd); ties at the winning depth are
/// ambiguous and require an explicit path. With
/// [`ServeGlobalDbMatch::UniqueOnly`], any multi-project registry is
/// ambiguous.
pub async fn resolve_serve_from_global_db(
    match_mode: ServeGlobalDbMatch,
) -> ServeGlobalDbResolution {
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
    match match_mode {
        ServeGlobalDbMatch::UniqueOnly => {
            return ServeGlobalDbResolution::Ambiguous(paths);
        }
        ServeGlobalDbMatch::CwdHeuristic => {}
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

/// Peeks at the first stdin line to read the MCP `initialize` request's
/// `roots` array, returning the local workspace root paths. The raw line is
/// stored in `out` so the caller can replay it into the MCP transport (the
/// server still needs to see it).
async fn read_initialize_roots(out: &mut Option<String>) -> Vec<std::path::PathBuf> {
    let Some(line) = read_first_non_empty_stdin_line().await else {
        return Vec::new();
    };
    *out = Some(line.trim().to_string());

    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return Vec::new();
    };
    let Some(roots) = parsed.pointer("/params/roots").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    roots
        .iter()
        .filter_map(|root| {
            let uri = root.get("uri").and_then(|v| v.as_str())?;
            local_path_from_mcp_root_uri(uri)
        })
        .collect()
}

/// Resolves a project from MCP `initialize` workspace roots: a root that IS a
/// registered project wins, otherwise the nearest enclosing project of any
/// root. First match wins.
async fn resolve_project_from_roots(roots: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    if roots.is_empty() {
        return None;
    }
    let registered = match GlobalDb::open().await {
        Some(gdb) => initialized_project_paths(gdb.list_project_paths().await),
        None => Vec::new(),
    };
    for root_path in roots {
        if let Some(hit) = registered
            .iter()
            .find(|p| std::path::Path::new(p) == root_path.as_path())
        {
            return Some(std::path::PathBuf::from(hit));
        }
        if let Some(discovered) = crate::config::discover_project_root(root_path) {
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
// Serve startup orchestration
// ---------------------------------------------------------------------------

/// Runs the `serve` command end to end: resolve the project (degrading
/// instead of exiting when resolution fails — see
/// [`ServeProjectResolver::resolve_once`] and [`run_degraded_mcp_server`]),
/// then serve MCP over stdio, proxying to the daemon when one owns the
/// socket.
pub async fn run_serve(path_arg: Option<String>, timings: bool) -> Result<()> {
    let original_cwd = std::env::current_dir().ok();
    let (resolver, error, peeked_line) = match resolve_serve_startup(path_arg).await {
        ServeStartup::Ready { cg, peeked_line } => {
            return Box::pin(serve_resolved_project(
                *cg,
                original_cwd,
                timings,
                peeked_line,
            ))
            .await;
        }
        ServeStartup::Degraded {
            resolver,
            error,
            peeked_line,
        } => (resolver, error, peeked_line),
    };

    // Do NOT exit: MCP hosts (Cursor especially) treat a dead server process
    // as a permanently failed scope and never retry it, so a startup exit
    // over a recoverable config problem breaks every later tool call in the
    // session. Serve a degraded MCP surface instead; it recovers on its own
    // once resolution starts succeeding.
    let mut transport = ReplayTransport::new(StdioTransport::new());
    if let Some(line) = peeked_line {
        transport.push_replay(line);
    }
    match run_degraded_mcp_server(&mut transport, &resolver, &error).await? {
        DegradedServeOutcome::Closed => Ok(()),
        DegradedServeOutcome::Recovered { cg, pending_line } => {
            // Keep serving on the SAME transport: requests pipelined behind
            // the recovery-triggering call may already sit in its read
            // buffer, and a raw-stdin handoff (fresh transport or daemon
            // proxy) would silently drop them. The recovery-triggering
            // tools/call itself is replayed first.
            transport.push_replay(pending_line);
            let scope_prefix = serve_scope_prefix(original_cwd.as_deref(), cg.project_root());
            let server = crate::mcp::McpServer::new(*cg, scope_prefix).await;
            server.run(&mut transport).await
        }
    }
}

/// Serves a startup-resolved project: proxy to the daemon when one owns the
/// socket, otherwise run the in-process MCP engine.
async fn serve_resolved_project(
    cg: TraceDecay,
    original_cwd: Option<std::path::PathBuf>,
    timings: bool,
    peeked_line: Option<String>,
) -> Result<()> {
    let scope_prefix = serve_scope_prefix(original_cwd.as_deref(), cg.project_root());
    let handshake = crate::daemon::DaemonHandshake::for_current_client(
        Some(cg.project_root().to_path_buf()),
        scope_prefix,
        timings,
        false,
    )?;
    let socket_path = crate::daemon::default_socket_path()?;
    if crate::daemon::should_proxy_serve_to_daemon(&socket_path).await {
        crate::daemon::proxy_stdio_to_daemon(&socket_path, &handshake, peeked_line).await
    } else {
        let server = crate::mcp::McpServer::new(cg, handshake.scope_prefix.clone()).await;
        let mut transport = ReplayTransport::new(StdioTransport::new());
        if let Some(line) = peeked_line {
            transport.push_replay(line);
        }
        server.run(&mut transport).await
    }
}

/// The scope prefix for a serve session: the relative path from the project
/// root to the directory serve was launched from, when the latter is inside
/// the project.
fn serve_scope_prefix(original_cwd: Option<&Path>, project_root: &Path) -> Option<String> {
    original_cwd.and_then(|cwd| {
        cwd.strip_prefix(project_root)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| rel.to_string_lossy().into_owned())
    })
}

// ---------------------------------------------------------------------------
// Startup project resolution
// ---------------------------------------------------------------------------

/// The outcome of startup project resolution for `serve`.
pub enum ServeStartup {
    /// A project resolved; serve it normally. `peeked_line` is a stdin line
    /// (the MCP `initialize` request) consumed while peeking workspace roots;
    /// it must be replayed to whatever serves the session.
    Ready {
        cg: Box<TraceDecay>,
        peeked_line: Option<String>,
    },
    /// Resolution failed with a recoverable config problem. The resolver
    /// retries the same resolution ladder on every degraded tool call.
    Degraded {
        resolver: ServeProjectResolver,
        error: TraceDecayError,
        peeked_line: Option<String>,
    },
}

/// Resolves the project to serve, capturing everything needed to retry the
/// exact same resolution later from degraded mode.
pub async fn resolve_serve_startup(path_arg: Option<String>) -> ServeStartup {
    // Some MCP hosts (e.g. Cursor headless agent sessions) pass config
    // template variables like `${workspaceFolder}` through literally; treat
    // such values as "no --path" and use discovery instead.
    let had_path_arg = path_arg.is_some();
    let path = sanitize_serve_path_arg(path_arg);
    let explicit_path = path.is_some();
    let path_was_template = had_path_arg && !explicit_path;
    let mut resolver = ServeProjectResolver {
        project_path: crate::config::resolve_path_with_discovery(path),
        explicit_path,
        path_was_template,
        // The host's spawn directory says nothing about the intended
        // workspace when it failed to expand a template path, so the
        // global-registry step must not depth-rank against cwd in that mode.
        global_db_match: if path_was_template {
            ServeGlobalDbMatch::UniqueOnly
        } else {
            ServeGlobalDbMatch::CwdHeuristic
        },
        initialize_roots: Vec::new(),
    };

    let mut peeked_line: Option<String> = None;
    let first_error = match ensure_initialized(&resolver.project_path).await {
        Ok(cg) => {
            resolver
                .log_choice_if_template(cg.project_root(), "discovered from the working directory");
            return ServeStartup::Ready {
                cg: Box::new(cg),
                peeked_line,
            };
        }
        Err(e) => e,
    };
    if resolver.explicit_path {
        // An explicit path is authoritative: no discovery fallbacks, and the
        // degraded retry only ever rechecks this path.
        return ServeStartup::Degraded {
            resolver,
            error: first_error,
            peeked_line,
        };
    }

    // CWD-based discovery failed (e.g. an MCP host launched us from ~). Peek
    // the MCP `initialize` request's workspace roots; the resolver remembers
    // them so degraded retries can keep consulting them.
    resolver.initialize_roots = read_initialize_roots(&mut peeked_line).await;
    match resolver.resolve_once().await {
        Ok(cg) => ServeStartup::Ready {
            cg: Box::new(cg),
            peeked_line,
        },
        Err(error) => ServeStartup::Degraded {
            resolver,
            error,
            peeked_line,
        },
    }
}

/// Everything needed to attempt `serve` project resolution — at startup and
/// again on every degraded tool call. One resolution ladder serves both, so
/// degraded mode recovers through exactly the fallbacks startup would have
/// used (cwd discovery, remembered MCP initialize roots, global registry).
pub struct ServeProjectResolver {
    /// The explicit `--path`, or the startup cwd-discovery result.
    project_path: std::path::PathBuf,
    /// A real (non-template) `--path` was given; it is authoritative.
    explicit_path: bool,
    /// The `--path` was a discarded unexpanded host template.
    path_was_template: bool,
    global_db_match: ServeGlobalDbMatch,
    /// Workspace roots from the peeked MCP `initialize` request.
    initialize_roots: Vec<std::path::PathBuf>,
}

impl ServeProjectResolver {
    /// The path named in degraded-mode error messages.
    pub(crate) fn project_path(&self) -> &Path {
        &self.project_path
    }

    /// Runs the full resolution ladder once. Explicit paths never fall
    /// back; discovery mode tries, in order: the startup path, a fresh cwd
    /// walk-up (an intervening `tracedecay init` can create an enclosing
    /// project), the remembered initialize roots, then the global registry.
    async fn resolve_once(&self) -> Result<TraceDecay> {
        let first_error = match ensure_initialized(&self.project_path).await {
            Ok(cg) => {
                self.log_choice_if_template(
                    cg.project_root(),
                    "discovered from the working directory",
                );
                return Ok(cg);
            }
            Err(e) => e,
        };
        if self.explicit_path {
            return Err(first_error);
        }

        let discovered = crate::config::resolve_path_with_discovery(None);
        if discovered != self.project_path {
            if let Ok(cg) = ensure_initialized(&discovered).await {
                self.log_choice_if_template(
                    cg.project_root(),
                    "discovered from the working directory",
                );
                return Ok(cg);
            }
        }

        if let Some(p) = resolve_project_from_roots(&self.initialize_roots).await {
            self.log_choice_if_template(&p, "matched an MCP initialize root");
            return ensure_initialized(&p).await;
        }

        match resolve_serve_from_global_db(self.global_db_match).await {
            ServeGlobalDbResolution::Found(p) => {
                self.log_choice_if_template(&p, "resolved from the global project registry");
                ensure_initialized(&p).await
            }
            ServeGlobalDbResolution::Ambiguous(paths) => Err(TraceDecayError::Config {
                message: global_db_ambiguity_message(&paths),
            }),
            ServeGlobalDbResolution::None => Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay index found at '{}' and no projects registered in the global database — run 'tracedecay init' in your project first",
                    self.project_path.display()
                ),
            }),
        }
    }

    fn log_choice_if_template(&self, project: &Path, why: &str) {
        if self.path_was_template {
            log_serve_project_choice(project, why);
        }
    }
}

// ---------------------------------------------------------------------------
// Degraded MCP serving (startup project-resolution failures)
// ---------------------------------------------------------------------------

/// Marker line written to stderr when serve enters degraded mode. Grepped by
/// `tracedecay doctor --agent cursor` from Cursor's MCP logs
/// (`crate::agents::cursor_diagnostics`), so keep the wording stable.
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
/// the failure, the fix, and the `tracedecay tool …` CLI fallback (protocol
/// responses shared with the full server via [`crate::mcp::degraded`]).
///
/// Each `tools/call` first re-runs the startup resolution ladder; when it
/// starts succeeding the loop hands control back so the caller can serve the
/// project for real — no toggle or window reload needed after `tracedecay
/// init`.
pub async fn run_degraded_mcp_server(
    transport: &mut impl McpTransport,
    resolver: &ServeProjectResolver,
    startup_error: &TraceDecayError,
) -> Result<DegradedServeOutcome> {
    eprintln!("Error: {startup_error}");
    eprintln!(
        "{DEGRADED_SERVE_STDERR_MARKER} — MCP handshake will complete and tool calls will \
         return this error until the project resolves"
    );
    let notice = degraded_serve_notice(resolver.project_path(), startup_error);
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
        if crate::mcp::degraded::is_tools_call_line(trimmed) {
            if let Ok(cg) = resolver.resolve_once().await {
                eprintln!(
                    "[tracedecay] serve: project resolution recovered for '{}'; leaving degraded MCP mode",
                    cg.project_root().display()
                );
                return Ok(DegradedServeOutcome::Recovered {
                    cg: Box::new(cg),
                    pending_line: trimmed.to_string(),
                });
            }
        }
        let Some(response) = crate::mcp::degraded::degraded_response_for_line(trimmed, &notice)
        else {
            continue;
        };
        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        transport.write_line(&out).await?;
        transport.flush().await?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_literal_workspace_folder_variable() {
        assert_eq!(
            unexpanded_template_variable("${workspaceFolder}"),
            Some("${workspaceFolder}")
        );
    }

    #[test]
    fn detects_template_variable_with_default_value_syntax() {
        assert_eq!(
            unexpanded_template_variable("${workspaceFolder:-/tmp/fallback}"),
            Some("${workspaceFolder:-/tmp/fallback}")
        );
    }

    #[test]
    fn detects_other_host_template_variables() {
        assert_eq!(
            unexpanded_template_variable("${workspaceRoot}"),
            Some("${workspaceRoot}")
        );
        assert_eq!(
            unexpanded_template_variable("${userHome}"),
            Some("${userHome}")
        );
    }

    #[test]
    fn detects_template_variable_embedded_in_a_longer_path() {
        assert_eq!(
            unexpanded_template_variable("${workspaceFolder}/packages/core"),
            Some("${workspaceFolder}")
        );
        assert_eq!(
            unexpanded_template_variable("/home/user/${workspaceFolderBasename}/src"),
            Some("${workspaceFolderBasename}")
        );
    }

    #[test]
    fn plain_paths_are_not_templates() {
        assert_eq!(unexpanded_template_variable("/home/user/project"), None);
        assert_eq!(unexpanded_template_variable("relative/dir"), None);
        assert_eq!(unexpanded_template_variable(""), None);
    }

    #[test]
    fn dollar_signs_without_brace_syntax_are_not_templates() {
        // Real directories can contain `$` — only `${...}` is template syntax.
        assert_eq!(unexpanded_template_variable("/tmp/pri$ce/data"), None);
        assert_eq!(unexpanded_template_variable("$workspaceFolder"), None);
        assert_eq!(unexpanded_template_variable("/tmp/{braces}/x"), None);
        assert_eq!(unexpanded_template_variable("/tmp/trailing$"), None);
    }

    #[test]
    fn unterminated_template_syntax_is_not_a_template() {
        assert_eq!(unexpanded_template_variable("/tmp/${unclosed"), None);
    }

    #[test]
    fn degenerate_brace_forms_are_not_templates() {
        // Empty / whitespace / path-like brace contents are not plausible
        // variable names, so a directory literally named that way stays a
        // real path.
        assert_eq!(unexpanded_template_variable("${}"), None);
        assert_eq!(unexpanded_template_variable("${ }"), None);
        assert_eq!(unexpanded_template_variable("/tmp/${a/b}/x"), None);
        assert_eq!(unexpanded_template_variable("${1invalid}"), None);
        assert_eq!(unexpanded_template_variable("${_underscore}"), None);
    }

    #[test]
    fn later_plausible_template_wins_over_earlier_degenerate_braces() {
        assert_eq!(
            unexpanded_template_variable("/tmp/${ }/x/${workspaceFolder}"),
            Some("${workspaceFolder}")
        );
    }

    #[test]
    fn sanitize_keeps_plain_paths_and_none() {
        assert_eq!(
            sanitize_serve_path_arg(Some("/home/user/project".to_string())),
            Some("/home/user/project".to_string())
        );
        assert_eq!(
            sanitize_serve_path_arg(Some("/tmp/pri$ce".to_string())),
            Some("/tmp/pri$ce".to_string())
        );
        assert_eq!(sanitize_serve_path_arg(None), None);
    }

    #[test]
    fn sanitize_discards_unexpanded_template_paths() {
        assert_eq!(
            sanitize_serve_path_arg(Some("${workspaceFolder}".to_string())),
            None
        );
        assert_eq!(
            sanitize_serve_path_arg(Some("${workspaceFolder}/nested".to_string())),
            None
        );
    }
}

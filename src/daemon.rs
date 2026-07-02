#[cfg(unix)]
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tokio::task::JoinHandle;
#[cfg(unix)]
use tokio::time::{timeout, Duration};

use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};
#[cfg(unix)]
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport, StdioTransport};

pub const SERVICE_NAME: &str = "tracedecay.service";
pub const SOCKET_ENV: &str = "TRACEDECAY_DAEMON_SOCKET";
pub const HOOK_EVENT_METHOD: &str = "tracedecay/hookEvent";
#[cfg(unix)]
const HOOK_EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);

mod service;
pub use service::{
    daemon_reachable, default_socket_path, install_service, installed_service_socket_path,
    refresh_installed_service, refresh_service, service_spec, service_status,
    socket_path_or_default, uninstall_service, DaemonServiceSpec,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHookEvent {
    pub agent: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rel_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

impl DaemonHookEvent {
    fn new(
        agent: &'static str,
        event: &'static str,
        rel_paths: Vec<String>,
        command: Option<String>,
        cwd: Option<PathBuf>,
    ) -> Self {
        Self {
            agent: agent.to_string(),
            event: event.to_string(),
            rel_paths,
            command,
            cwd,
        }
    }

    pub fn cursor_after_file_edit(rel_paths: Vec<String>) -> Self {
        Self::new("cursor", "afterFileEdit", rel_paths, None, None)
    }

    pub fn cursor_after_shell_execution(command: String, cwd: PathBuf) -> Self {
        Self::new(
            "cursor",
            "afterShellExecution",
            Vec::new(),
            Some(command),
            Some(cwd),
        )
    }

    pub fn cursor_workspace_open(cwd: PathBuf) -> Self {
        Self::new("cursor", "workspaceOpen", Vec::new(), None, Some(cwd))
    }

    pub fn codex_post_tool_use_edit(rel_paths: Vec<String>, cwd: PathBuf) -> Self {
        Self::new("codex", "postToolUseEdit", rel_paths, None, Some(cwd))
    }

    pub fn codex_post_tool_use_shell(command: String, cwd: PathBuf) -> Self {
        Self::new(
            "codex",
            "postToolUseShell",
            Vec::new(),
            Some(command),
            Some(cwd),
        )
    }

    pub fn kiro_post_tool_use(rel_paths: Vec<String>, cwd: Option<PathBuf>) -> Self {
        Self::new("kiro", "postToolUse", rel_paths, None, cwd)
    }
}

/// Per-connection metadata sent before JSON-RPC traffic.
///
/// The daemon process is shared. This handshake tells that shared process which
/// project, scope, timing preference, and client profile should apply to this
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHandshake {
    pub project_path: Option<PathBuf>,
    pub scope_prefix: Option<String>,
    pub timings: bool,
    pub allow_init: bool,
    pub client_identity: DaemonClientIdentity,
    /// Version of the tracedecay binary that opened this connection.
    ///
    /// `#[serde(default)]` keeps mixed-version pairs interoperable: a new
    /// daemon reads handshakes from old clients (missing field → empty), and
    /// old daemons ignore the extra field. The daemon uses it to detect and
    /// log version skew, e.g. a stale daemon still serving after
    /// `tracedecay update` replaced the binary.
    #[serde(default)]
    pub client_version: String,
}

impl DaemonHandshake {
    pub fn for_current_client(
        project_path: Option<PathBuf>,
        scope_prefix: Option<String>,
        timings: bool,
        allow_init: bool,
    ) -> Result<Self> {
        Ok(Self {
            project_path,
            scope_prefix,
            timings,
            allow_init,
            client_identity: DaemonClientIdentity::current()?,
            client_version: binary_version().to_string(),
        })
    }

    fn open_options(&self) -> crate::tracedecay::TraceDecayOpenOptions {
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(self.client_identity.profile_root.clone()),
            global_db_path: Some(self.client_identity.global_db_path.clone()),
        }
    }

    pub fn to_line(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_line(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line.trim())?)
    }
}

/// Version of this tracedecay binary, advertised in daemon handshakes and
/// compared against peers to detect stale daemons after `tracedecay update`.
fn binary_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The client version to report as skewed, or `None` when the versions match.
///
/// Old clients send no version (empty string); that is indistinguishable from
/// "same version before this field existed", so it never counts as skew.
#[cfg(unix)]
fn client_version_skew(client_version: &str, daemon_version: &str) -> Option<String> {
    if client_version.is_empty() || client_version == daemon_version {
        return None;
    }
    Some(client_version.to_string())
}

#[cfg(unix)]
pub async fn notify_hook_event(project_path: &Path, event: DaemonHookEvent) {
    let _ = timeout(
        HOOK_EVENT_NOTIFY_TIMEOUT,
        notify_hook_event_inner(project_path, event),
    )
    .await;
}

#[cfg(unix)]
async fn notify_hook_event_inner(project_path: &Path, event: DaemonHookEvent) {
    let Ok(socket_path) = default_socket_path() else {
        return;
    };
    if !socket_path.exists() {
        return;
    }
    let Ok(handshake) =
        DaemonHandshake::for_current_client(Some(project_path.to_path_buf()), None, false, false)
    else {
        return;
    };
    let Ok(params) = serde_json::to_value(event) else {
        return;
    };
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: HOOK_EVENT_METHOD.to_string(),
        params: Some(params),
    };
    let Ok(line) = serde_json::to_string(&request) else {
        return;
    };
    let Ok(stream) = UnixStream::connect(socket_path).await else {
        return;
    };
    let Ok(handshake_line) = handshake.to_line() else {
        return;
    };
    let (_reader, mut writer) = stream.into_split();
    if writer.write_all(handshake_line.as_bytes()).await.is_err() {
        return;
    }
    if writer.write_all(b"\n").await.is_err() {
        return;
    }
    if writer.write_all(line.as_bytes()).await.is_err() {
        return;
    }
    if writer.write_all(b"\n").await.is_err() {
        return;
    }
    let _ = writer.flush().await;
    let _ = writer.shutdown().await;
}

#[cfg(not(unix))]
pub async fn notify_hook_event(project_path: &Path, event: DaemonHookEvent) {
    if !crate::tracedecay::TraceDecay::has_initialized_store(project_path).await {
        return;
    }
    match event.event.as_str() {
        "afterFileEdit" | "postToolUseEdit" => {
            let rel_paths = safe_daemon_hook_rel_paths(&event.rel_paths);
            if rel_paths.is_empty() {
                return;
            }
            let Ok(cg) = crate::tracedecay::TraceDecay::open(project_path).await else {
                return;
            };
            let _ = cg.sync_if_stale_silent(&rel_paths).await;
        }
        "afterShellExecution" | "postToolUseShell" => {
            notify_shell_hook_event_without_daemon(project_path, event).await;
        }
        "workspaceOpen" => {
            if let Some(branch) = crate::branch::current_branch(project_path) {
                if matches!(
                    crate::tracedecay::TraceDecay::add_branch_tracking(project_path, &branch).await,
                    Ok(crate::branch::BranchAddOutcome::Added)
                ) {
                    return;
                }
            }
            run_debounced_hook_sync_without_daemon(project_path, hook_marker_file(&event.agent))
                .await;
        }
        "postToolUse" => {
            let rel_paths = safe_daemon_hook_rel_paths(&event.rel_paths);
            if !rel_paths.is_empty() {
                let Ok(cg) = crate::tracedecay::TraceDecay::open(project_path).await else {
                    return;
                };
                let _ = cg.sync_if_stale_silent(&rel_paths).await;
                return;
            }
            run_debounced_hook_sync_without_daemon(project_path, hook_marker_file(&event.agent))
                .await;
        }
        _ => {}
    }
}

#[cfg(not(unix))]
async fn notify_shell_hook_event_without_daemon(project_path: &Path, event: DaemonHookEvent) {
    let Some(command) = event.command.as_deref() else {
        return;
    };
    let cwd = event.cwd.as_deref().unwrap_or(project_path);
    if !crate::hooks::cursor_shell_command_targets_project(command, cwd, project_path) {
        return;
    }
    let current_branch = crate::branch::current_branch(project_path);
    match crate::hooks::cursor_shell_sync_plan_with_current_branch(
        command,
        current_branch.as_deref(),
    ) {
        crate::hooks::CursorShellSyncPlan::BranchAdd(branch) => {
            let _ = crate::tracedecay::TraceDecay::add_branch_tracking(project_path, &branch).await;
        }
        crate::hooks::CursorShellSyncPlan::WorktreeBranchAdd {
            branch,
            worktree_path,
        } => {
            let root = crate::hooks::resolve_worktree_add_root(command, cwd, &worktree_path);
            let _ = crate::tracedecay::TraceDecay::add_branch_tracking(&root, &branch).await;
        }
        crate::hooks::CursorShellSyncPlan::CurrentBranchSync(branch) => {
            if !matches!(
                crate::tracedecay::TraceDecay::add_branch_tracking(project_path, &branch).await,
                Ok(crate::branch::BranchAddOutcome::Added)
            ) {
                run_debounced_hook_sync_without_daemon(
                    project_path,
                    hook_marker_file(&event.agent),
                )
                .await;
            }
        }
        crate::hooks::CursorShellSyncPlan::IncrementalSync => {
            run_debounced_hook_sync_without_daemon(project_path, hook_marker_file(&event.agent))
                .await;
        }
        crate::hooks::CursorShellSyncPlan::Noop => {}
    }
}

#[cfg(not(unix))]
async fn run_debounced_hook_sync_without_daemon(project_path: &Path, marker_file: &str) {
    let Ok(cg) = crate::tracedecay::TraceDecay::open(project_path).await else {
        return;
    };
    let marker = cg.store_layout().data_root.join(marker_file);
    let now = crate::tracedecay::current_timestamp();
    if !crate::hooks::cursor_should_run_sync(now, read_hook_marker_secs(&marker), 3) {
        return;
    }
    match cg.sync().await {
        Ok(_) | Err(TraceDecayError::SyncLock { .. }) => {
            let _ = std::fs::write(marker, now.to_string());
        }
        Err(_) => {}
    }
}

#[cfg(not(unix))]
fn safe_daemon_hook_rel_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| {
            let path_ref = Path::new(path.as_str());
            !path.is_empty()
                && !path_ref.is_absolute()
                && path_ref
                    .components()
                    .all(|component| !matches!(component, std::path::Component::ParentDir))
        })
        .cloned()
        .collect()
}

#[cfg(not(unix))]
fn hook_marker_file(agent: &str) -> &'static str {
    match agent {
        "codex" => ".codex_shell_sync_at",
        "cursor" => ".cursor_shell_sync_at",
        "kiro" => ".kiro_post_tool_sync_at",
        _ => ".daemon_hook_shell_sync_at",
    }
}

#[cfg(not(unix))]
fn read_hook_marker_secs(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

fn format_daemon_log_line(event: &str, fields: &[(&str, String)]) -> String {
    let mut line = format!("[tracedecay] event={}", quote_log_value(event));
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&quote_log_value(value));
    }
    line
}

fn quote_log_value(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':'))
    {
        return value.to_string();
    }

    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(escaped, "\\u{{{:x}}}", ch as u32);
            }
            ch => escaped.push(ch),
        }
    }
    format!("\"{escaped}\"")
}

fn log_daemon_event(event: &str, fields: &[(&str, String)]) {
    eprintln!("{}", format_daemon_log_line(event, fields));
}

#[cfg(unix)]
fn scheduler_task_log_fields(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    outcome: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            crate::automation::backend::task_key(task).to_string(),
        ),
        ("outcome", outcome.to_string()),
    ]
}

#[cfg(unix)]
fn log_scheduler_task_start(project_path: &Path, task: crate::automation::backend::AgentTaskKind) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_task_log_fields(project_path, task, "start"),
    );
}

#[cfg(unix)]
fn scheduler_task_error_log_fields(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    error: &TraceDecayError,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            crate::automation::backend::task_key(task).to_string(),
        ),
        ("error", error.to_string()),
    ]
}

#[cfg(unix)]
fn log_scheduler_task_error(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    error: &TraceDecayError,
) {
    log_daemon_event(
        "scheduler_task_error",
        &scheduler_task_error_log_fields(project_path, task, error),
    );
}

#[cfg(unix)]
fn scheduler_record_log_fields(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) -> Vec<(&'static str, String)> {
    use crate::automation::run_ledger::AutomationRunStatus;

    let outcome = match record.status {
        AutomationRunStatus::Succeeded => "complete",
        AutomationRunStatus::Failed => "error",
        AutomationRunStatus::Skipped => "skipped",
        AutomationRunStatus::Queued => "queued",
        AutomationRunStatus::Running => "running",
    };
    let task = record
        .task_key
        .as_deref()
        .unwrap_or_else(|| crate::automation::backend::task_key(record.task))
        .to_string();
    let mut fields = vec![
        ("project", project_path.display().to_string()),
        ("task", task),
        ("outcome", outcome.to_string()),
        ("run_id", record.run_id.clone()),
    ];
    if let Some(reason) = record.fallback_status.as_ref().or(record.error.as_ref()) {
        fields.push(("reason", reason.clone()));
    }
    fields
}

#[cfg(all(unix, test))]
fn daemon_scheduler_record_log_line(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) -> String {
    format_daemon_log_line(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    )
}

#[cfg(unix)]
fn log_daemon_scheduler_record(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    );
}

pub fn unavailable_error(socket_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon socket '{}' is not available. Run `tracedecay daemon install-service` and ensure the service is running.",
            socket_path.display()
        ),
    }
}

fn default_available_socket_path() -> Result<PathBuf> {
    let socket_path = default_socket_path()?;
    if socket_path.exists() {
        Ok(socket_path)
    } else {
        Err(unavailable_error(&socket_path))
    }
}

/// How long daemon clients keep retrying a failed connect before giving up.
///
/// `tracedecay update` restarts the daemon service (`systemctl --user restart`);
/// between the old daemon unlinking its socket and the new one binding it,
/// connects fail with `NotFound` or `ConnectionRefused`. Long-lived MCP
/// sessions (Cursor's `tracedecay serve` stdio proxy) reconnect per request,
/// so retrying inside this window lets a live session ride out a self-update
/// instead of surfacing a hard JSON-RPC error.
#[cfg(unix)]
const DAEMON_RESTART_GRACE: Duration = Duration::from_secs(8);
#[cfg(unix)]
const DAEMON_RESTART_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[cfg(unix)]
fn is_transient_daemon_connect_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

#[cfg(unix)]
fn daemon_connect_error(socket_path: &Path, err: &std::io::Error) -> TraceDecayError {
    let hint = if is_transient_daemon_connect_error(err.kind()) {
        " The daemon may be restarting (e.g. after `tracedecay update`) — retry shortly, or check `tracedecay daemon status`."
    } else {
        ""
    };
    TraceDecayError::Config {
        message: format!(
            "could not connect to TraceDecay daemon socket '{}': {err}.{hint}",
            socket_path.display()
        ),
    }
}

/// Connects to the daemon socket, tolerating the restart outage caused by
/// `tracedecay update` (see [`DAEMON_RESTART_GRACE`]).
#[cfg(unix)]
async fn connect_to_daemon(socket_path: &Path) -> Result<UnixStream> {
    connect_with_restart_grace(
        socket_path,
        DAEMON_RESTART_GRACE,
        DAEMON_RESTART_POLL_INTERVAL,
    )
    .await
}

/// Connects to the daemon socket, tolerating a short restart outage.
///
/// Retrying here is safe: nothing has been written yet, so no request can be
/// duplicated. Non-transient errors (e.g. permission denied) fail immediately.
#[cfg(unix)]
async fn connect_with_restart_grace(
    socket_path: &Path,
    grace: Duration,
    poll_interval: Duration,
) -> Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                if !is_transient_daemon_connect_error(err.kind())
                    || tokio::time::Instant::now() >= deadline
                {
                    return Err(daemon_connect_error(socket_path, &err));
                }
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}

/// Decides at `tracedecay serve` startup whether to proxy to the daemon.
///
/// A missing socket usually means "no daemon", but `tracedecay update`
/// restarts the daemon service and shutdown unlinks the socket before the new
/// daemon rebinds it; a serve process starting inside that window would
/// otherwise silently commit to in-process mode for its whole lifetime. When
/// a daemon service is installed for this socket, wait out that window with
/// the same grace used for per-request connects before falling back.
#[cfg(unix)]
pub async fn should_proxy_serve_to_daemon(socket_path: &Path) -> bool {
    let installed_socket = installed_service_socket_path().ok().flatten();
    should_proxy_serve_to_daemon_with(
        socket_path,
        installed_socket.as_deref(),
        DAEMON_RESTART_GRACE,
        DAEMON_RESTART_POLL_INTERVAL,
    )
    .await
}

#[cfg(unix)]
async fn should_proxy_serve_to_daemon_with(
    socket_path: &Path,
    installed_service_socket: Option<&Path>,
    grace: Duration,
    poll_interval: Duration,
) -> bool {
    if socket_path.exists() {
        return true;
    }
    // Only wait when an installed service is expected to rebind this exact
    // socket; otherwise in-process startup must stay instant.
    if installed_service_socket != Some(socket_path) {
        return false;
    }
    connect_with_restart_grace(socket_path, grace, poll_interval)
        .await
        .is_ok()
}

/// Non-unix builds have no daemon; `proxy_stdio_to_daemon` would error anyway.
#[cfg(not(unix))]
pub async fn should_proxy_serve_to_daemon(socket_path: &Path) -> bool {
    socket_path.exists()
}

#[cfg(unix)]
pub async fn run_foreground(socket_path: PathBuf) -> Result<()> {
    run_foreground_unix(socket_path).await
}

#[cfg(not(unix))]
pub async fn run_foreground(_socket_path: PathBuf) -> Result<()> {
    Err(unsupported_platform())
}

#[cfg(unix)]
pub async fn proxy_stdio_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    let mut transport = StdioTransport::new();
    proxy_transport_to_daemon(socket_path, handshake, replay_line, &mut transport).await
}

#[cfg(unix)]
pub async fn proxy_transport_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
    transport: &mut impl McpTransport,
) -> Result<()> {
    if let Some(line) = replay_line {
        proxy_request_line_to_daemon(socket_path, handshake, &line, transport).await?;
    }

    while let Some(line) = transport.read_line().await? {
        proxy_request_line_to_daemon(socket_path, handshake, &line, transport).await?;
    }
    Ok(())
}

#[cfg(unix)]
async fn proxy_request_line_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
    transport: &mut impl McpTransport,
) -> Result<()> {
    if line.trim().is_empty() {
        return Ok(());
    }

    match send_daemon_request_line(socket_path, handshake, line).await {
        Ok(responses) => {
            if let Some(warning) = daemon_version_skew_warning(line, &responses, binary_version()) {
                eprintln!("[tracedecay] warning: {warning}");
            }
            for response in responses {
                transport.write_line(&response).await?;
                if !response.ends_with('\n') {
                    transport.write_line("\n").await?;
                }
            }
            transport.flush().await?;
        }
        Err(err) => {
            if let Some(response) = daemon_proxy_error_response(line, &err) {
                let json_line = serde_json::to_string(&response)?;
                transport.write_line(&json_line).await?;
                transport.write_line("\n").await?;
                transport.flush().await?;
            } else {
                log_daemon_event(
                    "daemon_proxy_drop",
                    &[
                        ("outcome", "dropped_notification".to_string()),
                        ("error", err.to_string()),
                    ],
                );
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn send_daemon_request_line(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
) -> Result<Vec<String>> {
    let stream = connect_to_daemon(socket_path).await?;
    let (reader, mut writer) = stream.into_split();

    writer.write_all(handshake.to_line()?.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.write_all(line.as_bytes()).await?;
    if !line.ends_with('\n') {
        writer.write_all(b"\n").await?;
    }
    writer.flush().await?;
    writer.shutdown().await?;

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let request_id = serde_json::from_str::<JsonRpcRequest>(line)
        .ok()
        .and_then(|request| request.id);
    let mut responses = Vec::new();
    let mut matched_response = request_id.is_none();
    while let Some(response_line) = lines.next_line().await? {
        if response_line.trim().is_empty() {
            continue;
        }
        let is_matching_response = request_id.as_ref().is_some_and(|id| {
            serde_json::from_str::<serde_json::Value>(&response_line)
                .ok()
                .and_then(|value| value.get("id").cloned())
                .as_ref()
                == Some(id)
        });
        responses.push(format!("{response_line}\n"));
        if is_matching_response {
            matched_response = true;
            break;
        }
    }
    if !matched_response {
        return Err(TraceDecayError::Config {
            message:
                "daemon closed the connection before returning a matching response \
                      — it may have been restarted (e.g. by `tracedecay update`); retry the request"
                    .to_string(),
        });
    }
    Ok(responses)
}

/// Extracts the daemon's advertised version from a proxied `initialize`
/// response (`result.serverInfo.version`, which daemons have always sent).
///
/// This works against daemons older than the handshake version field, so a
/// freshly-updated client can still detect a stale daemon left running by a
/// non-systemd setup or a plain `tracedecay upgrade`.
#[cfg(unix)]
fn daemon_version_from_initialize_response(
    request_line: &str,
    responses: &[String],
) -> Option<String> {
    let request = serde_json::from_str::<JsonRpcRequest>(request_line).ok()?;
    if request.method != "initialize" {
        return None;
    }
    responses.iter().find_map(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()?
            .pointer("/result/serverInfo/version")?
            .as_str()
            .map(str::to_string)
    })
}

/// The warning to surface when the daemon behind an `initialize` response is
/// running a different binary version than this client.
#[cfg(unix)]
fn daemon_version_skew_warning(
    request_line: &str,
    responses: &[String],
    client_version: &str,
) -> Option<String> {
    let daemon_version = daemon_version_from_initialize_response(request_line, responses)?;
    if daemon_version == client_version {
        return None;
    }
    Some(format!(
        "TraceDecay daemon is version {daemon_version} but this client is {client_version} — \
         run `tracedecay daemon restart` to reload the daemon binary"
    ))
}

#[cfg(unix)]
fn daemon_proxy_error_response(line: &str, err: &TraceDecayError) -> Option<JsonRpcResponse> {
    let request = serde_json::from_str::<JsonRpcRequest>(line).ok()?;
    request.id.map(|id| {
        JsonRpcResponse::error(
            id,
            ErrorCode::InternalError,
            format!("TraceDecay daemon connection failed: {err}"),
        )
    })
}

#[cfg(not(unix))]
pub async fn proxy_stdio_to_daemon(
    _socket_path: &Path,
    _handshake: &DaemonHandshake,
    _replay_line: Option<String>,
) -> Result<()> {
    Err(unsupported_platform())
}

pub async fn proxy_stdio_to_default_daemon(
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    let socket_path = default_available_socket_path()?;
    proxy_stdio_to_daemon(&socket_path, handshake, replay_line).await
}

#[cfg(unix)]
pub async fn call_tool(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let stream = connect_to_daemon(socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let id = json!(1);
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(id.clone()),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": tool_name,
            "arguments": arguments,
        })),
    };

    writer.write_all(handshake.to_line()?.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer
        .write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    writer.shutdown().await?;

    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("id") != Some(&id) {
            continue;
        }
        let response: JsonRpcResponse = serde_json::from_value(value)?;
        if let Some(error) = response.error {
            return Err(TraceDecayError::Config {
                message: format!("daemon tool call failed: {}", error.message),
            });
        }
        return response.result.ok_or_else(|| TraceDecayError::Config {
            message: "daemon tool call response did not include a result".to_string(),
        });
    }

    Err(TraceDecayError::Config {
        message: "daemon closed the connection before returning a tool result".to_string(),
    })
}

#[cfg(not(unix))]
pub async fn call_tool(
    _socket_path: &Path,
    _handshake: &DaemonHandshake,
    _tool_name: &str,
    _arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    Err(unsupported_platform())
}

pub async fn call_default_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    call_tool(&socket_path, handshake, tool_name, arguments).await
}

#[cfg(unix)]
async fn run_foreground_unix(socket_path: PathBuf) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        let parent_existed = parent.exists();
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to create socket directory '{}': {e}",
                parent.display()
            ),
        })?;
        if !parent_existed {
            set_owner_only_permissions(parent, 0o700)?;
        }
    }
    prepare_socket_path(&socket_path).await?;

    let listener = UnixListener::bind(&socket_path)?;
    set_owner_only_permissions(&socket_path, 0o600)?;
    log_daemon_event(
        "daemon_listening",
        &[("socket", socket_path.display().to_string())],
    );
    let engine = DaemonEngine::default();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        };
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = Box::pin(serve_socket_client(stream, engine)).await {
                log_daemon_event(
                    "daemon_client",
                    &[("outcome", "error".to_string()), ("error", e.to_string())],
                );
            }
        });
    }
    log_daemon_event(
        "daemon_shutdown",
        &[("socket", socket_path.display().to_string())],
    );
    // Stop accepting and unlink the socket before draining so clients that
    // connect during shutdown get NotFound/ConnectionRefused (which they retry
    // via `connect_with_restart_grace`) instead of a queued connection that
    // will never be served.
    drop(listener);
    let _ = std::fs::remove_file(&socket_path);
    engine.shutdown_all().await;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path, mode: u32) -> Result<()> {
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to restrict permissions on '{}': {e}",
            path.display()
        ),
    })
}

#[cfg(unix)]
async fn prepare_socket_path(socket_path: &Path) -> Result<()> {
    match UnixStream::connect(socket_path).await {
        Ok(_) => Err(TraceDecayError::Config {
            message: format!(
                "daemon socket '{}' is already in use",
                socket_path.display()
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => std::fs::remove_file(socket_path).map_err(|remove_err| TraceDecayError::Config {
            message: format!(
                "failed to remove stale daemon socket '{}': {remove_err}",
                socket_path.display()
            ),
        }),
    }
}

#[cfg(unix)]
#[derive(Clone, Default)]
struct DaemonEngine {
    /// Shared daemon state, partitioned by the client-scoped project server key.
    project_servers: Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, Arc<crate::mcp::McpServer>>>>,
    /// Background automation loops, partitioned with the same client/project identity as MCP state.
    automation_schedulers: Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, JoinHandle<()>>>>,
    /// Client versions whose skew was already logged. Proxy clients reconnect
    /// per request, so without this the mismatch would flood the daemon log.
    logged_client_version_skews: Arc<tokio::sync::Mutex<HashSet<String>>>,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectServerKey {
    project_path: PathBuf,
    scope_prefix: Option<String>,
    client_identity: DaemonClientIdentity,
}

#[cfg(unix)]
impl ProjectServerKey {
    fn from_handshake(project_path: PathBuf, handshake: &DaemonHandshake) -> Self {
        Self {
            project_path,
            scope_prefix: handshake.scope_prefix.clone(),
            client_identity: handshake.client_identity.clone(),
        }
    }
}

#[cfg(unix)]
impl DaemonEngine {
    /// Returns the client version to log for this handshake, once per distinct
    /// skewed version; repeat connections from the same client return `None`.
    async fn client_version_skew_to_log(&self, handshake: &DaemonHandshake) -> Option<String> {
        let skew = client_version_skew(&handshake.client_version, binary_version())?;
        let mut logged = self.logged_client_version_skews.lock().await;
        logged.insert(skew.clone()).then_some(skew)
    }

    async fn project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        let Some(project_path) = handshake.project_path.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "project server requested without project_path".to_string(),
            });
        };
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        let key = ProjectServerKey::from_handshake(canonical_project_path.clone(), handshake);

        let mut servers = self.project_servers.lock().await;
        if let Some(server) = servers.get(&key) {
            let server = Arc::clone(server);
            drop(servers);
            self.ensure_automation_scheduler(key, canonical_project_path, handshake.clone())
                .await;
            return Ok(server);
        }

        let cg = Box::pin(open_project_for_handshake(
            &canonical_project_path,
            handshake,
        ))
        .await?;
        let accounting_db = accounting_db_for_handshake(handshake).await;
        let registry_db = registry_db_for_handshake(handshake).await;
        let server = crate::mcp::McpServer::new_with_dbs(
            cg,
            handshake.scope_prefix.clone(),
            accounting_db,
            registry_db,
            false,
        )
        .await;
        servers.insert(key.clone(), Arc::clone(&server));
        drop(servers);
        self.ensure_automation_scheduler(key, canonical_project_path, handshake.clone())
            .await;
        Ok(server)
    }

    async fn ensure_automation_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) {
        {
            let schedulers = self.automation_schedulers.lock().await;
            if schedulers.contains_key(&key) {
                return;
            }
        }

        let scheduler_configured =
            match automation_scheduler_configured_for_project(&project_path, &handshake).await {
                Ok(configured) => configured,
                Err(e) => {
                    log_daemon_event(
                        "scheduler_config",
                        &[
                            ("project", project_path.display().to_string()),
                            ("outcome", "error".to_string()),
                            ("error", e.to_string()),
                        ],
                    );
                    false
                }
            };
        if scheduler_configured {
            self.start_automation_scheduler(key, project_path, handshake)
                .await;
        }
    }

    async fn start_automation_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) {
        let mut schedulers = self.automation_schedulers.lock().await;
        if schedulers.contains_key(&key) {
            return;
        }
        let handle = tokio::spawn(async move {
            Box::pin(run_automation_scheduler_loop(project_path, handshake)).await;
        });
        schedulers.insert(key, handle);
    }

    async fn shutdown_all(&self) {
        let scheduler_handles: Vec<JoinHandle<()>> = {
            let mut schedulers = self.automation_schedulers.lock().await;
            schedulers.drain().map(|(_, handle)| handle).collect()
        };
        for handle in scheduler_handles {
            handle.abort();
            let _ = handle.await;
        }

        let servers: Vec<Arc<crate::mcp::McpServer>> = {
            let servers = self.project_servers.lock().await;
            servers.values().cloned().collect()
        };
        for server in servers {
            server.shutdown().await;
        }
    }
}

#[cfg(unix)]
async fn run_automation_scheduler_loop(project_path: PathBuf, handshake: DaemonHandshake) {
    loop {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "start".to_string()),
            ],
        );
        if let Err(e) = Box::pin(run_automation_scheduler_tick(&project_path, &handshake)).await {
            log_daemon_event(
                "scheduler_tick",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
        }
        let tick_secs = Box::pin(automation_scheduler_tick_secs_for_project(
            &project_path,
            &handshake,
        ))
        .await;
        log_daemon_event(
            "scheduler_sleep",
            &[
                ("project", project_path.display().to_string()),
                ("next_tick_secs", tick_secs.to_string()),
            ],
        );
        tokio::time::sleep(Duration::from_secs(tick_secs)).await;
    }
}

#[cfg(unix)]
async fn automation_scheduler_tick_secs_for_project(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> u64 {
    match open_existing_project_with_options(project_path, handshake.open_options()).await {
        Ok(cg) => {
            match effective_automation_config_for_project(&cg, &handshake.client_identity).await {
                Ok(config) => config.scheduler_tick_secs,
                Err(e) => {
                    log_daemon_event(
                        "scheduler_config",
                        &[
                            ("project", project_path.display().to_string()),
                            ("outcome", "error".to_string()),
                            ("error", e.to_string()),
                        ],
                    );
                    crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS
                }
            }
        }
        Err(e) => {
            log_daemon_event(
                "scheduler_project_open",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
            crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS
        }
    }
}

#[cfg(unix)]
async fn run_automation_scheduler_tick(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<()> {
    use crate::automation::backend::{AgentTaskKind, CodexAppServerBackend};
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        run_memory_curator_with_backend, run_session_reflector_with_backend,
        run_skill_writer_with_backend, MemoryCuratorAutomationOptions,
        SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
    };

    let cg = open_existing_project_with_options(project_path, handshake.open_options()).await?;
    let control =
        crate::automation::scheduler::load_scheduler_control(&cg.store_layout().dashboard_root)
            .await?;
    if control.paused {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "paused".to_string()),
            ],
        );
        return Ok(());
    }
    let config = effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
    if !automation_scheduler_configured(&config) {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "not_configured".to_string()),
            ],
        );
        return Ok(());
    }
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let mut first_error: Option<TraceDecayError> = None;

    log_scheduler_task_start(project_path, AgentTaskKind::MemoryCurator);
    match run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..MemoryCuratorAutomationOptions::default()
        },
    )
    .await
    {
        Ok(run) => log_daemon_scheduler_record(project_path, &run.ledger_record),
        Err(e) => {
            log_scheduler_task_error(project_path, AgentTaskKind::MemoryCurator, &e);
            first_error.get_or_insert(e);
        }
    }
    log_scheduler_task_start(project_path, AgentTaskKind::SessionReflector);
    match run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    {
        Ok(run) => log_daemon_scheduler_record(project_path, &run.ledger_record),
        Err(e) => {
            log_scheduler_task_error(project_path, AgentTaskKind::SessionReflector, &e);
            first_error.get_or_insert(e);
        }
    }
    log_scheduler_task_start(project_path, AgentTaskKind::SkillWriter);
    match run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    {
        Ok(run) => log_daemon_scheduler_record(project_path, &run.ledger_record),
        Err(e) => {
            log_scheduler_task_error(project_path, AgentTaskKind::SkillWriter, &e);
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[cfg(unix)]
async fn effective_automation_config_for_project(
    cg: &crate::tracedecay::TraceDecay,
    client_identity: &DaemonClientIdentity,
) -> Result<crate::automation::config::AutomationConfig> {
    use crate::automation::config::{effective_config, load_project_config};

    let global = user_config_for_client(client_identity).automation;
    let project = load_project_config(&cg.store_layout().dashboard_root).await?;
    effective_config(&global, project.as_ref())
}

#[cfg(unix)]
async fn automation_scheduler_configured_for_project(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<bool> {
    let cg = open_existing_project_with_options(project_path, handshake.open_options()).await?;
    let config = effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
    Ok(automation_scheduler_configured(&config))
}

#[cfg(unix)]
fn user_config_for_client(
    client_identity: &DaemonClientIdentity,
) -> crate::user_config::UserConfig {
    let path = client_identity.profile_root.join("config.toml");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return crate::user_config::UserConfig::default();
    };
    toml::from_str(&contents).unwrap_or_default()
}

#[cfg(unix)]
fn automation_scheduler_configured(config: &crate::automation::config::AutomationConfig) -> bool {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};
    use crate::automation::scheduler::{parse_schedule, AutomationSchedule};

    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return false;
    }
    [
        &config.tasks.memory_curator,
        &config.tasks.session_reflector,
        &config.tasks.skill_writer,
    ]
    .into_iter()
    .any(|task| {
        if !task.enabled {
            return false;
        }
        match parse_schedule(task.schedule.as_deref()) {
            Ok(AutomationSchedule::Manual) | Err(_) => false,
            Ok(AutomationSchedule::ConfiguredInterval) => task.interval_secs.is_some(),
            Ok(AutomationSchedule::Interval { .. }) => true,
        }
    })
}

#[cfg(unix)]
async fn serve_socket_client(stream: tokio::net::UnixStream, engine: DaemonEngine) -> Result<()> {
    let mut transport = UnixStreamTransport::new(stream);
    let Some(line) = transport.read_line().await? else {
        return Ok(());
    };
    let handshake = DaemonHandshake::from_line(&line)?;
    if let Some(client_version) = engine.client_version_skew_to_log(&handshake).await {
        log_daemon_event(
            "daemon_version_skew",
            &[
                ("daemon_version", binary_version().to_string()),
                ("client_version", client_version),
                (
                    "hint",
                    "daemon binary differs from the connecting client; \
                     run `tracedecay daemon restart` to reload it"
                        .to_string(),
                ),
            ],
        );
    }
    if handshake.project_path.is_some() {
        let server = match Box::pin(engine.project_server(&handshake)).await {
            Ok(server) => server,
            Err(e) => {
                write_project_open_error(&mut transport, &e).await?;
                return Err(e);
            }
        };
        server
            .run_connection_with_timings(&mut transport, handshake.timings)
            .await?;
    } else {
        serve_projectless_client(&mut transport, &handshake.client_identity).await?;
    }
    Ok(())
}

async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<crate::tracedecay::TraceDecay> {
    let open_options = handshake.open_options();
    match open_existing_project_with_options(project_path, open_options.clone()).await {
        Ok(cg) => Ok(cg),
        Err(open_err) if handshake.allow_init && is_missing_index_error(&open_err) => {
            match crate::tracedecay::TraceDecay::init_with_options(project_path, open_options).await
            {
                Ok(cg) => Ok(cg),
                Err(_) => Err(open_err),
            }
        }
        Err(open_err) => Err(open_err),
    }
}

fn is_missing_index_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains("no TraceDecay index found")
                || message.contains("no TraceDecay database found")
    )
}

fn missing_index_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}' — run 'tracedecay init' first",
            project_path.display()
        ),
    }
}

async fn open_existing_project_with_options(
    project_path: &Path,
    open_options: crate::tracedecay::TraceDecayOpenOptions,
) -> Result<crate::tracedecay::TraceDecay> {
    match crate::tracedecay::TraceDecay::open_with_options(project_path, open_options.clone()).await
    {
        Ok(cg) => Ok(cg),
        Err(open_err) => {
            match crate::tracedecay::TraceDecay::open_read_only_with_options(
                project_path,
                open_options,
            )
            .await
            {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    Ok(cg)
                }
                Err(_) if is_missing_index_error(&open_err) => {
                    Err(missing_index_error(project_path))
                }
                Err(_) => Err(open_err),
            }
        }
    }
}

#[cfg(unix)]
async fn accounting_db_for_handshake(
    handshake: &DaemonHandshake,
) -> Option<Arc<crate::global_db::GlobalDb>> {
    if !crate::global_db::global_accounting_enabled() {
        return None;
    }
    crate::global_db::GlobalDb::open_at(&handshake.client_identity.global_db_path)
        .await
        .map(Arc::new)
}

#[cfg(unix)]
async fn registry_db_for_handshake(
    handshake: &DaemonHandshake,
) -> Option<Arc<crate::global_db::GlobalDb>> {
    crate::global_db::GlobalDb::open_at(&handshake.client_identity.global_db_path)
        .await
        .map(Arc::new)
}

#[cfg(unix)]
async fn write_project_open_error(
    transport: &mut UnixStreamTransport,
    error: &TraceDecayError,
) -> Result<()> {
    let id = read_json_rpc_request_id(transport).await?;
    let response = JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    write_json_rpc_response(transport, &response).await
}

#[cfg(unix)]
async fn read_json_rpc_request_id(
    transport: &mut UnixStreamTransport,
) -> Result<serde_json::Value> {
    let Some(line) = transport.read_line().await? else {
        return Ok(serde_json::Value::Null);
    };

    Ok(serde_json::from_str::<JsonRpcRequest>(&line)
        .ok()
        .and_then(|request| request.id)
        .unwrap_or(serde_json::Value::Null))
}

#[cfg(unix)]
async fn write_json_rpc_response(
    transport: &mut UnixStreamTransport,
    response: &crate::mcp::JsonRpcResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn serve_projectless_client(
    transport: &mut UnixStreamTransport,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    while let Some(line) = transport.read_line().await? {
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => projectless_response(&request, client_identity).await,
            Err(e) => Some(JsonRpcResponse::error(
                json!(null),
                ErrorCode::ParseError,
                format!("Parse error: {e}"),
            )),
        };
        if let Some(response) = response {
            write_json_rpc_response(transport, &response).await?;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn projectless_response(
    request: &crate::mcp::JsonRpcRequest,
    client_identity: &DaemonClientIdentity,
) -> Option<crate::mcp::JsonRpcResponse> {
    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "tracedecay",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "tools/call" => Some(
            projectless_tools_call_response(id, request.params.as_ref(), client_identity).await,
        ),
        "ping" | "logging/setLevel" => Some(JsonRpcResponse::success(id, json!({}))),
        _ => Some(JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("Method not found: {}", request.method),
        )),
    }
}

#[cfg(unix)]
async fn projectless_tools_call_response(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    _client_identity: &DaemonClientIdentity,
) -> crate::mcp::JsonRpcResponse {
    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };

    match crate::mcp::tools::handle_profile_scoped_lcm_tool_call(tool_name, arguments).await {
        Ok(result) => JsonRpcResponse::success(id, result.value),
        Err(e) => JsonRpcResponse::error(id, ErrorCode::InternalError, e.to_string()),
    }
}

#[cfg(unix)]
fn projectless_tool_call(
    params: Option<&serde_json::Value>,
) -> std::result::Result<(&str, serde_json::Value), &'static str> {
    let Some(params) = params else {
        return Err("missing params for tools/call");
    };
    let Some(tool_name) = params.get("name").and_then(|v| v.as_str()) else {
        return Err("missing 'name' in tools/call params");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Ok((tool_name, arguments))
}

#[cfg(unix)]
struct UnixStreamTransport {
    reader: tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

#[cfg(unix)]
impl UnixStreamTransport {
    fn new(stream: tokio::net::UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: tokio::io::BufReader::new(reader).lines(),
            writer,
        }
    }
}

#[cfg(unix)]
impl crate::mcp::McpTransport for UnixStreamTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        self.reader.next_line().await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.writer.write_all(line.as_bytes()).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush().await
    }
}

#[cfg(not(unix))]
fn unsupported_platform() -> TraceDecayError {
    TraceDecayError::Config {
        message: "TraceDecay daemon sockets are currently supported on Unix platforms".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    #[cfg(unix)]
    use serde_json::json;
    #[cfg(unix)]
    use serde_json::Value;
    #[cfg(unix)]
    use tempfile::TempDir;
    #[cfg(unix)]
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    use super::{DaemonClientIdentity, DaemonHandshake};

    fn test_client_identity() -> DaemonClientIdentity {
        test_client_identity_for(PathBuf::from("/profiles/client"))
    }

    fn test_client_identity_for(profile_root: PathBuf) -> DaemonClientIdentity {
        DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        }
    }

    fn test_handshake_defaults() -> DaemonHandshake {
        DaemonHandshake {
            project_path: None,
            scope_prefix: None,
            timings: false,
            allow_init: false,
            client_identity: test_client_identity(),
            client_version: super::binary_version().to_string(),
        }
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn daemon_log_line_formats_stable_key_value_fields() {
        let line = super::format_daemon_log_line(
            "scheduler_task",
            &[
                ("task", "memory_curator".to_string()),
                ("outcome", "not due yet".to_string()),
                ("project", "/tmp/example project".to_string()),
            ],
        );

        assert_eq!(
            line,
            "[tracedecay] event=scheduler_task task=memory_curator outcome=\"not due yet\" project=\"/tmp/example project\""
        );
    }

    #[test]
    fn daemon_log_line_escapes_quotes_and_backslashes() {
        let line = super::format_daemon_log_line(
            "client_error",
            &[("error", r#"failed at "step" \ retry"#.to_string())],
        );

        assert_eq!(
            line,
            r#"[tracedecay] event=client_error error="failed at \"step\" \\ retry""#
        );
    }

    #[test]
    fn daemon_log_line_escapes_control_characters() {
        let line = super::format_daemon_log_line(
            "client_error",
            &[("error", "first\nsecond\rthird\tfourth".to_string())],
        );

        assert_eq!(
            line,
            r#"[tracedecay] event=client_error error="first\nsecond\rthird\tfourth""#
        );
    }

    #[cfg(unix)]
    #[test]
    fn transient_daemon_connect_errors_cover_restart_window_only() {
        assert!(super::is_transient_daemon_connect_error(
            std::io::ErrorKind::NotFound
        ));
        assert!(super::is_transient_daemon_connect_error(
            std::io::ErrorKind::ConnectionRefused
        ));
        assert!(!super::is_transient_daemon_connect_error(
            std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connect_with_restart_grace_reconnects_once_daemon_rebinds() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");

        // Simulate the `tracedecay update` restart window: the socket is
        // missing for a while, then the new daemon binds the same path.
        let bind_path = socket.clone();
        let daemon = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            tokio::net::UnixListener::bind(&bind_path).expect("bind restarted daemon socket")
        });

        super::connect_with_restart_grace(
            &socket,
            std::time::Duration::from_secs(8),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("connect should succeed once the restarted daemon binds");
        daemon.await.expect("daemon bind task");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connect_with_restart_grace_gives_up_with_restart_hint() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");

        let err = super::connect_with_restart_grace(
            &socket,
            std::time::Duration::from_millis(300),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("connect should fail when no daemon ever binds");

        let message = err.to_string();
        assert!(
            message.contains("tracedecay update"),
            "error should hint that the daemon may be restarting after an update, got: {message}"
        );
        assert!(
            message.contains(&socket.display().to_string()),
            "error should name the socket path, got: {message}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serve_proxies_when_socket_already_exists() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");
        let _listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");

        assert!(
            super::should_proxy_serve_to_daemon_with(
                &socket,
                None,
                std::time::Duration::from_secs(8),
                std::time::Duration::from_millis(50),
            )
            .await
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serve_stays_in_process_without_socket_or_installed_service() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");
        let other_socket = dir.path().join("other.sock");

        // No socket and no service claiming it: fall back immediately, even
        // with a long grace configured — startup must not stall.
        let decision = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            super::should_proxy_serve_to_daemon_with(
                &socket,
                None,
                std::time::Duration::from_secs(8),
                std::time::Duration::from_millis(50),
            ),
        )
        .await
        .expect("decision without daemon evidence should be immediate");
        assert!(!decision);

        // A service installed for a different socket is not evidence either.
        let decision = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            super::should_proxy_serve_to_daemon_with(
                &socket,
                Some(&other_socket),
                std::time::Duration::from_secs(8),
                std::time::Duration::from_millis(50),
            ),
        )
        .await
        .expect("mismatched service socket should not delay the decision");
        assert!(!decision);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serve_waits_out_restart_window_when_service_owns_socket() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");

        // Simulate the `tracedecay update` restart window: the service is
        // installed but the old daemon already unlinked the socket; the new
        // daemon binds it shortly after serve starts.
        let bind_path = socket.clone();
        let daemon = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            tokio::net::UnixListener::bind(&bind_path).expect("bind restarted daemon socket")
        });

        assert!(
            super::should_proxy_serve_to_daemon_with(
                &socket,
                Some(&socket),
                std::time::Duration::from_secs(8),
                std::time::Duration::from_millis(50),
            )
            .await,
            "serve started during a daemon restart should still pick the daemon transport"
        );
        daemon.await.expect("daemon bind task");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serve_falls_back_when_installed_service_never_rebinds() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");

        assert!(
            !super::should_proxy_serve_to_daemon_with(
                &socket,
                Some(&socket),
                std::time::Duration::from_millis(200),
                std::time::Duration::from_millis(50),
            )
            .await,
            "a stopped service should fall back to in-process after the grace expires"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn proxied_request_survives_daemon_restart_window() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");

        let bind_path = socket.clone();
        let daemon = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let listener =
                tokio::net::UnixListener::bind(&bind_path).expect("bind restarted daemon socket");
            let (stream, _addr) = listener.accept().await.expect("accept proxied client");
            let (reader, mut writer) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(reader).lines();
            let handshake_line = lines
                .next_line()
                .await
                .expect("read handshake")
                .expect("handshake line");
            DaemonHandshake::from_line(&handshake_line).expect("parse handshake");
            let request_line = lines
                .next_line()
                .await
                .expect("read request")
                .expect("request line");
            let request: Value = serde_json::from_str(&request_line).expect("request json");
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": { "ok": true }
            });
            writer
                .write_all(
                    serde_json::to_string(&response)
                        .expect("response json")
                        .as_bytes(),
                )
                .await
                .expect("write response");
            writer.write_all(b"\n").await.expect("write newline");
        });

        let handshake = test_handshake_defaults();
        let request = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/list"
        }))
        .expect("request json");

        let responses = super::send_daemon_request_line(&socket, &handshake, &request)
            .await
            .expect("request should succeed once the restarted daemon is back");

        assert_eq!(responses.len(), 1);
        let response: Value =
            serde_json::from_str(responses[0].trim()).expect("proxied response json");
        assert_eq!(response["id"], json!(42));
        assert_eq!(response["result"]["ok"], json!(true));
        daemon.await.expect("fake daemon task");
    }

    #[cfg(unix)]
    #[test]
    fn scheduler_task_start_log_uses_task_key_and_project() {
        let line = super::format_daemon_log_line(
            "scheduler_task",
            &super::scheduler_task_log_fields(
                std::path::Path::new("/tmp/project with spaces"),
                crate::automation::backend::AgentTaskKind::SkillWriter,
                "start",
            ),
        );

        assert_eq!(
            line,
            "[tracedecay] event=scheduler_task project=\"/tmp/project with spaces\" task=skill_writer outcome=start"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scheduler_record_log_preserves_skipped_status_and_reason() {
        let record = crate::automation::run_ledger::AutomationRunLedgerRecord {
            schema_version: 2,
            run_id: "run-123".to_string(),
            trigger: crate::automation::run_ledger::AutomationTrigger::Scheduler,
            task: crate::automation::backend::AgentTaskKind::MemoryCurator,
            task_key: Some("memory_curator".to_string()),
            backend: "codex_app_server".to_string(),
            host_mode: Some("standalone".to_string()),
            prompt_version: Some("memory_curator:v1".to_string()),
            response_schema: None,
            strict_json: None,
            model: None,
            status: crate::automation::run_ledger::AutomationRunStatus::Skipped,
            evidence_hash: None,
            input_hash: None,
            output_hash: None,
            proposed_ops: None,
            applied_ops: None,
            rejected_ops: None,
            validation_report: None,
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 1,
            error: None,
            error_classification: None,
            error_retryable: None,
            fallback_status: Some("scheduler_interval_not_elapsed".to_string()),
            report_ref: None,
            artifacts: Vec::new(),
            started_at: "1000".to_string(),
            completed_at: "1001".to_string(),
        };

        let line =
            super::daemon_scheduler_record_log_line(std::path::Path::new("/tmp/project"), &record);

        assert_eq!(
            line,
            "[tracedecay] event=scheduler_task project=/tmp/project task=memory_curator outcome=skipped run_id=run-123 reason=scheduler_interval_not_elapsed"
        );
    }

    #[test]
    fn daemon_handshake_round_trips_project_scope_and_timings() {
        let handshake = DaemonHandshake {
            project_path: Some(PathBuf::from("/work/repo")),
            scope_prefix: Some("src/mcp".to_string()),
            timings: true,
            allow_init: true,
            ..test_handshake_defaults()
        };

        let encoded = handshake.to_line().expect("handshake should encode");
        let decoded = DaemonHandshake::from_line(&encoded).expect("handshake should decode");

        assert_eq!(decoded, handshake);
    }

    #[test]
    fn daemon_handshake_requires_client_identity() {
        let encoded = serde_json::json!({
            "project_path": "/work/repo",
            "scope_prefix": null,
            "timings": false,
            "allow_init": false
        })
        .to_string();

        assert!(DaemonHandshake::from_line(&encoded).is_err());
    }

    /// Old client → new daemon: handshakes without `client_version` (sent by
    /// binaries predating the field) must still parse, with an empty version.
    #[test]
    fn daemon_handshake_accepts_old_client_without_version() {
        let encoded = serde_json::json!({
            "project_path": "/work/repo",
            "scope_prefix": null,
            "timings": false,
            "allow_init": false,
            "client_identity": {
                "profile_root": "/profiles/client",
                "global_db_path": "/profiles/client/global.db"
            }
        })
        .to_string();

        let decoded = DaemonHandshake::from_line(&encoded).expect("old handshake should decode");

        assert_eq!(decoded.client_version, "");
    }

    /// New client → old daemon: the serde derive ignores unknown fields, so a
    /// daemon predating `client_version` (same derive) parses new handshakes.
    /// Adding another unknown field to a current handshake proves the
    /// tolerance the old daemon relies on.
    #[test]
    fn daemon_handshake_ignores_unknown_fields_for_old_daemons() {
        let handshake = test_handshake_defaults();
        let mut value: serde_json::Value =
            serde_json::from_str(&handshake.to_line().expect("handshake should encode"))
                .expect("handshake json");
        value["field_from_a_future_version"] = serde_json::json!("ignored");

        let decoded = DaemonHandshake::from_line(&value.to_string())
            .expect("handshake with unknown fields should decode");

        assert_eq!(decoded, handshake);
    }

    #[test]
    fn daemon_handshake_advertises_binary_version() {
        let handshake = test_handshake_defaults();

        let encoded = handshake.to_line().expect("handshake should encode");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("handshake json");

        assert_eq!(
            value["client_version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn client_version_skew_flags_only_real_mismatches() {
        assert_eq!(super::client_version_skew("1.2.3", "1.2.3"), None);
        assert_eq!(super::client_version_skew("", "1.2.3"), None);
        assert_eq!(
            super::client_version_skew("1.3.0", "1.2.3"),
            Some("1.3.0".to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_engine_logs_version_skew_once_per_client_version() {
        let engine = super::DaemonEngine::default();
        let mut handshake = test_handshake_defaults();
        handshake.client_version = "0.0.0-skewed".to_string();

        assert_eq!(
            engine.client_version_skew_to_log(&handshake).await,
            Some("0.0.0-skewed".to_string()),
            "first connection from a skewed client should be logged"
        );
        assert_eq!(
            engine.client_version_skew_to_log(&handshake).await,
            None,
            "repeat connections from the same client version must not spam the log"
        );

        let matching = test_handshake_defaults();
        assert_eq!(
            engine.client_version_skew_to_log(&matching).await,
            None,
            "matching client versions are not skew"
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_version_skew_warning_reads_initialize_server_info() {
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })
        .to_string();
        let response = |version: &str| {
            vec![serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "serverInfo": { "name": "tracedecay", "version": version } }
            })
            .to_string()]
        };

        let warning = super::daemon_version_skew_warning(&initialize, &response("9.9.9"), "1.0.0")
            .expect("mismatched daemon version should warn");
        assert!(
            warning.contains("9.9.9") && warning.contains("1.0.0"),
            "warning should name both versions, got: {warning}"
        );
        assert!(
            warning.contains("tracedecay daemon restart"),
            "warning should point at the restart command, got: {warning}"
        );

        assert_eq!(
            super::daemon_version_skew_warning(&initialize, &response("1.0.0"), "1.0.0"),
            None,
            "matching versions must not warn"
        );

        let tools_call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {}
        })
        .to_string();
        assert_eq!(
            super::daemon_version_skew_warning(&tools_call, &response("9.9.9"), "1.0.0"),
            None,
            "only initialize responses advertise the daemon version"
        );
    }

    #[cfg(unix)]
    #[test]
    fn automation_scheduler_starts_when_any_task_has_interval() {
        use crate::automation::config::{
            AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
        };

        let mut config = AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            ..AutomationConfig::default()
        };
        config.tasks.memory_curator = AutomationTaskConfig {
            enabled: true,
            schedule: Some("every:5m".to_string()),
            interval_secs: None,
            cooldown_secs: None,
            ..AutomationTaskConfig::default()
        };

        assert!(super::automation_scheduler_configured(&config));

        config.tasks.memory_curator.schedule = Some("manual".to_string());
        assert!(!super::automation_scheduler_configured(&config));

        config.tasks.memory_curator.schedule = Some("interval".to_string());
        config.tasks.memory_curator.interval_secs = None;
        assert!(!super::automation_scheduler_configured(&config));
        config.tasks.memory_curator.interval_secs = Some(300);
        assert!(super::automation_scheduler_configured(&config));

        config.tasks.memory_curator.enabled = false;
        config.tasks.session_reflector = AutomationTaskConfig {
            enabled: true,
            schedule: Some("hourly".to_string()),
            interval_secs: None,
            cooldown_secs: None,
            ..AutomationTaskConfig::default()
        };
        assert!(super::automation_scheduler_configured(&config));

        config.tasks.session_reflector.enabled = false;
        config.tasks.skill_writer = AutomationTaskConfig {
            enabled: true,
            schedule: Some("daily".to_string()),
            interval_secs: None,
            cooldown_secs: None,
            ..AutomationTaskConfig::default()
        };
        assert!(super::automation_scheduler_configured(&config));

        config.tasks.memory_curator.schedule = Some("every:5m".to_string());
        config.backend = AutomationBackend::ExternalCommand;
        assert!(!super::automation_scheduler_configured(&config));

        config.backend = AutomationBackend::CodexAppServer;
        config.host_mode = AutomationHostMode::DelegatedHost;
        assert!(!super::automation_scheduler_configured(&config));

        config.host_mode = AutomationHostMode::Standalone;
        config.enabled = false;
        assert!(!super::automation_scheduler_configured(&config));
    }

    #[cfg(unix)]
    #[test]
    fn automation_scheduler_loads_client_profile_config() {
        let profile = TempDir::new().expect("profile temp dir");
        std::fs::write(
            profile.path().join("config.toml"),
            "[automation]\n\
             enabled = true\n\
             backend = \"codex_app_server\"\n\
             \n\
             [automation.tasks.memory_curator]\n\
             enabled = true\n\
             schedule = \"every:5m\"\n",
        )
        .expect("write config");
        let client_identity = test_client_identity_for(profile.path().to_path_buf());

        let config = super::user_config_for_client(&client_identity);

        assert!(config.automation.enabled);
        assert!(super::automation_scheduler_configured(&config.automation));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn automation_scheduler_tick_secs_loads_dashboard_project_config() {
        use crate::automation::config::{save_project_config, AutomationConfigPatch};

        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().canonicalize().expect("canonical temp dir");
        let client_identity = test_client_identity_for(project.join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
        let cg = crate::tracedecay::TraceDecay::init_with_options(
            &project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(client_identity.profile_root.clone()),
                global_db_path: Some(client_identity.global_db_path.clone()),
            },
        )
        .await
        .expect("project init");
        save_project_config(
            &cg.store_layout().dashboard_root,
            &AutomationConfigPatch {
                scheduler_tick_secs: Some(17),
                ..AutomationConfigPatch::default()
            },
        )
        .await
        .expect("save automation config");
        let handshake = DaemonHandshake {
            project_path: Some(project.clone()),
            client_identity,
            ..test_handshake_defaults()
        };

        let tick_secs = Box::pin(super::automation_scheduler_tick_secs_for_project(
            &project, &handshake,
        ))
        .await;

        assert_eq!(tick_secs, 17);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_client_serves_initialize_after_handshake() {
        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().canonicalize().expect("canonical temp dir");
        let client_identity = test_client_identity_for(project.join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");

        let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_task = tokio::spawn(super::serve_socket_client(
            server,
            super::DaemonEngine::default(),
        ));

        let (reader, mut writer) = client.into_split();
        let handshake = DaemonHandshake {
            project_path: Some(project.clone()),
            allow_init: true,
            client_identity,
            ..test_handshake_defaults()
        };
        writer
            .write_all(handshake.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake");
        writer.write_all(b"\n").await.expect("newline");
        writer
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {}
                }))
                .expect("initialize json")
                .as_bytes(),
            )
            .await
            .expect("write initialize");
        writer.write_all(b"\n").await.expect("newline");
        writer.shutdown().await.expect("shutdown writer");

        let mut lines = tokio::io::BufReader::new(reader).lines();
        let line = lines
            .next_line()
            .await
            .expect("read response")
            .expect("initialize response");
        let response: Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["id"], json!(1));
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");

        server_task
            .await
            .expect("server task")
            .expect("server result");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_client_hook_event_adds_branch_tracking() {
        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().canonicalize().expect("canonical temp dir");
        let client_identity = test_client_identity_for(project.join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
        let cg = crate::tracedecay::TraceDecay::init_with_options(
            &project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(client_identity.profile_root.clone()),
                global_db_path: Some(client_identity.global_db_path.clone()),
            },
        )
        .await
        .expect("project init");
        let data_root = cg.store_layout().data_root.clone();
        run_git(&project, &["init", "-b", "main"]);
        run_git(&project, &["switch", "-c", "feature/hook-daemon"]);

        let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_task = tokio::spawn(super::serve_socket_client(
            server,
            super::DaemonEngine::default(),
        ));

        let (_reader, mut writer) = client.into_split();
        let handshake = DaemonHandshake {
            project_path: Some(project.clone()),
            client_identity,
            ..test_handshake_defaults()
        };
        writer
            .write_all(handshake.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake");
        writer.write_all(b"\n").await.expect("newline");
        writer
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "method": "tracedecay/hookEvent",
                    "params": {
                        "agent": "cursor",
                        "event": "afterShellExecution",
                        "command": "git switch feature/hook-daemon",
                        "cwd": project
                    }
                }))
                .expect("hook event json")
                .as_bytes(),
            )
            .await
            .expect("write hook event");
        writer.write_all(b"\n").await.expect("newline");
        writer.shutdown().await.expect("shutdown writer");

        server_task
            .await
            .expect("server task")
            .expect("server result");

        assert!(
            data_root.join("branches/feature_hook-daemon.db").exists(),
            "daemon hook events should add branch tracking in the active project store"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_project_server_reconciles_scheduler_after_config_change() {
        use crate::automation::backend::AgentTaskKind;
        use crate::automation::config::{
            save_project_config, AutomationBackend, AutomationConfigPatch, AutomationTaskPatch,
        };
        use crate::automation::run_ledger::{
            append_run_record, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
        };

        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().canonicalize().expect("canonical temp dir");
        let client_identity = test_client_identity_for(project.join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
        let cg = crate::tracedecay::TraceDecay::init_with_options(
            &project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(client_identity.profile_root.clone()),
                global_db_path: Some(client_identity.global_db_path.clone()),
            },
        )
        .await
        .expect("project init");
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let handshake = DaemonHandshake {
            project_path: Some(project.clone()),
            client_identity,
            ..test_handshake_defaults()
        };
        let key = super::ProjectServerKey::from_handshake(project.clone(), &handshake);
        let engine = super::DaemonEngine::default();

        let first_server = Box::pin(engine.project_server(&handshake))
            .await
            .expect("initial project server");
        assert!(engine.automation_schedulers.lock().await.is_empty());

        let now = crate::tracedecay::current_timestamp();
        append_run_record(
            &dashboard_root,
            &AutomationRunLedgerRecord {
                schema_version: 2,
                run_id: "recent-scheduler-run".to_string(),
                trigger: AutomationTrigger::Scheduler,
                task: AgentTaskKind::MemoryCurator,
                task_key: Some("memory_curator".to_string()),
                backend: "codex_app_server".to_string(),
                host_mode: Some("standalone".to_string()),
                prompt_version: Some("memory_curator:v1".to_string()),
                response_schema: None,
                strict_json: None,
                model: None,
                status: AutomationRunStatus::Succeeded,
                evidence_hash: None,
                input_hash: None,
                output_hash: None,
                proposed_ops: None,
                applied_ops: None,
                rejected_ops: None,
                validation_report: None,
                reviewed_count: 0,
                accepted_count: 0,
                rejected_count: 0,
                skipped_count: 0,
                error: None,
                error_classification: None,
                error_retryable: None,
                fallback_status: None,
                report_ref: None,
                artifacts: Vec::new(),
                started_at: (now - 1).to_string(),
                completed_at: now.to_string(),
            },
        )
        .await
        .expect("seed recent scheduler ledger");
        save_project_config(
            &dashboard_root,
            &AutomationConfigPatch {
                enabled: Some(true),
                backend: Some(AutomationBackend::CodexAppServer),
                memory_curator: AutomationTaskPatch {
                    enabled: Some(true),
                    schedule: Some(Some("interval".to_string())),
                    interval_secs: Some(Some(3600)),
                    ..AutomationTaskPatch::default()
                },
                ..AutomationConfigPatch::default()
            },
        )
        .await
        .expect("save automation config");

        let second_server = Box::pin(engine.project_server(&handshake))
            .await
            .expect("existing project server");
        assert!(std::sync::Arc::ptr_eq(&first_server, &second_server));
        assert!(engine.automation_schedulers.lock().await.contains_key(&key));

        engine.shutdown_all().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn automation_scheduler_tick_respects_pause_control_without_backend_call() {
        use crate::automation::config::{
            save_project_config, AutomationBackend, AutomationConfigPatch, AutomationTaskPatch,
        };
        use crate::automation::run_ledger::load_run_records;
        use crate::automation::scheduler::{save_scheduler_control, AutomationSchedulerControl};

        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().canonicalize().expect("canonical temp dir");
        let client_identity = test_client_identity_for(project.join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
        let cg = crate::tracedecay::TraceDecay::init_with_options(
            &project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(client_identity.profile_root.clone()),
                global_db_path: Some(client_identity.global_db_path.clone()),
            },
        )
        .await
        .expect("project init");
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        save_project_config(
            &dashboard_root,
            &AutomationConfigPatch {
                enabled: Some(true),
                backend: Some(AutomationBackend::CodexAppServer),
                memory_curator: AutomationTaskPatch {
                    enabled: Some(true),
                    schedule: Some(Some("every:1m".to_string())),
                    ..AutomationTaskPatch::default()
                },
                ..AutomationConfigPatch::default()
            },
        )
        .await
        .expect("save automation config");
        save_scheduler_control(
            &dashboard_root,
            &AutomationSchedulerControl { paused: true },
        )
        .await
        .expect("save paused scheduler control");
        let handshake = DaemonHandshake {
            project_path: Some(project.clone()),
            client_identity,
            ..test_handshake_defaults()
        };

        Box::pin(super::run_automation_scheduler_tick(&project, &handshake))
            .await
            .expect("paused scheduler tick should exit cleanly");

        let records = load_run_records(&dashboard_root, 10)
            .await
            .expect("load run ledger");
        assert!(
            records.is_empty(),
            "paused scheduler tick must not call backends or append run records"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_client_serves_profile_scoped_lcm_without_project() {
        let hermes_home = TempDir::new().expect("hermes home");
        let hermes_home = hermes_home
            .path()
            .canonicalize()
            .expect("canonical hermes home");
        let client_identity = test_client_identity_for(hermes_home.join("client-profile"));
        std::fs::create_dir_all(hermes_home.join(".tracedecay")).expect("profile root");

        let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_task = tokio::spawn(super::serve_socket_client(
            server,
            super::DaemonEngine::default(),
        ));

        let (reader, mut writer) = client.into_split();
        let handshake = DaemonHandshake {
            client_identity,
            ..test_handshake_defaults()
        };
        writer
            .write_all(handshake.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake");
        writer.write_all(b"\n").await.expect("newline");
        writer
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_lcm_status",
                        "arguments": {
                            "provider": "cursor",
                            "storage_scope": "hermes_profile",
                            "hermes_home": hermes_home
                        }
                    }
                }))
                .expect("tools/call json")
                .as_bytes(),
            )
            .await
            .expect("write tools/call");
        writer.write_all(b"\n").await.expect("newline");
        writer.shutdown().await.expect("shutdown writer");

        let mut lines = tokio::io::BufReader::new(reader).lines();
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("profile-scoped response should not time out")
            .expect("read response")
            .expect("profile-scoped response");
        let response: Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["id"], json!(7));
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("profile result text");
        let payload: Value = serde_json::from_str(text).expect("profile payload json");
        assert_eq!(payload["status"], "not_ingested");
        assert_eq!(payload["storage_scope"], "hermes_profile");

        server_task
            .await
            .expect("server task")
            .expect("server result");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_client_timings_are_connection_local() {
        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().canonicalize().expect("canonical temp dir");
        let client_identity = test_client_identity_for(project.join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
        crate::tracedecay::TraceDecay::init_with_options(
            &project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(client_identity.profile_root.clone()),
                global_db_path: Some(client_identity.global_db_path.clone()),
            },
        )
        .await
        .expect("project init");

        let engine = super::DaemonEngine::default();
        let (client_a, server_a) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_a_task = tokio::spawn(super::serve_socket_client(server_a, engine.clone()));
        let (reader_a, mut writer_a) = client_a.into_split();
        let mut lines_a = tokio::io::BufReader::new(reader_a).lines();

        let handshake_a = DaemonHandshake {
            project_path: Some(project.clone()),
            client_identity: client_identity.clone(),
            ..test_handshake_defaults()
        };
        writer_a
            .write_all(handshake_a.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake A");
        writer_a.write_all(b"\n").await.expect("newline A");
        writer_a
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {}
                }))
                .expect("initialize json")
                .as_bytes(),
            )
            .await
            .expect("write initialize A");
        writer_a.write_all(b"\n").await.expect("newline A");
        let line = lines_a
            .next_line()
            .await
            .expect("read initialize A")
            .expect("initialize A response");
        let response: Value = serde_json::from_str(&line).expect("initialize A response json");
        assert_eq!(response["id"], json!(1));

        let (client_b, server_b) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_b_task = tokio::spawn(super::serve_socket_client(server_b, engine));
        let (reader_b, mut writer_b) = client_b.into_split();
        let mut lines_b = tokio::io::BufReader::new(reader_b).lines();
        let handshake_b = DaemonHandshake {
            project_path: Some(project),
            timings: true,
            client_identity,
            ..test_handshake_defaults()
        };
        writer_b
            .write_all(handshake_b.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake B");
        writer_b.write_all(b"\n").await.expect("newline B");
        writer_b
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "initialize",
                    "params": {}
                }))
                .expect("initialize json")
                .as_bytes(),
            )
            .await
            .expect("write initialize B");
        writer_b.write_all(b"\n").await.expect("newline B");
        let line = lines_b
            .next_line()
            .await
            .expect("read initialize B")
            .expect("initialize B response");
        let response: Value = serde_json::from_str(&line).expect("initialize B response json");
        assert_eq!(response["id"], json!(2));

        writer_a
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": { "format": "json" }
                    }
                }))
                .expect("tools/call json")
                .as_bytes(),
            )
            .await
            .expect("write tools/call A");
        writer_a.write_all(b"\n").await.expect("newline A");
        writer_a.shutdown().await.expect("shutdown A writer");
        writer_b.shutdown().await.expect("shutdown B writer");

        let line = lines_a
            .next_line()
            .await
            .expect("read status A")
            .expect("status A response");
        let response: Value = serde_json::from_str(&line).expect("status A response json");
        assert_eq!(response["id"], json!(3));
        assert!(
            response["result"].get("_meta").is_none(),
            "client A disabled timings, but response included metadata: {response}"
        );

        server_a_task
            .await
            .expect("server A task")
            .expect("server A result");
        server_b_task
            .await
            .expect("server B task")
            .expect("server B result");
    }
}

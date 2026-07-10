#[cfg(unix)]
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tokio::task::{JoinHandle, JoinSet};
#[cfg(unix)]
use tokio::time::{Duration, timeout};

use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};
#[cfg(unix)]
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport, StdioTransport};

pub const SERVICE_NAME: &str = "tracedecay.service";
pub const SOCKET_ENV: &str = "TRACEDECAY_DAEMON_SOCKET";
pub const HOOK_EVENT_METHOD: &str = "tracedecay/hookEvent";
#[cfg(unix)]
const HOOK_EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);
/// Upper bound on graceful-shutdown persistence work (per-server token
/// persistence and WAL checkpoints). Must stay comfortably below systemd's
/// stop timeout (90s by default) so the daemon exits cleanly instead of
/// being killed with `SIGKILL` mid-checkpoint.
#[cfg(unix)]
const DAEMON_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(45);
#[cfg(unix)]
const DAEMON_CLIENT_DRAIN_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
pub(crate) struct DaemonLifecycle {
    inner: Arc<DaemonLifecycleInner>,
}

#[derive(Default)]
struct DaemonLifecycleInner {
    draining: AtomicBool,
    active: AtomicUsize,
    idle: tokio::sync::Notify,
    draining_notify: tokio::sync::Notify,
}

pub(crate) struct DaemonActivity {
    inner: Arc<DaemonLifecycleInner>,
}

impl DaemonLifecycle {
    pub(crate) fn accepting(&self) -> bool {
        !self.inner.draining.load(Ordering::Acquire)
    }

    pub(crate) fn try_enter(&self) -> Option<DaemonActivity> {
        if !self.accepting() {
            return None;
        }
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        if self.accepting() {
            Some(DaemonActivity {
                inner: Arc::clone(&self.inner),
            })
        } else {
            if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.inner.idle.notify_waiters();
            }
            None
        }
    }

    fn begin_draining(&self) {
        if !self.inner.draining.swap(true, Ordering::AcqRel) {
            self.inner.draining_notify.notify_waiters();
        }
    }

    pub(crate) async fn wait_for_draining(&self) {
        loop {
            let notified = self.inner.draining_notify.notified();
            if !self.accepting() {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            if self.inner.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for DaemonActivity {
    fn drop(&mut self) {
        if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.idle.notify_waiters();
        }
    }
}

#[cfg(unix)]
mod git_watch;
#[cfg(unix)]
pub mod pr_autotrack;
mod service;
pub use service::{
    DaemonServiceSpec, DaemonServiceState, daemon_reachable, default_socket_path, install_service,
    installed_service_socket_path, quiesce_installed_service_under_lease,
    refresh_installed_service, refresh_installed_service_under_lease,
    refresh_installed_service_under_lease_with_state, refresh_service, service_spec,
    service_status, socket_path_or_default, uninstall_service,
};

/// A host whose lifecycle hooks notify the daemon.
///
/// Kept shared between hook emitters and daemon-side parsing so new hosts
/// cannot be accepted by one side and dropped by the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAgent {
    Claude,
    Codex,
    Cursor,
    Kiro,
}

impl HookAgent {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Kiro => "kiro",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            "kiro" => Some(Self::Kiro),
            _ => None,
        }
    }

    /// Marker file used to debounce this agent's incremental syncs.
    pub fn sync_marker_file(self) -> &'static str {
        match self {
            Self::Claude => ".claude_post_tool_sync_at",
            Self::Codex => ".codex_shell_sync_at",
            Self::Cursor => ".cursor_shell_sync_at",
            Self::Kiro => ".kiro_post_tool_sync_at",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRouteMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<HookRouteMetadata>,
}

impl DaemonHookEvent {
    fn new(
        agent: HookAgent,
        event: &'static str,
        rel_paths: Vec<String>,
        command: Option<String>,
        cwd: Option<PathBuf>,
    ) -> Self {
        Self {
            agent: agent.as_wire().to_string(),
            event: event.to_string(),
            rel_paths,
            command,
            cwd,
            route: None,
        }
    }

    #[must_use]
    pub fn with_route(mut self, route: Option<HookRouteMetadata>) -> Self {
        self.route = route;
        self
    }

    pub fn cursor_after_file_edit(rel_paths: Vec<String>) -> Self {
        Self::new(HookAgent::Cursor, "afterFileEdit", rel_paths, None, None)
    }

    pub fn cursor_after_shell_execution(command: String, cwd: PathBuf) -> Self {
        Self::new(
            HookAgent::Cursor,
            "afterShellExecution",
            Vec::new(),
            Some(command),
            Some(cwd),
        )
    }

    pub fn cursor_workspace_open(cwd: PathBuf) -> Self {
        Self::new(
            HookAgent::Cursor,
            "workspaceOpen",
            Vec::new(),
            None,
            Some(cwd),
        )
    }

    /// A file-edit tool finished: request targeted sync of the edited paths.
    pub fn post_tool_use_edit(agent: HookAgent, rel_paths: Vec<String>, cwd: PathBuf) -> Self {
        Self::new(agent, "postToolUseEdit", rel_paths, None, Some(cwd))
    }

    /// A shell command finished: let the daemon classify it (branch add,
    /// worktree add, incremental sync, or noop).
    pub fn post_tool_use_shell(agent: HookAgent, command: String, cwd: PathBuf) -> Self {
        Self::new(
            agent,
            "postToolUseShell",
            Vec::new(),
            Some(command),
            Some(cwd),
        )
    }

    pub fn kiro_post_tool_use(rel_paths: Vec<String>, cwd: Option<PathBuf>) -> Self {
        Self::new(HookAgent::Kiro, "postToolUse", rel_paths, None, cwd)
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
    #[serde(default)]
    pub allow_initialize_root_routing: bool,
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
            allow_initialize_root_routing: false,
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
    HookAgent::from_wire(agent).map_or(".daemon_hook_shell_sync_at", HookAgent::sync_marker_file)
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

/// A single git-watcher lifecycle event recovered from the daemon log, for the
/// `tracedecay doctor` watcher-health section.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct WatcherEvent {
    /// The `git_watch_*` event name (`started`, `synced`, `degraded`, `restart`).
    pub event: String,
    /// The `project=` field, when present.
    pub project: Option<String>,
    /// The `action=`/`reason=` field, when present (context for the event).
    pub detail: Option<String>,
}

/// Parses one daemon log line into a [`WatcherEvent`] when it is a `git_watch_*`
/// event. Mirrors [`format_daemon_log_line`] (space-separated `key=value`, values
/// optionally double-quoted). Returns `None` for non-watcher lines.
#[cfg(unix)]
fn parse_watcher_log_line(line: &str) -> Option<WatcherEvent> {
    let idx = line.find("event=")?;
    let rest = &line[idx + "event=".len()..];
    let mut fields = parse_log_fields(rest);
    let event = fields.remove("__first__")?;
    if !event.starts_with("git_watch_") {
        return None;
    }
    let detail = fields
        .remove("action")
        .or_else(|| fields.remove("reason"))
        .or_else(|| fields.remove("branch"));
    Some(WatcherEvent {
        event,
        project: fields.remove("project"),
        detail,
    })
}

/// Splits a `key=value key="quoted value" …` tail into a map. The leading value
/// (the event name, which has no key) is stored under `__first__`.
#[cfg(unix)]
fn parse_log_fields(rest: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut first = true;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if first {
            // Leading unkeyed event-name token.
            let start = i;
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            out.insert("__first__".to_string(), unquote(&rest[start..i]));
            first = false;
            continue;
        }
        // key
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b' ' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            break;
        }
        let key = rest[key_start..i].to_string();
        i += 1; // skip '='
        let value = if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let val_start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            let v = rest[val_start..i.min(rest.len())].to_string();
            if i < bytes.len() {
                i += 1; // closing quote
            }
            v.replace("\\\"", "\"").replace("\\\\", "\\")
        } else {
            let val_start = i;
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            rest[val_start..i].to_string()
        };
        out.insert(key, value);
    }
    out
}

#[cfg(unix)]
fn unquote(s: &str) -> String {
    s.trim_matches('"').to_string()
}

/// Reads recent `git_watch_*` events from the daemon log and returns the most
/// recent event per project. Read-only; used by `tracedecay doctor`.
///
/// Source is platform-specific: systemd user journal on Linux, the launchd
/// `daemon.err.log` on macOS. Returns an empty map when no log source is
/// readable (the doctor treats that as "no watcher telemetry available").
#[cfg(unix)]
pub fn recent_watcher_events(max_lines: usize) -> HashMap<String, WatcherEvent> {
    let text = read_daemon_log_tail(max_lines);
    let mut latest: HashMap<String, WatcherEvent> = HashMap::new();
    for line in text.lines() {
        if let Some(ev) = parse_watcher_log_line(line) {
            let key = ev.project.clone().unwrap_or_else(|| "<global>".to_string());
            latest.insert(key, ev);
        }
    }
    latest
}

/// Best-effort read of the tail of the daemon log across service runners.
#[cfg(unix)]
fn read_daemon_log_tail(max_lines: usize) -> String {
    // macOS launchd: a plain err-log file next to the data dir.
    if let Some(data_dir) = crate::config::user_data_dir() {
        let err_log = data_dir.join("daemon.err.log");
        if let Ok(contents) = std::fs::read_to_string(&err_log) {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            return lines[start..].join("\n");
        }
    }
    // Linux systemd: pull recent journal lines for the user unit.
    let output = std::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            SERVICE_NAME,
            "--no-pager",
            "-n",
            &max_lines.to_string(),
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => String::new(),
    }
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

#[cfg(unix)]
fn automation_staged_log_fields(
    project_path: &Path,
    counts: crate::automation::staged_notice::AutomationPendingCounts,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "pending_fact_proposals",
            counts.pending_fact_proposals.to_string(),
        ),
        ("pending_skills", counts.pending_skills.to_string()),
    ]
}

/// After a scheduler tick where at least one task completed, emit a stable
/// `event=automation_staged` line with managed-skill review counts plus fact
/// proposal telemetry.
/// Silent when nothing is pending or the profile root is unavailable.
#[cfg(unix)]
async fn log_automation_staged_if_pending(project_path: &Path, dashboard_root: &Path) {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return;
    };
    let counts = crate::automation::staged_notice::count_pending_automation_output(
        dashboard_root,
        &profile_root,
    )
    .await;
    if counts.total() == 0 {
        return;
    }
    log_daemon_event(
        "automation_staged",
        &automation_staged_log_fields(project_path, counts),
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
    let mut routed_handshake = handshake.clone();
    if let Some(line) = replay_line {
        update_proxy_handshake_from_initialize(handshake, &mut routed_handshake, &line).await;
        proxy_request_line_to_daemon(socket_path, &routed_handshake, &line, transport).await?;
    }

    while let Some(line) = transport.read_line().await? {
        update_proxy_handshake_from_initialize(handshake, &mut routed_handshake, &line).await;
        proxy_request_line_to_daemon(socket_path, &routed_handshake, &line, transport).await?;
    }
    Ok(())
}

#[cfg(unix)]
async fn update_proxy_handshake_from_initialize(
    base_handshake: &DaemonHandshake,
    handshake: &mut DaemonHandshake,
    line: &str,
) {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(line.trim()) else {
        return;
    };
    if request.method != "initialize" {
        return;
    }
    *handshake = base_handshake.clone();
    if !base_handshake.allow_initialize_root_routing {
        return;
    }
    let Some((project_path, allow_init)) =
        resolve_proxy_initialize_route(request.params.as_ref(), &base_handshake.client_identity)
            .await
    else {
        return;
    };
    if base_handshake.project_path.as_deref() != Some(project_path.as_path()) {
        handshake.scope_prefix = None;
    }
    handshake.project_path = Some(project_path);
    handshake.allow_init = allow_init;
}

#[cfg(unix)]
async fn resolve_proxy_initialize_route(
    params: Option<&serde_json::Value>,
    client_identity: &DaemonClientIdentity,
) -> Option<(PathBuf, bool)> {
    let registry = crate::global_db::GlobalDb::open_at(&client_identity.global_db_path).await;
    if let Some(project_path) =
        crate::mcp::server::resolve_initialize_roots_project_path(params, registry.as_ref()).await
    {
        return Some((project_path, false));
    }

    for root in crate::mcp::server::initialize_root_paths(params) {
        if let Some(project_path) = crate::config::discover_project_root(&root) {
            return Some((project_path, false));
        }
        if let Some(git_root) = crate::worktree::git_worktree_root(&root) {
            let allow_init = crate::config::load_sync_config(&git_root).auto_init;
            return Some((git_root, allow_init));
        }
    }
    None
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
            message: "daemon closed the connection before returning a matching response \
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
    // Install the git-metadata watcher (design D3/D5). The daemon has no single
    // project root, so it uses the default `[sync]` config plus env overrides.
    // When `auto_watch` is off the watcher is inert.
    let git_watcher =
        git_watch::GitWatcher::new(crate::config::SyncConfig::default().with_env_overrides());
    git_watcher.spawn(crate::global_db::global_db_path()).await;
    // PR-branch auto-tracking runs independently of the metadata watcher: it is
    // gated per-project on `sync.auto_track_pr_branches` (default off), so this
    // loop is inert unless a project opts in.
    let pr_autotrack_task = pr_autotrack::spawn(crate::global_db::global_db_path());
    let engine = DaemonEngine::default()
        .with_git_watcher(git_watcher)
        .with_pr_autotrack_task(pr_autotrack_task)
        .await;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut client_tasks: JoinSet<Result<()>> = JoinSet::new();

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            completed = client_tasks.join_next(), if !client_tasks.is_empty() => {
                if let Some(completed) = completed {
                    log_client_task_result(completed);
                }
                continue;
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        };
        let engine = engine.clone();
        client_tasks.spawn(async move { Box::pin(serve_socket_client(stream, engine)).await });
    }
    engine.lifecycle.begin_draining();
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
    let clients_drained = drain_client_tasks(&mut client_tasks, DAEMON_CLIENT_DRAIN_DEADLINE).await;
    engine.lifecycle.wait_for_idle().await;
    // Client setup and in-flight requests may create schedulers or project
    // servers. Sweep owned background tasks only after all client work drains.
    engine.shutdown_background_tasks().await;
    if !clients_drained {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "client_drain_timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_CLIENT_DRAIN_DEADLINE.as_secs().to_string(),
                ),
                (
                    "checkpoint",
                    "skipped_active_clients_were_aborted".to_string(),
                ),
            ],
        );
        return Ok(());
    }
    // Graceful shutdown persists tokens-saved counters and checkpoints WALs
    // for every live project server sequentially; with many servers or large
    // WALs that can exceed systemd's stop timeout, which then sends `SIGKILL`
    // to the daemon. On timeout the shutdown future is dropped and we proceed
    // to exit: the remaining persistence is best-effort and the database WAL
    // keeps state crash-safe.
    let completed = timeout(DAEMON_SHUTDOWN_DEADLINE, engine.shutdown_servers())
        .await
        .is_ok();
    if !completed {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_SHUTDOWN_DEADLINE.as_secs().to_string(),
                ),
            ],
        );
    }
    Ok(())
}

#[cfg(unix)]
fn log_client_task_result(completed: std::result::Result<Result<()>, tokio::task::JoinError>) {
    let error = match completed {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error.to_string(),
        Err(error) if error.is_cancelled() => return,
        Err(error) => error.to_string(),
    };
    log_daemon_event(
        "daemon_client",
        &[("outcome", "error".to_string()), ("error", error)],
    );
}

#[cfg(unix)]
async fn drain_client_tasks(clients: &mut JoinSet<Result<()>>, deadline: Duration) -> bool {
    let drained = timeout(deadline, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await
    .is_ok();
    if drained {
        return true;
    }

    clients.abort_all();
    while let Some(completed) = clients.join_next().await {
        log_client_task_result(completed);
    }
    false
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
    lifecycle: DaemonLifecycle,
    /// Shared daemon state, partitioned by the client-scoped project server key.
    project_servers: Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, Arc<crate::mcp::McpServer>>>>,
    /// Background automation loops, partitioned with the same client/project identity as MCP state.
    automation_schedulers: Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, JoinHandle<()>>>>,
    /// Client versions whose skew was already logged. Proxy clients reconnect
    /// per request, so without this the mismatch would flood the daemon log.
    logged_client_version_skews: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Git-metadata watcher (design D3/D5). Default-constructed inert; the real
    /// config-driven watcher is installed by `run_foreground_unix` via
    /// [`DaemonEngine::with_git_watcher`] before the accept loop starts.
    git_watcher: git_watch::GitWatcher,
    /// PR reconciliation task, retained so shutdown never leaves it writing.
    pr_autotrack_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
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
    /// Installs the config-driven git-metadata watcher on this engine. Called
    /// once by `run_foreground_unix` before the accept loop.
    fn with_git_watcher(mut self, watcher: git_watch::GitWatcher) -> Self {
        self.git_watcher = watcher;
        self
    }

    async fn with_pr_autotrack_task(self, task: JoinHandle<()>) -> Self {
        *self.pr_autotrack_task.lock().await = Some(task);
        self
    }

    /// Returns the client version to log for this handshake, once per distinct
    /// skewed version; repeat connections from the same client return `None`.
    async fn client_version_skew_to_log(&self, handshake: &DaemonHandshake) -> Option<String> {
        let skew = client_version_skew(&handshake.client_version, binary_version())?;
        let mut logged = self.logged_client_version_skews.lock().await;
        logged.insert(skew.clone()).then_some(skew)
    }

    /// Logs a `daemon_version_skew` event when this handshake's client runs a
    /// different binary version, deduped per distinct client version.
    async fn log_client_version_skew(&self, handshake: &DaemonHandshake) {
        let Some(client_version) = self.client_version_skew_to_log(handshake).await else {
            return;
        };
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
            // A freshly-handshaken project should be watched even on a cache
            // hit (the watcher may have started after this server was cached).
            self.git_watcher
                .ensure_watching(&canonical_project_path)
                .await;
            Box::pin(self.ensure_automation_scheduler(
                key,
                canonical_project_path,
                handshake.clone(),
            ))
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
        let reconciler = self.automation_scheduler_reconciler(
            key.clone(),
            canonical_project_path.clone(),
            handshake.clone(),
        );
        let server = crate::mcp::McpServer::new_with_dbs_and_automation_reconciler(
            cg,
            handshake.scope_prefix.clone(),
            accounting_db,
            registry_db,
            false,
            Some(reconciler),
        )
        .await;
        servers.insert(key.clone(), Arc::clone(&server));
        drop(servers);
        self.git_watcher
            .ensure_watching(&canonical_project_path)
            .await;
        Box::pin(self.ensure_automation_scheduler(key, canonical_project_path, handshake.clone()))
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

        let configured = match Box::pin(automation_scheduler_has_work_for_project(
            &project_path,
            &handshake,
        ))
        .await
        {
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
        if !configured {
            log_daemon_event(
                "scheduler_config",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "skipped".to_string()),
                    ("reason", "not_configured".to_string()),
                ],
            );
            return;
        }

        self.start_automation_scheduler(key, project_path, handshake)
            .await;
    }

    fn automation_scheduler_reconciler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> crate::dashboard::AutomationSchedulerReconciler {
        let engine = self.clone();
        std::sync::Arc::new(move || {
            let engine = engine.clone();
            let key = key.clone();
            let project_path = project_path.clone();
            let handshake = handshake.clone();
            tokio::spawn(async move {
                engine
                    .ensure_automation_scheduler(key, project_path, handshake)
                    .await;
            });
        })
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

    async fn shutdown_background_tasks(&self) {
        let scheduler_handles: Vec<JoinHandle<()>> = {
            let mut schedulers = self.automation_schedulers.lock().await;
            schedulers.drain().map(|(_, handle)| handle).collect()
        };
        for handle in scheduler_handles {
            handle.abort();
            let _ = handle.await;
        }

        self.git_watcher.shutdown().await;
        if let Some(handle) = self.pr_autotrack_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn shutdown_servers(&self) {
        let servers: Vec<Arc<crate::mcp::McpServer>> = {
            let servers = self.project_servers.lock().await;
            let mut seen = HashSet::new();
            servers
                .values()
                .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
                .cloned()
                .collect()
        };
        for server in servers {
            server.shutdown().await;
        }
    }

    #[cfg(test)]
    async fn shutdown_all(&self) {
        self.lifecycle.begin_draining();
        self.shutdown_background_tasks().await;
        self.shutdown_servers().await;
    }
}

#[cfg(unix)]
async fn run_automation_scheduler_loop(project_path: PathBuf, handshake: DaemonHandshake) {
    loop {
        match Box::pin(automation_scheduler_has_work_for_project(
            &project_path,
            &handshake,
        ))
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                log_daemon_event(
                    "scheduler_exit",
                    &[
                        ("project", project_path.display().to_string()),
                        ("reason", "not_configured".to_string()),
                    ],
                );
                break;
            }
            Err(e) => {
                // A transient failure (e.g. a momentarily corrupt jobs file or
                // a project that cannot be opened this instant) must not
                // permanently kill the scheduler loop. Surface the cause and
                // retry on the next tick instead of exiting for good.
                log_daemon_event(
                    "scheduler_project_open",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "error".to_string()),
                        ("error", e.to_string()),
                    ],
                );
                tokio::time::sleep(Duration::from_secs(
                    crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS,
                ))
                .await;
                continue;
            }
        }
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
async fn automation_scheduler_has_work_for_project(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<bool> {
    let cg = Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await?;
    let config = effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
    automation_scheduler_has_work(&cg, &config).await
}

#[cfg(unix)]
async fn automation_scheduler_tick_secs_for_project(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> u64 {
    match Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await
    {
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

/// Minimum wall-clock interval between global-database retention passes,
/// shared across every project's scheduler loop so retention runs at most
/// this often no matter how many projects are active.
#[cfg(unix)]
const RETENTION_MIN_INTERVAL_SECS: u64 = 6 * 60 * 60;

#[cfg(unix)]
static LAST_GLOBAL_RETENTION: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// Returns whether a retention pass is due, recording `now` as the last run
/// when it is. The gate is process-global so N project loops do not each run
/// their own retention every tick.
#[cfg(unix)]
fn global_retention_pass_due(now: std::time::Instant) -> bool {
    let mut guard = match LAST_GLOBAL_RETENTION.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let due = guard.is_none_or(|last| {
        now.duration_since(last) >= Duration::from_secs(RETENTION_MIN_INTERVAL_SECS)
    });
    if due {
        *guard = Some(now);
    }
    due
}

/// Applies the configured retention windows to the global telemetry tables,
/// at most once per [`RETENTION_MIN_INTERVAL_SECS`]. Best-effort: retention is
/// housekeeping, so failures are logged and never abort a scheduler tick.
#[cfg(unix)]
async fn maybe_run_global_retention(
    project_path: &Path,
    config: &crate::automation::config::AutomationConfig,
) {
    if !global_retention_pass_due(std::time::Instant::now()) {
        return;
    }
    let Some(db) = crate::global_db::GlobalDb::open().await else {
        return;
    };
    let now_secs = crate::tracedecay::current_timestamp();
    match db.prune_global_retention(&config.retention, now_secs).await {
        Ok(reports) => {
            for report in reports {
                if report.applied && report.rows > 0 {
                    log_daemon_event(
                        "retention_prune",
                        &[
                            ("project", project_path.display().to_string()),
                            ("table", report.table.to_string()),
                            ("rows", report.rows.to_string()),
                            (
                                "window_days",
                                report
                                    .window_days
                                    .map_or_else(|| "unlimited".to_string(), |d| d.to_string()),
                            ),
                        ],
                    );
                }
            }
        }
        Err(e) => {
            log_daemon_event(
                "retention_prune",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
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
        CombinedReviewAutomationOptions, CombinedReviewDispatch, MemoryCuratorAutomationOptions,
        SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
        run_combined_review_with_backend, run_memory_curator_with_backend,
        run_session_reflector_with_backend, run_skill_writer_with_backend,
    };

    let cg = Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await?;
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
    if !automation_scheduler_has_work(&cg, &config).await? {
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
    maybe_run_global_retention(project_path, &config).await;
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let mut first_error: Option<TraceDecayError> = None;
    let mut any_succeeded = false;

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
        Ok(run) => {
            any_succeeded |= run.ledger_record.status
                == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
            log_daemon_scheduler_record(project_path, &run.ledger_record);
        }
        Err(e) => {
            log_scheduler_task_error(project_path, AgentTaskKind::MemoryCurator, &e);
            first_error.get_or_insert(e);
        }
    }
    // When both the reflector and the skill writer are due in this tick, the
    // combined path serves them with one backend call. Any other outcome
    // (combined mode disabled, only one task due, missing evidence) falls
    // back to the sequential per-task runs below.
    let mut combined_handled = false;
    if config.combine_due_tasks {
        log_scheduler_task_start(project_path, AgentTaskKind::CombinedReview);
        match run_combined_review_with_backend(
            &cg,
            &config,
            &backend,
            CombinedReviewAutomationOptions::default(),
        )
        .await
        {
            Ok(CombinedReviewDispatch::Ran(run)) => {
                any_succeeded |= run.session_reflector.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                any_succeeded |= run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
                log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
                combined_handled = true;
            }
            Ok(CombinedReviewDispatch::RecordedFailure { run, error }) => {
                any_succeeded |= run.session_reflector.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                any_succeeded |= run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
                log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
                log_scheduler_task_error(project_path, AgentTaskKind::CombinedReview, &error);
                first_error.get_or_insert(error);
                combined_handled = true;
            }
            Ok(CombinedReviewDispatch::NotCombined { reason }) => {
                log_daemon_event(
                    "scheduler_task",
                    &[
                        ("project", project_path.display().to_string()),
                        ("task", "combined_review".to_string()),
                        ("outcome", "not_combined".to_string()),
                        ("reason", reason.to_string()),
                    ],
                );
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::CombinedReview, &e);
            }
        }
    }
    if !combined_handled {
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
            Ok(run) => {
                any_succeeded |= run.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.ledger_record);
            }
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
            Ok(run) => {
                any_succeeded |= run.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.ledger_record);
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::SkillWriter, &e);
                first_error.get_or_insert(e);
            }
        }
    }
    if any_succeeded {
        log_automation_staged_if_pending(project_path, &cg.store_layout().dashboard_root).await;
    }
    run_user_jobs_scheduler_pass(
        project_path,
        &handshake.client_identity.profile_root,
        &cg,
        &config,
        &backend,
        &mut first_error,
    )
    .await;
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
fn user_config_for_client(
    client_identity: &DaemonClientIdentity,
) -> crate::user_config::UserConfig {
    let path = client_identity.profile_root.join("config.toml");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return crate::user_config::UserConfig::default();
    };
    crate::user_config::parse_or_warn_default(&path, &contents)
}

#[cfg(unix)]
fn automation_scheduler_configured(config: &crate::automation::config::AutomationConfig) -> bool {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};
    use crate::automation::scheduler::{AutomationSchedule, parse_schedule};

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
            Ok(AutomationSchedule::Interval { .. } | AutomationSchedule::Cron(_)) => true,
        }
    })
}

/// True when the scheduler loop has anything to do for this project: a
/// scheduled fixed task or a schedulable user-defined job.
#[cfg(unix)]
async fn automation_scheduler_has_work(
    cg: &crate::tracedecay::TraceDecay,
    config: &crate::automation::config::AutomationConfig,
) -> Result<bool> {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};

    if automation_scheduler_configured(config) {
        return Ok(true);
    }
    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return Ok(false);
    }
    crate::automation::jobs::jobs_configured_for_scheduler(&cg.store_layout().dashboard_root).await
}

/// Ticks every schedulable user-defined job with the same lock/cooldown
/// discipline as the fixed tasks (enforced inside the job runner).
#[cfg(unix)]
async fn run_user_jobs_scheduler_pass(
    project_path: &Path,
    profile_root: &Path,
    cg: &crate::tracedecay::TraceDecay,
    config: &crate::automation::config::AutomationConfig,
    backend: &crate::automation::backend::CodexAppServerBackend,
    first_error: &mut Option<TraceDecayError>,
) {
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let jobs = match crate::automation::jobs::load_jobs(&dashboard_root).await {
        Ok(jobs) => jobs,
        Err(e) => {
            log_daemon_event(
                "scheduler_user_jobs",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
            first_error.get_or_insert(e);
            return;
        }
    };
    for job in jobs
        .iter()
        .filter(|job| crate::automation::jobs::job_is_schedulable(job))
    {
        log_scheduler_task_start(
            project_path,
            crate::automation::backend::AgentTaskKind::UserJob,
        );
        match crate::automation::jobs::run_user_job_with_backend(
            &dashboard_root,
            config,
            backend,
            job,
            crate::automation::jobs::UserJobRunOptions {
                trigger: crate::automation::run_ledger::AutomationTrigger::Scheduler,
                profile_root: Some(profile_root.to_path_buf()),
                project_root: Some(project_path.to_path_buf()),
                ..crate::automation::jobs::UserJobRunOptions::default()
            },
        )
        .await
        {
            Ok(run) => log_daemon_scheduler_record(project_path, &run.ledger_record),
            Err(e) => {
                log_scheduler_task_error(
                    project_path,
                    crate::automation::backend::AgentTaskKind::UserJob,
                    &e,
                );
                first_error.get_or_insert(e);
            }
        }
    }
}

#[cfg(unix)]
async fn serve_socket_client(stream: tokio::net::UnixStream, engine: DaemonEngine) -> Result<()> {
    let mut transport = UnixStreamTransport::new(stream);
    let line = tokio::select! {
        result = transport.read_line() => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(line) = line else {
        return Ok(());
    };
    let Some(setup_activity) = engine.lifecycle.try_enter() else {
        return Ok(());
    };
    let handshake = DaemonHandshake::from_line(&line)?;
    engine.log_client_version_skew(&handshake).await;
    let server = if handshake.project_path.is_some() {
        let server = match Box::pin(engine.project_server(&handshake)).await {
            Ok(server) => server,
            Err(e) => {
                write_project_open_error(&mut transport, &e).await?;
                return Err(e);
            }
        };
        Some(server)
    } else {
        None
    };
    drop(setup_activity);
    if !engine.lifecycle.accepting() {
        return Ok(());
    }

    if let Some(server) = server {
        Box::pin(server.run_daemon_connection_with_timings(
            &mut transport,
            handshake.timings,
            &engine.lifecycle,
        ))
        .await?;
    } else {
        serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            &engine.lifecycle,
        )
        .await?;
    }
    Ok(())
}

async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<crate::tracedecay::TraceDecay> {
    let open_options = handshake.open_options();
    match Box::pin(open_existing_project_with_options(
        project_path,
        open_options.clone(),
    ))
    .await
    {
        Ok(cg) => Ok(cg),
        Err(open_err) if handshake.allow_init && is_missing_index_error(&open_err) => {
            match crate::tracedecay::TraceDecay::init_and_index_with_options(
                project_path,
                open_options,
            )
            .await
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
                || message.contains("parent DB not found")
                || (message.contains("parent branch '") && message.contains("' has no DB"))
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
    lifecycle: &DaemonLifecycle,
) -> Result<()> {
    loop {
        let line = tokio::select! {
            result = transport.read_line() => result?,
            () = lifecycle.wait_for_draining() => break,
        };
        let Some(line) = line else {
            break;
        };
        let Some(_activity) = lifecycle.try_enter() else {
            break;
        };
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
        if !lifecycle.accepting() {
            break;
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
mod tests;

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};

use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::ReplayTransport;
use crate::mcp::server::{McpMethod, SERVER_INSTRUCTIONS, classify_mcp_method, initialize_result};
use crate::mcp::tools::{
    explore_call_budget, get_tool_definitions_with_budget, get_tool_definitions_with_warming_budget,
};
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport, StdioTransport};
use branch_add::{branch_add_response, coordinated_hook_branch_writer, parse_branch_add_request};
use branch_admin::{StoreAdministration, parse_branch_admin_request, write_branch_admin_response};
#[cfg(unix)]
use scheduler::AutomationSchedulerHandle;
#[cfg(all(unix, test))]
use scheduler::{
    automation_scheduler_configured, automation_scheduler_tick_secs_for_project,
    automation_staged_log_fields, daemon_scheduler_record_log_line, run_automation_scheduler_tick,
    scheduler_task_log_fields, user_config_for_client,
};
use transport::{BrokerListener, BrokerStream, DaemonAuthPreface, DaemonEndpoint};

pub const SERVICE_NAME: &str = "tracedecay.service";
pub const SOCKET_ENV: &str = "TRACEDECAY_DAEMON_SOCKET";
pub const HOOK_EVENT_METHOD: &str = "tracedecay/hookEvent";
#[cfg(unix)]
const TOOL_LIST_CHANGED_METHOD: &str = "notifications/tools/list_changed";
#[cfg(unix)]
const MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION: usize = 1_024;
const HOOK_EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);
const DAEMON_TOOL_LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(5);
const DAEMON_TOOL_HEALTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

fn coordinated_dashboard_automation_writer(
    administration: StoreAdministration,
) -> crate::dashboard::DashboardAutomationWriter {
    Arc::new(move |operation| {
        let administration = administration.clone();
        Box::pin(async move { administration.with_writer(operation).await })
    })
}

fn coordinated_background_refresh_writer(
    administration: StoreAdministration,
) -> crate::mcp::server::BackgroundRefreshWriter {
    Arc::new(move |request| {
        let administration = administration.clone();
        Box::pin(async move {
            administration
                .with_writer(|| async move {
                    crate::mcp::server::execute_background_refresh_direct(request).await
                })
                .await
        })
    })
}

/// Upper bound on graceful-shutdown persistence work (per-server token
/// persistence and WAL checkpoints). Must stay comfortably below systemd's
/// stop timeout (90s by default) so the daemon exits cleanly instead of
/// being killed with `SIGKILL` mid-checkpoint.
#[cfg(unix)]
const DAEMON_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(45);
const DAEMON_CLIENT_DRAIN_DEADLINE: Duration = Duration::from_secs(15);
#[cfg(unix)]
const DAEMON_TASK_ABORT_DEADLINE: Duration = Duration::from_secs(2);
/// How long a project open may queue behind an unrelated writer before the
/// client is told to retry. The open itself keeps running in the background.
#[cfg(unix)]
const CONTENDED_PROJECT_OPEN_GRACE: Duration = Duration::from_millis(500);

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

mod authority;
mod branch_add;
mod branch_admin;
#[cfg(unix)]
mod git_watch;
#[cfg(unix)]
pub mod pr_autotrack;
#[cfg(unix)]
mod scheduler;
mod service;
pub(crate) mod transport;
pub use service::{
    DaemonServiceSpec, DaemonServiceState, daemon_reachable, default_socket_path, install_service,
    installed_service_socket_path, quiesce_installed_service_for_restart,
    quiesce_installed_service_under_lease, refresh_installed_service,
    refresh_installed_service_under_lease, refresh_installed_service_under_lease_with_state,
    refresh_service, restore_quiesced_installed_service, service_spec, service_status,
    socket_path_or_default, uninstall_service,
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
    Hermes,
}

impl HookAgent {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Kiro => "kiro",
            Self::Hermes => "hermes",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            "kiro" => Some(Self::Kiro),
            "hermes" => Some(Self::Hermes),
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
            Self::Hermes => ".hermes_terminal_receipt_at",
        }
    }
}

pub use tracedecay_agent_hosts::automation::host_receipts::{
    HookRouteMetadata, HookTerminalReceipt,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<HookRouteMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<HookTerminalReceipt>,
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
            receipt: None,
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

    /// A provider session started: let the daemon own branch tracking and
    /// index refresh for the session's actual working directory.
    pub fn session_start(agent: HookAgent, cwd: PathBuf) -> Self {
        Self::new(agent, "sessionStart", Vec::new(), None, Some(cwd))
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

    pub fn hermes_terminal_receipt(
        cwd: PathBuf,
        route: HookRouteMetadata,
        receipt: HookTerminalReceipt,
    ) -> Self {
        let mut event = Self::new(
            HookAgent::Hermes,
            "terminalReceipt",
            Vec::new(),
            None,
            Some(cwd),
        );
        event.route = Some(route);
        event.receipt = Some(receipt);
        event
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
    /// Stable id for the connecting client process. A stdio MCP proxy reuses
    /// this across its per-request daemon connections, allowing one
    /// generation-local catalog refresh notification instead of one per
    /// request. Old clients omit it and deserialize to an empty string.
    #[serde(default)]
    pub client_instance_id: String,
    /// Whether this proxy already forwarded an initialize response declaring
    /// `tools.listChanged=true` to its MCP host.
    #[serde(default)]
    pub tool_list_changed_capable: bool,
    /// Daemon version whose initialize response established the host's
    /// current tool catalog. A nonempty value proves explicit negotiation;
    /// generation-local daemon state decides whether a refresh is due.
    #[serde(default)]
    pub catalog_version: String,
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
            client_instance_id: crate::runtime_identity::process_run_id().to_string(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
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
fn release_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version
        .strip_prefix('v')
        .unwrap_or(version)
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(unix)]
fn version_skew_action(daemon_version: &str, client_version: &str) -> &'static str {
    match release_version(daemon_version)
        .zip(release_version(client_version))
        .map(|(daemon, client)| daemon.cmp(&client))
    {
        Some(std::cmp::Ordering::Greater) => {
            "restart or reconnect the MCP host so it loads the current TraceDecay client and tool catalog"
        }
        Some(std::cmp::Ordering::Less) => {
            "run `tracedecay daemon restart` to load the current daemon binary"
        }
        _ => "restart or reconnect whichever TraceDecay component is stale",
    }
}

pub async fn notify_hook_event(project_path: &Path, event: DaemonHookEvent) {
    let _ = timeout(
        HOOK_EVENT_NOTIFY_TIMEOUT,
        notify_hook_event_inner(project_path, event),
    )
    .await;
}

async fn notify_hook_event_inner(project_path: &Path, event: DaemonHookEvent) {
    #[cfg(unix)]
    let connection = std::env::var_os(SOCKET_ENV)
        .filter(|path| !path.is_empty())
        .map(|path| connection_for_socket_path(Path::new(&path)))
        .map(Ok)
        .unwrap_or_else(current_daemon_connection);
    #[cfg(not(unix))]
    let connection = current_daemon_connection();
    let Ok(connection) = connection else {
        return;
    };
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
    let Ok(stream) = BrokerStream::connect(&connection.endpoint).await else {
        return;
    };
    let (_reader, mut writer) = stream.into_split();
    if write_daemon_preamble(&mut writer, &connection, &handshake)
        .await
        .is_err()
    {
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

pub fn unavailable_error(socket_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon socket '{}' is not available. Run `tracedecay daemon install-service` and ensure the service is running.",
            socket_path.display()
        ),
    }
}

#[derive(Clone)]
struct DaemonConnection {
    endpoint: DaemonEndpoint,
    auth_token: Option<String>,
    authority_record: Option<authority::DaemonAuthorityRecord>,
}

fn current_daemon_connection() -> Result<DaemonConnection> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let record =
        authority::current_record(&profile_root)?.ok_or_else(|| TraceDecayError::Config {
            message:
                "TraceDecay daemon authority record is not available. Start or restart the daemon."
                    .to_string(),
        })?;
    Ok(DaemonConnection {
        endpoint: record.endpoint.clone(),
        auth_token: Some(record.auth_token.clone()),
        authority_record: Some(record),
    })
}

#[cfg(unix)]
fn connection_for_socket_path(socket_path: &Path) -> DaemonConnection {
    if let Ok(connection) = current_daemon_connection()
        && let DaemonEndpoint::Unix(authority_path) = &connection.endpoint
        && authority::canonical_identity_path(authority_path).ok()
            == authority::canonical_identity_path(socket_path).ok()
    {
        return connection;
    }
    if let Some(profile_root) = socket_path.parent()
        && let Ok(Some(record)) = authority::current_record(profile_root)
        && let DaemonEndpoint::Unix(authority_path) = &record.endpoint
        && authority::canonical_identity_path(authority_path).ok()
            == authority::canonical_identity_path(socket_path).ok()
    {
        return DaemonConnection {
            endpoint: record.endpoint.clone(),
            auth_token: Some(record.auth_token.clone()),
            authority_record: Some(record),
        };
    }
    // Explicit paths are retained for test harnesses and legacy one-shot
    // callers without a discoverable authority record. Default production
    // routing always uses the authority record.
    DaemonConnection {
        endpoint: DaemonEndpoint::Unix(socket_path.to_path_buf()),
        auth_token: None,
        authority_record: None,
    }
}

async fn ensure_daemon_connection_live(
    connection: &DaemonConnection,
    request_label: &str,
) -> Result<()> {
    if let Some(expected) = connection.authority_record.as_ref() {
        let current = authority::current_record(&expected.profile_root)?;
        let Some(current) = current else {
            return Err(TraceDecayError::Config {
                message: format!(
                    "daemon authority disappeared while request '{request_label}' was awaiting a response; the request was already sent and was not retried"
                ),
            });
        };
        if current.epoch != expected.epoch || current.process_run_id != expected.process_run_id {
            return Err(TraceDecayError::Config {
                message: format!(
                    "daemon restarted while request '{request_label}' was awaiting a response (expected epoch {}, current epoch {}); the request was already sent and was not retried",
                    expected.epoch, current.epoch
                ),
            });
        }
    }

    timeout(
        DAEMON_TOOL_HEALTH_CONNECT_TIMEOUT,
        BrokerStream::connect(&connection.endpoint),
    )
    .await
    .map_err(|_| TraceDecayError::Config {
        message: format!(
            "daemon health check timed out at '{}' while request '{request_label}' was awaiting a response; the request was already sent and was not retried",
            connection.endpoint
        ),
    })?
    .map(|_| ())
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "daemon became unreachable at '{}' while request '{request_label}' was awaiting a response: {error}; the request was already sent and was not retried",
            connection.endpoint
        ),
    })
}

async fn next_daemon_response_line<R>(
    lines: &mut tokio::io::Lines<R>,
    connection: &DaemonConnection,
    request_label: &str,
    liveness_poll_interval: Duration,
) -> Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match timeout(liveness_poll_interval, lines.next_line()).await {
            Ok(line) => return line.map_err(Into::into),
            Err(_) => ensure_daemon_connection_live(connection, request_label).await?,
        }
    }
}

fn client_connection(socket_path: &Path) -> Result<DaemonConnection> {
    #[cfg(unix)]
    {
        Ok(connection_for_socket_path(socket_path))
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        current_daemon_connection()
    }
}

async fn write_daemon_preamble(
    writer: &mut tokio::io::WriteHalf<BrokerStream>,
    connection: &DaemonConnection,
    handshake: &DaemonHandshake,
) -> Result<()> {
    if let Some(token) = connection.auth_token.as_deref() {
        writer
            .write_all(DaemonAuthPreface::new(token).to_line()?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
    }
    writer.write_all(handshake.to_line()?.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

fn default_available_socket_path() -> Result<PathBuf> {
    let socket_path = default_socket_path()?;
    #[cfg(unix)]
    {
        if socket_path.exists() {
            Ok(socket_path)
        } else {
            Err(unavailable_error(&socket_path))
        }
    }
    #[cfg(not(unix))]
    {
        current_daemon_connection()?;
        Ok(socket_path)
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
const DAEMON_RESTART_GRACE: Duration = Duration::from_secs(8);
const DAEMON_RESTART_POLL_INTERVAL: Duration = Duration::from_millis(200);

fn is_transient_daemon_connect_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

async fn connect_to_current_daemon(socket_path: &Path) -> Result<(DaemonConnection, BrokerStream)> {
    connect_with_restart_grace_resolving(
        || client_connection(socket_path),
        DAEMON_RESTART_GRACE,
        DAEMON_RESTART_POLL_INTERVAL,
    )
    .await
}

/// Connects to the daemon socket, tolerating a short restart outage.
///
/// Retrying here is safe: nothing has been written yet, so no request can be
/// duplicated. Non-transient errors (e.g. permission denied) fail immediately.
async fn connect_with_restart_grace(
    connection: &DaemonConnection,
    grace: Duration,
    poll_interval: Duration,
) -> Result<BrokerStream> {
    let (_, stream) =
        connect_with_restart_grace_resolving(|| Ok(connection.clone()), grace, poll_interval)
            .await?;
    Ok(stream)
}

/// Resolves the current authority record on every connect attempt.
///
/// A daemon restart replaces both its endpoint authority epoch and auth token.
/// Resolving only before the restart window would connect to the new socket
/// with stale credentials after it rebinds.
async fn connect_with_restart_grace_resolving(
    mut resolve: impl FnMut() -> Result<DaemonConnection>,
    grace: Duration,
    poll_interval: Duration,
) -> Result<(DaemonConnection, BrokerStream)> {
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        let connection = resolve()?;
        match BrokerStream::connect(&connection.endpoint).await {
            Ok(stream) => return Ok((connection, stream)),
            Err(TraceDecayError::Io(err)) => {
                if !is_transient_daemon_connect_error(err.kind())
                    || tokio::time::Instant::now() >= deadline
                {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "could not connect to TraceDecay daemon endpoint '{}': {err}. The daemon may be restarting (e.g. after `tracedecay update`) — retry shortly, or check `tracedecay daemon status`.",
                            connection.endpoint
                        ),
                    });
                }
                tokio::time::sleep(poll_interval).await;
            }
            Err(error) => return Err(error),
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
    let connection = connection_for_socket_path(socket_path);
    connect_with_restart_grace(&connection, grace, poll_interval)
        .await
        .is_ok()
}

#[cfg(any(test, not(unix)))]
fn proxy_required_by_platform(transport_supported: bool, endpoint_exists: bool) -> bool {
    !transport_supported || endpoint_exists
}

/// Non-Unix clients always use the authenticated loopback broker. There is no
/// in-process SQLite fallback.
#[cfg(not(unix))]
pub async fn should_proxy_serve_to_daemon(socket_path: &Path) -> bool {
    proxy_required_by_platform(false, socket_path.exists())
}

#[cfg(unix)]
pub async fn run_foreground(socket_path: PathBuf) -> Result<()> {
    run_foreground_unix(socket_path).await
}

#[cfg(not(unix))]
pub async fn run_foreground(_socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let requested = transport::default_loopback_endpoint();
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &requested, binary_version())?;
    let _lifecycle_lease = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let (listener, endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&endpoint)?;
    log_daemon_event("daemon_listening", &[("endpoint", endpoint.to_string())]);

    let lifecycle = DaemonLifecycle::default();
    let store_administration = StoreAdministration::default();
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
    let mut clients: JoinSet<Result<()>> = JoinSet::new();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(Err(error)) = completed {
                    log_daemon_event("daemon_client", &[("outcome", error.to_string())]);
                }
                continue;
            },
            _ = tokio::signal::ctrl_c() => break,
        };
        let auth_token = authority.auth_token().to_string();
        let client_lifecycle = lifecycle.clone();
        let store_administration = store_administration.clone();
        let project_open_gates = Arc::clone(&project_open_gates);
        clients.spawn(async move {
            serve_windows_broker_client(
                stream,
                &auth_token,
                &client_lifecycle,
                store_administration,
                project_open_gates,
                #[cfg(test)]
                None,
            )
            .await
        });
    }
    lifecycle.begin_draining();
    let in_flight_drained = timeout(DAEMON_CLIENT_DRAIN_DEADLINE, lifecycle.wait_for_idle())
        .await
        .is_ok();
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    if !in_flight_drained {
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
        return endpoint_cleanup;
    }
    shutdown_project_servers(&store_administration).await;
    endpoint_cleanup
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
        reset_proxy_handshake_for_initialize(handshake, &mut routed_handshake, &line);
        let metadata =
            proxy_request_line_to_daemon(socket_path, &routed_handshake, &line, transport).await?;
        apply_proxy_initialize_metadata(&mut routed_handshake, metadata);
    }

    while let Some(line) = transport.read_line().await? {
        reset_proxy_handshake_for_initialize(handshake, &mut routed_handshake, &line);
        let metadata =
            proxy_request_line_to_daemon(socket_path, &routed_handshake, &line, transport).await?;
        apply_proxy_initialize_metadata(&mut routed_handshake, metadata);
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Default)]
struct ProxyInitializeMetadata {
    daemon_version: Option<String>,
    tool_list_changed: bool,
    route: Option<InitializeRouteMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeRouteMetadata {
    project_path: PathBuf,
    allow_init: bool,
}

#[cfg(unix)]
fn apply_proxy_initialize_metadata(
    handshake: &mut DaemonHandshake,
    metadata: ProxyInitializeMetadata,
) {
    if let Some(route) = metadata.route {
        if handshake.project_path.as_deref() != Some(route.project_path.as_path()) {
            handshake.scope_prefix = None;
        }
        handshake.project_path = Some(route.project_path);
        handshake.allow_init = route.allow_init;
    }
    if metadata.tool_list_changed {
        handshake.tool_list_changed_capable = true;
        if let Some(version) = metadata.daemon_version {
            handshake.catalog_version = version;
        }
    }
}

#[cfg(unix)]
fn reset_proxy_handshake_for_initialize(
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
}

async fn resolve_daemon_initialize_route(
    params: Option<&serde_json::Value>,
    registry: Option<&crate::global_db::GlobalDb>,
) -> Option<InitializeRouteMetadata> {
    let roots = crate::mcp::server::initialize_root_paths(params);
    if let Some(registry) = registry {
        for root in &roots {
            let mut candidate = root.canonicalize().unwrap_or_else(|_| root.clone());
            loop {
                if registry
                    .project_registry_context_by_alias(&candidate)
                    .await
                    .is_some()
                {
                    return Some(InitializeRouteMetadata {
                        project_path: candidate,
                        allow_init: false,
                    });
                }
                if !candidate.pop() {
                    break;
                }
            }
            if let Some(identity) = crate::worktree::git_repo_identity(root) {
                if registry
                    .project_registry_context_by_identity(
                        &identity.worktree_root,
                        Some(&identity.common_dir),
                    )
                    .await
                    .is_some()
                {
                    return Some(InitializeRouteMetadata {
                        project_path: identity.worktree_root,
                        allow_init: false,
                    });
                }
            }
        }
    }
    if let Some(project_path) =
        crate::mcp::server::resolve_initialize_roots_project_path(params, registry).await
    {
        return Some(InitializeRouteMetadata {
            project_path,
            allow_init: false,
        });
    }

    for root in roots {
        if let Some(project_path) = crate::config::discover_project_root(&root) {
            return Some(InitializeRouteMetadata {
                project_path,
                allow_init: false,
            });
        }
        if let Some(identity) = crate::worktree::git_repo_identity(&root) {
            let allow_init = crate::config::load_sync_config(&identity.worktree_root).auto_init;
            return Some(InitializeRouteMetadata {
                project_path: identity.worktree_root,
                allow_init,
            });
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
) -> Result<ProxyInitializeMetadata> {
    if line.trim().is_empty() {
        return Ok(ProxyInitializeMetadata::default());
    }

    match send_daemon_request_line(socket_path, handshake, line).await {
        Ok(responses) => {
            let metadata = proxy_initialize_metadata(line, &responses);
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
            Ok(metadata)
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
            Ok(ProxyInitializeMetadata::default())
        }
    }
}

async fn send_daemon_request_line(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
) -> Result<Vec<String>> {
    send_daemon_request_line_with_liveness_poll(
        socket_path,
        handshake,
        line,
        DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
    )
    .await
}

async fn send_daemon_request_line_with_liveness_poll(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
    liveness_poll_interval: Duration,
) -> Result<Vec<String>> {
    let (connection, stream) = connect_to_current_daemon(socket_path).await?;
    let (reader, mut writer) = stream.into_split();

    write_daemon_preamble(&mut writer, &connection, handshake).await?;
    writer.write_all(line.as_bytes()).await?;
    if !line.ends_with('\n') {
        writer.write_all(b"\n").await?;
    }
    writer.flush().await?;
    writer.shutdown().await?;

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let request = serde_json::from_str::<JsonRpcRequest>(line).ok();
    let request_id = request.as_ref().and_then(|request| request.id.clone());
    let request_label = request
        .as_ref()
        .map(|request| request.method.as_str())
        .unwrap_or("daemon request");
    let mut responses = Vec::new();
    let mut matched_response = request_id.is_none();
    while let Some(response_line) = next_daemon_response_line(
        &mut lines,
        &connection,
        request_label,
        liveness_poll_interval,
    )
    .await?
    {
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
            message: "daemon closed the connection after the request was sent but before returning a matching response; the outcome is unknown and the request was not retried"
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
fn proxy_initialize_metadata(request_line: &str, responses: &[String]) -> ProxyInitializeMetadata {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(request_line) else {
        return ProxyInitializeMetadata::default();
    };
    if request.method != "initialize" {
        return ProxyInitializeMetadata::default();
    }
    let mut metadata = ProxyInitializeMetadata::default();
    for line in responses {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if metadata.daemon_version.is_none() {
            metadata.daemon_version = value
                .pointer("/result/serverInfo/version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
        metadata.tool_list_changed |= value
            .pointer("/result/capabilities/tools/listChanged")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if metadata.route.is_none() {
            metadata.route = value
                .pointer("/result/_meta/tracedecayInitializeRoute")
                .cloned()
                .and_then(|route| serde_json::from_value(route).ok());
        }
    }
    metadata
}

#[cfg(unix)]
fn daemon_version_from_initialize_response(
    request_line: &str,
    responses: &[String],
) -> Option<String> {
    proxy_initialize_metadata(request_line, responses).daemon_version
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
    let action = version_skew_action(&daemon_version, client_version);
    Some(format!(
        "TraceDecay daemon is version {daemon_version} but this client is {client_version} — \
         {action}"
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
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    let mut transport = StdioTransport::new();
    if let Some(line) = replay_line {
        proxy_one_request(socket_path, handshake, &line, &mut transport).await?;
    }
    while let Some(line) = transport.read_line().await? {
        proxy_one_request(socket_path, handshake, &line, &mut transport).await?;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn proxy_one_request(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
    transport: &mut impl McpTransport,
) -> Result<()> {
    if line.trim().is_empty() {
        return Ok(());
    }
    for response in send_daemon_request_line(socket_path, handshake, line).await? {
        transport.write_line(&response).await?;
        if !response.ends_with('\n') {
            transport.write_line("\n").await?;
        }
    }
    transport.flush().await?;
    Ok(())
}

pub async fn proxy_stdio_to_default_daemon(
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    let socket_path = default_available_socket_path()?;
    proxy_stdio_to_daemon(&socket_path, handshake, replay_line).await
}

pub async fn call_tool(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    call_tool_with_liveness_poll(
        socket_path,
        handshake,
        tool_name,
        arguments,
        DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
    )
    .await
}

async fn call_tool_with_liveness_poll(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
    liveness_poll_interval: Duration,
) -> Result<serde_json::Value> {
    let (connection, stream) = connect_to_current_daemon(socket_path).await?;
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

    write_daemon_preamble(&mut writer, &connection, handshake).await?;
    writer
        .write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    writer.shutdown().await?;

    let mut lines = tokio::io::BufReader::new(reader).lines();
    loop {
        let line =
            next_daemon_response_line(&mut lines, &connection, tool_name, liveness_poll_interval)
                .await?;
        let Some(line) = line else {
            return Err(TraceDecayError::Config {
                message: "daemon closed the connection after the tool request was sent but before returning a result; the outcome is unknown and the request was not retried"
                    .to_string(),
            });
        };
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
}

pub async fn call_default_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    call_tool(&socket_path, handshake, tool_name, arguments).await
}

/// Extracts the single JSON payload from an MCP tool result while ignoring
/// human-facing notice blocks.
#[doc(hidden)]
pub fn tool_json_payload(
    result: &serde_json::Value,
    tool_name: &str,
) -> crate::errors::Result<serde_json::Value> {
    let blocks = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no content blocks"),
        })?;
    let mut payloads = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .filter_map(|text| serde_json::from_str(text).ok());
    let payload = payloads
        .next()
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no JSON payload"),
        })?;
    if payloads.next().is_some() {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned multiple JSON payloads"),
        });
    }
    Ok(payload)
}

#[cfg(unix)]
async fn run_foreground_unix(socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let endpoint = transport::DaemonEndpoint::Unix(socket_path);
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &endpoint, binary_version())?;
    let _lifecycle = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path.clone(),
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
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
    prepare_socket_path(&authority).await?;

    let (listener, bound_endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&bound_endpoint)?;
    log_daemon_event(
        "daemon_listening",
        &[("endpoint", bound_endpoint.to_string())],
    );
    let engine = DaemonEngine::default();
    // Install the git-metadata watcher (design D3/D5). The daemon has no single
    // project root, so it uses the default `[sync]` config plus env overrides.
    // When `auto_watch` is off the watcher is inert. The watcher shares the
    // engine's administration coordinator before it can spawn any writer.
    let git_watcher = git_watch::GitWatcher::new_with_administration(
        crate::config::SyncConfig::default().with_env_overrides(),
        engine.store_administration.clone(),
        profile_root.clone(),
    );
    git_watcher.spawn(crate::global_db::global_db_path()).await;
    // PR-branch auto-tracking runs independently of the metadata watcher: it is
    // gated per-project on `sync.auto_track_pr_branches` (default off), so this
    // loop is inert unless a project opts in.
    let pr_autotrack_task = pr_autotrack::spawn_with_administration(
        crate::global_db::global_db_path(),
        engine.store_administration.clone(),
    );
    let engine = engine
        .with_git_watcher(git_watcher)
        .with_pr_autotrack_task(pr_autotrack_task)
        .await;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut client_tasks: JoinSet<Result<()>> = JoinSet::new();

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
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
        let auth_token = authority.auth_token().to_string();
        client_tasks.spawn(async move {
            Box::pin(serve_authenticated_socket_client(
                stream, engine, auth_token,
            ))
            .await
        });
    }
    engine.lifecycle.begin_draining();
    // Stop accepting and unlink the socket before draining so clients that
    // connect during shutdown get NotFound/ConnectionRefused (which they retry
    // via `connect_with_restart_grace`) instead of a queued connection that
    // will never be served.
    drop(listener);
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    // Keep auxiliary process creation blocked until every scheduler and client
    // task is drained or abandoned. A killed app-server call may retry before
    // unwinding, so a shorter guard leaves a shutdown-time respawn race.
    let _codex_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
    // Stop automation before announcing shutdown or waiting for clients.
    // Scheduler tasks may be inside a synchronous auxiliary-agent call, so
    // shutdown also terminates their tracked process trees before joining.
    engine.shutdown_automation_schedulers().await;
    log_daemon_event(
        "daemon_shutdown",
        &[("socket", socket_path.display().to_string())],
    );
    let in_flight_drained = timeout(
        DAEMON_CLIENT_DRAIN_DEADLINE,
        engine.lifecycle.wait_for_idle(),
    )
    .await
    .is_ok();
    // Once admitted requests are finished (or their bound elapsed), every
    // remaining client task is an idle socket reader or already-cancelled
    // request wrapper. Abort those immediately instead of making shutdown wait
    // for clients to close persistent connections themselves.
    client_tasks.abort_all();
    let clients_drained = drain_client_tasks(&mut client_tasks, DAEMON_TASK_ABORT_DEADLINE).await;
    // Client setup and in-flight requests may create schedulers or project
    // servers. Sweep owned background tasks only after all client work drains.
    engine.shutdown_background_tasks().await;
    if !in_flight_drained || !clients_drained {
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
        return endpoint_cleanup;
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
    endpoint_cleanup
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
    let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await;
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
async fn prepare_socket_path(authority: &authority::DaemonAuthority) -> Result<()> {
    authority.ensure_current()?;
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path,
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
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
    /// One coordinator owns the project-server registry, scheduler registry,
    /// and the writer gate that orders all mutations of either identity map.
    store_administration: StoreAdministration,
    /// Per-canonical-route singleflight gates. Weak entries disappear after
    /// the last waiter, so failed opens are never cached.
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    #[cfg(test)]
    project_open_attempts: Arc<AtomicUsize>,
    /// Client versions whose skew was already logged. Proxy clients reconnect
    /// per request, so without this the mismatch would flood the daemon log.
    logged_client_version_skews: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Client processes already told to refresh their tool catalog during
    /// this daemon generation. The set is process-local by design: a daemon
    /// restart creates a new generation and permits one fresh notification.
    catalog_refresh_notified_clients: Arc<tokio::sync::Mutex<HashSet<CatalogRefreshClientKey>>>,
    /// Prevents capacity exhaustion from flooding the daemon log.
    catalog_refresh_saturation_logged: Arc<AtomicBool>,
    /// Git-metadata watcher (design D3/D5). Default-constructed inert; the real
    /// config-driven watcher is installed by `run_foreground_unix` via
    /// [`DaemonEngine::with_git_watcher`] before the accept loop starts.
    git_watcher: git_watch::GitWatcher,
    /// PR reconciliation task, retained so shutdown never leaves it writing.
    pr_autotrack_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectServerKey {
    owner: StoreOwnerKey,
    scope_prefix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct StoreOwnerKey {
    profile_root: PathBuf,
    global_db_path: PathBuf,
    project_id: Option<String>,
    store_root: PathBuf,
    graph_db_path: PathBuf,
}

/// A client route known before any project database is opened. This is the
/// cache/singleflight key; [`ProjectServerKey`] remains the post-open physical
/// owner key so linked aliases and branch DBs still converge correctly.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectRouteKey {
    profile_root: PathBuf,
    global_db_path: PathBuf,
    project_path: PathBuf,
    scope_prefix: Option<String>,
}

type ProjectOpenGate = tokio::sync::Mutex<()>;
type ProjectOpenGates = HashMap<ProjectRouteKey, std::sync::Weak<ProjectOpenGate>>;

/// Scope-specific MCP servers routed through one canonical physical DB owner.
/// `Database` performs the actual same-process handle sharing; this registry
/// keeps daemon cache aliases and branch-drift rekeys consistent with it.
struct DatabaseOwnerRegistry<Server = Arc<crate::mcp::McpServer>> {
    servers: HashMap<ProjectServerKey, Server>,
    aliases: HashMap<ProjectRouteKey, ProjectServerKey>,
}

impl<Server> Default for DatabaseOwnerRegistry<Server> {
    fn default() -> Self {
        Self {
            servers: HashMap::new(),
            aliases: HashMap::new(),
        }
    }
}

impl<Server> DatabaseOwnerRegistry<Server> {
    fn get(&self, key: &ProjectServerKey) -> Option<&Server> {
        self.servers.get(key)
    }

    fn insert(&mut self, key: ProjectServerKey, server: Server) {
        self.servers.insert(key, server);
    }

    fn get_route(&self, route: &ProjectRouteKey) -> Option<(&ProjectServerKey, &Server)> {
        let key = self.aliases.get(route)?;
        self.servers.get_key_value(key)
    }

    fn bind_route(&mut self, route: ProjectRouteKey, key: ProjectServerKey) {
        debug_assert!(self.servers.contains_key(&key));
        self.aliases.insert(route, key);
    }

    fn insert_route(&mut self, route: ProjectRouteKey, key: ProjectServerKey, server: Server) {
        self.insert(key.clone(), server);
        self.bind_route(route, key);
    }

    fn bind_or_insert_route(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        candidate: Server,
    ) -> (Server, bool)
    where
        Server: Clone,
    {
        if let Some(existing) = self.get(&key).cloned() {
            self.bind_route(route, key);
            return (existing, false);
        }
        self.insert_route(route, key, candidate.clone());
        (candidate, true)
    }

    fn rekey(&mut self, old: &ProjectServerKey, new: &ProjectServerKey) -> bool {
        if old == new {
            return true;
        }
        let Some(server) = self.servers.remove(old) else {
            return false;
        };
        if self.servers.contains_key(new) {
            self.aliases.retain(|_, key| key != old);
            return false;
        }
        self.servers.insert(new.clone(), server);
        for key in self.aliases.values_mut() {
            if key == old {
                *key = new.clone();
            }
        }
        true
    }

    fn values(&self) -> impl Iterator<Item = &Server> {
        self.servers.values()
    }
}

impl StoreOwnerKey {
    fn from_paths(
        profile_root: &Path,
        global_db_path: &Path,
        project_id: Option<String>,
        store_root: &Path,
        graph_db_path: &Path,
    ) -> Result<Self> {
        Ok(Self {
            profile_root: authority::canonical_identity_path(profile_root)?,
            global_db_path: authority::canonical_identity_path(global_db_path)?,
            project_id,
            store_root: authority::canonical_identity_path(store_root)?,
            graph_db_path: authority::canonical_identity_path(graph_db_path)?,
        })
    }
}

impl ProjectRouteKey {
    fn from_handshake(project_path: &Path, handshake: &DaemonHandshake) -> Result<Self> {
        Ok(Self {
            profile_root: authority::canonical_identity_path(
                &handshake.client_identity.profile_root,
            )?,
            global_db_path: authority::canonical_identity_path(
                &handshake.client_identity.global_db_path,
            )?,
            project_path: authority::canonical_identity_path(project_path)?,
            scope_prefix: handshake.scope_prefix.clone(),
        })
    }
}

async fn project_open_gate(
    gates: &tokio::sync::Mutex<ProjectOpenGates>,
    route: &ProjectRouteKey,
) -> Arc<ProjectOpenGate> {
    let mut gates = gates.lock().await;
    if let Some(gate) = gates.get(route).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(ProjectOpenGate::new(()));
    gates.insert(route.clone(), Arc::downgrade(&gate));
    gate
}

#[cfg(any(not(unix), test))]
fn portable_database_owner_reconciler(
    store_administration: StoreAdministration,
    current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
    route_registered: Arc<AtomicBool>,
    handshake: DaemonHandshake,
) -> crate::mcp::DatabaseOwnerReconciler {
    Arc::new(move |fresh| {
        let store_administration = store_administration.clone();
        let current_key = Arc::clone(&current_key);
        let route_registered = Arc::clone(&route_registered);
        let handshake = handshake.clone();
        Box::pin(async move {
            store_administration
                .with_writer(|| async {
                    if !route_registered.load(Ordering::Acquire) {
                        return;
                    }
                    let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake) {
                        Ok(key) => key,
                        Err(error) => {
                            eprintln!(
                                "[tracedecay] failed to rekey daemon database owner: {error}"
                            );
                            return;
                        }
                    };
                    let mut current = current_key.lock().await;
                    if *current == new_key {
                        return;
                    }
                    let old_key = current.clone();
                    if !store_administration
                        .project_servers()
                        .lock()
                        .await
                        .rekey(&old_key, &new_key)
                    {
                        route_registered.store(false, Ordering::Release);
                    }
                    *current = new_key;
                })
                .await;
        })
    })
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CatalogRefreshClientKey {
    client_identity: DaemonClientIdentity,
    client_instance_id: String,
}

#[cfg(unix)]
impl CatalogRefreshClientKey {
    fn from_handshake(handshake: &DaemonHandshake) -> Self {
        Self {
            client_identity: handshake.client_identity.clone(),
            client_instance_id: handshake.client_instance_id.clone(),
        }
    }
}

#[cfg(unix)]
fn valid_client_instance_id(client_instance_id: &str) -> bool {
    let bytes = client_instance_id.as_bytes();
    (bytes.len() == 32
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
        || client_instance_id.strip_prefix("mcp-").is_some_and(|tail| {
            !tail.is_empty() && tail.len() <= 20 && tail.bytes().all(|byte| byte.is_ascii_digit())
        })
}

impl ProjectServerKey {
    fn from_open_project(
        cg: &crate::tracedecay::TraceDecay,
        handshake: &DaemonHandshake,
    ) -> Result<Self> {
        let layout = cg.store_layout();
        Ok(Self {
            owner: StoreOwnerKey::from_paths(
                &handshake.client_identity.profile_root,
                &handshake.client_identity.global_db_path,
                layout.identity.project_id.clone(),
                &layout.data_root,
                &cg.db_path(),
            )?,
            scope_prefix: handshake.scope_prefix.clone(),
        })
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

    /// Runs destructive branch administration before any project server is
    /// opened for the request, under the daemon-wide store administration gate.
    async fn execute_branch_admin(
        &self,
        handshake: &DaemonHandshake,
        action: crate::branch::BranchAdminAction,
    ) -> Result<crate::branch::BranchAdminReport> {
        self.store_administration
            .execute_branch_admin_for_handshake(handshake, action)
            .await
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
        let hint = version_skew_action(binary_version(), &client_version).to_string();
        log_daemon_event(
            "daemon_version_skew",
            &[
                ("daemon_version", binary_version().to_string()),
                ("client_version", client_version),
                ("hint", hint),
            ],
        );
    }

    /// Claims the one catalog-refresh notification for this client in the
    /// current daemon generation. Only proxies that already advertised the
    /// capability are eligible. `initialize` and `tools/list` mark the client
    /// current without emitting because those requests already fetch the new
    /// generation's catalog.
    async fn claim_catalog_refresh(
        &self,
        handshake: &DaemonHandshake,
        request_line: &str,
    ) -> Option<CatalogRefreshClientKey> {
        if !valid_client_instance_id(&handshake.client_instance_id) {
            return None;
        }
        let request = serde_json::from_str::<JsonRpcRequest>(request_line).ok()?;
        if request.method == HOOK_EVENT_METHOD {
            return None;
        }
        let catalog_is_current = matches!(request.method.as_str(), "initialize" | "tools/list");
        if !catalog_is_current
            && (!handshake.tool_list_changed_capable || handshake.catalog_version.is_empty())
        {
            return None;
        }
        let key = CatalogRefreshClientKey::from_handshake(handshake);
        let mut notified_clients = self.catalog_refresh_notified_clients.lock().await;
        if notified_clients.contains(&key) {
            return None;
        }
        if notified_clients.len() >= MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION {
            drop(notified_clients);
            if !self
                .catalog_refresh_saturation_logged
                .swap(true, Ordering::Relaxed)
            {
                log_daemon_event(
                    "catalog_refresh",
                    &[
                        ("outcome", "skipped".to_string()),
                        ("reason", "client_capacity_reached".to_string()),
                        (
                            "capacity",
                            MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION.to_string(),
                        ),
                    ],
                );
            }
            return None;
        }
        notified_clients.insert(key.clone());
        drop(notified_clients);
        if catalog_is_current {
            return None;
        }
        Some(key)
    }

    async fn release_catalog_refresh(&self, key: CatalogRefreshClientKey) {
        self.catalog_refresh_notified_clients
            .lock()
            .await
            .remove(&key);
    }

    async fn project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        let (project_path, route) = Self::project_route(handshake)?;
        let cached = {
            let servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route(&route)
                .map(|(key, server)| (key.clone(), Arc::clone(server)))
        };
        if let Some((key, server)) = cached {
            return Ok(self
                .activate_project_server(key, project_path, handshake, server)
                .await);
        }

        let gate = project_open_gate(&self.project_open_gates, &route).await;
        let _singleflight = gate.lock().await;
        self.project_server_after_open_gate(handshake).await
    }

    async fn project_server_after_open_gate(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        let (project_path, route) = Self::project_route(handshake)?;
        let cached = {
            let servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route(&route)
                .map(|(key, server)| (key.clone(), Arc::clone(server)))
        };
        if let Some((key, server)) = cached {
            return Ok(self
                .activate_project_server(key, project_path, handshake, server)
                .await);
        }

        let (key, project_path, server) = self
            .store_administration
            .with_writer(|| self.open_project_server(handshake))
            .await?;
        Ok(self
            .activate_project_server(key, project_path, handshake, server)
            .await)
    }

    async fn spawn_project_server_warmup(
        &self,
        handshake: DaemonHandshake,
        initialize_request: JsonRpcRequest,
    ) {
        let (_, route) = match Self::project_route(&handshake) {
            Ok(route) => route,
            Err(error) => {
                spawn_lifecycle_project_server_warmup(
                    self.lifecycle.clone(),
                    initialize_request,
                    async move { Err(error) },
                );
                return;
            }
        };
        let gate = project_open_gate(&self.project_open_gates, &route).await;
        let engine = self.clone();
        match Arc::clone(&gate).try_lock_owned() {
            Ok(singleflight) => {
                spawn_lifecycle_project_server_warmup(
                    self.lifecycle.clone(),
                    initialize_request,
                    async move {
                        let _singleflight = singleflight;
                        Box::pin(engine.project_server_after_open_gate(&handshake)).await
                    },
                );
            }
            Err(_) => {
                spawn_lifecycle_project_server_warmup(
                    self.lifecycle.clone(),
                    initialize_request,
                    async move { Box::pin(engine.project_server(&handshake)).await },
                );
            }
        }
    }

    async fn spawn_direct_project_server_open(
        &self,
        handshake: DaemonHandshake,
    ) -> Result<(JoinHandle<Result<Arc<crate::mcp::McpServer>>>, bool)> {
        let (_, route) = Self::project_route(&handshake)?;
        let gate = project_open_gate(&self.project_open_gates, &route).await;
        let engine = self.clone();
        let (singleflight, joins_existing_open) = match Arc::clone(&gate).try_lock_owned() {
            Ok(singleflight) => (Some(singleflight), false),
            Err(_) => (None, true),
        };
        let task = tokio::spawn(async move {
            let Some(activity) = engine.lifecycle.try_enter() else {
                return Err(TraceDecayError::Config {
                    message: "daemon is draining before project warm-up".to_string(),
                });
            };
            let _activity = activity;
            let open = async {
                match singleflight {
                    Some(singleflight) => {
                        let _singleflight = singleflight;
                        engine.project_server_after_open_gate(&handshake).await
                    }
                    None => engine.project_server(&handshake).await,
                }
            };
            let result = tokio::select! {
                biased;
                () = engine.lifecycle.wait_for_draining() => Err(TraceDecayError::Config {
                    message: "daemon began draining during project warm-up".to_string(),
                }),
                result = Box::pin(open) => result,
            };
            if let Err(error) = &result {
                log_daemon_event(
                    "project_server_warmup",
                    &[
                        ("outcome", "error".to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
            result
        });
        Ok((task, joins_existing_open))
    }

    /// Opens or resolves a project server while writer administration is held.
    /// Watcher and scheduler activation happen only after this returns so those
    /// components can acquire the same coordinator without recursive locking.
    async fn open_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>)> {
        let (canonical_project_path, route) = Self::project_route(handshake)?;
        let cached = {
            let servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route(&route)
                .map(|(key, server)| (key.clone(), Arc::clone(server)))
        };
        if let Some((key, server)) = cached {
            return Ok((key, canonical_project_path, server));
        }

        #[cfg(test)]
        self.project_open_attempts.fetch_add(1, Ordering::Relaxed);
        let cg = Box::pin(open_project_for_handshake(
            &canonical_project_path,
            handshake,
        ))
        .await?;
        cg.register_project_store_in_global_registry().await;
        let key = ProjectServerKey::from_open_project(&cg, handshake)?;

        let existing = {
            let mut servers = self.store_administration.project_servers().lock().await;
            let server = servers.get(&key).cloned();
            if server.is_some() {
                servers.bind_route(route.clone(), key.clone());
            }
            server
        };
        if let Some(server) = existing {
            return Ok((key, canonical_project_path, server));
        }

        let registry_db = self
            .store_administration
            .global_database(&handshake.client_identity.global_db_path)
            .await?;
        let accounting_db =
            crate::global_db::global_accounting_enabled().then(|| Arc::clone(&registry_db));
        let registry_db = Some(registry_db);
        let current_key = Arc::new(tokio::sync::Mutex::new(key.clone()));
        let route_registered = Arc::new(AtomicBool::new(true));
        let reconciler = self.automation_scheduler_reconciler(
            Arc::clone(&current_key),
            canonical_project_path.clone(),
            handshake.clone(),
        );
        let database_owner_reconciler = self.database_owner_reconciler(
            current_key,
            Arc::clone(&route_registered),
            handshake.clone(),
        );
        let candidate = crate::mcp::McpServer::new_with_dbs_and_reconcilers_and_writers(
            cg,
            handshake.scope_prefix.clone(),
            accounting_db,
            registry_db,
            false,
            Some(reconciler),
            Some(database_owner_reconciler),
            coordinated_dashboard_automation_writer(self.store_administration.clone()),
            coordinated_hook_branch_writer(self.store_administration.clone()),
            coordinated_background_refresh_writer(self.store_administration.clone()),
        )
        .await;
        let (server, inserted) = self
            .store_administration
            .project_servers()
            .lock()
            .await
            .bind_or_insert_route(route, key.clone(), candidate);
        if !inserted {
            route_registered.store(false, Ordering::Release);
        }
        Ok((key, canonical_project_path, server))
    }

    fn project_route(handshake: &DaemonHandshake) -> Result<(PathBuf, ProjectRouteKey)> {
        let Some(project_path) = handshake.project_path.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "project server requested without project_path".to_string(),
            });
        };
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake)?;
        Ok((canonical_project_path, route))
    }

    async fn activate_project_server(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: &DaemonHandshake,
        server: Arc<crate::mcp::McpServer>,
    ) -> Arc<crate::mcp::McpServer> {
        // A freshly-handshaken project should be watched even on a cache hit
        // (the watcher may have started after this server was cached).
        self.git_watcher.ensure_watching(&project_path).await;
        // Scheduler discovery is ancillary, so it must not make a cached MCP
        // server wait. Reuse the already-open project instead of opening the
        // same writable store again, and count the detached task as lifecycle
        // activity so shutdown cancels it before taking server snapshots.
        let engine = self.clone();
        let handshake = handshake.clone();
        let scheduler_server = Arc::clone(&server);
        spawn_lifecycle_automation_scheduler_activation(self.lifecycle.clone(), async move {
            let cg = scheduler_server.cg().await;
            engine
                .ensure_automation_scheduler(key, project_path, handshake, cg)
                .await;
        });
        server
    }

    fn database_owner_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        route_registered: Arc<AtomicBool>,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        let engine = self.clone();
        Arc::new(move |fresh| {
            let engine = engine.clone();
            let current_key = Arc::clone(&current_key);
            let route_registered = Arc::clone(&route_registered);
            let handshake = handshake.clone();
            Box::pin(async move {
                engine
                    .store_administration
                    .with_writer(|| async {
                        if !route_registered.load(Ordering::Acquire) {
                            return;
                        }
                        let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake)
                        {
                            Ok(key) => key,
                            Err(error) => {
                                eprintln!(
                                    "[tracedecay] failed to rekey daemon database owner: {error}"
                                );
                                return;
                            }
                        };
                        let mut current = current_key.lock().await;
                        if *current == new_key {
                            return;
                        }
                        let old_key = current.clone();
                        let rekeyed = engine
                            .store_administration
                            .project_servers()
                            .lock()
                            .await
                            .rekey(&old_key, &new_key);
                        if !rekeyed {
                            route_registered.store(false, Ordering::Release);
                        }
                        let removed_scheduler = {
                            let mut schedulers = engine
                                .store_administration
                                .automation_schedulers()
                                .lock()
                                .await;
                            let removed = schedulers.remove(&old_key);
                            if let Some(handle) = removed {
                                if schedulers.contains_key(&new_key) {
                                    Some(handle)
                                } else {
                                    schedulers.insert(new_key.clone(), handle);
                                    None
                                }
                            } else {
                                None
                            }
                        };
                        if let Some(handle) = removed_scheduler {
                            handle.task.abort();
                        }
                        *current = new_key;
                    })
                    .await;
            })
        })
    }

    async fn shutdown_background_tasks(&self) {
        self.shutdown_automation_schedulers().await;

        self.git_watcher.shutdown().await;
        if let Some(handle) = self.pr_autotrack_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn shutdown_servers(&self) {
        shutdown_project_servers(&self.store_administration).await;
    }

    #[cfg(test)]
    async fn shutdown_all(&self) {
        self.lifecycle.begin_draining();
        self.shutdown_background_tasks().await;
        self.shutdown_servers().await;
    }
}

async fn shutdown_project_servers(store_administration: &StoreAdministration) {
    let servers: Vec<Arc<crate::mcp::McpServer>> = store_administration
        .with_writer(|| async {
            let servers = store_administration.project_servers().lock().await;
            let mut seen = HashSet::new();
            servers
                .values()
                .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
                .cloned()
                .collect()
        })
        .await;
    for server in servers {
        server.shutdown().await;
    }
}

#[cfg(all(unix, test))]
async fn serve_socket_client(stream: tokio::net::UnixStream, engine: DaemonEngine) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        BrokerStream::Unix(stream),
        engine,
        None,
    ))
    .await
}

#[cfg(unix)]
async fn serve_authenticated_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
) -> Result<()> {
    Box::pin(serve_broker_socket_client(stream, engine, Some(auth_token))).await
}

async fn apply_daemon_initialize_route(
    handshake: &mut DaemonHandshake,
    first_request_line: &str,
    store_administration: &StoreAdministration,
) -> Result<Option<InitializeRouteMetadata>> {
    if !handshake.allow_initialize_root_routing {
        return Ok(None);
    }
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) else {
        return Ok(None);
    };
    if request.method != "initialize" {
        return Ok(None);
    }
    let registry = store_administration
        .global_database(&handshake.client_identity.global_db_path)
        .await?;
    let Some(route) =
        resolve_daemon_initialize_route(request.params.as_ref(), Some(&registry)).await
    else {
        return Ok(None);
    };
    if handshake.project_path.as_deref() != Some(route.project_path.as_path()) {
        handshake.scope_prefix = None;
    }
    handshake.project_path = Some(route.project_path.clone());
    handshake.allow_init = route.allow_init;
    Ok(Some(route))
}

fn attach_initialize_route_metadata(
    response: &mut JsonRpcResponse,
    route: &InitializeRouteMetadata,
) {
    let Some(result) = response.result.as_mut() else {
        return;
    };
    result["_meta"]["tracedecayInitializeRoute"] = json!(route);
}

/// A static MCP bootstrap call the daemon answers without opening a project.
enum DaemonBootstrap {
    /// A notification that needs no response written back.
    Handled,
    /// A static response to write back to the client.
    Respond(JsonRpcResponse),
}

/// Returns `None` for project-dependent requests, which the caller must route
/// to a project server instead.
fn daemon_bootstrap_response(
    request: &JsonRpcRequest,
    route: Option<&InitializeRouteMetadata>,
    project_node_count: Option<u64>,
) -> Option<DaemonBootstrap> {
    match classify_mcp_method(&request.method) {
        McpMethod::Initialize => Some(match request.id.clone() {
            Some(id) => {
                let mut response =
                    JsonRpcResponse::success(id, initialize_result(SERVER_INSTRUCTIONS));
                if let Some(route) = route {
                    attach_initialize_route_metadata(&mut response, route);
                }
                DaemonBootstrap::Respond(response)
            }
            None => DaemonBootstrap::Handled,
        }),
        McpMethod::InitializedAck => Some(DaemonBootstrap::Handled),
        McpMethod::ToolsList => Some(match request.id.clone() {
            Some(id) => {
                let tools = project_node_count.map_or_else(
                    || get_tool_definitions_with_warming_budget(10),
                    |node_count| {
                        let budget = explore_call_budget(node_count);
                        get_tool_definitions_with_budget(node_count, budget)
                    },
                );
                DaemonBootstrap::Respond(JsonRpcResponse::success(id, json!({ "tools": tools })))
            }
            None => DaemonBootstrap::Handled,
        }),
        _ => None,
    }
}

async fn cached_project_node_count(
    store_administration: &StoreAdministration,
    handshake: &DaemonHandshake,
) -> Option<u64> {
    let project_path = handshake.project_path.as_ref()?;
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.clone());
    let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake).ok()?;
    let server = {
        let servers = store_administration.project_servers().lock().await;
        servers
            .get_route(&route)
            .map(|(_, server)| Arc::clone(server))
    }?;
    server
        .cg()
        .await
        .get_stats()
        .await
        .ok()
        .map(|stats| stats.node_count)
}

fn spawn_lifecycle_project_server_warmup<OpenFuture>(
    lifecycle: DaemonLifecycle,
    initialize_request: JsonRpcRequest,
    open_project_server: OpenFuture,
) where
    OpenFuture: std::future::Future<Output = Result<Arc<crate::mcp::McpServer>>> + Send + 'static,
{
    let Some(activity) = lifecycle.try_enter() else {
        return;
    };
    let _warmup = tokio::spawn(async move {
        let _activity = activity;
        let project_server = tokio::select! {
            biased;
            () = lifecycle.wait_for_draining() => return,
            result = Box::pin(open_project_server) => result,
        };
        match project_server {
            Ok(server) => {
                // Preserve the regular initialize side effect that records
                // the negotiated MCP client name on the real server.
                let _ = server.handle_request(&initialize_request).await;
            }
            Err(error) => log_daemon_event(
                "project_server_warmup",
                &[
                    ("outcome", "error".to_string()),
                    ("error", error.to_string()),
                ],
            ),
        }
    });
}

fn spawn_lifecycle_automation_scheduler_activation<ActivationFuture>(
    lifecycle: DaemonLifecycle,
    activation: ActivationFuture,
) where
    ActivationFuture: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(activity) = lifecycle.try_enter() else {
        return;
    };
    tokio::spawn(async move {
        let _activity = activity;
        tokio::select! {
            biased;
            () = lifecycle.wait_for_draining() => {}
            () = activation => {}
        }
    });
}

#[cfg(any(not(unix), test))]
fn spawn_portable_project_server_warmup(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: DaemonHandshake,
    initialize_request: JsonRpcRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) {
    let Some(project_path) = handshake.project_path.clone() else {
        return;
    };
    spawn_lifecycle_project_server_warmup(lifecycle, initialize_request, async move {
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        store_administration
            .with_writer(|| {
                portable_project_server(
                    &store_administration,
                    &project_open_gates,
                    &canonical_project_path,
                    &handshake,
                    #[cfg(test)]
                    project_open_attempts.as_ref(),
                )
            })
            .await
    });
}

async fn write_routed_initialize_response(
    server: &crate::mcp::McpServer,
    transport: &mut impl McpTransport,
    first_request_line: &str,
    route: Option<&InitializeRouteMetadata>,
) -> Result<bool> {
    let Some(route) = route else {
        return Ok(false);
    };
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) else {
        return Ok(false);
    };
    if request.method != "initialize" {
        return Ok(false);
    }
    let Some(mut response) = server.handle_request(&request).await else {
        return Ok(false);
    };
    attach_initialize_route_metadata(&mut response, route);
    write_json_rpc_response(transport, &response).await?;
    Ok(true)
}

#[cfg(unix)]
async fn serve_broker_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: Option<String>,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    if let Some(expected_token) = auth_token.as_deref() {
        let preface_line = tokio::select! {
            result = transport.read_line() => result?,
            () = engine.lifecycle.wait_for_draining() => return Ok(()),
        };
        let Some(preface_line) = preface_line else {
            return Ok(());
        };
        let preface =
            DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            })?;
        if !preface.authenticate(expected_token) {
            return Err(TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            });
        }
    }
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
    let mut handshake = DaemonHandshake::from_line(&line)?;
    engine.log_client_version_skew(&handshake).await;
    // Resolve initialize roots only after authentication and inside daemon
    // authority. The proxy process never opens the registry database.
    let first_request_line = tokio::select! {
        result = transport.read_line() => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(first_request_line) = first_request_line else {
        return Ok(());
    };
    let initialize_route = apply_daemon_initialize_route(
        &mut handshake,
        &first_request_line,
        &engine.store_administration,
    )
    .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => engine.execute_branch_admin(&handshake, action).await,
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response =
            branch_add_response(&engine.store_administration, &handshake, &request).await;
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&engine.store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if let Some(bootstrap) =
            daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            // Keep catalog-refresh bookkeeping consistent with the regular MCP
            // server path: initialize and tools/list mark this catalog current.
            if let Some(key) = engine
                .claim_catalog_refresh(&handshake, &first_request_line)
                .await
                && let Err(error) = write_tool_list_changed_notification(&mut transport).await
            {
                engine.release_catalog_refresh(key).await;
                return Err(error);
            }
            if matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                && handshake.project_path.is_some()
            {
                engine
                    .spawn_project_server_warmup(handshake.clone(), request)
                    .await;
            }
            drop(setup_activity);
            if let DaemonBootstrap::Respond(response) = bootstrap {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let server = if let Some(project_path) = handshake.project_path.as_ref() {
        // Queuing behind an unrelated writer can take that writer's whole
        // operation, so answer with a retry hint rather than holding the
        // client. An uncontended open is this client's own work and must run
        // to completion, otherwise one-shot callers never get a result.
        let writer_contended = engine.store_administration.writer_is_busy();
        let (mut project_open, joins_existing_open) = engine
            .spawn_direct_project_server_open(handshake.clone())
            .await?;
        let contended = writer_contended && !joins_existing_open;
        let opened = if contended {
            tokio::time::timeout(CONTENDED_PROJECT_OPEN_GRACE, &mut project_open).await
        } else {
            Ok((&mut project_open).await)
        };
        let server = match opened {
            Ok(Ok(Ok(server))) => server,
            Ok(Ok(Err(error))) => {
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Err(error);
            }
            Ok(Err(error)) => {
                let error = TraceDecayError::Config {
                    message: format!("project warm-up task failed: {error}"),
                };
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Err(error);
            }
            Err(_) => {
                let error = TraceDecayError::Config {
                    message: format!(
                        "TraceDecay project '{}' is warming in the background; retry the same tool shortly",
                        project_path.display()
                    ),
                };
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
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

    // The stdio proxy creates one daemon connection per request. The request
    // was peeked above so initialize-root routing happens before project open.
    if let Some(key) = engine
        .claim_catalog_refresh(&handshake, &first_request_line)
        .await
    {
        if let Err(error) = write_tool_list_changed_notification(&mut transport).await {
            engine.release_catalog_refresh(key).await;
            return Err(error);
        }
    }
    let initialize_handled = match server.as_deref() {
        Some(server) => {
            write_routed_initialize_response(
                server,
                &mut transport,
                &first_request_line,
                initialize_route.as_ref(),
            )
            .await?
        }
        None => false,
    };
    let mut transport = ReplayTransport::new(transport);
    if !initialize_handled {
        transport.push_replay(first_request_line);
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
            &engine.store_administration,
        )
        .await?;
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
async fn serve_windows_broker_client(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    let Some(preface_line) = transport.read_line().await? else {
        return Ok(());
    };
    let preface =
        DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        })?;
    if !preface.authenticate(auth_token) {
        return Err(TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        });
    }
    let Some(handshake_line) = transport.read_line().await? else {
        return Ok(());
    };
    let Some(setup_activity) = lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&handshake_line)?;
    let Some(first_request_line) = transport.read_line().await? else {
        return Ok(());
    };
    let initialize_route =
        apply_daemon_initialize_route(&mut handshake, &first_request_line, &store_administration)
            .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => {
                store_administration
                    .execute_branch_admin_for_handshake(&handshake, action)
                    .await
            }
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response = branch_add_response(&store_administration, &handshake, &request).await;
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if let Some(bootstrap) =
            daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            if matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                && handshake.project_path.is_some()
            {
                spawn_portable_project_server_warmup(
                    lifecycle.clone(),
                    store_administration.clone(),
                    Arc::clone(&project_open_gates),
                    handshake.clone(),
                    request,
                    #[cfg(test)]
                    project_open_attempts.clone(),
                );
            }
            drop(setup_activity);
            if let DaemonBootstrap::Respond(response) = bootstrap {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    if let Some(project_path) = handshake.project_path.as_deref() {
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.to_path_buf());
        let server_result = store_administration
            .with_writer(|| {
                portable_project_server(
                    &store_administration,
                    &project_open_gates,
                    &canonical_project_path,
                    &handshake,
                    #[cfg(test)]
                    project_open_attempts.as_ref(),
                )
            })
            .await;
        let server = match server_result {
            Ok(server) => server,
            Err(error) => {
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Err(error);
            }
        };
        drop(setup_activity);
        let initialize_handled = write_routed_initialize_response(
            &server,
            &mut transport,
            &first_request_line,
            initialize_route.as_ref(),
        )
        .await?;
        let mut transport = ReplayTransport::new(transport);
        if !initialize_handled {
            transport.push_replay(first_request_line);
        }
        Box::pin(server.run_daemon_connection_with_timings(
            &mut transport,
            handshake.timings,
            lifecycle,
        ))
        .await?;
    } else {
        drop(setup_activity);
        let mut transport = ReplayTransport::new(transport);
        transport.push_replay(first_request_line);
        serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            lifecycle,
            &store_administration,
        )
        .await?;
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
async fn portable_project_server(
    store_administration: &StoreAdministration,
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
    #[cfg(test)] project_open_attempts: Option<&Arc<AtomicUsize>>,
) -> Result<Arc<crate::mcp::McpServer>> {
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    if let Some(server) = store_administration
        .project_servers()
        .lock()
        .await
        .get_route(&route)
        .map(|(_, server)| Arc::clone(server))
    {
        return Ok(server);
    }

    let gate = project_open_gate(project_open_gates, &route).await;
    let _singleflight = gate.lock().await;
    if let Some(server) = store_administration
        .project_servers()
        .lock()
        .await
        .get_route(&route)
        .map(|(_, server)| Arc::clone(server))
    {
        return Ok(server);
    }

    #[cfg(test)]
    if let Some(attempts) = project_open_attempts {
        attempts.fetch_add(1, Ordering::Relaxed);
    }
    let cg = Box::pin(open_project_for_handshake(
        canonical_project_path,
        handshake,
    ))
    .await?;
    cg.register_project_store_in_global_registry().await;
    let key = ProjectServerKey::from_open_project(&cg, handshake)?;
    let existing = {
        let mut servers = store_administration.project_servers().lock().await;
        let existing = servers.get(&key).cloned();
        if existing.is_some() {
            servers.bind_route(route.clone(), key.clone());
        }
        existing
    };
    if let Some(existing) = existing {
        return Ok(existing);
    }

    let current_key = Arc::new(tokio::sync::Mutex::new(key.clone()));
    let route_registered = Arc::new(AtomicBool::new(true));
    let database_owner_reconciler = portable_database_owner_reconciler(
        store_administration.clone(),
        current_key,
        Arc::clone(&route_registered),
        handshake.clone(),
    );
    let registry_db = store_administration
        .global_database(&handshake.client_identity.global_db_path)
        .await?;
    let accounting_db =
        crate::global_db::global_accounting_enabled().then(|| Arc::clone(&registry_db));
    let registry_db = Some(registry_db);
    let candidate = crate::mcp::McpServer::new_with_dbs_and_reconcilers_and_writers(
        cg,
        handshake.scope_prefix.clone(),
        accounting_db,
        registry_db,
        false,
        None,
        Some(database_owner_reconciler),
        coordinated_dashboard_automation_writer(store_administration.clone()),
        coordinated_hook_branch_writer(store_administration.clone()),
        coordinated_background_refresh_writer(store_administration.clone()),
    )
    .await;
    let (resolved, inserted) = store_administration
        .project_servers()
        .lock()
        .await
        .bind_or_insert_route(route, key, candidate);
    if !inserted {
        route_registered.store(false, Ordering::Release);
    }
    Ok(resolved)
}

#[cfg(unix)]
async fn write_tool_list_changed_notification(transport: &mut impl McpTransport) -> Result<()> {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": TOOL_LIST_CHANGED_METHOD,
    });
    transport
        .write_line(&format!("{}\n", serde_json::to_string(&notification)?))
        .await?;
    transport.flush().await?;
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

fn is_readonly_database_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Database { message, .. }
            if message.to_ascii_lowercase().contains("readonly database")
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
        Err(open_err) if is_readonly_database_error(&open_err) => {
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
                Err(_) => Err(open_err),
            }
        }
        Err(error) if is_missing_index_error(&error) => Err(missing_index_error(project_path)),
        Err(error) => Err(error),
    }
}

async fn write_project_open_error(
    transport: &mut impl McpTransport,
    request_line: &str,
    error: &TraceDecayError,
) -> Result<()> {
    let id = serde_json::from_str::<JsonRpcRequest>(request_line)
        .ok()
        .and_then(|request| request.id)
        .unwrap_or(serde_json::Value::Null);
    let response = JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    write_json_rpc_response(transport, &response).await
}

async fn write_json_rpc_response(
    transport: &mut impl McpTransport,
    response: &crate::mcp::JsonRpcResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

async fn serve_projectless_client(
    transport: &mut impl McpTransport,
    client_identity: &DaemonClientIdentity,
    lifecycle: &DaemonLifecycle,
    store_administration: &StoreAdministration,
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
            Ok(request) => {
                projectless_response(&request, client_identity, store_administration).await
            }
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

async fn projectless_response(
    request: &crate::mcp::JsonRpcRequest,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> Option<crate::mcp::JsonRpcResponse> {
    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {
                        "listChanged": true
                    }
                },
                "serverInfo": {
                    "name": "tracedecay",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "tools/call" => Some(
            projectless_tools_call_response(
                id,
                request.params.as_ref(),
                client_identity,
                store_administration,
            )
            .await,
        ),
        "ping" | "logging/setLevel" => Some(JsonRpcResponse::success(id, json!({}))),
        _ => Some(JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("Method not found: {}", request.method),
        )),
    }
}

async fn projectless_tools_call_response(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };
    if tool_name == "tracedecay_hook_runtime" {
        return match crate::mcp::tools::handle_projectless_hook_runtime(
            arguments,
            &client_identity.profile_root,
        )
        .await
        {
            Ok(result) => JsonRpcResponse::success(id, result.value),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    if tool_name == "tracedecay_admin_cli" {
        let global_db = match store_administration
            .global_database(&client_identity.global_db_path)
            .await
        {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        return match crate::mcp::tools::handle_projectless_admin_cli(arguments, &global_db).await {
            Ok(result) => JsonRpcResponse::success(id, result.value),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    JsonRpcResponse::error(
        id,
        ErrorCode::InternalError,
        format!("{tool_name} requires an initialized code project"),
    )
}

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

struct BrokerStreamTransport {
    reader: tokio::io::Lines<tokio::io::BufReader<tokio::io::ReadHalf<BrokerStream>>>,
    writer: tokio::io::WriteHalf<BrokerStream>,
}

impl BrokerStreamTransport {
    fn new(stream: BrokerStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: tokio::io::BufReader::new(reader).lines(),
            writer,
        }
    }
}

impl crate::mcp::McpTransport for BrokerStreamTransport {
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;

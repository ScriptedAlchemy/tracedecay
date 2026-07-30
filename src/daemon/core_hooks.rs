//! Host hook events: wire metadata, event constructors, and daemon notification.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, timeout};

use super::{
    BrokerStream, DaemonHandshake, JsonRpcRequest, current_daemon_connection, write_daemon_preamble,
};
#[cfg(unix)]
use super::{SOCKET_ENV, connection_for_socket_path};

/// A domain-catalogued host whose lifecycle hooks notify the daemon.
pub use tracedecay_domain::HostIntegrationIdV1 as HookAgent;

pub const HOOK_EVENT_METHOD: &str = "tracedecay/hookEvent";
pub(crate) const HOOK_EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEventNotifyOutcomeV1 {
    Delivered,
    Unavailable,
    TimedOut,
    Malformed,
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
pub struct HookTerminalReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_watermark: Option<String>,
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

    pub fn cursor_after_shell_execution(cwd: PathBuf) -> Self {
        Self::new(
            HookAgent::Cursor,
            "afterShellExecution",
            Vec::new(),
            None,
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

    /// A shell command finished. Command text is deliberately discarded:
    /// native daemon state, not shell parsing, owns Git reconciliation.
    pub fn post_tool_use_shell(agent: HookAgent, cwd: PathBuf) -> Self {
        Self::new(agent, "postToolUseShell", Vec::new(), None, Some(cwd))
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

pub async fn notify_hook_event(
    project_path: &Path,
    event: DaemonHookEvent,
) -> HookEventNotifyOutcomeV1 {
    let connection = {
        #[cfg(unix)]
        {
            std::env::var_os(SOCKET_ENV)
                .filter(|path| !path.is_empty())
                .map(|path| connection_for_socket_path(Path::new(&path)))
                .map_or_else(current_daemon_connection, Ok)
        }
        #[cfg(not(unix))]
        {
            current_daemon_connection()
        }
    };
    let Ok(connection) = connection else {
        return HookEventNotifyOutcomeV1::Unavailable;
    };
    match timeout(
        HOOK_EVENT_NOTIFY_TIMEOUT,
        notify_hook_event_to_connection(project_path, event, connection),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => HookEventNotifyOutcomeV1::TimedOut,
    }
}

async fn notify_hook_event_to_connection(
    project_path: &Path,
    event: DaemonHookEvent,
    connection: super::DaemonConnection,
) -> HookEventNotifyOutcomeV1 {
    let Ok(handshake) =
        DaemonHandshake::for_current_client(Some(project_path.to_path_buf()), None, false, false)
    else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let Ok(params) = serde_json::to_value(event) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: HOOK_EVENT_METHOD.to_string(),
        params: Some(params),
    };
    let Ok(line) = serde_json::to_string(&request) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let Ok(stream) = BrokerStream::connect(&connection.endpoint).await else {
        return HookEventNotifyOutcomeV1::Unavailable;
    };
    let (_reader, mut writer) = stream.into_split();
    if write_daemon_preamble(&mut writer, &connection, &handshake)
        .await
        .is_err()
    {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.write_all(line.as_bytes()).await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.write_all(b"\n").await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.flush().await.is_err() || writer.shutdown().await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    HookEventNotifyOutcomeV1::Delivered
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_hook_socket_returns_typed_unavailable_without_retry_delay() {
        let socket_dir = tempfile::tempdir().unwrap();
        let missing_socket = socket_dir.path().join("missing.sock");
        let connection = connection_for_socket_path(&missing_socket);
        let started = Instant::now();

        let outcome = notify_hook_event_to_connection(
            socket_dir.path(),
            DaemonHookEvent::cursor_after_shell_execution(socket_dir.path().to_path_buf()),
            connection,
        )
        .await;

        assert_eq!(outcome, HookEventNotifyOutcomeV1::Unavailable);
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "a missing socket must not consume the outer hook timeout"
        );
    }
}

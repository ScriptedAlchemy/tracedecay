//! Host hook wire metadata and event constructors.
//!
//! Pure data: the notification method name, route/receipt metadata, and the
//! `DaemonHookEvent` envelope with its host-specific constructors. Delivery of
//! these events over a daemon connection remains with the daemon runtime.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A domain-catalogued host whose lifecycle hooks notify the daemon.
pub use tracedecay_domain::HostIntegrationIdV1 as HookAgent;

pub const HOOK_EVENT_METHOD: &str = "tracedecay/hookEvent";

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
}

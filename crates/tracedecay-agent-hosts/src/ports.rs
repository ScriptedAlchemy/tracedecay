//! Narrow callbacks for process-level behavior retained by the root package.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

pub type CursorPostInstallFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
pub type UserMemoryCuratorFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<crate::automation::memory_curator::MemoryCuratorAutomationRun>>
            + Send
            + 'a,
    >,
>;
pub type AnalyticsEventsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<AnalyticsEventRecord>>> + Send + 'a>>;
pub type SessionActivityFuture<'a> = Pin<Box<dyn Future<Output = Option<i64>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventRecord {
    pub id: i64,
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CursorSessionHealth {
    pub max_transcript_pending_bytes: u64,
    pub pending_bytes: u64,
    pub pending_transcripts: u64,
    pub tracked_transcripts: u64,
    pub literal_workspace_placeholder_paths: Vec<String>,
}

pub struct RootPorts {
    pub tool_definitions: fn() -> Vec<ToolDescriptor>,
    pub format_capable_tool_names: fn() -> Vec<String>,
    pub cursor_catch_up_ingest_max_bytes: fn() -> u64,
    pub cursor_post_install: fn(PathBuf) -> CursorPostInstallFuture,
    pub cursor_session_health: fn(&Path) -> Option<CursorSessionHealth>,
    pub memory_injection_enabled: fn() -> bool,
    pub degraded_serve_stderr_marker: fn() -> &'static str,
    pub user_memory_curator: for<'a> fn(
        &'a Path,
        &'a crate::automation::config::AutomationConfig,
        &'a dyn crate::automation::backend::AgentTaskBackend,
        crate::automation::memory_curator::MemoryCuratorAutomationOptions,
    ) -> UserMemoryCuratorFuture<'a>,
    pub project_analytics_events: for<'a> fn(&'a Path, usize) -> AnalyticsEventsFuture<'a>,
    pub latest_session_activity: for<'a> fn(&'a Path) -> SessionActivityFuture<'a>,
}

static ROOT_PORTS: OnceLock<RootPorts> = OnceLock::new();

pub fn install_root_ports(ports: RootPorts) {
    let _ = ROOT_PORTS.set(ports);
}

fn root_ports() -> Result<&'static RootPorts> {
    ROOT_PORTS.get().ok_or_else(|| TraceDecayError::Config {
        message: "agent-host root ports are not configured".to_string(),
    })
}

pub(crate) fn tool_definitions() -> Result<Vec<ToolDescriptor>> {
    if let Some(ports) = ROOT_PORTS.get() {
        return Ok((ports.tool_definitions)());
    }
    #[cfg(test)]
    return Ok(Vec::new());
    #[cfg(not(test))]
    Err(TraceDecayError::Config {
        message: "agent-host root ports are not configured".to_string(),
    })
}

pub(crate) fn format_capable_tool_names() -> Result<Vec<String>> {
    if let Some(ports) = ROOT_PORTS.get() {
        return Ok((ports.format_capable_tool_names)());
    }
    #[cfg(test)]
    return Ok(Vec::new());
    #[cfg(not(test))]
    Err(TraceDecayError::Config {
        message: "agent-host root ports are not configured".to_string(),
    })
}

pub(crate) fn cursor_catch_up_ingest_max_bytes() -> Result<u64> {
    Ok((root_ports()?.cursor_catch_up_ingest_max_bytes)())
}

pub(crate) fn cursor_post_install(project_path: PathBuf) -> Result<CursorPostInstallFuture> {
    Ok((root_ports()?.cursor_post_install)(project_path))
}

pub(crate) fn cursor_session_health(project_path: &Path) -> Result<Option<CursorSessionHealth>> {
    Ok((root_ports()?.cursor_session_health)(project_path))
}

pub(crate) fn memory_injection_enabled() -> Result<bool> {
    Ok((root_ports()?.memory_injection_enabled)())
}

pub(crate) fn degraded_serve_stderr_marker() -> Result<&'static str> {
    Ok((root_ports()?.degraded_serve_stderr_marker)())
}

pub(crate) async fn run_user_memory_curator(
    profile_root: &Path,
    config: &crate::automation::config::AutomationConfig,
    backend: &dyn crate::automation::backend::AgentTaskBackend,
    options: crate::automation::memory_curator::MemoryCuratorAutomationOptions,
) -> Result<crate::automation::memory_curator::MemoryCuratorAutomationRun> {
    (root_ports()?.user_memory_curator)(profile_root, config, backend, options).await
}

pub(crate) async fn project_analytics_events(
    project_root: &Path,
    limit: usize,
) -> Result<Vec<AnalyticsEventRecord>> {
    (root_ports()?.project_analytics_events)(project_root, limit).await
}

pub(crate) async fn latest_session_activity(sessions_db_path: &Path) -> Option<i64> {
    (root_ports().ok()?.latest_session_activity)(sessions_db_path).await
}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::tool_hints::{HintAgent, ToolHint};
use super::{
    HookWorkspaceStatus, claude, event_cwd_from_parsed, event_session_id, prompt_like_text,
    text_field,
};

pub(super) const HOOK_ANALYTICS_FILENAME: &str = "hook_analytics.jsonl";

pub(super) struct HookTimingSpan {
    root: Option<PathBuf>,
    agent: &'static str,
    hook_name: String,
    parsed: Value,
    started: Instant,
    enabled: bool,
}

impl HookTimingSpan {
    fn new(root: Option<&Path>, agent: HintAgent, hook_name: &str, parsed: Value) -> Self {
        let enabled = root
            .map(crate::config::load_telemetry_config)
            .is_none_or(|telemetry| telemetry.timings);
        Self {
            root: root.map(Path::to_path_buf),
            agent: agent.as_key(),
            hook_name: hook_name.to_string(),
            parsed,
            started: Instant::now(),
            enabled,
        }
    }
}

impl Drop for HookTimingSpan {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        let elapsed_us = self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        record_hook_analytics(
            self.root.as_deref(),
            "hook_completed",
            serde_json::json!({
                "agent": self.agent,
                "hook_name": self.hook_name,
                "hook_event_name": text_field(&self.parsed, &["hook_event_name", "hookEventName"]),
                "session_id": event_session_id(&self.parsed),
                "tool_name": text_field(&self.parsed, &["tool_name", "toolName", "name"]),
                "command": text_field(&self.parsed, &["command", "cmd", "shell_command"]),
                "prompt_category": inferred_prompt_category(&self.parsed),
                "event_cwd": event_cwd_from_parsed(&self.parsed).map(|cwd| cwd.display().to_string()),
                "duration_us": elapsed_us,
                "duration_ms": elapsed_us / 1000,
            }),
        );
    }
}

pub(super) fn record_hook_invoked(
    root: Option<&Path>,
    agent: HintAgent,
    hook_name: &str,
    event_json: &str,
) -> HookTimingSpan {
    let parsed: Value = serde_json::from_str(event_json).unwrap_or(Value::Null);
    record_hook_analytics(
        root,
        "hook_invoked",
        serde_json::json!({
            "agent": agent.as_key(),
            "hook_name": hook_name,
            "hook_event_name": text_field(&parsed, &["hook_event_name", "hookEventName"]),
            "session_id": event_session_id(&parsed),
            "tool_name": text_field(&parsed, &["tool_name", "toolName", "name"]),
            "command": text_field(&parsed, &["command", "cmd", "shell_command"]),
            "prompt_category": inferred_prompt_category(&parsed),
            "event_cwd": event_cwd_from_parsed(&parsed).map(|cwd| cwd.display().to_string()),
        }),
    );
    HookTimingSpan::new(root, agent, hook_name, parsed)
}

pub(super) fn mint_hint_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "h-{:x}-{:x}-{:x}",
        now_unix_millis(),
        std::process::id(),
        seq
    )
}

pub(super) fn record_hint_analytics(
    root: Option<&Path>,
    event: &str,
    agent: HintAgent,
    session_id: Option<&str>,
    hint_id: &str,
    hint: &ToolHint,
) {
    record_hook_analytics(
        root,
        event,
        serde_json::json!({
            "agent": agent.as_key(),
            "session_id": session_id,
            "category": hint.category.as_key(),
            "hint_id": hint_id,
        }),
    );
}

pub(super) fn record_workspace_status_analytics(
    root: Option<&Path>,
    status: HookWorkspaceStatus,
    session_id: Option<&str>,
) {
    record_hook_analytics(
        root,
        "workspace_status",
        serde_json::json!({
            "agent": HintAgent::Codex.as_key(),
            "session_id": session_id,
            "workspace_status": status.as_key(),
        }),
    );
}

pub(super) fn record_hint_emitted(
    root: Option<&Path>,
    agent: HintAgent,
    session_id: Option<&str>,
    hint_id: &str,
    hint: &ToolHint,
) {
    let event = if session_id.is_none() {
        "missing_session"
    } else {
        "hint_emitted"
    };
    record_hint_analytics(root, event, agent, session_id, hint_id, hint);
}

fn inferred_prompt_category(parsed: &Value) -> Option<&'static str> {
    let text = prompt_like_text(parsed)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if text.is_empty() {
        return None;
    }
    if claude::is_code_research_prompt(&text) {
        Some("code_research")
    } else if text.contains("test") || text.contains("failing") || text.contains("ci") {
        Some("test_or_ci")
    } else if text.contains("dashboard") || text.contains("ui") || text.contains("frontend") {
        Some("dashboard_or_ui")
    } else if text.contains("bug") || text.contains("fix") || text.contains("error") {
        Some("debug_or_fix")
    } else {
        Some("general")
    }
}

pub(super) fn record_hook_analytics(
    root: Option<&Path>,
    event: &str,
    mut fields: serde_json::Value,
) {
    let Some(path) = hook_analytics_path(root) else {
        return;
    };
    let Some(fields) = fields.as_object_mut() else {
        return;
    };
    if let Some(root) = root {
        fields.insert(
            "project_root".to_string(),
            serde_json::Value::String(root.display().to_string()),
        );
    }
    fields.insert(
        "event".to_string(),
        serde_json::Value::String(event.to_string()),
    );
    fields.insert(
        "ts_unix_ms".to_string(),
        serde_json::Value::Number(serde_json::Number::from(now_unix_millis())),
    );
    let Ok(line) = serde_json::to_string(&fields) else {
        return;
    };
    append_private_jsonl(&path, &line);
}

fn hook_analytics_path(root: Option<&Path>) -> Option<PathBuf> {
    match root {
        Some(root) => crate::storage::resolve_layout_for_current_profile(root)
            .ok()
            .map(|layout| layout.data_root.join(HOOK_ANALYTICS_FILENAME)),
        None => crate::storage::default_profile_root()
            .ok()
            .map(|root| root.join(HOOK_ANALYTICS_FILENAME)),
    }
}

fn append_private_jsonl(path: &Path, line: &str) {
    let _ = crate::storage::PrivateStoreIo::append_line(path, line);
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

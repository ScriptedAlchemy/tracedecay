use serde_json::{Value, json};

use crate::global_db::{AnalyticsEventInsert, GlobalDb};
use crate::mcp::hook_events::HookEvent;

pub(super) struct McpToolAnalyticsEvent<'a> {
    pub(super) project_root: &'a std::path::Path,
    pub(super) session_id: Option<String>,
    pub(super) tool_name: &'a str,
    pub(super) outcome: &'a str,
    pub(super) raw_file_tokens: u64,
    pub(super) response_tokens: u64,
    pub(super) net_saved_tokens: u64,
    pub(super) duration_us: Option<u64>,
    pub(super) timestamp: i64,
    pub(super) request_id: &'a Value,
    pub(super) arguments: &'a Value,
    pub(super) internal_analytics: Option<&'a Value>,
    /// The negotiated MCP client name from the `initialize` handshake's
    /// `clientInfo.name` (e.g. `"claude-code"`, `"codex"`, `"cursor"`).
    /// `None` when the client omitted `clientInfo` or no `initialize` was
    /// observed yet (e.g. a daemon-proxied first call). Bounded to the
    /// negotiated name only — never the full `clientInfo` payload.
    pub(super) client_name: Option<&'a str>,
}

pub(super) fn mcp_tool_analytics_event(input: McpToolAnalyticsEvent<'_>) -> AnalyticsEventInsert {
    let category = crate::accounting::classifier::classify(&[input.tool_name], &[]);
    let mut metadata = json!({
        "request_id": input.request_id,
        "transport": "mcp",
        "tool_kind": "mcp_tool",
        "before_tokens": input.raw_file_tokens,
        "after_tokens": input.response_tokens,
        "tokens_saved": input.net_saved_tokens,
        "duration_us": input.duration_us,
        "duration_ms": input.duration_us.map(|us| us / 1000),
        "client_name": input.client_name,
    });
    if input.outcome == "error" {
        metadata["failure_reason"] = json!("tool_dispatch_error");
    }
    if crate::analytics::is_skill_view_tool(input.tool_name) {
        metadata["arguments"] = input.arguments.clone();
        metadata["function"] = json!({
            "name": input.tool_name,
            "arguments": input.arguments,
        });
    }
    // Fact-store adoption is currently invisible in analytics: add/search/list
    // (tracedecay_fact_store) and helpful/unhelpful (tracedecay_fact_feedback)
    // calls all look identical without this. Record only the bounded action
    // string — never the fact content/arguments body.
    if matches!(
        input.tool_name,
        "tracedecay_fact_store" | "tracedecay_fact_feedback"
    ) {
        if let Some(action) = input.arguments.get("action").and_then(Value::as_str) {
            metadata["action"] = json!(action);
        }
    }
    append_tool_response_analytics(
        input.tool_name,
        input.arguments,
        input.internal_analytics,
        &mut metadata,
    );
    AnalyticsEventInsert {
        provider: "mcp".to_string(),
        project_id: GlobalDb::canonical_project_key(input.project_root),
        session_id: input.session_id,
        timestamp: input.timestamp,
        event_kind: "mcp_tool_call".to_string(),
        hook_name: None,
        tool_name: Some(input.tool_name.to_string()),
        tool_category: Some(category.as_str().to_string()),
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: Some(input.outcome.to_string()),
        metadata_json: Some(metadata.to_string()),
    }
}

pub(super) fn hook_route_analytics_event(
    project_root: &std::path::Path,
    event: &HookEvent,
    current_branch: Option<&str>,
    timestamp: i64,
) -> Option<AnalyticsEventInsert> {
    let route = event.route.as_ref()?;
    let metadata = json!({
        "agent": event.agent.as_wire(),
        "hook_kind": event.kind.as_key(),
        "event_cwd": event.cwd.as_ref().map(|path| path.display().to_string()),
        "route_cwd": route.cwd.as_ref().map(|path| path.display().to_string()),
        "worktree": route.worktree.as_ref().map(|path| path.display().to_string()),
        "route_branch": route.branch.as_deref(),
        "current_branch": current_branch,
        "thread_id": route.thread_id.as_deref(),
        "rel_path_count": event.rel_paths.len(),
        "has_command": event.command.is_some(),
    });
    Some(AnalyticsEventInsert {
        provider: "daemon_hook".to_string(),
        project_id: GlobalDb::canonical_project_key(project_root),
        session_id: route.session_id.clone(),
        timestamp,
        event_kind: "hook_route".to_string(),
        hook_name: Some(event.kind.as_key().to_string()),
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: Some("observed".to_string()),
        metadata_json: Some(metadata.to_string()),
    })
}

fn append_tool_response_analytics(
    tool_name: &str,
    arguments: &Value,
    internal_analytics: Option<&Value>,
    metadata: &mut Value,
) {
    if tool_name != "tracedecay_context" {
        return;
    }
    let include_memory = arguments
        .get("include_memory")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit = arguments
        .get("memory_limit")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 10);
    let min_trust = arguments
        .get("memory_min_trust")
        .and_then(Value::as_f64)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    if let Some(context_memory) = internal_analytics.and_then(|value| value.get("context_memory")) {
        metadata["context_memory"] = context_memory.clone();
        return;
    }
    metadata["context_memory"] = json!({
        "include_memory": include_memory,
        "limit": limit,
        "min_trust": min_trust,
        "match_count": 0,
        "fact_ids": [],
        "error": null,
    });
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use crate::daemon::HookRouteMetadata;
    use crate::mcp::hook_events::{HookAgent, HookEvent, HookEventKind};

    use super::{McpToolAnalyticsEvent, hook_route_analytics_event, mcp_tool_analytics_event};

    #[test]
    fn hook_route_analytics_event_preserves_correlation_fields() {
        let event = HookEvent {
            agent: HookAgent::Codex,
            kind: HookEventKind::Shell,
            rel_paths: Vec::new(),
            command: Some("cargo test".to_string()),
            cwd: Some(PathBuf::from("/repo")),
            route: Some(HookRouteMetadata {
                session_id: Some("session-123".to_string()),
                thread_id: Some("thread-456".to_string()),
                cwd: Some(PathBuf::from("/repo")),
                worktree: Some(PathBuf::from("/repo")),
                branch: Some("feature/hook-route".to_string()),
            }),
        };

        let Some(record) =
            hook_route_analytics_event(Path::new("/repo"), &event, Some("main"), 12345)
        else {
            panic!("route metadata should create analytics record");
        };
        let metadata: serde_json::Value =
            match serde_json::from_str(record.metadata_json.as_deref().unwrap_or("{}")) {
                Ok(metadata) => metadata,
                Err(err) => panic!("metadata should parse: {err}"),
            };

        assert_eq!(record.provider, "daemon_hook");
        assert_eq!(record.session_id.as_deref(), Some("session-123"));
        assert_eq!(record.event_kind, "hook_route");
        assert_eq!(record.hook_name.as_deref(), Some("shell"));
        assert_eq!(record.outcome.as_deref(), Some("observed"));
        assert_eq!(metadata["agent"], "codex");
        assert_eq!(metadata["thread_id"], "thread-456");
        assert_eq!(metadata["route_branch"], "feature/hook-route");
        assert_eq!(metadata["current_branch"], "main");
        assert_eq!(metadata["has_command"], true);
    }

    #[test]
    fn mcp_tool_analytics_event_records_action_and_client_for_fact_store() {
        let request_id = json!(1);
        let arguments = json!({"action": "add", "content": "secret fact body"});
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root: Path::new("/repo"),
            session_id: Some("session-abc".to_string()),
            tool_name: "tracedecay_fact_store",
            outcome: "success",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us: Some(500),
            timestamp: 12345,
            request_id: &request_id,
            arguments: &arguments,
            internal_analytics: None,
            client_name: Some("claude-code"),
        });

        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}"))
                .expect("metadata should parse");

        assert_eq!(event.tool_name.as_deref(), Some("tracedecay_fact_store"));
        assert_eq!(metadata["action"], "add");
        assert_eq!(metadata["client_name"], "claude-code");
        // The action string is recorded, but never the fact content/arguments body.
        assert!(metadata.get("content").is_none());
        assert!(metadata.get("arguments").is_none());
    }

    #[test]
    fn mcp_tool_analytics_event_records_action_for_fact_feedback() {
        let request_id = json!(2);
        let arguments = json!({"action": "unhelpful", "fact_id": 42});
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root: Path::new("/repo"),
            session_id: None,
            tool_name: "tracedecay_fact_feedback",
            outcome: "success",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us: None,
            timestamp: 12345,
            request_id: &request_id,
            arguments: &arguments,
            internal_analytics: None,
            client_name: None,
        });

        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}"))
                .expect("metadata should parse");

        assert_eq!(metadata["action"], "unhelpful");
        assert!(metadata["client_name"].is_null());
    }

    #[test]
    fn mcp_tool_analytics_event_omits_action_for_unrelated_tools() {
        let request_id = json!(3);
        let arguments = json!({"action": "add"});
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root: Path::new("/repo"),
            session_id: None,
            tool_name: "tracedecay_search",
            outcome: "success",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us: None,
            timestamp: 12345,
            request_id: &request_id,
            arguments: &arguments,
            internal_analytics: None,
            client_name: Some("codex"),
        });

        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}"))
                .expect("metadata should parse");

        assert!(metadata.get("action").is_none());
        assert_eq!(metadata["client_name"], "codex");
    }
}

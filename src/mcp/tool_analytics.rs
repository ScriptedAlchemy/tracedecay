use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::global_db::{AnalyticsEventInsert, RegisteredGlobalDb};
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
    /// Stable per-process MCP server instance id (a random hex token minted
    /// once at server start). Recorded in `metadata.mcp_instance_id` on every
    /// event so calls from one server lifetime can be grouped even when
    /// `session_id` is absent — which is the common case: the MCP transport
    /// negotiates only `clientInfo` (host name), never a session/conversation
    /// id, so `session_id` is populated only when the client happens to thread
    /// `session_id`/`sessionId` through the tool arguments (rare — ~97.6% of
    /// historical events had a NULL `session_id`). This is an honest grouping
    /// key, NOT a real session id, so it stays in metadata rather than
    /// masquerading in the `session_id` column.
    pub(super) mcp_instance_id: Option<&'a str>,
    /// Bounded, sanitized (no argument bodies) reason for a `outcome ==
    /// "error"` call, e.g. a structural edit-tool failure message or a
    /// dispatch error's `Display` text. `None` falls back to a generic
    /// marker so pre-existing callers that have not been migrated to supply
    /// a real reason keep working.
    pub(super) failure_reason: Option<&'a str>,
}

/// Failure reasons are capped well below the metadata column's practical
/// size so a pathological message can't bloat the analytics event; this
/// intentionally excludes tool `arguments`, which may carry user file
/// contents.
const FAILURE_REASON_MAX_CHARS: usize = 160;

/// Hex chars kept from a SHA-256 digest for cardinality-capped labels
/// (paths, session/thread ids, branches). Full digests are unnecessary for
/// correlation and inflate label cardinality / export size.
const CARDINALITY_LABEL_HASH_CHARS: usize = 16;
const LOOKUP_IDENTIFIER_MAX_BYTES: usize = 256;

/// Collapse whitespace and cap a failure reason to
/// [`FAILURE_REASON_MAX_CHARS`] characters (never argument bodies — callers
/// must derive `reason` from response/error text only).
pub(super) fn bounded_failure_reason(reason: &str) -> String {
    let collapsed: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= FAILURE_REASON_MAX_CHARS {
        collapsed
    } else {
        collapsed.chars().take(FAILURE_REASON_MAX_CHARS).collect()
    }
}

/// Stable short hash for high-cardinality or private identifiers. Never
/// embeds the raw value — only `h:` + truncated SHA-256 hex.
fn hashed_cardinality_label(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!(
        "h:{}",
        hex::encode(&digest[..(CARDINALITY_LABEL_HASH_CHARS / 2)])
    )
}

fn optional_hashed_label(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(hashed_cardinality_label)
}

fn optional_hashed_path(path: Option<&Path>) -> Option<String> {
    path.map(|path| hashed_cardinality_label(&path.display().to_string()))
}

fn bounded_lookup_identifier(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= LOOKUP_IDENTIFIER_MAX_BYTES
                && !value.chars().any(char::is_control)
        })
        .and_then(|value| crate::privacy::protect_sensitive_structural_id(value).ok())
}

/// One durable spool sequence represents one admitted host event. Identical
/// envelopes intentionally receive distinct sequences, so the sequence—not
/// route/session content—is the non-lossy analytics idempotency identity.
fn hook_route_idempotency_key(project_root: &Path, admission_seq: u64) -> String {
    hashed_cardinality_label(&format!(
        "hook_route_v1|{}|{admission_seq}",
        RegisteredGlobalDb::canonical_project_key(project_root)
    ))
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
        "mcp_instance_id": input.mcp_instance_id,
    });
    if input.outcome == "error" {
        metadata["failure_reason"] = json!(
            input
                .failure_reason
                .map_or_else(|| "tool_dispatch_error".to_string(), bounded_failure_reason)
        );
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
    ) && let Some(action) = input.arguments.get("action").and_then(Value::as_str)
    {
        metadata["action"] = json!(action);
    }

    append_tool_response_analytics(
        input.tool_name,
        input.arguments,
        input.internal_analytics,
        &mut metadata,
    );
    AnalyticsEventInsert {
        provider: "mcp".to_string(),
        project_id: RegisteredGlobalDb::canonical_project_key(input.project_root),
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

/// Build a hook-route analytics insert.
///
/// Paths and branch labels are hashed; structural session/thread identities
/// have already passed through deterministic credential protection at the
/// hook boundary and remain byte-identical for joins. Command bodies and
/// receipt payloads are never included. `admission_seq` is the per-admission
/// idempotency identity.
pub(super) fn hook_route_analytics_event(
    project_root: &std::path::Path,
    event: &HookEvent,
    current_branch: Option<&str>,
    timestamp: i64,
    admission_seq: u64,
) -> Option<AnalyticsEventInsert> {
    let route = event.route.as_ref()?;
    let session_id = bounded_lookup_identifier(route.session_id.as_deref());
    let idempotency_key = hook_route_idempotency_key(project_root, admission_seq);
    let metadata = json!({
        "agent": event.agent.as_wire(),
        "hook_kind": event.kind.as_key(),
        "event_cwd": optional_hashed_path(event.cwd.as_deref()),
        "route_cwd": optional_hashed_path(route.cwd.as_deref()),
        "worktree": optional_hashed_path(route.worktree.as_deref()),
        "route_branch": optional_hashed_label(route.branch.as_deref()),
        "current_branch": optional_hashed_label(current_branch),
        "thread_id": bounded_lookup_identifier(route.thread_id.as_deref()),
        "rel_path_count": event.rel_paths.len(),
        "has_command": event.had_command,
        "admission_seq": admission_seq,
        "idempotency_key": idempotency_key.clone(),
    });
    Some(AnalyticsEventInsert {
        provider: "daemon_hook".to_string(),
        project_id: RegisteredGlobalDb::canonical_project_key(project_root),
        session_id,
        timestamp,
        event_kind: "hook_route".to_string(),
        hook_name: Some(event.kind.as_key().to_string()),
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some(idempotency_key),
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
    if let Some(stage_timings) = internal_analytics.and_then(|value| value.get("stage_timings_us"))
    {
        metadata["stage_timings_us"] = stage_timings.clone();
    }
    if let Some(symbol_coverage) = internal_analytics.and_then(|value| value.get("symbol_coverage"))
    {
        metadata["symbol_coverage"] = symbol_coverage.clone();
    }
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

    use super::{
        FAILURE_REASON_MAX_CHARS, McpToolAnalyticsEvent, bounded_failure_reason,
        hashed_cardinality_label, hook_route_analytics_event, mcp_tool_analytics_event,
    };

    #[test]
    fn hook_route_analytics_event_preserves_protected_ids_and_omits_payloads() {
        let event = HookEvent {
            agent: HookAgent::Codex,
            kind: HookEventKind::Shell,
            rel_paths: Vec::new(),
            had_command: true,
            cwd: Some(PathBuf::from("/home/user/private-repo")),
            route: Some(HookRouteMetadata {
                session_id: Some("session-123".to_string()),
                thread_id: Some("thread-456".to_string()),
                cwd: Some(PathBuf::from("/home/user/private-repo")),
                worktree: Some(PathBuf::from("/home/user/private-repo")),
                branch: Some("feature/hook-route".to_string()),
            }),
            receipt: None,
        };

        let Some(record) =
            hook_route_analytics_event(Path::new("/repo"), &event, Some("main"), 12345, 7)
        else {
            panic!("route metadata should create analytics record");
        };
        let metadata: serde_json::Value =
            match serde_json::from_str(record.metadata_json.as_deref().unwrap_or("{}")) {
                Ok(metadata) => metadata,
                Err(err) => panic!("metadata should parse: {err}"),
            };

        let expected_branch = hashed_cardinality_label("feature/hook-route");
        let expected_cwd = hashed_cardinality_label("/home/user/private-repo");
        let expected_key = record.hint_id.clone().expect("idempotency key");

        assert_eq!(record.provider, "daemon_hook");
        assert_eq!(record.session_id.as_deref(), Some("session-123"));
        assert_eq!(record.event_kind, "hook_route");
        assert_eq!(record.hook_name.as_deref(), Some("shell"));
        assert_eq!(record.outcome.as_deref(), Some("observed"));
        assert_eq!(record.hint_id.as_deref(), Some(expected_key.as_str()));
        assert_eq!(metadata["agent"], "codex");
        assert_eq!(metadata["thread_id"], "thread-456");
        assert_eq!(metadata["route_branch"], expected_branch);
        assert_eq!(metadata["current_branch"], hashed_cardinality_label("main"));
        assert_eq!(metadata["event_cwd"], expected_cwd);
        assert_eq!(metadata["route_cwd"], expected_cwd);
        assert_eq!(metadata["worktree"], expected_cwd);
        assert_eq!(metadata["has_command"], true);
        assert_eq!(metadata["admission_seq"], 7);
        assert_eq!(metadata["idempotency_key"], expected_key);
        // Reject secret/payload leakage: no raw paths or command bodies.
        // Public structural ids stay byte-identical for joins.
        let serialized = record.metadata_json.clone().unwrap_or_default();
        assert!(!serialized.contains("private-repo"));
        assert!(!serialized.contains("cargo test"));
        assert!(!serialized.contains("session-123"));
        assert!(serialized.contains("thread-456"));
        assert!(!serialized.contains("feature/hook-route"));
    }

    #[test]
    fn hook_route_idempotency_key_distinguishes_durable_admissions() {
        let event = HookEvent {
            agent: HookAgent::Codex,
            kind: HookEventKind::Shell,
            rel_paths: Vec::new(),
            had_command: false,
            cwd: Some(PathBuf::from("/repo")),
            route: Some(HookRouteMetadata {
                session_id: Some("session-123".to_string()),
                thread_id: Some("thread-456".to_string()),
                cwd: Some(PathBuf::from("/repo")),
                worktree: None,
                branch: Some("main".to_string()),
            }),
            receipt: None,
        };
        let first =
            hook_route_analytics_event(Path::new("/repo"), &event, Some("main"), 1, 1).unwrap();
        let second =
            hook_route_analytics_event(Path::new("/repo"), &event, Some("main"), 2, 99).unwrap();
        assert_ne!(first.hint_id, second.hint_id);
        assert!(
            first
                .metadata_json
                .as_deref()
                .unwrap()
                .contains("\"admission_seq\":1")
        );
        assert!(
            second
                .metadata_json
                .as_deref()
                .unwrap()
                .contains("\"admission_seq\":99")
        );
    }

    #[test]
    fn bounded_failure_reason_collapses_whitespace_and_truncates() {
        assert_eq!(
            bounded_failure_reason("old_str  not\nfound"),
            "old_str not found"
        );
        let long = "x".repeat(FAILURE_REASON_MAX_CHARS + 50);
        let bounded = bounded_failure_reason(&long);
        assert_eq!(bounded.chars().count(), FAILURE_REASON_MAX_CHARS);
    }

    #[test]
    fn mcp_tool_analytics_event_uses_real_failure_reason_when_provided() {
        let request_id = json!(1);
        let arguments = json!({});
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root: Path::new("/repo"),
            session_id: None,
            tool_name: "tracedecay_str_replace",
            outcome: "error",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us: None,
            timestamp: 0,
            request_id: &request_id,
            arguments: &arguments,
            internal_analytics: None,
            client_name: None,
            mcp_instance_id: None,
            failure_reason: Some("old_str not found in src/main.rs"),
        });
        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}")).unwrap();
        assert_eq!(
            metadata["failure_reason"],
            "old_str not found in src/main.rs"
        );
    }

    #[test]
    fn mcp_tool_analytics_event_falls_back_to_generic_reason_when_none_provided() {
        let request_id = json!(1);
        let arguments = json!({});
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root: Path::new("/repo"),
            session_id: None,
            tool_name: "tracedecay_not_a_real_tool",
            outcome: "error",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us: None,
            timestamp: 0,
            request_id: &request_id,
            arguments: &arguments,
            internal_analytics: None,
            client_name: None,
            mcp_instance_id: None,
            failure_reason: None,
        });
        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}")).unwrap();
        assert_eq!(metadata["failure_reason"], "tool_dispatch_error");
    }

    #[test]
    fn mcp_tool_analytics_event_omits_failure_reason_on_success() {
        let request_id = json!(1);
        let arguments = json!({});
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root: Path::new("/repo"),
            session_id: None,
            tool_name: "tracedecay_str_replace",
            outcome: "success",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us: None,
            timestamp: 0,
            request_id: &request_id,
            arguments: &arguments,
            internal_analytics: None,
            client_name: None,
            mcp_instance_id: None,
            failure_reason: Some("should be ignored on success"),
        });
        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}")).unwrap();
        assert!(metadata.get("failure_reason").is_none());
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
            mcp_instance_id: Some("mcp-instance-test"),
            failure_reason: None,
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
            mcp_instance_id: None,
            failure_reason: None,
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
            mcp_instance_id: Some("mcp-instance-test"),
            failure_reason: None,
        });

        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}"))
                .expect("metadata should parse");

        assert!(metadata.get("action").is_none());
        assert_eq!(metadata["client_name"], "codex");
        assert_eq!(metadata["mcp_instance_id"], "mcp-instance-test");
    }

    #[test]
    fn mcp_tool_analytics_event_records_instance_id_when_session_absent() {
        // The common case: no client-supplied session_id, so the honest
        // grouping key is the per-process mcp_instance_id in metadata while
        // the session_id column stays NULL.
        let request_id = json!(9);
        let arguments = json!({});
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
            client_name: Some("claude-code"),
            mcp_instance_id: Some("mcp-abc123"),
            failure_reason: None,
        });

        assert!(
            event.session_id.is_none(),
            "instance id must not masquerade as a real session id"
        );
        let metadata: serde_json::Value =
            serde_json::from_str(event.metadata_json.as_deref().unwrap_or("{}"))
                .expect("metadata should parse");
        assert_eq!(metadata["mcp_instance_id"], "mcp-abc123");
    }
}

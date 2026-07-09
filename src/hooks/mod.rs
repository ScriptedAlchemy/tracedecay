//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! Each agent sends its own event schema and expects its own output shape, so
//! handlers stay agent-specific while shared plumbing lives here.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

mod analytics;
mod claude;
mod codex;
mod cursor;
mod cursor_compact;
mod cursor_shell;
pub mod hint_outcomes;
mod kiro;
pub(crate) mod memory_inject;
mod post_tool_use;
mod steering;
pub mod tool_hints;

pub use claude::{
    claude_session_context_for_event, evaluate_hook_decision, hook_claude_post_tool_use,
    hook_claude_session_start, hook_claude_subagent_start, hook_pre_tool_use, hook_prompt_submit,
    hook_stop,
};
pub use codex::{
    codex_additional_context_json, codex_apply_patch_rel_paths, codex_project_root_from_event,
    codex_subagent_start_log_line, codex_user_prompt_submit_context_for_event,
    codex_workspace_status_from_event, evaluate_codex_subagent_start, hook_codex_post_compact,
    hook_codex_post_tool_use, hook_codex_session_start, hook_codex_subagent_start,
    hook_codex_user_prompt_submit, record_codex_subagent_start,
};
pub use cursor::{
    CURSOR_CATCH_UP_INGEST_MAX_BYTES, cursor_after_file_edit_decision,
    cursor_after_file_edit_rel_paths, cursor_before_submit_prompt_json,
    cursor_post_tool_use_decision, cursor_project_root_from_event, cursor_session_start_json,
    cursor_should_run_sync, evaluate_cursor_after_file_edit, evaluate_cursor_post_tool_use,
    evaluate_cursor_subagent_start, hook_cursor_after_file_edit, hook_cursor_after_shell,
    hook_cursor_before_submit_prompt, hook_cursor_post_tool_use, hook_cursor_pre_compact,
    hook_cursor_session_end, hook_cursor_session_start, hook_cursor_stop,
    hook_cursor_subagent_start, hook_cursor_workspace_open,
};
pub use cursor_compact::{CursorPreCompactOutcome, cursor_pre_compact_for_event_with_config};
pub use cursor_shell::{
    CursorShellSyncPlan, cursor_branch_switch_target, cursor_shell_command_targets_project,
    cursor_shell_sync_plan, cursor_shell_sync_plan_with_current_branch,
    is_git_state_changing_command, resolve_worktree_add_root,
};
pub use kiro::{
    evaluate_kiro_pre_tool_use, hook_kiro_post_tool_use, hook_kiro_pre_tool_use,
    hook_kiro_prompt_submit, kiro_post_tool_use_rel_paths,
};
pub use post_tool_use::{
    CLAUDE_POST_TOOL_USE_EDIT_TOOLS, CLAUDE_POST_TOOL_USE_SHELL_TOOLS, claude_post_tool_use_matcher,
};
pub use steering::{
    CURSOR_PLUGIN_SKILLS, HookWorkspaceStatus, build_codex_session_context,
    build_codex_session_context_for_workspace, build_cursor_session_context, cursor_staleness_hint,
};

#[cfg(test)]
use analytics::HOOK_ANALYTICS_FILENAME;
use analytics::{
    mint_hint_id, record_hint_analytics, record_hint_emitted, record_hook_analytics,
    record_hook_invoked, record_workspace_status_analytics,
};
use tool_hints::{HintAgent, ToolHint};

macro_rules! read_hook_event {
    () => {{
        match $crate::hooks::read_stdin_to_string() {
            Ok(event) => event,
            Err(e) => {
                eprintln!("tracedecay hook: failed to read stdin: {e}");
                return 1;
            }
        }
    }};
}
pub(crate) use read_hook_event;

const TRACEDECAY_RESEARCH_BLOCK_REASON: &str = "STOP: Use tracedecay MCP tools \
(tracedecay_context, tracedecay_grep, tracedecay_search, tracedecay_callees, \
tracedecay_callers, tracedecay_impact, tracedecay_files, tracedecay_affected) \
instead of agents for code research. Route literal/regex text to tracedecay_grep, \
symbol names to tracedecay_search, and concepts to tracedecay_context. TraceDecay \
is faster and more precise for symbol relationships, call paths, and code structure. \
Only use agents for code exploration if you have already tried tracedecay and it \
cannot answer the question.";

fn research_block_reason(hint: Option<ToolHint>) -> String {
    let base = crate::config::brand_env("RESEARCH_BLOCK_REASON")
        .unwrap_or_else(|| TRACEDECAY_RESEARCH_BLOCK_REASON.to_string());
    hint.map_or_else(
        || base.clone(),
        |hint| format!("{}\n\n{}", base, format_tool_hint(&hint)),
    )
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
pub(crate) fn lock_test_env() -> std::sync::MutexGuard<'static, ()> {
    crate::config::lock_user_data_dir_test_env()
}

fn hook_route_metadata_from_event(
    event_json: &str,
    project_root: &Path,
) -> Option<crate::daemon::HookRouteMetadata> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    Some(hook_route_metadata_from_parsed(&parsed, project_root))
}

fn hook_route_metadata_from_parsed(
    parsed: &Value,
    project_root: &Path,
) -> crate::daemon::HookRouteMetadata {
    let cwd = event_cwd_from_parsed(parsed);
    let route_root = cwd.as_deref().unwrap_or(project_root);
    let worktree = crate::worktree::git_worktree_root(route_root)
        .unwrap_or_else(|| project_root.to_path_buf());
    let branch = crate::branch::current_branch(&worktree);
    crate::daemon::HookRouteMetadata {
        session_id: hook_route_session_id(parsed),
        thread_id: text_field(
            parsed,
            &[
                "thread_id",
                "threadId",
                "conversation_thread_id",
                "conversationThreadId",
            ],
        ),
        cwd,
        worktree: Some(worktree),
        branch,
    }
}

fn hook_route_session_id(parsed: &Value) -> Option<String> {
    text_field(
        parsed,
        &[
            "session_id",
            "sessionId",
            "conversation_id",
            "conversationId",
            "chat_id",
            "chatId",
        ],
    )
}

/// Dedupes a hint for a project without a pre-minted candidate id. Callers that
/// do not record their own `hint_candidate` (e.g. the Claude post-tool-use
/// surface) use this shape; it mints an id so the terminal row still correlates
/// via `hint_id`. Callers that already recorded a `hint_candidate` must instead
/// use [`deduped_project_hint_with_id`] so the terminal shares that id.
fn deduped_project_hint(
    root: Option<PathBuf>,
    agent: HintAgent,
    session_id: Option<String>,
    hint: ToolHint,
) -> Option<ToolHint> {
    let hint_id = mint_hint_id();
    deduped_project_hint_with_id(root, agent, session_id, &hint_id, hint)
}

fn deduped_project_hint_with_id(
    root: Option<PathBuf>,
    agent: HintAgent,
    session_id: Option<String>,
    hint_id: &str,
    hint: ToolHint,
) -> Option<ToolHint> {
    let Some(root) = root else {
        record_hint_emitted(None, agent, session_id.as_deref(), hint_id, &hint);
        return Some(hint);
    };
    let Some(session_id) = session_id else {
        record_hint_emitted(Some(&root), agent, None, hint_id, &hint);
        return Some(hint);
    };
    // Without a resolvable data dir we cannot count budget/escalation across
    // fires, so fall back to emitting the raw hint once per candidate.
    let Ok(layout) = crate::storage::resolve_layout_for_current_profile(&root) else {
        record_hint_emitted(Some(&root), agent, Some(&session_id), hint_id, &hint);
        return Some(hint);
    };
    if !layout.data_root.is_dir() {
        record_hint_emitted(Some(&root), agent, Some(&session_id), hint_id, &hint);
        return Some(hint);
    }
    let path = layout.data_root.join("tool_hints_seen.json");
    let mut dedupe = tool_hints::ToolHintDedupe::load_or_default(&path);
    match dedupe.decide(&session_id, hint.category) {
        tool_hints::HintDecision::Emit => {
            let _ = dedupe.save(&path);
            record_hint_analytics(
                Some(&root),
                "hint_emitted",
                agent,
                Some(&session_id),
                hint_id,
                &hint,
            );
            Some(hint)
        }
        tool_hints::HintDecision::Escalate => {
            let _ = dedupe.save(&path);
            let escalated = hint.escalated();
            record_hint_analytics(
                Some(&root),
                "hint_escalated",
                agent,
                Some(&session_id),
                hint_id,
                &escalated,
            );
            Some(escalated)
        }
        tool_hints::HintDecision::SuppressedBudget => {
            let _ = dedupe.save(&path);
            record_hint_analytics(
                Some(&root),
                "suppressed_budget",
                agent,
                Some(&session_id),
                hint_id,
                &hint,
            );
            None
        }
        tool_hints::HintDecision::SuppressedDuplicate => {
            let _ = dedupe.save(&path);
            record_hint_analytics(
                Some(&root),
                "suppressed_duplicate",
                agent,
                Some(&session_id),
                hint_id,
                &hint,
            );
            None
        }
    }
}

fn nearest_project_like_root(start: &Path) -> Option<PathBuf> {
    if let Some(root) = crate::worktree::git_worktree_root(start) {
        return Some(root);
    }
    let mut dir = start.to_path_buf();
    loop {
        if project_marker_exists(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn is_project_like_workspace(cwd: &Path) -> bool {
    nearest_project_like_root(cwd).is_some()
}

fn project_marker_exists(dir: &Path) -> bool {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "deno.json",
        "tsconfig.json",
    ];
    MARKERS.iter().any(|marker| dir.join(marker).exists())
}

fn rel_under_root(root: &Path, abs: &Path) -> Option<String> {
    let stripped = abs.strip_prefix(root).ok()?;
    if stripped.as_os_str().is_empty() {
        return None;
    }
    if stripped.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(stripped.to_string_lossy().replace('\\', "/"))
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn prompt_like_text(parsed: &Value) -> Option<String> {
    [
        "prompt",
        "user_prompt",
        "message",
        "input",
        "task",
        "description",
    ]
    .iter()
    .find_map(|key| parsed.get(*key).and_then(Value::as_str))
    .filter(|text| !text.is_empty())
    .map(str::to_string)
}

fn event_session_id(parsed: &Value) -> Option<String> {
    ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn event_i64(parsed: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let value = parsed.get(*key)?;
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_str()?.parse::<i64>().ok())
    })
}

fn event_usize(parsed: &Value, keys: &[&str]) -> Option<usize> {
    event_i64(parsed, keys).and_then(|value| usize::try_from(value).ok())
}

/// Reads the `cwd` string field from a hook event JSON payload. Shared by the
/// Kiro and Codex handlers, both of which send the session working directory.
fn event_cwd(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    event_cwd_from_parsed(&parsed)
}

fn event_cwd_from_parsed(parsed: &Value) -> Option<PathBuf> {
    let cwd = parsed.get("cwd").and_then(Value::as_str)?;
    let path = Path::new(cwd);
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path.to_path_buf())
    }
}

fn format_tool_hint(hint: &ToolHint) -> String {
    format!("tracedecay hint: {}\n{}", hint.message, hint.context)
}

fn append_tool_hint(context: &mut String, hint: &ToolHint) {
    if !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(&format_tool_hint(hint));
    context.push('\n');
}

pub(crate) fn read_stdin_to_string() -> std::io::Result<String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    Ok(input)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod hint_analytics_tests {
    use super::tool_hints::{HintCategory, MAX_HINTS_PER_SESSION};
    use super::{
        HintAgent, Path, PathBuf, ToolHint, Value, deduped_project_hint_with_id, mint_hint_id,
        record_hint_emitted, record_hook_invoked,
    };
    use crate::config::USER_DATA_DIR_ENV;
    use std::collections::HashSet;

    /// Terminal event kinds a single `hint_candidate` may resolve to. Every
    /// candidate must be followed by exactly one of these.
    const TERMINAL_EVENTS: &[&str] = &[
        "hint_emitted",
        "hint_escalated",
        "suppressed_duplicate",
        "suppressed_budget",
        "dropped_no_root",
        "missing_session",
    ];

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn test_hint() -> ToolHint {
        ToolHint {
            category: HintCategory::Impact,
            message: "use tracedecay_impact".to_string(),
            context: "context".to_string(),
            nonblocking: true,
        }
    }

    /// Enrolls `project_root` in the profile store and materializes its data dir
    /// so `deduped_project_hint` reaches the on-disk dedupe branch.
    fn enroll_project(project_root: &Path, project_id: &str) -> PathBuf {
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let layout = crate::storage::resolve_layout_for_current_profile(project_root).unwrap();
        std::fs::create_dir_all(&layout.data_root).unwrap();
        layout.data_root
    }

    /// Reads every recorded analytics row visible to a project: its own store
    /// file plus the user-level fallback file.
    fn recorded_rows(data_root: &Path, profile_root: &Path) -> Vec<Value> {
        let mut rows = Vec::new();
        for path in [
            data_root.join(super::HOOK_ANALYTICS_FILENAME),
            profile_root.join(super::HOOK_ANALYTICS_FILENAME),
        ] {
            if let Ok(text) = std::fs::read_to_string(&path) {
                for line in text.lines() {
                    if let Ok(row) = serde_json::from_str::<Value>(line) {
                        rows.push(row);
                    }
                }
            }
        }
        rows
    }

    fn event_kind(row: &Value) -> &str {
        row.get("event").and_then(Value::as_str).unwrap_or_default()
    }

    fn hint_id(row: &Value) -> &str {
        row.get("hint_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// Rows carrying a specific `hint_id`, in insertion order.
    fn events_for<'a>(rows: &'a [Value], id: &str) -> Vec<&'a Value> {
        rows.iter().filter(|row| hint_id(row) == id).collect()
    }

    #[test]
    fn mint_hint_id_is_unique_across_calls() {
        let ids: HashSet<String> = (0..256).map(|_| mint_hint_id()).collect();
        assert_eq!(ids.len(), 256, "hint ids must be unique");
    }

    #[test]
    fn hook_invocation_rows_include_duration_telemetry() {
        let _lock = super::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_hook_duration");

        {
            let _hook_telemetry = record_hook_invoked(
                Some(&project_root),
                HintAgent::Codex,
                "PostToolUse",
                r#"{"session_id":"s1","tool_name":"Bash","cwd":"/tmp"}"#,
            );
        }

        let rows = recorded_rows(&data_root, &profile_root);
        let row = rows
            .iter()
            .find(|row| event_kind(row) == "hook_completed")
            .expect("hook_completed row");
        assert_eq!(row["hook_name"].as_str(), Some("PostToolUse"));
        assert_eq!(row["tool_name"].as_str(), Some("Bash"));
        assert!(row["duration_us"].as_u64().is_some());
        assert!(row["duration_ms"].as_u64().is_some());
    }

    #[test]
    fn record_hint_emitted_missing_session_is_single_terminal() {
        let _lock = super::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_missing_session");
        let hint = test_hint();
        let id = mint_hint_id();

        record_hint_emitted(Some(&project_root), HintAgent::Cursor, None, &id, &hint);

        let rows = recorded_rows(&data_root, &profile_root);
        let seq: Vec<&str> = events_for(&rows, &id)
            .iter()
            .map(|row| event_kind(row))
            .collect();
        // Exactly one terminal, and it is `missing_session` (never also
        // `hint_emitted`) so the per-candidate outcome count stays 1.
        assert_eq!(seq, vec!["missing_session"], "single terminal expected");
    }

    /// Walks each terminal branch of the hint pipeline and asserts that the
    /// candidate resolves to exactly one terminal event carrying the same
    /// `hint_id`, and that the row is attributed to the project when a root is
    /// known.
    #[test]
    fn every_hint_branch_yields_exactly_one_terminal_with_hint_id() {
        let _lock = super::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_terminal_invariant");

        let project_key = crate::global_db::GlobalDb::canonical_project_key(&project_root);

        // Branch: root known, session known → on-disk dedupe emits once.
        let emit_id = mint_hint_id();
        assert!(
            deduped_project_hint_with_id(
                Some(project_root.clone()),
                HintAgent::Cursor,
                Some("session-emit".to_string()),
                &emit_id,
                test_hint(),
            )
            .is_some()
        );

        // Branch: same (session, category) again → suppressed as duplicate.
        let dup_id = mint_hint_id();
        assert!(
            deduped_project_hint_with_id(
                Some(project_root.clone()),
                HintAgent::Cursor,
                Some("session-emit".to_string()),
                &dup_id,
                test_hint(),
            )
            .is_none()
        );

        // Branch: root known, session missing → single `missing_session` terminal.
        let no_session_id = mint_hint_id();
        assert!(
            deduped_project_hint_with_id(
                Some(project_root.clone()),
                HintAgent::Cursor,
                None,
                &no_session_id,
                test_hint(),
            )
            .is_some()
        );

        // Branch: no root at all → emits with no attribution.
        let no_root_id = mint_hint_id();
        assert!(
            deduped_project_hint_with_id(
                None,
                HintAgent::Cursor,
                Some("session-noroot".to_string()),
                &no_root_id,
                test_hint(),
            )
            .is_some()
        );

        let rows = recorded_rows(&data_root, &profile_root);

        let cases = [
            (&emit_id, "hint_emitted", true),
            (&dup_id, "suppressed_duplicate", true),
            (&no_session_id, "missing_session", true),
            (&no_root_id, "hint_emitted", false),
        ];
        for (id, expected_terminal, expect_attribution) in cases {
            let matched = events_for(&rows, id);
            let terminals: Vec<&str> = matched
                .iter()
                .map(|row| event_kind(row))
                .filter(|kind| TERMINAL_EVENTS.contains(kind))
                .collect();
            assert_eq!(
                terminals,
                vec![expected_terminal],
                "hint_id {id} must have exactly one terminal ({expected_terminal})"
            );
            for row in &matched {
                assert_eq!(hint_id(row), id.as_str(), "hint_id must be carried");
                let attributed = row
                    .get("project_root")
                    .and_then(Value::as_str)
                    .map(|root| crate::global_db::GlobalDb::canonical_project_key(Path::new(root)));
                if expect_attribution {
                    assert_eq!(
                        attributed.as_deref(),
                        Some(project_key.as_str()),
                        "row for {id} must carry the canonical project key"
                    );
                }
            }
        }
    }

    /// A hint over the per-session budget resolves to a single `suppressed_budget`
    /// terminal, and no hint is returned to the caller.
    #[test]
    fn budget_exhaustion_records_suppressed_budget_terminal() {
        let _lock = super::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_budget");

        let session = "session-budget".to_string();
        // Fill the budget with distinct categories.
        let categories = [
            HintCategory::Search,
            HintCategory::FileRead,
            HintCategory::Impact,
        ];
        assert_eq!(categories.len(), MAX_HINTS_PER_SESSION);
        for category in categories {
            let hint = ToolHint {
                category,
                message: "m".to_string(),
                context: "c".to_string(),
                nonblocking: true,
            };
            assert!(
                deduped_project_hint_with_id(
                    Some(project_root.clone()),
                    HintAgent::Cursor,
                    Some(session.clone()),
                    &mint_hint_id(),
                    hint,
                )
                .is_some()
            );
        }

        // A fourth, not-yet-seen category is over budget (test_hint's Impact is
        // already spent above, so use a distinct category to isolate the budget
        // branch from the duplicate branch).
        let over_id = mint_hint_id();
        let over = deduped_project_hint_with_id(
            Some(project_root.clone()),
            HintAgent::Cursor,
            Some(session.clone()),
            &over_id,
            ToolHint {
                category: HintCategory::CallGraph,
                message: "m".to_string(),
                context: "c".to_string(),
                nonblocking: true,
            },
        );
        assert!(over.is_none(), "over-budget hint must be suppressed");

        let rows = recorded_rows(&data_root, &profile_root);
        let terminals: Vec<&str> = events_for(&rows, &over_id)
            .iter()
            .map(|row| event_kind(row))
            .filter(|kind| TERMINAL_EVENTS.contains(kind))
            .collect();
        assert_eq!(terminals, vec!["suppressed_budget"]);
    }

    /// Repeated native usage past the escalation threshold surfaces exactly one
    /// stronger re-hint recorded as `hint_escalated`, with the escalation prefix.
    #[test]
    fn repeated_usage_records_hint_escalated_terminal() {
        let _lock = super::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_escalate");

        let session = "session-escalate".to_string();
        let emit = |id: &str| {
            deduped_project_hint_with_id(
                Some(project_root.clone()),
                HintAgent::Cursor,
                Some(session.clone()),
                id,
                test_hint(),
            )
        };

        // First fire emits; the next fires below the threshold are silent; the
        // threshold fire escalates.
        assert!(emit(&mint_hint_id()).is_some(), "first fire emits");
        assert!(
            emit(&mint_hint_id()).is_none(),
            "below-threshold fire silent"
        );
        assert!(
            emit(&mint_hint_id()).is_none(),
            "below-threshold fire silent"
        );

        let escalate_id = mint_hint_id();
        let escalated = emit(&escalate_id).expect("threshold fire escalates");
        assert!(
            escalated.message.starts_with("Repeated native"),
            "escalation must carry the stronger prefix: {}",
            escalated.message
        );

        // A further fire is permanently silent.
        assert!(
            emit(&mint_hint_id()).is_none(),
            "post-escalation fire silent"
        );

        let rows = recorded_rows(&data_root, &profile_root);
        let terminals: Vec<&str> = events_for(&rows, &escalate_id)
            .iter()
            .map(|row| event_kind(row))
            .filter(|kind| TERMINAL_EVENTS.contains(kind))
            .collect();
        assert_eq!(terminals, vec!["hint_escalated"]);
    }
}

#[cfg(test)]
mod tests {
    use super::hook_route_metadata_from_event;

    #[test]
    fn hook_route_metadata_preserves_camel_case_session_ids() {
        let event = serde_json::json!({
            "sessionId": "session-camel",
            "conversationId": "conversation-camel",
            "cwd": "/tmp/project"
        })
        .to_string();

        let Some(route) =
            hook_route_metadata_from_event(&event, std::path::Path::new("/tmp/project"))
        else {
            panic!("route metadata should parse");
        };

        assert_eq!(route.session_id.as_deref(), Some("session-camel"));

        let event = serde_json::json!({
            "conversationId": "conversation-camel",
            "cwd": "/tmp/project"
        })
        .to_string();

        let Some(route) =
            hook_route_metadata_from_event(&event, std::path::Path::new("/tmp/project"))
        else {
            panic!("route metadata should parse");
        };

        assert_eq!(route.session_id.as_deref(), Some("conversation-camel"));
    }
}

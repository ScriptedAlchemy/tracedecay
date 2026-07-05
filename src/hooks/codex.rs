//! Codex CLI hook handlers.
//!
//! Codex emits its own hook output shape.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::claude::is_code_research_prompt;
use super::memory_inject;
use super::post_tool_use::{notify_post_tool_use, CODEX_POST_TOOL_USE_SPEC};
use super::steering::{
    append_context_block, append_context_recovery_hint, build_codex_session_context_for_workspace,
    cursor_index_signals_for_root, session_start_from_compaction, HookWorkspaceStatus,
};
use super::tool_hints::{decide_hint, HintAgent, HintCategory, ToolHint, ToolHintInput};
use super::{
    append_tool_hint, deduped_project_hint_with_id, event_cwd_from_parsed, event_session_id,
    format_tool_hint, is_project_like_workspace, mint_hint_id, prompt_like_text, read_hook_event,
    record_hint_analytics, record_hook_analytics, record_hook_invoked,
    record_workspace_status_analytics, rel_under_root, text_field,
};

const CODEX_SUBAGENT_START_CONTEXT: &str = "tracedecay MCP tools subagent context: this looks \
like a new/no-history subagent or code-research subagent. Use `tracedecay:using-tracedecay` \
and the matching TraceDecay workflow before broad file reads: `tracedecay:exploring-code` with \
`tracedecay_context` for code exploration, `tracedecay_grep` for literal/regex code search, \
`tracedecay_search` for symbol names, `tracedecay_outline` or `tracedecay_body` before \
whole-file reads, `tracedecay:tracing-functions` with `tracedecay_find_exact_symbol`, \
`tracedecay_callers`, and `tracedecay_callees` when asked to trace functions, find callers, \
or inspect setup/helper/fixture dependencies, `tracedecay:assessing-impact` with \
`tracedecay_affected` and `tracedecay_test_map` before guessing affected tests, \
`tracedecay:fixing-build-and-type-errors` before running `cargo check`/`tsc`/`clippy` \
in the shell or when shell output shows compile errors — paste captured output into \
`tracedecay_diagnose`, or run `tracedecay_diagnostics` for fresh structured errors \
mapped to symbols, `tracedecay:project-memory` when project decisions/preferences matter, and \
`tracedecay:managing-session-context` with `tracedecay_message_search`, \
`tracedecay_lcm_expand_query`, and `tracedecay_lcm_describe` when prior conversation context \
may be missing.";

const CODEX_POST_COMPACT_BUDGET: Duration = Duration::from_secs(115);

/// Codex `SessionStart` hook handler.
pub async fn hook_codex_session_start() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "SessionStart", &event);
    let (mut context, _) = codex_session_context_for_event(&event).await;
    if let Some(root) = root.as_deref() {
        let session_id = serde_json::from_str::<Value>(&event)
            .ok()
            .as_ref()
            .and_then(event_session_id);
        if let Some(digest) =
            memory_inject::session_memory_digest(root, session_id.as_deref()).await
        {
            append_context_block(&mut context, &digest);
        }
    }
    if session_start_from_compaction(&event) {
        append_context_recovery_hint(&mut context);
    }
    println!(
        "{}",
        codex_additional_context_json("SessionStart", &context)
    );
    0
}

/// Codex `UserPromptSubmit` hook handler.
///
/// Resets the local counter and injects steering context for the new turn.
pub async fn hook_codex_user_prompt_submit() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(
        root.as_deref(),
        HintAgent::Codex,
        "UserPromptSubmit",
        &event,
    );
    reset_counter_for_codex_event(&event).await;
    let context = codex_user_prompt_submit_context_for_event(&event).await;
    println!(
        "{}",
        codex_additional_context_json("UserPromptSubmit", &context)
    );
    0
}

pub async fn codex_user_prompt_submit_context_for_event(event: &str) -> String {
    let (mut context, status) = codex_session_context_for_event(event).await;
    if !matches!(status, HookWorkspaceStatus::Generic) {
        if let Some(hint) = codex_prompt_hint(event) {
            append_tool_hint(&mut context, &hint);
        }
        if let Some(recall) = codex_prompt_memory_recall(event).await {
            append_context_block(&mut context, &recall);
        }
    }
    context
}

async fn codex_prompt_memory_recall(event_json: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    let root = codex_project_root_from_parsed_event(&parsed)?;
    let prompt = prompt_like_text(&parsed)?;
    let session_id = event_session_id(&parsed);
    memory_inject::prompt_memory_recall(&root, session_id.as_deref(), &prompt).await
}

/// Builds Codex session/prompt context.
async fn codex_session_context_for_event(event_json: &str) -> (String, HookWorkspaceStatus) {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let root = codex_project_root_from_parsed_event(&parsed);
    let cwd = event_cwd_from_parsed(&parsed);
    let session_id = event_session_id(&parsed);
    let status = codex_workspace_status(root.as_deref(), cwd.as_deref());
    record_workspace_status_analytics(root.as_deref(), status, session_id.as_deref());
    let staleness = match (status, root.as_deref()) {
        (HookWorkspaceStatus::Initialized, Some(r)) => {
            let (staleness, _) = cursor_index_signals_for_root(r).await;
            staleness
        }
        _ => None,
    };
    (
        build_codex_session_context_for_workspace(status, staleness.as_deref()),
        status,
    )
}

/// Codex `SubagentStart` hook handler.
pub async fn hook_codex_subagent_start() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "SubagentStart", &event);
    let count = record_codex_subagent_start(&event);
    let output = evaluate_codex_subagent_start(&event);
    let digest = match root.as_deref() {
        Some(root) => memory_inject::session_memory_digest(root, None).await,
        None => None,
    };
    let output = merge_codex_subagent_output(output, digest);
    eprintln!(
        "{}",
        codex_subagent_start_log_line(&event, count, output.is_some())
    );
    if let Some(output) = output {
        println!("{output}");
    }
    0
}

fn merge_codex_subagent_output(output: Option<String>, digest: Option<String>) -> Option<String> {
    let Some(digest) = digest else {
        return output;
    };
    let Some(output) = output else {
        return Some(codex_additional_context_json("SubagentStart", &digest));
    };
    let Ok(mut parsed) = serde_json::from_str::<Value>(&output) else {
        return Some(output);
    };
    let Some(context) = parsed
        .pointer_mut("/hookSpecificOutput/additionalContext")
        .and_then(|value| value.as_str().map(str::to_string))
    else {
        return Some(output);
    };
    let mut merged = context;
    append_context_block(&mut merged, &digest);
    parsed["hookSpecificOutput"]["additionalContext"] = Value::String(merged);
    Some(parsed.to_string())
}

/// Codex `PostToolUse` hook handler used to keep the graph fresh.
pub async fn hook_codex_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "PostToolUse", &event);
    notify_post_tool_use(&CODEX_POST_TOOL_USE_SPEC, &event).await;
    0
}

/// Codex `PostCompact` hook handler.
///
/// Replaces temporary compaction summaries from visible LCM source messages.
pub async fn hook_codex_post_compact() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "PostCompact", &event);
    if std::env::var_os(crate::sessions::codex_app_server::CODEX_SUMMARY_CHILD_ENV).is_none() {
        codex_post_compact(&event).await;
    }
    println!("{}", serde_json::json!({}));
    0
}

/// Builds a Codex hook stdout payload with `additionalContext`.
pub fn codex_additional_context_json(event_name: &str, additional_context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": additional_context,
        }
    })
    .to_string()
}

/// Pure decision logic for Codex `SubagentStart` events.
pub fn evaluate_codex_subagent_start(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let agent_type = parsed
        .get("agent_type")
        .or_else(|| parsed.get("subagent_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let task = parsed
        .get("prompt")
        .or_else(|| parsed.get("task"))
        .or_else(|| parsed.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Codex,
        session_id: event_session_id(&parsed),
        tool_name: Some("SubagentStart".to_string()),
        command: None,
        prompt: (!task.is_empty()).then(|| task.to_string()),
        subagent_type: (!agent_type.is_empty()).then(|| agent_type.to_string()),
        file_path: None,
        hints_enabled: true,
    });
    let is_explore = agent_type.eq_ignore_ascii_case("explore");
    let is_research = is_explore || is_code_research_prompt(task);
    let needs_context = codex_subagent_needs_context(&parsed);
    if is_research || needs_context {
        let dedupe_hint = ToolHint {
            category: if is_research {
                HintCategory::ExploreSubagent
            } else {
                HintCategory::SubagentStartContext
            },
            message: "For Codex subagents, add compact TraceDecay context before isolated work."
                .to_string(),
            context: CODEX_SUBAGENT_START_CONTEXT.to_string(),
            nonblocking: true,
        };
        let root = codex_project_root_from_event(event_json);
        let hint_id = mint_hint_id();
        record_hint_analytics(
            root.as_deref(),
            "hint_candidate",
            HintAgent::Codex,
            event_session_id(&parsed).as_deref(),
            &hint_id,
            &dedupe_hint,
        );
        let _ = deduped_codex_hint(event_json, &parsed, &hint_id, dedupe_hint)?;
        let context = codex_subagent_start_context(hint, needs_context);
        return Some(codex_additional_context_json("SubagentStart", &context));
    }
    None
}

/// Records a Codex `SubagentStart` and returns the session-local count.
pub fn record_codex_subagent_start(event_json: &str) -> Option<u64> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let root = codex_project_root_from_parsed_event(&parsed)?;
    let layout = crate::storage::resolve_layout_for_current_profile(&root).ok()?;
    let path = layout.data_root.join("codex_subagent_starts.json");
    let analytics_session_id = event_session_id(&parsed);
    let session_id = analytics_session_id
        .clone()
        .unwrap_or_else(|| "unknown-codex-session".to_string());
    let mut counts = read_codex_subagent_start_counts(&path);
    let count = counts.entry(session_id).or_insert(0);
    *count = count.saturating_add(1);
    let next = *count;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&counts) {
        let _ = std::fs::write(path, format!("{json}\n"));
    }
    let agent_type = parsed
        .get("agent_type")
        .or_else(|| parsed.get("subagent_type"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    record_hook_analytics(
        Some(&root),
        "codex_subagent_start",
        serde_json::json!({
            "agent": HintAgent::Codex.as_key(),
            "session_id": analytics_session_id.as_deref(),
            "agent_type": agent_type,
            "count": next,
        }),
    );
    Some(next)
}

pub fn codex_subagent_start_log_line(
    event_json: &str,
    count: Option<u64>,
    emitted_context: bool,
) -> String {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let agent_type = parsed
        .get("agent_type")
        .or_else(|| parsed.get("subagent_type"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let session_id = event_session_id(&parsed).unwrap_or_else(|| "unknown".to_string());
    let count = count.map_or_else(|| "#?".to_string(), |value| format!("#{value}"));
    format!(
        "tracedecay Codex SubagentStart {count}: session_id={session_id} agent_type={agent_type} additional_context={emitted_context}"
    )
}

fn read_codex_subagent_start_counts(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn codex_subagent_start_context(hint: Option<ToolHint>, no_history: bool) -> String {
    let mut context = String::new();
    if no_history {
        context.push_str("new/no-history subagent: recover only relevant project memory or prior-session context before assuming missing decisions.\n");
    }
    context.push_str(CODEX_SUBAGENT_START_CONTEXT);
    context.push('\n');
    if let Some(hint) = hint {
        context.push('\n');
        context.push_str(&format_tool_hint(&hint));
        context.push('\n');
    }
    context
}

fn codex_subagent_needs_context(parsed: &Value) -> bool {
    bool_field(
        parsed,
        &["is_new", "new_subagent", "fresh_subagent", "no_history"],
    ) == Some(true)
        || bool_field(
            parsed,
            &[
                "has_history",
                "history_included",
                "receives_history",
                "conversation_history_included",
            ],
        ) == Some(false)
        || text_field(
            parsed,
            &[
                "history_mode",
                "context_mode",
                "conversation_history",
                "source",
                "reason",
            ],
        )
        .is_some_and(|value| matches_no_history_marker(&value))
        || empty_array_field(parsed, &["history", "messages", "conversation"])
}

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

fn empty_array_field(value: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        value
            .get(*key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    })
}

fn matches_no_history_marker(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "new" | "fresh" | "none" | "empty" | "nohistory" | "withoutconversationhistory"
    )
}

/// Resolves the tracedecay project root for a Codex event from its `cwd`.
pub fn codex_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    codex_project_root_from_parsed_event(&parsed)
}

pub(super) fn codex_project_root_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    let cwd = event_cwd_from_parsed(parsed)?;
    crate::config::discover_project_root(&cwd)
}

fn codex_workspace_status(root: Option<&Path>, cwd: Option<&Path>) -> HookWorkspaceStatus {
    if root.is_some() {
        return HookWorkspaceStatus::Initialized;
    }
    if cwd.is_some_and(is_project_like_workspace) {
        HookWorkspaceStatus::UnindexedProject
    } else {
        HookWorkspaceStatus::Generic
    }
}

pub fn codex_workspace_status_from_event(event_json: &str) -> HookWorkspaceStatus {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let root = codex_project_root_from_parsed_event(&parsed);
    let cwd = event_cwd_from_parsed(&parsed);
    codex_workspace_status(root.as_deref(), cwd.as_deref())
}

/// Extracts the project-relative paths touched by a Codex `apply_patch` command.
pub fn codex_apply_patch_rel_paths(command: &str, cwd: &Path, project_root: &Path) -> Vec<String> {
    const PREFIXES: [&str; 4] = [
        "*** Add File:",
        "*** Update File:",
        "*** Delete File:",
        "*** Move to:",
    ];
    let mut rels: Vec<String> = Vec::new();
    for line in command.lines() {
        let line = line.trim();
        for prefix in PREFIXES {
            if let Some(rest) = line.strip_prefix(prefix) {
                let raw = rest.trim();
                if raw.is_empty() {
                    continue;
                }
                let candidate = Path::new(raw);
                let abs = if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    cwd.join(candidate)
                };
                if let Some(rel) = rel_under_root(project_root, &abs) {
                    if !rels.contains(&rel) {
                        rels.push(rel);
                    }
                }
            }
        }
    }
    rels
}

async fn codex_post_compact(event_json: &str) {
    let work = async {
        let Some(project_root) = codex_project_root_from_event(event_json) else {
            return;
        };
        if !crate::tracedecay::TraceDecay::has_initialized_store(&project_root).await {
            return;
        }
        let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
            return;
        };
        if let Some(source) = crate::sessions::codex::CodexSource::new() {
            let _ = crate::sessions::source::ingest_source(&db, &source, &project_root, None).await;
        }
        let session_id = serde_json::from_str::<Value>(event_json)
            .ok()
            .and_then(|parsed| event_session_id(&parsed));
        let Ok(mut pending) = db
            .pending_codex_compaction_summary_requests(session_id.as_deref(), 1)
            .await
        else {
            return;
        };
        let Some(pending) = pending.pop() else {
            return;
        };
        let config = crate::sessions::codex_app_server::CodexAppServerSummaryConfig::from_env();
        let summary = match crate::sessions::codex_app_server::summarize_with_codex_app_server(
            &pending.request,
            &config,
        ) {
            Ok(summary) => summary,
            Err(err) => {
                eprintln!("tracedecay Codex PostCompact summary failed: {err}");
                return;
            }
        };
        if let Err(err) = db
            .replace_codex_compaction_summary(
                &pending.node_id,
                &summary.text,
                "codex_app_server",
                summary.model.as_deref().or(config.model.as_deref()),
            )
            .await
        {
            eprintln!("tracedecay Codex PostCompact summary replacement failed: {err}");
        }
    };
    let _ = tokio::time::timeout(CODEX_POST_COMPACT_BUDGET, work).await;
}

async fn reset_counter_for_codex_event(event_json: &str) {
    let Some(project_root) = codex_project_root_from_event(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

fn deduped_codex_hint(
    event_json: &str,
    parsed: &Value,
    hint_id: &str,
    hint: ToolHint,
) -> Option<ToolHint> {
    deduped_project_hint_with_id(
        codex_project_root_from_event(event_json),
        HintAgent::Codex,
        event_session_id(parsed),
        hint_id,
        hint,
    )
}

fn codex_prompt_hint(event_json: &str) -> Option<ToolHint> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Codex,
        session_id: event_session_id(&parsed),
        tool_name: None,
        command: None,
        prompt: prompt_like_text(&parsed),
        subagent_type: None,
        file_path: None,
        hints_enabled: true,
    })?;
    let root = codex_project_root_from_event(event_json);
    let hint_id = mint_hint_id();
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Codex,
        event_session_id(&parsed).as_deref(),
        &hint_id,
        &hint,
    );
    deduped_codex_hint(event_json, &parsed, &hint_id, hint)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::USER_DATA_DIR_ENV;

    #[test]
    fn codex_subagent_start_context_carries_diagnostics_moment() {
        // Subagents must route the shell compile/type-check moment to tracedecay
        // diagnostics and name the fixing-build skill, matching session steering.
        assert!(CODEX_SUBAGENT_START_CONTEXT.contains("fixing-build-and-type-errors"));
        assert!(CODEX_SUBAGENT_START_CONTEXT.contains("tracedecay_diagnose"));
        assert!(CODEX_SUBAGENT_START_CONTEXT.contains("tracedecay_diagnostics"));
        assert!(CODEX_SUBAGENT_START_CONTEXT.contains("cargo check"));
        // The consolidated skill ladder and grep routing stay intact.
        assert!(CODEX_SUBAGENT_START_CONTEXT.contains("tracedecay_grep"));
        assert!(CODEX_SUBAGENT_START_CONTEXT.contains("exploring-code"));
    }

    #[test]
    fn codex_subagent_output_merges_memory_digest_into_additional_context() {
        let steering = codex_additional_context_json("SubagentStart", "steering text");
        let digest = "Durable project memory:\n- [decision #1 trust 0.90] fact".to_string();

        let merged =
            merge_codex_subagent_output(Some(steering.clone()), Some(digest.clone())).unwrap();
        let parsed: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "SubagentStart"
        );
        let context = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.starts_with("steering text"));
        assert!(context.contains("Durable project memory"));

        let digest_only = merge_codex_subagent_output(None, Some(digest)).unwrap();
        let parsed: Value = serde_json::from_str(&digest_only).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "SubagentStart"
        );

        assert_eq!(
            merge_codex_subagent_output(Some(steering.clone()), None),
            Some(steering)
        );
        assert_eq!(merge_codex_subagent_output(None, None), None);
    }

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

    #[test]
    fn codex_prompt_hints_dedupe_by_session_and_category() {
        let _lock = crate::hooks::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_hook_codex_prompt".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let layout = crate::storage::resolve_layout_for_current_profile(&project_root).unwrap();
        std::fs::create_dir_all(&layout.data_root).unwrap();
        let event = serde_json::json!({
            "session_id": "codex-session-1",
            "cwd": project_root,
            "prompt": "Please explain the impact of changing parse_user"
        })
        .to_string();

        let first = codex_prompt_hint(&event).unwrap();
        assert_eq!(first.category, HintCategory::Impact);

        assert!(
            codex_prompt_hint(&event).is_none(),
            "Codex should use shared per-session hint dedupe for prompt hints"
        );
    }
}

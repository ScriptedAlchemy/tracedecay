//! Codex CLI hook handlers.
//!
//! Codex emits its own hook output shape.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
#[cfg(test)]
use tracedecay_hooks::{DaemonHookEvent, HookAgent};

use super::claude::is_code_research_prompt;
use super::steering::{
    HookWorkspaceStatus, build_codex_session_context_for_workspace, cursor_index_signals_for_root,
};
use super::tool_hints::{HintAgent, HintCategory, ToolHint, ToolHintInput, decide_hint};
use super::{
    append_tool_hint, deduped_project_hint_with_id, event_cwd_from_parsed, event_project_root,
    event_project_root_from_json, event_project_root_with_identity,
    event_project_root_with_identity_from_json, event_session_id, format_tool_hint,
    is_project_like_workspace, mint_hint_id, prompt_like_text, read_hook_event,
    record_hint_analytics, record_hook_analytics, record_hook_invoked, record_hook_invoked_parsed,
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

/// Codex `SessionStart` hook handler.
pub async fn hook_codex_session_start() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Codex,
        "SessionStart",
        &event,
        &parsed,
    );
    let guidance = super::dispatch::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::Codex,
        &event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    let output = guidance.map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| codex_additional_context_json("SessionStart", &guidance),
    );
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::Codex,
        &event,
        &output,
        Some(&hook_telemetry),
    )
    .await
    {
        return 1;
    }
    0
}

#[cfg(test)]
fn codex_session_start_hook_event(parsed: &Value) -> Option<DaemonHookEvent> {
    event_cwd_from_parsed(parsed).map(|cwd| DaemonHookEvent::session_start(HookAgent::Codex, cwd))
}

/// Codex `UserPromptSubmit` hook handler.
///
/// Resets the local counter and injects steering context for the new turn.
pub async fn hook_codex_user_prompt_submit() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_with_identity_from_json(&event).await;
    let hook_telemetry = record_hook_invoked(
        root.as_deref(),
        HintAgent::Codex,
        "UserPromptSubmit",
        &event,
    );
    reset_counter_for_codex_event(&event, Some(&hook_telemetry)).await;
    let session_id = serde_json::from_str::<Value>(&event)
        .ok()
        .as_ref()
        .and_then(event_session_id);
    if root.is_none() {
        // Keep recall current, but wait for the native Stop receipt before
        // reflection so one completed turn schedules one review rather than a
        // prompt-only review followed immediately by a final-turn review.
        let _ = ingest_user_codex_session(session_id, Some(&hook_telemetry)).await;
    }
    let context = Box::pin(codex_user_prompt_submit_context_for_event(&event)).await;
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::Codex,
        &event,
        &codex_additional_context_json("UserPromptSubmit", &context),
        Some(&hook_telemetry),
    )
    .await
    {
        return 1;
    }
    0
}

pub async fn codex_user_prompt_submit_context_for_event(event: &str) -> String {
    let (mut context, status) = codex_session_context_for_event(event).await;
    if !matches!(status, HookWorkspaceStatus::Generic)
        && let Some(hint) = codex_prompt_hint(event)
    {
        append_tool_hint(&mut context, &hint);
    }
    context
}

/// Builds Codex session/prompt context.
async fn codex_session_context_for_event(event_json: &str) -> (String, HookWorkspaceStatus) {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let cwd = event_cwd_from_parsed(&parsed);
    let root = event_project_root_with_identity(&parsed).await;
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
    let root = event_project_root_with_identity_from_json(&event).await;
    let _hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Codex, "SubagentStart", &event);
    let count = record_codex_subagent_start(&event).await;
    let output = evaluate_codex_subagent_start(&event);
    eprintln!(
        "{}",
        codex_subagent_start_log_line(&event, count, output.is_some())
    );
    if let Some(output) = output
        && !super::write_hook_output(
            root.as_deref(),
            tracedecay_hooks::HookHostV1::Codex,
            &event,
            &output,
            Some(&_hook_telemetry),
        )
        .await
    {
        return 1;
    }
    0
}

/// Codex `PostToolUse` hook handler.
///
/// The native event enters the canonical V2 admission/replay journey. Only
/// daemon-approved ready guidance is rendered in Codex's documented
/// `additionalContext` shape; unavailable or guidance-free admission is silent.
pub async fn hook_codex_post_tool_use() -> i32 {
    let event = read_hook_event!();
    // One parse supplies exact scope and analytics attribution.
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Codex,
        "PostToolUse",
        &event,
        &parsed,
    );
    let guidance = super::dispatch::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::Codex,
        &event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    if let Some(guidance) = guidance
        && !super::write_hook_output(
            root.as_deref(),
            tracedecay_hooks::HookHostV1::Codex,
            &event,
            &codex_additional_context_json("PostToolUse", &guidance),
            Some(&hook_telemetry),
        )
        .await
    {
        return 1;
    }
    0
}

/// Codex `PostCompact` hook handler.
///
/// Codex exposes a pressure boundary but no authenticated compacted payload.
/// The daemon lands the session's rollout through the canonical transcript
/// ingest route and then runs the daemon-owned compression journey; the hook
/// itself only forwards the boundary and fails open.
pub async fn hook_codex_post_compact() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_with_identity_from_json(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Codex, "PostCompact", &event);
    if std::env::var_os(tracedecay_sessions::runtime::codex_app_server::CODEX_SUMMARY_CHILD_ENV)
        .is_none()
    {
        codex_post_compact(&event, Some(&hook_telemetry)).await;
    }
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::Codex,
        &event,
        &serde_json::json!({}).to_string(),
        Some(&hook_telemetry),
    )
    .await
    {
        return 1;
    }
    0
}

/// Bounds the wait for the daemon's terminal-receipt acknowledgement. The
/// follow-up work (transcript ingest, user review) is daemon-owned and is not
/// covered by this budget.
const CODEX_STOP_RETENTION_BUDGET: Duration = Duration::from_secs(3);

/// Codex `Stop` hook handler.
///
/// Codex emits this after the assistant finishes a turn. Projectless sessions
/// need this terminal receipt because the prompt hook runs before the final
/// assistant message has been appended to the rollout.
pub async fn hook_codex_stop() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry =
        record_hook_invoked_parsed(root.as_deref(), HintAgent::Codex, "Stop", &event, &parsed);
    let session_id = event_session_id(&parsed);
    if let Some(root) = root.as_deref()
        && let Some(guidance) = super::dispatch::dispatch(
            tracedecay_hooks::HookHostV1::Codex,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    {
        // A daemon-admitted project Stop still hands the provider's historical
        // session to the daemon; the capture kernel correlates it back to
        // registered projects.
        retain_codex_stop_in_daemon(session_id.as_deref(), Some(&hook_telemetry)).await;
        let output = if let Some(guidance) = guidance {
            codex_additional_context_json("Stop", &guidance)
        } else {
            serde_json::json!({}).to_string()
        };
        if !super::write_hook_output(
            Some(root),
            tracedecay_hooks::HookHostV1::Codex,
            &event,
            &output,
            Some(&hook_telemetry),
        )
        .await
        {
            return 1;
        }
        return 0;
    }
    if root.is_none() {
        retain_codex_stop_in_daemon(session_id.as_deref(), Some(&hook_telemetry)).await;
    }
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::Codex,
        &event,
        &serde_json::json!({}).to_string(),
        Some(&hook_telemetry),
    )
    .await
    {
        return 1;
    }
    0
}

/// Hands the terminal receipt to the daemon, which retains transcript ingest
/// and user review as cancellable daemon-owned work keyed to this exact
/// session. The hook only waits (bounded) for the acknowledgement; an
/// unavailable daemon fails open.
async fn retain_codex_stop_in_daemon(
    session_id: Option<&str>,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> bool {
    let Some(session_id) = session_id else {
        return false;
    };
    let retain = async {
        match super::daemon_hook_action(
            None,
            serde_json::json!({
                "action": "codex_stop",
                "session_id": session_id,
            }),
            telemetry,
        )
        .await
        {
            Ok(result) => result.get("status").and_then(Value::as_str) == Some("accepted"),
            Err(error) => {
                eprintln!("[tracedecay] codex stop daemon retention failed: {error}");
                false
            }
        }
    };
    await_within_stop_budget(retain, CODEX_STOP_RETENTION_BUDGET, telemetry).await
}

async fn await_within_stop_budget(
    capture: impl std::future::Future<Output = bool>,
    budget: Duration,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> bool {
    if let Some(telemetry) = telemetry {
        telemetry.note_timeout_budget(budget);
    }
    match tokio::time::timeout(budget, capture).await {
        Ok(captured) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(false);
            }
            captured
        }
        Err(_) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(true);
            }
            false
        }
    }
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

    let is_explore = agent_type.eq_ignore_ascii_case("explore");
    let is_research = is_explore || is_code_research_prompt(task);
    let needs_context = codex_subagent_needs_context(&parsed);
    if is_research || needs_context {
        let hint = ToolHint {
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
            &hint,
        );
        let _ = deduped_codex_hint(event_json, &parsed, &hint_id, hint.clone())?;
        let context = codex_subagent_start_context(Some(hint), needs_context);
        return Some(codex_additional_context_json("SubagentStart", &context));
    }
    None
}

/// Records a Codex `SubagentStart` and returns the session-local count.
pub async fn record_codex_subagent_start(event_json: &str) -> Option<u64> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let root = event_project_root_with_identity(&parsed).await?;
    let layout = crate::tracedecay::TraceDecay::resolve_store_layout_for_identity(&root)
        .await
        .ok()?;
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
///
/// Kept as the published Codex-named entry point; the resolution itself is the
/// host-neutral [`super::event_project_root`] every `cwd`-carrying host shares.
pub fn codex_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    event_project_root_from_json(event_json)
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
    let root = event_project_root(&parsed);
    let cwd = event_cwd_from_parsed(&parsed);
    codex_workspace_status(root.as_deref(), cwd.as_deref())
}

/// Codex `apply_patch` envelope prefixes that name a target file path.
const CODEX_APPLY_PATCH_PATH_PREFIXES: [&str; 4] = [
    "*** Add File:",
    "*** Update File:",
    "*** Delete File:",
    "*** Move to:",
];

/// Extracts the project-relative paths touched by a Codex `apply_patch` command.
pub fn codex_apply_patch_rel_paths(command: &str, cwd: &Path, project_root: &Path) -> Vec<String> {
    let mut rels: Vec<String> = Vec::new();
    for line in command.lines() {
        let line = line.trim();
        for prefix in CODEX_APPLY_PATCH_PATH_PREFIXES {
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
                if let Some(rel) = rel_under_root(project_root, &abs)
                    && !rels.contains(&rel)
                {
                    rels.push(rel);
                }
            }
        }
    }
    rels
}

async fn codex_post_compact(
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    let Some(root) = event_project_root_with_identity_from_json(event_json).await else {
        return;
    };
    let session_id = serde_json::from_str::<Value>(event_json)
        .ok()
        .as_ref()
        .and_then(event_session_id);
    let mut args = serde_json::json!({
        "action": "codex_compact",
        "provider": "codex",
        "user_scope": false,
        "event_json": event_json,
    });
    if let Some(session_id) = session_id {
        args["session_id"] = serde_json::json!(session_id);
    }
    if let Err(error) = super::daemon_hook_action(Some(&root), args, telemetry).await {
        eprintln!("[tracedecay] Codex PostCompact daemon call failed: {error}");
    }
}

async fn ingest_user_codex_session(
    session_id: Option<String>,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> bool {
    super::ingest_user_session("Codex", session_id, telemetry).await
}

async fn reset_counter_for_codex_event(
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    let Some(project_root) = event_project_root_with_identity_from_json(event_json).await else {
        return;
    };
    super::reset_counter_for_project(&project_root, telemetry).await;
}

fn deduped_codex_hint(
    event_json: &str,
    parsed: &Value,
    hint_id: &str,
    hint: ToolHint,
) -> Option<ToolHint> {
    let root = codex_project_root_from_event(event_json);
    deduped_project_hint_with_id(
        root.as_deref(),
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
        captured_output: None,
        trusted_failure: false,
        edit_text: None,
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
    fn codex_session_start_event_signals_daemon_with_real_cwd() {
        let event = codex_session_start_hook_event(&serde_json::json!({
            "cwd": "/workspace/codex-session"
        }))
        .unwrap();

        assert_eq!(event.agent, HookAgent::Codex.as_wire());
        assert_eq!(event.event, "sessionStart");
        assert_eq!(
            event.cwd.as_deref(),
            Some(Path::new("/workspace/codex-session"))
        );
    }

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

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn codex_stop_hands_terminal_receipt_to_daemon() {
        let _lock = crate::hooks::lock_test_env();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "codex_stop",
            "status": "accepted",
            "session_id": "final-turn",
        })]);

        assert!(
            !retain_codex_stop_in_daemon(None, None).await,
            "a receipt without a session id has nothing to retain"
        );
        assert!(retain_codex_stop_in_daemon(Some("final-turn"), None).await);

        let calls = daemon.calls();
        assert_eq!(
            calls.len(),
            1,
            "a session-less receipt must not reach the daemon"
        );
        let (project_root, arguments) = &calls[0];
        assert_eq!(*project_root, None, "terminal retention is projectless");
        assert_eq!(arguments["action"], "codex_stop");
        assert_eq!(arguments["session_id"], "final-turn");
        assert_eq!(arguments["format"], "json");
    }

    #[tokio::test]
    async fn codex_stop_retention_timeout_is_fail_open() {
        assert_eq!(CODEX_STOP_RETENTION_BUDGET, Duration::from_secs(3));
        assert!(
            !await_within_stop_budget(std::future::pending(), Duration::ZERO, None).await,
            "bounded daemon retention must not prevent Stop guidance from returning"
        );
    }

    #[test]
    fn codex_prompt_hints_dedupe_by_session_and_category() {
        let _lock = crate::hooks::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        crate::storage::pin_fixture_repository_identity(&project_root, "proj_hook_codex_prompt")
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

    #[tokio::test]
    async fn codex_session_context_resolves_global_only_and_preserves_nudge() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = crate::storage::default_profile_root().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let project_root = project_dir.path().canonicalize().unwrap();
        let project_id = "proj_codex_identity";
        let gdb = crate::host_admission::HostAdmissionTestRuntimeV1::project(
            &profile_root,
            &project_root,
            tracedecay_domain::ProjectId::new(project_id).unwrap(),
        )
        .await
        .unwrap();
        let graph = gdb
            .initialize_project_graph_for_test(
                &project_root,
                crate::tracedecay::TraceDecayOpenOptions::default(),
            )
            .await
            .unwrap();
        drop(graph);
        // Nothing lives in the working tree: the project's durable anchors
        // are the `.git/` repository identity marker and the registry row
        // that initialization published.
        assert!(
            !project_root.join(".tracedecay").exists(),
            "initialization must not create working-tree project state"
        );

        let event = serde_json::json!({ "cwd": project_root.to_string_lossy() }).to_string();
        let (_context, status) = codex_session_context_for_event(&event).await;
        assert_eq!(
            status,
            HookWorkspaceStatus::Initialized,
            "a registered, graph-db-backed global-only repo must report Initialized"
        );

        let unindexed = tempfile::tempdir().unwrap();
        let unindexed_root = unindexed.path().canonicalize().unwrap();
        std::fs::write(unindexed_root.join("Cargo.toml"), b"[package]\n").unwrap();
        let bogus = serde_json::json!({ "cwd": unindexed_root.to_string_lossy() }).to_string();
        let (_bogus_context, bogus_status) = codex_session_context_for_event(&bogus).await;
        assert_eq!(
            bogus_status,
            HookWorkspaceStatus::UnindexedProject,
            "an unindexed project-like cwd must still report UnindexedProject"
        );
    }
}

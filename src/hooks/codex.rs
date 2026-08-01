//! Codex CLI hook handlers.
//!
//! Codex emits its own hook output shape.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tracedecay_hooks::{DaemonHookEvent, HookAgent};

use super::claude::is_code_research_prompt;
use super::memory_inject;
use super::post_tool_use::{
    CODEX_POST_TOOL_USE_SPEC, captured_tool_output, notify_post_tool_use, tool_input_command_str,
    trusted_tool_failure,
};
use super::steering::{
    HookWorkspaceStatus, append_context_block, append_context_recovery_hint,
    build_codex_session_context_for_workspace, cursor_index_signals_for_root,
    session_start_from_compaction,
};
use super::tool_hints::{HintAgent, HintCategory, ToolHint, ToolHintInput, decide_hint};
use super::{
    append_tool_hint, deduped_project_hint, deduped_project_hint_with_id, event_cwd_from_parsed,
    event_project_root, event_project_root_from_json, event_project_root_with_identity,
    event_project_root_with_identity_from_json, event_session_id, format_tool_hint,
    is_project_like_workspace, mint_hint_id, prompt_like_text, read_hook_event,
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

/// Codex `SessionStart` hook handler.
pub async fn hook_codex_session_start() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity_from_json(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Codex, "SessionStart", &event);
    if let (Some(root), Some(event)) = (root.as_ref(), codex_session_start_hook_event(&parsed)) {
        super::notify_hook_event_with_telemetry(root, event, &hook_telemetry).await;
    }
    let (mut context, _) = codex_session_context_for_event(&event).await;
    let session_id = event_session_id(&parsed);
    if root.is_none() && ingest_user_codex_session(session_id.clone(), Some(&hook_telemetry)).await
    {
        super::schedule_user_session_review("codex", session_id.as_deref());
    }
    let digest = match root.as_deref() {
        Some(root) => {
            memory_inject::combined_session_memory_digest(root, session_id.as_deref()).await
        }
        None => memory_inject::user_session_memory_digest(session_id.as_deref()).await,
    };
    if let Some(digest) = digest {
        append_context_block(&mut context, &digest);
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
    println!(
        "{}",
        codex_additional_context_json("UserPromptSubmit", &context)
    );
    0
}

pub async fn codex_user_prompt_submit_context_for_event(event: &str) -> String {
    let (mut context, status) = codex_session_context_for_event(event).await;
    if !matches!(status, HookWorkspaceStatus::Generic)
        && let Some(hint) = codex_prompt_hint(event)
    {
        append_tool_hint(&mut context, &hint);
    }
    if let Some(recall) = Box::pin(codex_prompt_memory_recall(event)).await {
        append_context_block(&mut context, &recall);
    }
    context
}

async fn codex_prompt_memory_recall(event_json: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    memory_inject::prompt_memory_recall(&parsed, || event_project_root_with_identity(&parsed)).await
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
    let digest = match root.as_deref() {
        Some(root) => memory_inject::combined_session_memory_digest(root, None).await,
        None => memory_inject::user_session_memory_digest(None).await,
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

/// Codex `PostToolUse` hook handler used to keep the graph fresh and to surface
/// a soft tracedecay hint for edits and shell commands.
///
/// Two independent outputs, mirroring [`super::claude::hook_claude_post_tool_use`]:
/// the daemon notification (targeted sync / branch tracking, via IPC only) and,
/// for `apply_patch` edits plus `Bash`/`shell` commands, a `PostToolUse`
/// `additionalContext` hint printed to stdout (Codex injects it as developer
/// context). The daemon path never writes stdout, so the two do not interfere.
/// Fail-open: no surviving hint leaves prior behavior unchanged.
pub async fn hook_codex_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_with_identity_from_json(&event).await;
    let _hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Codex, "PostToolUse", &event);
    if let Some(context) = codex_post_tool_use_hint(&event) {
        println!("{}", codex_additional_context_json("PostToolUse", &context));
    }
    notify_post_tool_use(&CODEX_POST_TOOL_USE_SPEC, &event).await;
    0
}

/// Builds the `PostToolUse` `additionalContext` string for a Codex edit or
/// shell event, or `None` when no hint survives dedupe. Decides the raw hint
/// with [`decide_codex_post_tool_use_hint`], then dedupes per (session,
/// category) via [`deduped_project_hint`] exactly like the Claude post-tool-use
/// surface (which mints its own candidate id and records only the terminal row).
fn codex_post_tool_use_hint(event_json: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    let hint = decide_codex_post_tool_use_hint(&parsed)?;
    let root = event_project_root(&parsed);
    let session_id = event_session_id(&parsed);
    let hint = deduped_project_hint(root.as_deref(), HintAgent::Codex, session_id, hint)?;
    Some(format_tool_hint(&hint))
}

/// Pure hint decision for a Codex `PostToolUse` event: shapes a
/// [`ToolHintInput`] from the event's tool name and `tool_input.command`, then
/// runs [`decide_hint`]. Returns `None` for tools outside the installed
/// `Bash|apply_patch` matcher and when no hint applies. No I/O, so it is
/// unit-testable without a profile store.
///
/// The tool name is mapped onto the hint system's Claude-shaped vocabulary so
/// the shared classifiers apply unchanged: `apply_patch` -> `Edit` (an edit
/// tool, with the added source and target path extracted from the patch
/// envelope so the redundancy heuristic sees the same function-body shape
/// Claude's `new_string` carries), and `shell`/`bash` -> `Bash` (the command
/// drives the search/build classifiers). The patch envelope is never forwarded
/// as a shell `command`, so command classifiers cannot misfire on patch text.
fn decide_codex_post_tool_use_hint(parsed: &Value) -> Option<ToolHint> {
    let tool_name = parsed.get("tool_name").and_then(Value::as_str)?;
    let command = tool_input_command_str(parsed);
    let session_id = event_session_id(parsed);
    let input = match tool_name.to_ascii_lowercase().as_str() {
        "apply_patch" => {
            let command = command?;
            ToolHintInput {
                agent: HintAgent::Codex,
                session_id,
                tool_name: Some("Edit".to_string()),
                command: None,
                prompt: None,
                subagent_type: None,
                file_path: codex_apply_patch_first_target_path(&command),
                captured_output: captured_tool_output(parsed),
                trusted_failure: trusted_tool_failure(parsed),
                edit_text: codex_apply_patch_added_text(&command),
                hints_enabled: true,
            }
        }
        "bash" | "shell" => ToolHintInput {
            agent: HintAgent::Codex,
            session_id,
            tool_name: Some("Bash".to_string()),
            command,
            prompt: None,
            subagent_type: None,
            file_path: None,
            captured_output: captured_tool_output(parsed),
            trusted_failure: trusted_tool_failure(parsed),
            edit_text: None,
            hints_enabled: true,
        },
        _ => return None,
    };
    decide_hint(&input)
}

/// Codex `PostCompact` hook handler.
///
/// Replaces temporary compaction summaries from visible LCM source messages.
pub async fn hook_codex_post_compact() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_with_identity_from_json(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Codex, "PostCompact", &event);
    if std::env::var_os(crate::sessions::codex_app_server::CODEX_SUMMARY_CHILD_ENV).is_none() {
        codex_post_compact(&event, Some(&hook_telemetry)).await;
    }
    println!("{}", serde_json::json!({}));
    0
}

const CODEX_STOP_INGEST_BUDGET: Duration = Duration::from_secs(3);

/// Codex `Stop` hook handler.
///
/// Codex emits this after the assistant finishes a turn. Projectless sessions
/// need this terminal receipt because the prompt hook runs before the final
/// assistant message has been appended to the rollout.
pub async fn hook_codex_stop() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked(root.as_deref(), HintAgent::Codex, "Stop", &event);
    if let Some(root) = root.as_deref()
        && let Some(guidance) = super::v2::dispatch(
            tracedecay_hooks::HookHostV1::Codex,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    {
        if let Some(guidance) = guidance {
            println!("{}", codex_additional_context_json("Stop", &guidance));
        } else {
            println!("{}", serde_json::json!({}));
        }
        return 0;
    }
    let session_id = event_session_id(&parsed);
    hook_telemetry.note_timeout_budget(CODEX_STOP_INGEST_BUDGET);
    let ingested = if let Ok(ingested) = tokio::time::timeout(
        CODEX_STOP_INGEST_BUDGET,
        finalize_codex_user_session(root.as_deref(), session_id.clone(), Some(&hook_telemetry)),
    )
    .await
    {
        hook_telemetry.note_timed_out(false);
        ingested
    } else {
        hook_telemetry.note_timed_out(true);
        false
    };
    if ingested {
        super::schedule_user_session_review("codex", session_id.as_deref());
    }
    println!("{}", serde_json::json!({}));
    0
}

async fn finalize_codex_user_session(
    project_root: Option<&Path>,
    session_id: Option<String>,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> bool {
    if project_root.is_some() {
        return false;
    }
    ingest_user_codex_session(session_id, telemetry).await
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

/// First target file path named by a Codex `apply_patch` envelope, verbatim (not
/// resolved against any root). Enough for the redundancy hint classifier, which
/// keys only off the path's extension to pick the language. `O(len(command))`.
fn codex_apply_patch_first_target_path(command: &str) -> Option<String> {
    command.lines().find_map(|line| {
        let line = line.trim();
        CODEX_APPLY_PATCH_PATH_PREFIXES.iter().find_map(|prefix| {
            line.strip_prefix(prefix)
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
                .map(str::to_string)
        })
    })
}

/// The added-line text of a Codex `apply_patch` envelope: the body of every
/// `+`-prefixed addition with the marker stripped, joined by newlines. This is
/// the Codex analogue of Claude's edit `new_string`/`content` — the source the
/// model just wrote — so it feeds the shared function-body redundancy heuristic
/// without the envelope markers, hunk headers, or unchanged context lines
/// defeating the line-count and keyword checks. Returns `None` when the patch
/// adds nothing. `O(len(command))`: one line scan, no patch application.
fn codex_apply_patch_added_text(command: &str) -> Option<String> {
    let mut added: Vec<&str> = Vec::new();
    for line in command.lines() {
        // Envelope (`*** …`) and hunk (`@@ …`) markers are never added source.
        if line.starts_with("***") || line.starts_with("@@") {
            continue;
        }
        if let Some(body) = line.strip_prefix('+') {
            added.push(body);
        }
    }
    (!added.is_empty()).then(|| added.join("\n"))
}

async fn codex_post_compact(
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    let root = event_project_root_with_identity_from_json(event_json).await;
    let action = if root.is_some() {
        "codex_compact"
    } else {
        "ingest_transcript"
    };
    let session_id = serde_json::from_str::<Value>(event_json)
        .ok()
        .as_ref()
        .and_then(event_session_id);
    let mut args = serde_json::json!({
        "action": action,
        "provider": "codex",
        "user_scope": root.is_none(),
        "event_json": event_json,
    });
    if let Some(session_id) = session_id {
        args["session_id"] = serde_json::json!(session_id);
    }
    if let Err(error) = super::daemon_hook_action(root.as_deref(), args, telemetry).await {
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

    const QUALIFYING_RUST_PATCH: &str = "*** Begin Patch\n\
*** Add File: src/util.rs\n\
+pub fn summarize(hits: &[Hit]) -> u32 {\n\
+    let mut total = 0;\n\
+    for hit in hits {\n\
+        if hit.active {\n\
+            total += hit.count;\n\
+        }\n\
+    }\n\
+    total\n\
+}\n\
*** End Patch\n";

    #[test]
    fn codex_apply_patch_added_text_strips_markers_and_plus() {
        let added = codex_apply_patch_added_text(QUALIFYING_RUST_PATCH).unwrap();
        assert!(added.starts_with("pub fn summarize("));
        assert!(added.contains("fn "));
        // Envelope markers and hunk headers must not leak into the added text.
        assert!(!added.contains("*** "));
        assert!(!added.contains("Begin Patch"));
        // Nine `+` body lines, one per addition, marker stripped.
        assert_eq!(added.lines().count(), 9);
        assert!(!added.lines().any(|line| line.starts_with('+')));

        // A patch that adds nothing yields no text.
        let no_adds = "*** Begin Patch\n*** Delete File: src/gone.rs\n*** End Patch\n";
        assert!(codex_apply_patch_added_text(no_adds).is_none());
    }

    #[test]
    fn codex_apply_patch_first_target_path_reads_first_marker() {
        assert_eq!(
            codex_apply_patch_first_target_path(QUALIFYING_RUST_PATCH).as_deref(),
            Some("src/util.rs")
        );
        assert!(codex_apply_patch_first_target_path("no markers here").is_none());
    }

    fn post_tool_use_event(tool_name: &str, command: &str) -> Value {
        serde_json::json!({
            "session_id": "codex-post-tool",
            "cwd": "/repo",
            "tool_name": tool_name,
            "tool_input": { "command": command },
        })
    }

    #[test]
    fn codex_apply_patch_event_nudges_edit_redundancy() {
        let event = post_tool_use_event("apply_patch", QUALIFYING_RUST_PATCH);
        let hint = decide_codex_post_tool_use_hint(&event)
            .expect("a function-sized apply_patch must nudge edit redundancy");
        assert_eq!(hint.category, HintCategory::EditRedundancy);
    }

    #[test]
    fn codex_small_apply_patch_stays_silent() {
        let small = "*** Begin Patch\n\
*** Update File: src/util.rs\n\
+    let x = compute();\n\
*** End Patch\n";
        let event = post_tool_use_event("apply_patch", small);
        assert!(
            decide_codex_post_tool_use_hint(&event).is_none(),
            "a one-line apply_patch is below the redundancy line threshold"
        );
    }

    #[test]
    fn codex_non_code_apply_patch_stays_silent() {
        let notes = "*** Begin Patch\n\
*** Add File: NOTES.md\n\
+# Heading\n\
+first paragraph line\n\
+second paragraph line\n\
+third paragraph line\n\
+fourth paragraph line\n\
+fifth paragraph line\n\
+sixth paragraph line\n\
+seventh paragraph line\n\
+eighth paragraph line\n\
*** End Patch\n";
        let event = post_tool_use_event("apply_patch", notes);
        assert!(
            decide_codex_post_tool_use_hint(&event).is_none(),
            "a prose markdown patch has no function shape and must stay silent"
        );
    }

    #[test]
    fn codex_shell_event_carries_command_and_no_edit_text() {
        // A recursive shell search routes to the graph-search hint off the
        // command alone, with no edit_text — the Bash-shaped surface.
        let event = post_tool_use_event("shell", "grep -r needle src/");
        let hint = decide_codex_post_tool_use_hint(&event)
            .expect("a recursive shell search must produce a search hint");
        assert_eq!(hint.category, HintCategory::Search);

        // A plain shell command is not a candidate.
        let plain = post_tool_use_event("bash", "ls -la");
        assert!(decide_codex_post_tool_use_hint(&plain).is_none());
    }

    #[test]
    fn codex_post_tool_use_ignores_untracked_tools() {
        // No tool_name and tools outside the Bash|apply_patch matcher are silent.
        assert!(decide_codex_post_tool_use_hint(&serde_json::json!({})).is_none());
        let read = serde_json::json!({
            "tool_name": "read",
            "tool_input": { "command": "" },
        });
        assert!(decide_codex_post_tool_use_hint(&read).is_none());
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

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn codex_stop_ingests_final_user_turn_once() {
        let _lock = crate::hooks::lock_test_env();
        let temp = tempfile::tempdir().unwrap();
        let general = temp.path().join("general-chat");
        std::fs::create_dir_all(&general).unwrap();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([
            serde_json::json!({ "messages_upserted": 1 }),
            serde_json::json!({ "messages_upserted": 0 }),
        ]);

        assert!(finalize_codex_user_session(None, Some("final-turn".to_string()), None).await);
        assert!(
            !finalize_codex_user_session(None, Some("final-turn".to_string()), None).await,
            "a repeated Stop receipt must not schedule another review"
        );
        assert!(
            !finalize_codex_user_session(Some(&general), Some("final-turn".to_string()), None,)
                .await,
            "project-scoped Stop receipts must never write the user session store"
        );

        let calls = daemon.calls();
        assert_eq!(calls.len(), 2);
        for (project_root, arguments) in calls {
            assert_eq!(project_root, None);
            assert_eq!(arguments["action"], "ingest_transcript");
            assert_eq!(arguments["provider"], "codex");
            assert_eq!(arguments["user_scope"], true);
            assert_eq!(arguments["session_id"], "final-turn");
            assert_eq!(arguments["format"], "json");
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

    #[tokio::test]
    async fn codex_session_context_resolves_global_only_and_preserves_nudge() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = crate::storage::default_profile_root().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let project_root = project_dir.path().canonicalize().unwrap();
        let project_id = "proj_codex_identity";
        let gdb = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
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
        crate::storage::remove_enrollment_marker(&project_root, project_id).unwrap();

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

//! Cursor hook handlers: subagent/tool-use steering, transcript ingest,
//! post-edit / post-shell daemon notifications, and session lifecycle
//! context.
//!
//! Cursor expects Cursor-shaped stdout, separate from Claude, Codex, and Kiro.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tracedecay_hooks::{DaemonHookEvent, HookAgent};

use super::memory_inject;
use super::post_tool_use::{captured_tool_output, trusted_tool_failure};
use super::steering::{
    append_context_block, append_context_recovery_hint, build_cursor_session_context,
    cursor_index_signals_for_root, session_start_from_compaction,
};
use super::tool_hints::{HintAgent, ToolHint, ToolHintInput, decide_hint};
use super::{
    append_tool_hint, deduped_project_hint_with_id, event_session_id, format_tool_hint,
    hook_route_metadata_from_event, mint_hint_id, nearest_project_like_root, prompt_like_text,
    read_hook_event, record_hint_analytics, record_hook_invoked, rel_under_root, text_field,
};

/// Largest tail the `beforeSubmitPrompt` hot path will read in one call. Larger
/// backlogs are left for the `sessionStart` / `stop` catch-up ingests.
const CURSOR_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Largest transcript tail a low-priority Cursor catch-up hook will read.
/// Oversized backlogs stay queued instead of blocking hook execution.
pub const CURSOR_CATCH_UP_INGEST_MAX_BYTES: u64 =
    crate::sessions::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;
/// Hard wall-clock budget for the `beforeSubmitPrompt` tail ingest. Well under
/// Cursor's 5s hook timeout; on expiry we fail open and let heavier hooks catch up.
const CURSOR_HOT_INGEST_BUDGET: Duration = Duration::from_millis(1_500);
/// Budget for the `sessionStart` catch-up ingest (registered with a 5s timeout).
const CURSOR_SESSION_INGEST_BUDGET: Duration = Duration::from_secs(4);
/// Budget for the end-of-turn `stop` catch-up ingest (registered with a 30s timeout).
const CURSOR_STOP_INGEST_BUDGET: Duration = Duration::from_secs(25);

fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

const CURSOR_FILE_PATH_FIELDS: &[&str] = &[
    "file_path",
    "filePath",
    "path",
    "target_file",
    "targetFile",
    "relative_workspace_path",
    "relativeWorkspacePath",
];

/// Cursor `subagentStart` hook handler.
///
/// Allows Cursor subagents while preserving legacy hook compatibility.
pub async fn hook_cursor_subagent_start() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let _hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "subagentStart", &event);
    if let Some(decision) = evaluate_cursor_subagent_start(&event) {
        println!("{decision}");
    }
    0
}

/// Cursor `postToolUse` hook handler.
///
/// Emits soft `additional_context` hints steering exploration tools (Grep,
/// Glob, Read, semantic search, shell `rg`) toward tracedecay MCP tools.
/// Registered on `postToolUse` rather than `preToolUse` because Cursor's
/// documented `preToolUse` output schema has no context-injection field —
/// `additional_context` is only honored on `postToolUse`. The hook runs
/// unmatched (the docs enumerate no matcher value for Cursor's semantic
/// search tool) and irrelevant tools fail open with no output. Each hint
/// category is emitted at most once per session via
/// [`super::tool_hints::ToolHintDedupe`] persisted under `.tracedecay/`.
pub async fn hook_cursor_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let _hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "postToolUse", &event);
    if let Some(decision) = cursor_post_tool_use_decision(&event) {
        println!("{decision}");
    }
    0
}

/// Cursor `beforeSubmitPrompt` hook handler.
///
/// Resets the project-local counter for a new prompt turn and does at most a
/// small, time-boxed *tail* ingest of newly-appended transcript lines (the bulk
/// catch-up lives on the lower-frequency `sessionStart` / `stop` hooks). It also
/// injects the same prompt-shaped steering Codex's `UserPromptSubmit` hook does
/// — a deduped `decide_hint` for the prompt plus prompt-relevance-gated memory
/// recall — through Cursor's `additional_context` channel. `postToolUse` hints
/// still ride *after* a tool runs (Cursor's only tool-hint surface); this
/// closes the gap so prompt-shaped triggers steer identically on both agents.
/// The output uses Cursor's documented `beforeSubmitPrompt` shape and never
/// blocks submission, even if the tail ingest or hint injection times out.
pub async fn hook_cursor_before_submit_prompt() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let hook_telemetry = record_hook_invoked(
        root.as_deref(),
        HintAgent::Cursor,
        "beforeSubmitPrompt",
        &event,
    );
    reset_counter_for_cursor_event(&event, Some(&hook_telemetry)).await;
    ingest_cursor_transcript_for_event_inner(
        &event,
        Some(CURSOR_HOT_INGEST_MAX_BYTES),
        CURSOR_HOT_INGEST_BUDGET,
        Some(&hook_telemetry),
    )
    .await;
    let context = Box::pin(cursor_before_submit_prompt_context(&event)).await;
    println!("{}", cursor_before_submit_prompt_json(context.as_deref()));
    0
}

/// Builds the Cursor `beforeSubmitPrompt` output JSON. Always keeps
/// `continue: true` (submission is never blocked) and attaches
/// `additional_context` only when there is steering to inject.
pub fn cursor_before_submit_prompt_json(additional_context: Option<&str>) -> String {
    match additional_context.filter(|text| !text.trim().is_empty()) {
        Some(context) => {
            serde_json::json!({ "continue": true, "additional_context": context }).to_string()
        }
        None => serde_json::json!({ "continue": true }).to_string(),
    }
}

/// Assembles the prompt-shaped steering for a Cursor `beforeSubmitPrompt` event:
/// a deduped prompt hint (`decide_hint`) followed by prompt-relevance-gated
/// memory recall, mirroring [`super::codex::codex_user_prompt_submit_context_for_event`].
/// Returns `None` when neither surfaces anything (fail-open: no context key).
pub(super) async fn cursor_before_submit_prompt_context(event_json: &str) -> Option<String> {
    let mut context = String::new();
    if let Some(hint) = cursor_prompt_hint(event_json) {
        append_tool_hint(&mut context, &hint);
    }
    if let Some(recall) = Box::pin(cursor_prompt_memory_recall(event_json)).await {
        append_context_block(&mut context, &recall);
    }
    (!context.trim().is_empty()).then_some(context)
}

/// Deduped prompt-hint decision for a Cursor `beforeSubmitPrompt` event, mirroring
/// [`super::codex::codex_prompt_hint`]. Runs the same prompt-path `decide_hint`
/// (session recall, project context, call graph, impact categories) and suppresses
/// hints already emitted for the session / in uninitialized workspaces.
fn cursor_prompt_hint(event_json: &str) -> Option<ToolHint> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Cursor,
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
    let root = cursor_project_root_candidate_from_parsed_event(&parsed);
    let hint_id = mint_hint_id();
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Cursor,
        event_session_id(&parsed).as_deref(),
        &hint_id,
        &hint,
    );
    deduped_cursor_hint(event_json, &hint_id, hint)
}

async fn cursor_prompt_memory_recall(event_json: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    memory_inject::prompt_memory_recall(&parsed, || {
        cursor_project_root_from_parsed_event_with_identity(&parsed)
    })
    .await
}

/// Cursor `sessionEnd` hook handler (fire-and-forget).
///
/// Final transcript-ingest flush when a conversation ends (including
/// `window_close` / `user_close`, which the end-of-turn `stop` hook can
/// miss). `sessionEnd` receives the common-schema `transcript_path`, so the
/// regular capped catch-up ingest applies. The response is logged but unused,
/// so an empty object is emitted. Fail-open.
pub async fn hook_cursor_session_end() -> i32 {
    hook_cursor_session_completion("sessionEnd").await
}

async fn hook_cursor_session_completion(hook_name: &str) -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let hook_telemetry = record_hook_invoked(root.as_deref(), HintAgent::Cursor, hook_name, &event);
    if hook_name == "stop"
        && let Some(root) = root.as_deref()
        && let Some(guidance) = super::v2::dispatch(
            tracedecay_hooks::HookHostV1::CursorDesktop,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    {
        if let Some(guidance) = guidance {
            println!("{}", serde_json::json!({ "additional_context": guidance }));
        } else {
            println!("{}", serde_json::json!({}));
        }
        return 0;
    }
    let outcome = ingest_cursor_transcript_for_event_inner(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_STOP_INGEST_BUDGET,
        Some(&hook_telemetry),
    )
    .await;
    if outcome.user_scope && outcome.messages_upserted > 0 {
        let session_id = event_session_id_from_json(&event);
        super::schedule_user_session_review("cursor", session_id.as_deref());
    }
    println!("{}", serde_json::json!({}));
    0
}

/// Cursor `stop` hook handler (fire-and-forget).
///
/// Fires at the end of an agent turn and performs the primary transcript
/// ingest: a time-boxed incremental catch-up that picks up bounded transcript
/// tails appended during the turn. The `stop` output is informational only, so
/// we emit an empty object and never ask the agent to continue. Fail-open.
pub async fn hook_cursor_stop() -> i32 {
    hook_cursor_session_completion("stop").await
}

/// Cursor `preCompact` hook handler.
///
/// Cursor's compaction event exposes pressure metadata but not Cursor's own
/// generated summary text. The hook delegates to the daemon, which ingests the
/// current transcript tail, asks LCM for the compactable raw-message backlog,
/// generates a summary through `cursor-agent -p`, and stores that summary as a
/// normal LCM summary node. The hook is fail-open and emits Cursor's empty
/// object shape.
pub async fn hook_cursor_pre_compact() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "preCompact", &event);
    if std::env::var(crate::sessions::cursor_agent::CURSOR_SUMMARY_CHILD_ENV).is_err() {
        let outcome = super::cursor_compact::cursor_pre_compact_via_daemon_with_telemetry(
            &event,
            Some(&hook_telemetry),
        )
        .await;
        if outcome.status == "error" {
            eprintln!(
                "tracedecay Cursor preCompact summary failed: {}",
                outcome.reason
            );
        }
    }
    println!("{}", serde_json::json!({}));
    0
}

/// Cursor `afterFileEdit` hook handler.
///
/// Two jobs, both fail-open:
/// 1. Keeps the graph fresh after Cursor Agent writes files by notifying the
///    daemon about the edited path(s). The daemon owns targeted sync scheduling
///    and the notification no-ops when no daemon is available.
/// 2. Emits the edit-driven redundancy nudge ([`HintCategory::EditRedundancy`]),
///    matching Claude's `PostToolUse` surface. `afterFileEdit` is the only
///    Cursor hook whose recorded payload carries the *applied* edit body
///    (`edits[].new_string`); Cursor's `postToolUse` edit payload carries only
///    the target `file_path` (see the recorded fixtures in
///    `tests/hooks_lsp_suite/hooks_test.rs`), so the redundancy classifier —
///    which needs the added text — can only run here. The hint rides Cursor's
///    documented `additional_context` output shape with the same per-session
///    dedupe and initialized-store gating as `postToolUse`.
pub async fn hook_cursor_after_file_edit() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "afterFileEdit", &event);
    if let Some(root) = root.as_deref()
        && let Some(guidance) = super::v2::dispatch(
            tracedecay_hooks::HookHostV1::CursorDesktop,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    {
        if let Some(guidance) = guidance {
            println!("{}", serde_json::json!({ "additional_context": guidance }));
        }
        return 0;
    }
    notify_cursor_after_file_edit(&event, &hook_telemetry).await;
    if let Some(decision) = cursor_after_file_edit_decision(&event) {
        println!("{decision}");
    }
    0
}

/// Cursor `sessionStart` hook handler (fire-and-forget).
///
/// Emits Cursor's `sessionStart` output shape (`additional_context` + `env`)
/// steering the agent toward tracedecay MCP tools and reporting index freshness
/// for the resolved workspace. Never blocks session creation.
pub async fn hook_cursor_session_start() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "sessionStart", &event);
    if let (Some(root), Some(event)) = (root.as_ref(), cursor_session_start_hook_event(&parsed)) {
        super::notify_hook_event_with_telemetry(root, event, &hook_telemetry).await;
    }
    ingest_cursor_transcript_for_event_inner(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_SESSION_INGEST_BUDGET,
        Some(&hook_telemetry),
    )
    .await;
    let mut context = cursor_session_context_for_root(root.as_deref()).await;
    let session_id = event_session_id(&parsed);
    let digest = match root.as_deref() {
        Some(root) => {
            memory_inject::combined_session_memory_digest(root, session_id.as_deref()).await
        }
        None => memory_inject::user_session_memory_digest(session_id.as_deref()).await,
    };
    if let Some(digest) = digest {
        append_context_block(&mut context, &digest);
    }
    if let Some(root) = root.as_deref() {
        memory_inject::regenerate_cursor_memory_rule(root).await;
    } else {
        memory_inject::regenerate_cursor_user_memory_rule().await;
    }
    if session_start_from_compaction(&event) {
        append_context_recovery_hint(&mut context);
    }
    println!("{}", cursor_session_start_json(root.as_deref(), &context));
    0
}

fn cursor_session_start_hook_event(parsed: &Value) -> Option<DaemonHookEvent> {
    cursor_event_cwd(parsed).map(|cwd| DaemonHookEvent::session_start(HookAgent::Cursor, cwd))
}

/// Builds the lean Cursor `sessionStart` context for a resolved project root.
///
/// Adds index freshness, the skill index, and tokens-saved counter that the
/// always-on plugin rule cannot know.
async fn cursor_session_context_for_root(root: Option<&Path>) -> String {
    let (initialized, staleness, tokens_saved) = match root {
        Some(r) if crate::tracedecay::TraceDecay::is_initialized(r) => {
            let (staleness, tokens_saved) = cursor_index_signals_for_root(r).await;
            (true, staleness, tokens_saved)
        }
        _ => (false, None, None),
    };
    build_cursor_session_context(initialized, staleness.as_deref(), tokens_saved)
}

/// Cursor `afterShellExecution` hook handler.
///
/// Notifies the daemon that Cursor completed a shell action. Command text is
/// not forwarded and cannot become Git or synchronization authority.
pub async fn hook_cursor_after_shell() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let hook_telemetry = record_hook_invoked(
        root.as_deref(),
        HintAgent::Cursor,
        "afterShellExecution",
        &event,
    );
    notify_cursor_after_shell_event(&event, &hook_telemetry).await;
    0
}

/// Cursor `workspaceOpen` hook handler.
///
/// Notifies the daemon to run one-shot workspace catch-up. Fail-open.
pub async fn hook_cursor_workspace_open() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "workspaceOpen", &event);
    notify_cursor_workspace_open(&event, &hook_telemetry).await;
    if let Some(root) = root.as_deref() {
        memory_inject::regenerate_cursor_memory_rule(root).await;
    }
    println!("{}", serde_json::json!({}));
    0
}

/// Pure decision logic for Cursor `subagentStart` hook events.
///
/// Cursor subagents must be allowed to start.
///
/// Earlier versions denied research/explore subagents in favor of tracedecay MCP
/// tools. In Cursor this can surface as a misleading "bubble creation" timeout,
/// and it prevents explicit user requests to use agents. Keep this handler
/// fail-open so stale installs that still register `subagentStart` do not block
/// subagent creation.
pub fn evaluate_cursor_subagent_start(event_json: &str) -> Option<String> {
    let _ = event_json;
    None
}

/// Pure decision logic for Cursor `postToolUse` hook events.
///
/// Returns a soft `additional_context` payload (Cursor's documented
/// `postToolUse` output shape) for exploration tools tracedecay can replace.
/// Invalid or unrelated tool events fail open with no output. Session-level
/// dedupe lives in [`cursor_post_tool_use_decision`]; this stays pure for
/// tests.
pub fn evaluate_cursor_post_tool_use(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_tool_hint_input(&parsed))?;
    Some(format_cursor_post_tool_use_decision(&hint))
}

fn format_cursor_post_tool_use_decision(hint: &ToolHint) -> String {
    serde_json::json!({
        "additional_context": format_tool_hint(hint),
    })
    .to_string()
}

fn prepare_cursor_post_tool_use_hint(event_json: &str) -> Option<(String, ToolHint)> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_tool_hint_input(&parsed))?;
    let root = cursor_project_root_candidate_from_parsed_event(&parsed);
    let hint_id = mint_hint_id();
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Cursor,
        event_session_id(&parsed).as_deref(),
        &hint_id,
        &hint,
    );
    Some((hint_id, hint))
}

/// Impure `postToolUse` path: [`evaluate_cursor_post_tool_use`] plus
/// per-session hint dedupe persisted under the project's `.tracedecay/` dir.
pub fn cursor_post_tool_use_decision(event_json: &str) -> Option<String> {
    let (hint_id, hint) = prepare_cursor_post_tool_use_hint(event_json)?;
    let hint = deduped_cursor_hint(event_json, &hint_id, hint)?;
    Some(format_cursor_post_tool_use_decision(&hint))
}

/// Suppresses hints that were already emitted for this session.
///
/// The `(session_id, category)` pairs are persisted in
/// `.tracedecay/tool_hints_seen.json` so each hint category surfaces at most
/// once per Cursor session across short-lived hook processes. Hints are also
/// suppressed entirely when the workspace has no tracedecay index (suggesting
/// tracedecay tools there would be misleading). When no session id is present
/// the hint is emitted as-is — dedupe is impossible but the hint is still
/// useful (fail-open).
fn cursor_hint_root(
    event_json: &str,
    hint_id: &str,
    hint: &ToolHint,
) -> Option<(PathBuf, Option<String>)> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        record_hint_analytics(
            None,
            "dropped_no_root",
            HintAgent::Cursor,
            None,
            hint_id,
            hint,
        );
        return None;
    };
    let session_id = event_session_id(&parsed);
    let Some(root) = cursor_project_root_candidate_from_parsed_event(&parsed) else {
        record_hint_analytics(
            None,
            "dropped_no_root",
            HintAgent::Cursor,
            session_id.as_deref(),
            hint_id,
            hint,
        );
        return None;
    };
    Some((root, session_id))
}

fn deduped_cursor_hint(event_json: &str, hint_id: &str, hint: ToolHint) -> Option<ToolHint> {
    let (root, session_id) = cursor_hint_root(event_json, hint_id, &hint)?;
    if !crate::tracedecay::TraceDecay::is_initialized(&root) {
        record_hint_analytics(
            Some(&root),
            "suppressed_uninitialized",
            HintAgent::Cursor,
            session_id.as_deref(),
            hint_id,
            &hint,
        );
        return None;
    }
    deduped_project_hint_with_id(Some(&root), HintAgent::Cursor, session_id, hint_id, hint)
}

pub fn cursor_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    cursor_project_root_from_parsed_event(&parsed)
}

fn cursor_project_root_candidate_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    cursor_project_root_from_parsed_event(parsed).or_else(|| {
        cursor_event_candidates(parsed)
            .into_iter()
            .find_map(|candidate| nearest_project_like_root(&candidate))
    })
}

pub(super) fn cursor_project_root_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    let resolved = cursor_event_candidates(parsed)
        .into_iter()
        .find_map(|candidate| crate::config::discover_project_root(&candidate));
    let cwd_root = cursor_event_cwd(parsed)
        .as_deref()
        .and_then(crate::config::discover_project_root);
    match (cwd_root, resolved) {
        // Prefer the root derived from cwd when available; this avoids routing
        // a root-B event into root A just because workspace_roots listed A first.
        (Some(cwd_root), Some(resolved)) if !paths_same(&cwd_root, &resolved) => Some(cwd_root),
        (Some(cwd_root), None) => Some(cwd_root),
        (_, other) => other,
    }
}

async fn cursor_project_root_from_event_with_identity(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    cursor_project_root_from_parsed_event_with_identity(&parsed).await
}

async fn cursor_project_root_from_parsed_event_with_identity(parsed: &Value) -> Option<PathBuf> {
    let mut resolved = None;
    for candidate in cursor_event_candidates(parsed) {
        if let Some(root) = crate::config::discover_project_root_with_identity(&candidate).await {
            resolved = Some(root);
            break;
        }
    }
    let cwd_root = match cursor_event_cwd(parsed) {
        Some(cwd) => crate::config::discover_project_root_with_identity(&cwd).await,
        None => None,
    };
    match (cwd_root, resolved) {
        (Some(cwd_root), Some(resolved)) if !paths_same(&cwd_root, &resolved) => Some(cwd_root),
        (Some(cwd_root), None) => Some(cwd_root),
        (_, other) => other,
    }
}

fn cursor_event_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|seen| seen == &candidate) {
            candidates.push(candidate);
        }
    };
    if let Some(cwd) = cursor_event_cwd(event) {
        push_unique(cwd);
    }
    if let Some(project_root) = crate::config::brand_env("PROJECT_ROOT") {
        push_unique(PathBuf::from(project_root));
    }
    if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let path = Path::new(file_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let path = Path::new(transcript_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(roots) = event.get("workspace_roots").and_then(Value::as_array) {
        for root in roots {
            if let Some(path) = root.as_str().filter(|s| !s.is_empty()) {
                push_unique(PathBuf::from(path));
            }
        }
    }
    candidates
}

fn cursor_event_cwd(event: &Value) -> Option<PathBuf> {
    event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Extracts the repo-relative paths edited in a Cursor `afterFileEdit` event.
///
/// Cursor sends an absolute `file_path` (plus an `edits` array). We strip the
/// resolved `project_root` prefix and normalize to forward slashes so the hook
/// can notify the daemon about only the changed files. Paths outside the project
/// root are skipped.
pub fn cursor_after_file_edit_rel_paths(event_json: &str, project_root: &Path) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return Vec::new();
    };

    let mut abs_paths: Vec<String> = Vec::new();
    if let Some(p) = parsed
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        abs_paths.push(p.to_string());
    }
    // Defensive: some edit payloads may carry per-edit file paths.
    if let Some(edits) = parsed.get("edits").and_then(Value::as_array) {
        for edit in edits {
            if let Some(p) = edit
                .get("file_path")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                abs_paths.push(p.to_string());
            }
        }
    }

    let mut rels: Vec<String> = Vec::new();
    for abs in abs_paths {
        if let Some(rel) = rel_under_root(project_root, Path::new(&abs))
            && !rels.contains(&rel)
        {
            rels.push(rel);
        }
    }
    rels
}

/// Returns `true` when a sync should run given the last marker time and a
/// debounce window. Used to coalesce back-to-back `afterShellExecution` syncs.
pub fn cursor_should_run_sync(now_secs: i64, last_secs: Option<i64>, debounce_secs: i64) -> bool {
    match last_secs {
        Some(last) => now_secs - last >= debounce_secs,
        None => true,
    }
}

/// Builds the Cursor `sessionStart` output JSON (`additional_context` + `env`).
/// When `project_root` is known, exposes it as `TRACEDECAY_PROJECT_ROOT` so
/// subsequent session hooks can reuse it.
pub fn cursor_session_start_json(project_root: Option<&Path>, additional_context: &str) -> String {
    let mut env = serde_json::Map::new();
    if let Some(root) = project_root {
        env.insert(
            "TRACEDECAY_PROJECT_ROOT".to_string(),
            Value::String(root.to_string_lossy().to_string()),
        );
    }
    serde_json::json!({
        "additional_context": additional_context,
        "env": Value::Object(env),
    })
    .to_string()
}

/// Best-effort daemon notification for Cursor `afterFileEdit`.
///
/// Resolves the edited repo-relative paths locally, then lets the daemon own
/// scheduling and sync execution. No-ops when no in-project paths were edited.
async fn notify_cursor_after_file_edit(
    event_json: &str,
    telemetry: &super::analytics::HookTimingSpan,
) {
    let Some(root) = cursor_project_root_from_event_with_identity(event_json).await else {
        return;
    };
    if !crate::tracedecay::TraceDecay::is_initialized(&root) {
        return;
    }
    let rels = cursor_after_file_edit_rel_paths(event_json, &root);
    if rels.is_empty() {
        return;
    }
    super::notify_hook_event_with_telemetry(
        &root,
        DaemonHookEvent::cursor_after_file_edit(rels)
            .with_route(hook_route_metadata_from_event(event_json, &root)),
        telemetry,
    )
    .await;
}

/// Best-effort daemon notification for Cursor `afterShellExecution`.
async fn notify_cursor_after_shell_event(
    event_json: &str,
    telemetry: &super::analytics::HookTimingSpan,
) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let Some(root) = cursor_project_root_from_event_with_identity(event_json).await else {
        return;
    };
    if !crate::tracedecay::TraceDecay::is_initialized(&root) {
        return;
    }
    let cwd = cursor_event_cwd(&parsed).unwrap_or_else(|| root.clone());
    super::notify_hook_event_with_telemetry(
        &root,
        DaemonHookEvent::cursor_after_shell_execution(cwd)
            .with_route(hook_route_metadata_from_event(event_json, &root)),
        telemetry,
    )
    .await;
}

/// Best-effort daemon notification for Cursor `workspaceOpen`.
async fn notify_cursor_workspace_open(
    event_json: &str,
    telemetry: &super::analytics::HookTimingSpan,
) {
    let Some(root) = cursor_project_root_from_event_with_identity(event_json).await else {
        return;
    };
    if !crate::tracedecay::TraceDecay::is_initialized(&root) {
        return;
    }
    super::notify_hook_event_with_telemetry(
        &root,
        DaemonHookEvent::cursor_workspace_open(root.clone())
            .with_route(hook_route_metadata_from_event(event_json, &root)),
        telemetry,
    )
    .await;
}

async fn reset_counter_for_cursor_event(
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    let Some(project_root) = cursor_project_root_from_event_with_identity(event_json).await else {
        return;
    };
    super::reset_counter_for_project(&project_root, telemetry).await;
}

/// Incrementally ingests the Cursor transcript referenced by `event_json` into
/// the resolved project session DB, bounded by `max_new_bytes` (the hot-path cap)
/// and an overall `budget`. Always fails open: a timeout, missing transcript, or
/// any error is swallowed so the calling hook never blocks the agent.
#[derive(Default)]
struct CursorIngestOutcome {
    user_scope: bool,
    messages_upserted: u64,
}

async fn ingest_cursor_transcript_for_event_inner(
    event_json: &str,
    max_new_bytes: Option<u64>,
    budget: Duration,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> CursorIngestOutcome {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return CursorIngestOutcome::default();
    };
    let project_root = cursor_project_root_from_parsed_event_with_identity(&parsed).await;
    let mut args = serde_json::json!({
        "action": "ingest_transcript",
        "provider": "cursor",
        "user_scope": project_root.is_none(),
        "event_json": event_json,
    });
    if let Some(max_new_bytes) = max_new_bytes {
        args["max_new_bytes"] = serde_json::json!(max_new_bytes);
    }
    args["timeout_budget_ms"] = serde_json::json!(budget.as_millis() as u64);
    if let Some(telemetry) = telemetry {
        telemetry.note_timeout_budget(budget);
    }
    match tokio::time::timeout(
        budget,
        super::daemon_hook_action(project_root.as_deref(), args, telemetry),
    )
    .await
    {
        Ok(Ok(result)) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(false);
            }
            CursorIngestOutcome {
                user_scope: result
                    .get("user_scope")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                messages_upserted: result
                    .get("messages_upserted")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            }
        }
        Ok(Err(error)) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(false);
            }
            eprintln!("[tracedecay] Cursor transcript ingest daemon call failed: {error}");
            CursorIngestOutcome::default()
        }
        Err(_) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(true);
            }
            eprintln!("[tracedecay] Cursor transcript ingest daemon call timed out");
            CursorIngestOutcome::default()
        }
    }
}

fn event_session_id_from_json(event_json: &str) -> Option<String> {
    serde_json::from_str::<Value>(event_json)
        .ok()
        .as_ref()
        .and_then(event_session_id)
}

fn cursor_tool_hint_input(parsed: &Value) -> ToolHintInput {
    let tool_input = parsed
        .get("tool_input")
        .or_else(|| parsed.get("toolInput"))
        .or_else(|| parsed.get("input"))
        .unwrap_or(&Value::Null);
    ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(parsed),
        tool_name: text_field(parsed, &["tool_name", "toolName", "name"]),
        command: text_field(tool_input, &["command", "cmd"])
            .or_else(|| text_field(parsed, &["command", "cmd"])),
        prompt: text_field(
            tool_input,
            &["prompt", "query", "pattern", "task", "description"],
        )
        .or_else(|| {
            text_field(
                parsed,
                &["prompt", "query", "pattern", "task", "description"],
            )
        }),
        subagent_type: text_field(parsed, &["subagent_type", "subagentType", "agent_type"]),
        file_path: text_field(tool_input, CURSOR_FILE_PATH_FIELDS)
            .or_else(|| text_field(parsed, CURSOR_FILE_PATH_FIELDS)),
        captured_output: captured_tool_output(parsed),
        trusted_failure: trusted_tool_failure(parsed),
        // Cursor's `postToolUse` edit payload carries only the target
        // `file_path`, not the applied edit body (confirmed by the recorded
        // fixtures in `tests/hooks_lsp_suite/hooks_test.rs`), so the
        // edit-redundancy nudge cannot run from this surface. The applied edit
        // text only reaches TraceDecay on `afterFileEdit`, where
        // [`cursor_after_file_edit_hint_input`] populates `edit_text` from
        // `edits[].new_string`.
        edit_text: None,
        hints_enabled: true,
    }
}

/// Builds the redundancy-hint input for a Cursor `afterFileEdit` event.
///
/// `afterFileEdit` reports `file_path` at the top level and the applied edit(s)
/// as `edits: [{ old_string, new_string }]`. We join the `new_string`s (mirroring
/// the Claude `MultiEdit` handling in [`super::post_tool_use::tool_input_edit_text`])
/// into `edit_text` and label the synthetic tool `Edit` so the shared
/// [`is_redundancy_candidate_edit`](super::tool_hints) classifier recognizes it.
/// Prompt/command/subagent fields are left empty: this surface only ever drives
/// the edit-shaped categories (redundancy and harness-memory edits), never a
/// prompt- or shell-shaped hint.
fn cursor_after_file_edit_hint_input(parsed: &Value) -> ToolHintInput {
    ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(parsed),
        tool_name: Some("Edit".to_string()),
        command: None,
        prompt: None,
        subagent_type: None,
        file_path: text_field(parsed, CURSOR_FILE_PATH_FIELDS),
        captured_output: None,
        trusted_failure: false,
        edit_text: cursor_after_file_edit_new_text(parsed),
        hints_enabled: true,
    }
}

/// Joins the `new_string`s an `afterFileEdit` event applied, or `None` when the
/// event carries no non-empty added text. `O(len)`: concatenates existing JSON
/// string fields without parsing code.
fn cursor_after_file_edit_new_text(parsed: &Value) -> Option<String> {
    let joined = parsed
        .get("edits")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|edit| edit.get("new_string").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

/// Pure decision logic for Cursor `afterFileEdit` redundancy hints.
///
/// Returns a soft `additional_context` payload (the same shape `postToolUse`
/// uses) when the applied edit adds a new function-sized body. Non-qualifying
/// edits (too small, no function shape, non-source file) and events without an
/// edit body fail open with no output. Session-level dedupe lives in the impure
/// paths; this stays pure for tests.
pub fn evaluate_cursor_after_file_edit(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_after_file_edit_hint_input(&parsed))?;
    Some(format_cursor_post_tool_use_decision(&hint))
}

fn prepare_cursor_after_file_edit_hint(event_json: &str) -> Option<(String, ToolHint)> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_after_file_edit_hint_input(&parsed))?;
    let root = cursor_project_root_candidate_from_parsed_event(&parsed);
    let hint_id = mint_hint_id();
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Cursor,
        event_session_id(&parsed).as_deref(),
        &hint_id,
        &hint,
    );
    Some((hint_id, hint))
}

/// Impure `afterFileEdit` redundancy path: [`evaluate_cursor_after_file_edit`]
/// plus per-session hint dedupe persisted under the project's `.tracedecay/` dir.
/// Shares [`deduped_cursor_hint`] with `postToolUse`, so the redundancy category
/// surfaces at most once per Cursor session regardless of which surface first
/// emits it.
pub fn cursor_after_file_edit_decision(event_json: &str) -> Option<String> {
    let (hint_id, hint) = prepare_cursor_after_file_edit_hint(event_json)?;
    let hint = deduped_cursor_hint(event_json, &hint_id, hint)?;
    Some(format_cursor_post_tool_use_decision(&hint))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::tool_hints::HintCategory;
    use super::*;
    use crate::config::USER_DATA_DIR_ENV;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn transcript_ingest_forwards_its_budget_to_the_daemon() {
        let _lock = crate::hooks::lock_test_env();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([
            serde_json::json!({ "user_scope": true, "messages_upserted": 2 }),
        ]);
        let event = serde_json::json!({ "session_id": "cursor-budget" }).to_string();

        let outcome = ingest_cursor_transcript_for_event_inner(
            &event,
            Some(4_096),
            Duration::from_millis(250),
            None,
        )
        .await;

        assert!(outcome.user_scope);
        assert_eq!(outcome.messages_upserted, 2);
        let calls = daemon.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, None);
        assert_eq!(calls[0].1["action"], "ingest_transcript");
        assert_eq!(calls[0].1["provider"], "cursor");
        assert_eq!(calls[0].1["max_new_bytes"], 4_096);
        assert_eq!(calls[0].1["timeout_budget_ms"], 250);
        assert_eq!(calls[0].1["format"], "json");
    }

    #[test]
    fn cursor_session_start_event_signals_daemon_with_real_cwd() {
        let event = cursor_session_start_hook_event(&serde_json::json!({
            "cwd": "/workspace/cursor-session"
        }))
        .unwrap();

        assert_eq!(event.agent, HookAgent::Cursor.as_wire());
        assert_eq!(event.event, "sessionStart");
        assert_eq!(
            event.cwd.as_deref(),
            Some(Path::new("/workspace/cursor-session"))
        );
    }

    #[test]
    fn cursor_before_submit_prompt_json_attaches_context_only_when_present() {
        // No steering: bare `continue: true`, no `additional_context` key.
        let empty = cursor_before_submit_prompt_json(None);
        let parsed: Value = serde_json::from_str(&empty).unwrap();
        assert_eq!(parsed["continue"], Value::Bool(true));
        assert!(parsed.get("additional_context").is_none());

        // Whitespace-only context is treated as no steering (fail-open).
        let blank = cursor_before_submit_prompt_json(Some("  \n"));
        let parsed: Value = serde_json::from_str(&blank).unwrap();
        assert!(parsed.get("additional_context").is_none());

        // Real steering rides `additional_context` while still allowing submission.
        let filled =
            cursor_before_submit_prompt_json(Some("tracedecay hint: use tracedecay_impact"));
        let parsed: Value = serde_json::from_str(&filled).unwrap();
        assert_eq!(parsed["continue"], Value::Bool(true));
        assert_eq!(
            parsed["additional_context"].as_str().unwrap(),
            "tracedecay hint: use tracedecay_impact"
        );
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

    /// The `beforeSubmitPrompt` surface must run the same prompt-path `decide_hint`
    /// Codex's `UserPromptSubmit` hook does, and share the per-session hint dedupe
    /// so a prompt-shaped trigger steers identically on both agents.
    #[test]
    fn cursor_prompt_hint_runs_decide_hint_and_dedupes_per_session() {
        let _lock = crate::hooks::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_hook_cursor_prompt".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let layout = crate::storage::resolve_layout_for_current_profile(&project_root).unwrap();
        std::fs::create_dir_all(&layout.data_root).unwrap();
        let event = serde_json::json!({
            "hook_event_name": "beforeSubmitPrompt",
            "session_id": "cursor-session-1",
            "cwd": project_root,
            "workspace_roots": [project_root],
            "prompt": "Please explain the impact of changing parse_user"
        })
        .to_string();

        let first = cursor_prompt_hint(&event).unwrap();
        assert_eq!(first.category, HintCategory::Impact);

        assert!(
            cursor_prompt_hint(&event).is_none(),
            "Cursor prompt hints must reuse the shared per-session hint dedupe"
        );
    }

    #[tokio::test]
    async fn cursor_root_uses_identity_resolver_for_global_only_store() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = crate::storage::default_profile_root().unwrap();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let status = std::process::Command::new("git")
            .arg("init")
            .arg(&project_root)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");

        let project_id = "proj_cursor_identity";
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

        let nested = project_root.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let parsed = serde_json::json!({
            "cwd": nested,
            "workspace_roots": [project_root.clone()],
        });

        assert!(cursor_project_root_from_parsed_event(&parsed).is_none());
        assert_eq!(
            cursor_project_root_from_parsed_event_with_identity(&parsed).await,
            Some(project_root)
        );
    }

    /// A qualifying `afterFileEdit` (a new function-sized body written to a
    /// source file) must fire the edit-redundancy nudge on the Cursor surface,
    /// mirroring Claude's `PostToolUse`. The applied text arrives as
    /// `edits[].new_string`, which the handler joins into `edit_text`.
    #[test]
    fn cursor_after_file_edit_nudges_redundancy_for_new_function_body() {
        let body = [
            "fn compute_widget_total(items: &[Item]) -> u64 {",
            "    let mut total = 0;",
            "    for item in items {",
            "        if item.active {",
            "            total += item.count;",
            "        }",
            "    }",
            "    total",
            "}",
        ]
        .join("\n");
        let event = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "file_path": "src/widgets.rs",
            "edits": [{ "old_string": "", "new_string": body }],
            "session_id": "cursor-after-edit"
        })
        .to_string();

        let output = evaluate_cursor_after_file_edit(&event)
            .expect("a new function-sized edit should nudge redundancy");
        let v: Value = serde_json::from_str(&output).unwrap();
        let context = v["additional_context"].as_str().unwrap_or_default();
        assert!(context.contains("tracedecay hint:"), "context: {context}");
        assert!(
            context.contains("tracedecay_redundancy"),
            "context: {context}"
        );
        // The Cursor surface uses the soft `additional_context` shape only — no
        // permission / hookSpecificOutput keys.
        assert!(v.get("permission").is_none());
        assert!(v.get("hookSpecificOutput").is_none());
    }

    /// A qualifying edit split across multiple `edits[]` entries still reaches
    /// the line/keyword heuristic: the handler joins every `new_string`.
    #[test]
    fn cursor_after_file_edit_joins_multiple_edits() {
        let event = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "file_path": "src/widgets.rs",
            "edits": [
                { "old_string": "", "new_string": "fn compute_widget_total(items: &[Item]) -> u64 {\n    let mut total = 0;" },
                { "old_string": "", "new_string": "    for item in items {\n        if item.active {\n            total += item.count;\n        }\n    }\n    total\n}" }
            ],
            "session_id": "cursor-after-edit-multi"
        })
        .to_string();

        let output = evaluate_cursor_after_file_edit(&event)
            .expect("a function body spread across edits should still nudge");
        let v: Value = serde_json::from_str(&output).unwrap();
        assert!(
            v["additional_context"]
                .as_str()
                .unwrap_or_default()
                .contains("tracedecay_redundancy")
        );
    }

    /// Small edits, non-source files, and edit-less events stay silent so the
    /// nudge never spams ordinary Cursor edits.
    #[test]
    fn cursor_after_file_edit_stays_silent_for_non_redundancy_edits() {
        // A one-line edit is below the redundancy line threshold.
        let small = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "file_path": "src/widgets.rs",
            "edits": [{ "old_string": "", "new_string": "fn tiny() -> u8 { 1 }" }],
            "session_id": "s1"
        })
        .to_string();
        assert!(evaluate_cursor_after_file_edit(&small).is_none());

        // A markdown/data file never trips the source-language heuristic even
        // with a long body.
        let long_body = (0..12)
            .map(|i| format!("line number {i} of prose"))
            .collect::<Vec<_>>()
            .join("\n");
        let markdown = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "file_path": "notes.md",
            "edits": [{ "old_string": "", "new_string": long_body }],
            "session_id": "s2"
        })
        .to_string();
        assert!(evaluate_cursor_after_file_edit(&markdown).is_none());

        // An event with no `edits` array carries no added text.
        let no_edits = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "file_path": "src/widgets.rs",
            "session_id": "s3"
        })
        .to_string();
        assert!(evaluate_cursor_after_file_edit(&no_edits).is_none());
    }

    /// End-to-end dedupe: the impure `afterFileEdit` decision emits the nudge
    /// once per session and reuses the shared per-session hint dedupe (so the
    /// same category never double-fires across the `postToolUse` /
    /// `afterFileEdit` surfaces).
    #[test]
    fn cursor_after_file_edit_decision_dedupes_per_session() {
        let _lock = crate::hooks::lock_test_env();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_hook_cursor_after_edit".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let layout = crate::storage::resolve_layout_for_current_profile(&project_root).unwrap();
        std::fs::create_dir_all(&layout.data_root).unwrap();
        std::fs::write(&layout.graph_db_path, "").unwrap();
        let body = [
            "fn compute_widget_total(items: &[Item]) -> u64 {",
            "    let mut total = 0;",
            "    for item in items {",
            "        if item.active {",
            "            total += item.count;",
            "        }",
            "    }",
            "    total",
            "}",
        ]
        .join("\n");
        let event = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "file_path": project_root.join("src/widgets.rs"),
            "edits": [{ "old_string": "", "new_string": body }],
            "session_id": "cursor-after-edit-dedupe",
            "cwd": project_root,
            "workspace_roots": [project_root],
        })
        .to_string();

        assert!(
            cursor_after_file_edit_decision(&event).is_some(),
            "first qualifying edit in a session must emit the nudge"
        );
        assert!(
            cursor_after_file_edit_decision(&event).is_none(),
            "the redundancy nudge must be deduped within the session"
        );
    }
}

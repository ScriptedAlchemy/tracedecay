//! Cursor hook handlers: subagent/tool-use steering, transcript ingest,
//! post-edit / post-shell daemon notifications, and session lifecycle
//! context.
//!
//! Cursor expects Cursor-shaped stdout, separate from Claude, Codex, and Kiro.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::cursor_compact::{
    cursor_pre_compact_for_event_with_config, CURSOR_PRE_COMPACT_SUMMARY_BUDGET,
};
use super::cursor_shell::paths_same;
use super::steering::{
    append_context_recovery_hint, build_cursor_session_context, cursor_index_signals_for_root,
    session_start_from_compaction,
};
use super::tool_hints::{decide_hint, HintAgent, ToolHint, ToolHintInput};
use super::{
    deduped_project_hint, event_session_id, format_tool_hint, nearest_project_like_root,
    read_hook_event, record_hint_analytics, record_hook_invoked, rel_under_root, text_field,
};

/// Largest tail the `beforeSubmitPrompt` hot path will read in one call. Larger
/// backlogs are left for the `sessionStart` / `stop` catch-up ingests.
const CURSOR_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Largest transcript tail a low-priority Cursor catch-up hook will read.
/// Oversized backlogs stay queued instead of blocking hook execution. Public
/// so ingest-health reporting (`tracedecay_status`, doctor) can flag a backlog
/// the hooks will never drain on their own.
pub const CURSOR_CATCH_UP_INGEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Hard wall-clock budget for the `beforeSubmitPrompt` tail ingest. Well under
/// Cursor's 5s hook timeout; on expiry we fail open and let heavier hooks catch up.
const CURSOR_HOT_INGEST_BUDGET: Duration = Duration::from_millis(1_500);
/// Budget for the `sessionStart` catch-up ingest (registered with a 5s timeout).
const CURSOR_SESSION_INGEST_BUDGET: Duration = Duration::from_secs(4);
/// Budget for the end-of-turn `stop` catch-up ingest (registered with a 30s timeout).
const CURSOR_STOP_INGEST_BUDGET: Duration = Duration::from_secs(25);

/// Cursor `subagentStart` hook handler.
///
/// Allows Cursor subagents while preserving legacy hook compatibility.
pub fn hook_cursor_subagent_start() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
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
pub fn hook_cursor_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
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
/// catch-up lives on the lower-frequency `sessionStart` / `stop` hooks). The
/// output uses Cursor's documented `beforeSubmitPrompt` shape and never blocks
/// submission, even if the tail ingest times out.
pub async fn hook_cursor_before_submit_prompt() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(
        root.as_deref(),
        HintAgent::Cursor,
        "beforeSubmitPrompt",
        &event,
    );
    reset_counter_for_cursor_event(&event).await;
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_HOT_INGEST_MAX_BYTES),
        CURSOR_HOT_INGEST_BUDGET,
    )
    .await;
    println!("{}", serde_json::json!({ "continue": true }));
    0
}

/// Cursor `sessionEnd` hook handler (fire-and-forget).
///
/// Final transcript-ingest flush when a conversation ends (including
/// `window_close` / `user_close`, which the end-of-turn `stop` hook can
/// miss). `sessionEnd` receives the common-schema `transcript_path`, so the
/// regular capped catch-up ingest applies. The response is logged but unused,
/// so an empty object is emitted. Fail-open.
pub async fn hook_cursor_session_end() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "sessionEnd", &event);
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_STOP_INGEST_BUDGET,
    )
    .await;
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
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "stop", &event);
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_STOP_INGEST_BUDGET,
    )
    .await;
    println!("{}", serde_json::json!({}));
    0
}

/// Cursor `preCompact` hook handler.
///
/// Cursor's compaction event exposes pressure metadata but not Cursor's own
/// generated summary text. At the boundary, `TraceDecay` ingests the current
/// transcript tail, asks LCM for the compactable raw-message backlog, generates
/// a summary through `cursor-agent -p`, and stores that summary as a normal LCM
/// summary node. The hook is fail-open and emits Cursor's empty object shape.
pub async fn hook_cursor_pre_compact() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "preCompact", &event);
    if std::env::var(crate::sessions::cursor_agent::CURSOR_SUMMARY_CHILD_ENV).is_err() {
        let mut config = crate::sessions::cursor_agent::CursorAgentSummaryConfig::from_env();
        config.timeout = config.timeout.min(CURSOR_PRE_COMPACT_SUMMARY_BUDGET);
        let outcome = cursor_pre_compact_for_event_with_config(&event, &config).await;
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
/// Keeps the graph fresh after Cursor Agent writes files by notifying the
/// daemon about the edited path(s). The daemon owns targeted sync scheduling
/// and the hook fails open when no daemon is available.
pub async fn hook_cursor_after_file_edit() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "afterFileEdit", &event);
    notify_cursor_after_file_edit(&event).await;
    0
}

/// Cursor `sessionStart` hook handler (fire-and-forget).
///
/// Emits Cursor's `sessionStart` output shape (`additional_context` + `env`)
/// steering the agent toward tracedecay MCP tools and reporting index freshness
/// for the resolved workspace. Never blocks session creation.
pub async fn hook_cursor_session_start() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "sessionStart", &event);
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_SESSION_INGEST_BUDGET,
    )
    .await;
    let mut context = cursor_session_context_for_root(root.as_deref()).await;
    if session_start_from_compaction(&event) {
        append_context_recovery_hint(&mut context);
    }
    println!("{}", cursor_session_start_json(root.as_deref(), &context));
    0
}

/// Builds the lean Cursor `sessionStart` context for a resolved project root.
///
/// Adds index freshness, the skill index, and tokens-saved counter that the
/// always-on plugin rule cannot know.
async fn cursor_session_context_for_root(root: Option<&Path>) -> String {
    let (initialized, staleness, tokens_saved) = match root {
        Some(r) if crate::tracedecay::TraceDecay::has_initialized_store(r).await => {
            let (staleness, tokens_saved) = cursor_index_signals_for_root(r).await;
            (true, staleness, tokens_saved)
        }
        _ => (false, None, None),
    };
    build_cursor_session_context(initialized, staleness.as_deref(), tokens_saved)
}

/// Cursor `afterShellExecution` hook handler.
///
/// Notifies the daemon after Cursor shell execution. The daemon decides whether
/// the command requires branch tracking or coalesced incremental sync.
pub async fn hook_cursor_after_shell() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(
        root.as_deref(),
        HintAgent::Cursor,
        "afterShellExecution",
        &event,
    );
    notify_cursor_after_shell_event(&event).await;
    0
}

/// Cursor `workspaceOpen` hook handler.
///
/// Notifies the daemon to run one-shot workspace catch-up. Fail-open.
pub async fn hook_cursor_workspace_open() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "workspaceOpen", &event);
    notify_cursor_workspace_open(&event).await;
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
    Some(
        serde_json::json!({
            "additional_context": format_tool_hint(&hint),
        })
        .to_string(),
    )
}

/// Impure `postToolUse` path: [`evaluate_cursor_post_tool_use`] plus
/// per-session hint dedupe persisted under the project's `.tracedecay/` dir.
pub fn cursor_post_tool_use_decision(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_tool_hint_input(&parsed))?;
    let root = cursor_project_root_candidate_from_parsed_event(&parsed);
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Cursor,
        event_session_id(&parsed).as_deref(),
        &hint,
    );
    let hint = deduped_cursor_hint(event_json, hint)?;
    Some(
        serde_json::json!({
            "additional_context": format_tool_hint(&hint),
        })
        .to_string(),
    )
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
fn deduped_cursor_hint(event_json: &str, hint: ToolHint) -> Option<ToolHint> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let root = cursor_project_root_candidate_from_parsed_event(&parsed)?;
    if !crate::tracedecay::TraceDecay::is_initialized(&root) {
        record_hint_analytics(
            Some(&root),
            "suppressed_uninitialized",
            HintAgent::Cursor,
            event_session_id(&parsed).as_deref(),
            &hint,
        );
        return None;
    }
    deduped_project_hint(
        Some(root),
        HintAgent::Cursor,
        event_session_id(&parsed),
        hint,
    )
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
        if let Some(rel) = rel_under_root(project_root, Path::new(&abs)) {
            if !rels.contains(&rel) {
                rels.push(rel);
            }
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
async fn notify_cursor_after_file_edit(event_json: &str) {
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }
    let rels = cursor_after_file_edit_rel_paths(event_json, &root);
    if rels.is_empty() {
        return;
    }
    crate::daemon::notify_hook_event(
        &root,
        crate::daemon::DaemonHookEvent::cursor_after_file_edit(rels),
    )
    .await;
}

/// Best-effort daemon notification for Cursor `afterShellExecution`.
async fn notify_cursor_after_shell_event(event_json: &str) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let command = parsed
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }
    let cwd = cursor_event_cwd(&parsed).unwrap_or_else(|| root.clone());
    crate::daemon::notify_hook_event(
        &root,
        crate::daemon::DaemonHookEvent::cursor_after_shell_execution(command.to_string(), cwd),
    )
    .await;
}

/// Best-effort daemon notification for Cursor `workspaceOpen`.
async fn notify_cursor_workspace_open(event_json: &str) {
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }
    crate::daemon::notify_hook_event(
        &root,
        crate::daemon::DaemonHookEvent::cursor_workspace_open(root.clone()),
    )
    .await;
}

async fn reset_counter_for_cursor_event(event_json: &str) {
    let Some(project_root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Incrementally ingests the Cursor transcript referenced by `event_json` into
/// the resolved project session DB, bounded by `max_new_bytes` (the hot-path cap)
/// and an overall `budget`. Always fails open: a timeout, missing transcript, or
/// any error is swallowed so the calling hook never blocks the agent.
pub(super) async fn ingest_cursor_transcript_for_event(
    event_json: &str,
    max_new_bytes: Option<u64>,
    budget: Duration,
) -> bool {
    let work = async {
        let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
            return false;
        };
        let Some(project_root) = cursor_project_root_from_parsed_event(&parsed) else {
            return false;
        };
        if let Some(cwd_root) = cursor_event_cwd(&parsed)
            .as_deref()
            .and_then(crate::config::discover_project_root)
        {
            if !paths_same(&cwd_root, &project_root) {
                return false;
            }
        }
        let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
            return false;
        };
        let _ = crate::sessions::cursor::ingest_cursor_transcript_event_capped(
            event_json,
            &db,
            max_new_bytes,
        )
        .await;
        true
    };
    // Short-lived CLI hook processes exit immediately, so the ingest must run
    // inline (not on a detached task); the timeout keeps it inside budget.
    tokio::time::timeout(budget, work).await.unwrap_or(false)
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
        file_path: text_field(tool_input, &["file_path", "filePath", "path"])
            .or_else(|| text_field(parsed, &["file_path", "filePath", "path"])),
        hints_enabled: true,
    }
}

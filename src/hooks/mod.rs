//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! Each agent sends its own event schema and expects its own output shape, so
//! handlers stay agent-specific while shared plumbing lives here.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use tracedecay_hooks::{DaemonHookEvent, HookRouteMetadata};

mod analytics;
mod claude;
mod codex;
mod cursor;
mod cursor_compact;
mod daemon_ports;
pub mod hint_outcomes;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod hook_boundary_failure_matrix;
mod kiro;
pub(crate) mod memory_inject;
mod post_tool_use;
mod steering;
pub mod tool_hints;
mod v2;
pub(crate) use v2::HOOK_V2_BOUND_HOSTS;
pub(crate) use v2::NativeContextScoutLifecycleV1;
pub(crate) use v2::project_and_worktree_locators_for_scope as hook_v2_scope_locators;
pub(crate) use v2::project_id_for_layout as hook_v2_project_id_for_layout;
pub(crate) use v2::protected_session_id_for_native as hook_v2_protected_session_id_for_native;
pub(crate) use v2::publish_daemon_bindings as publish_hook_v2_bindings;

pub use claude::{
    claude_session_context_for_event, evaluate_hook_decision, hook_claude_post_tool_use,
    hook_claude_session_start, hook_claude_subagent_start, hook_pre_tool_use, hook_prompt_submit,
    hook_stop,
};
pub use codex::{
    codex_additional_context_json, codex_apply_patch_rel_paths, codex_project_root_from_event,
    codex_subagent_start_log_line, codex_user_prompt_submit_context_for_event,
    codex_workspace_status_from_event, evaluate_codex_subagent_start, hook_codex_post_compact,
    hook_codex_post_tool_use, hook_codex_session_start, hook_codex_stop, hook_codex_subagent_start,
    hook_codex_user_prompt_submit, record_codex_subagent_start,
};
pub use cursor::{
    CURSOR_CATCH_UP_INGEST_MAX_BYTES, cursor_after_file_edit_rel_paths,
    cursor_post_tool_use_decision, cursor_project_root_from_event, cursor_session_start_json,
    cursor_should_run_sync, evaluate_cursor_after_file_edit, evaluate_cursor_post_tool_use,
    evaluate_cursor_subagent_start, hook_cursor_after_file_edit, hook_cursor_after_shell,
    hook_cursor_before_submit_prompt, hook_cursor_post_tool_use, hook_cursor_pre_compact,
    hook_cursor_session_end, hook_cursor_session_start, hook_cursor_stop,
    hook_cursor_subagent_start, hook_cursor_workspace_open,
};
pub use cursor_compact::{CursorPreCompactOutcome, cursor_pre_compact_via_daemon};
pub use kiro::{
    evaluate_kiro_pre_tool_use, hook_kiro_post_tool_use, hook_kiro_pre_tool_use,
    hook_kiro_prompt_submit, kiro_post_tool_use_rel_paths,
};
pub use post_tool_use::claude_post_tool_use_matcher;
pub use steering::{
    CURSOR_PLUGIN_SKILLS, HookWorkspaceStatus, build_codex_session_context,
    build_codex_session_context_for_workspace, build_cursor_session_context, cursor_staleness_hint,
};

#[cfg(test)]
use analytics::HOOK_ANALYTICS_FILENAME;
pub(crate) use analytics::HookCompletedReadinessDistributions;
#[cfg(test)]
pub(crate) use analytics::{host_hook_telemetry_contract, measure_host_event_payload_bytes};
use analytics::{
    mint_hint_id, record_hint_analytics, record_hint_emitted, record_hook_analytics,
    record_hook_invoked, record_hook_invoked_parsed, record_other_hook_invoked,
    record_workspace_status_analytics,
};

pub(crate) fn aggregate_hook_completed_readiness(
    rows: &[Value],
) -> HookCompletedReadinessDistributions {
    analytics::aggregate_hook_completed_readiness(rows)
}

struct RootHookReadinessProjection;

impl tracedecay_dashboard_api::hooks::HookReadinessProjectionPort for RootHookReadinessProjection {
    fn aggregate_hook_completed_readiness(&self, rows: &[Value]) -> Value {
        let distribution = aggregate_hook_completed_readiness(rows);
        match serde_json::to_value(distribution) {
            Ok(value) => value,
            Err(error) => {
                panic!("failed to serialize canonical hook readiness distribution: {error}")
            }
        }
    }
}

pub(crate) fn install_dashboard_hook_readiness_projection() -> crate::errors::Result<()> {
    static INSTALLATION: std::sync::LazyLock<std::result::Result<(), String>> =
        std::sync::LazyLock::new(|| {
            tracedecay_dashboard_api::hooks::install_hook_readiness_projection(std::sync::Arc::new(
                RootHookReadinessProjection,
            ))
            .map_err(|_| "dashboard hook readiness projection is already installed".to_owned())
        });
    INSTALLATION
        .as_ref()
        .map_err(|message| crate::errors::TraceDecayError::Config {
            message: message.clone(),
        })
        .copied()
}
use tool_hints::{HintAgent, ToolHint};

pub async fn hook_kimi_v2(event_json: &str, project_root: &Path) -> Option<String> {
    let telemetry = record_other_hook_invoked(Some(project_root), "kimiV2Event", event_json);
    v2::dispatch(
        tracedecay_hooks::HookHostV1::KimiCode,
        event_json,
        project_root,
        Some(&telemetry),
    )
    .await
    .into_recorded_guidance(&telemetry)
    .flatten()
}

pub async fn hook_opencode_v2_event(event_json: &str, project_root: &Path) -> Option<String> {
    let telemetry = record_other_hook_invoked(Some(project_root), "openCodeV2Event", event_json);
    let dispatch = if tracedecay_hooks::decode_opencode_lsp_event(event_json.as_bytes()).is_ok() {
        v2::dispatch_opencode_lsp_updated(event_json, project_root, Some(&telemetry)).await
    } else {
        v2::dispatch(
            tracedecay_hooks::HookHostV1::OpenCode,
            event_json,
            project_root,
            Some(&telemetry),
        )
        .await
    };
    dispatch.into_recorded_guidance(&telemetry).flatten()
}

pub async fn hook_opencode_v2_tool_after(event_json: &str, project_root: &Path) -> Option<String> {
    let telemetry =
        record_other_hook_invoked(Some(project_root), "openCodeV2ToolAfter", event_json);
    v2::dispatch_opencode_tool_after(event_json, project_root, Some(&telemetry))
        .await
        .into_recorded_guidance(&telemetry)
        .flatten()
}

macro_rules! read_hook_event {
    () => {{
        match $crate::hooks::read_stdin_bounded() {
            Ok($crate::hooks::HookStdinRead::Event(event)) => event,
            Ok($crate::hooks::HookStdinRead::Oversized) => {
                eprintln!(
                    "tracedecay hook: stdin exceeds wire message bound ({})",
                    $crate::application::host_admission::WIRE_RECORD_TOO_LARGE
                );
                return 0;
            }
            Err(e) => {
                eprintln!("tracedecay hook: failed to read stdin: {e}");
                return 1;
            }
        }
    }};
}
pub(crate) use read_hook_event;

pub async fn hook_kimi_event() -> i32 {
    let event = read_hook_event!();
    let Some(root) = native_event_project_root(&event).await else {
        return 0;
    };
    if let Some(guidance) = hook_kimi_v2(&event, &root).await {
        println!("{guidance}");
    }
    0
}

pub async fn hook_opencode_event() -> i32 {
    let event = read_hook_event!();
    let Some(root) = native_event_project_root(&event).await else {
        return 0;
    };
    if let Some(guidance) = hook_opencode_v2_event(&event, &root).await {
        println!("{guidance}");
    }
    0
}

pub async fn hook_opencode_tool_after() -> i32 {
    let event = read_hook_event!();
    let Some(root) = native_event_project_root(&event).await else {
        return 0;
    };
    if let Some(guidance) = hook_opencode_v2_tool_after(&event, &root).await {
        println!("{guidance}");
    }
    0
}

async fn native_event_project_root(event: &str) -> Option<PathBuf> {
    let parsed = serde_json::from_str::<Value>(event).ok();
    let start = parsed
        .as_ref()
        .and_then(|value| value.get("cwd"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    crate::config::discover_project_root_with_identity(&start).await
}

pub(crate) async fn daemon_tool_json(
    project_root: Option<&Path>,
    tool_name: &str,
    arguments: Value,
) -> crate::errors::Result<Value> {
    let handshake = crate::daemon::DaemonHandshake::for_current_client(
        project_root.map(Path::to_path_buf),
        None,
        false,
        false,
    )?;
    let result = crate::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
    parse_daemon_tool_json_content(&result, tool_name)
}

fn parse_daemon_tool_json_content(result: &Value, tool_name: &str) -> crate::errors::Result<Value> {
    let payloads = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .collect::<Vec<_>>();

    match payloads.as_slice() {
        [payload] => Ok(payload.clone()),
        [] => Err(crate::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no JSON payload"),
        }),
        _ => Err(crate::errors::TraceDecayError::Config {
            message: format!(
                "daemon tool {tool_name} returned multiple JSON payloads ({})",
                payloads.len()
            ),
        }),
    }
}

pub(crate) async fn daemon_hook_action(
    project_root: Option<&Path>,
    mut arguments: Value,
    telemetry: Option<&analytics::HookTimingSpan>,
) -> crate::errors::Result<Value> {
    arguments["format"] = serde_json::json!("json");
    let payload_bytes = analytics::measure_json_payload_bytes(&arguments);
    #[cfg(test)]
    if let Some(result) = take_test_daemon_hook_action(project_root, &arguments) {
        if let Some(telemetry) = telemetry {
            telemetry.note_completed_daemon_call(payload_bytes, 0, &result);
        }
        return result;
    }
    let handshake = match crate::daemon::DaemonHandshake::for_current_client(
        project_root.map(Path::to_path_buf),
        None,
        false,
        project_root.is_some(),
    ) {
        Ok(handshake) => handshake,
        Err(error) => {
            let err = Err(error);
            if let Some(telemetry) = telemetry {
                telemetry.note_daemon_result(&err);
            }
            return err;
        }
    };
    let started = std::time::Instant::now();
    let result = crate::daemon::call_default_tool(&handshake, "tracedecay_hook_runtime", arguments)
        .await
        .and_then(|result| parse_daemon_tool_json_content(&result, "tracedecay_hook_runtime"));
    if let Some(telemetry) = telemetry {
        telemetry.note_completed_daemon_call(
            payload_bytes,
            analytics::elapsed_us(started),
            &result,
        );
    }
    result
}

pub(crate) async fn ingest_user_session(
    provider: &str,
    session_id: Option<String>,
    telemetry: Option<&analytics::HookTimingSpan>,
) -> bool {
    if session_id.is_none() {
        return false;
    }
    match daemon_hook_action(
        None,
        serde_json::json!({
            "action": "ingest_transcript",
            "provider": provider.to_lowercase(),
            "user_scope": true,
            "session_id": session_id,
        }),
        telemetry,
    )
    .await
    {
        Ok(result) => result
            .get("messages_upserted")
            .and_then(Value::as_u64)
            .is_some_and(|count| count > 0),
        Err(error) => {
            eprintln!("[tracedecay] user {provider} ingest daemon call failed: {error}");
            false
        }
    }
}

pub(crate) async fn reset_counter_for_project(
    project_root: &Path,
    telemetry: Option<&analytics::HookTimingSpan>,
) {
    if let Err(error) = daemon_hook_action(
        Some(project_root),
        serde_json::json!({ "action": "reset_counter" }),
        telemetry,
    )
    .await
    {
        eprintln!("[tracedecay] local counter reset daemon call failed: {error}");
    }
}

pub(crate) async fn notify_hook_event_with_telemetry(
    project_root: &Path,
    event: DaemonHookEvent,
    telemetry: &analytics::HookTimingSpan,
) {
    let payload_bytes = analytics::measure_json_payload_bytes(&event);
    crate::daemon::notify_hook_event(project_root, event).await;
    telemetry.note_completed_daemon_notification(payload_bytes);
}

pub(crate) async fn notify_hook_event_with_optional_telemetry(
    project_root: &Path,
    event: DaemonHookEvent,
    telemetry: Option<&analytics::HookTimingSpan>,
) {
    match telemetry {
        Some(telemetry) => {
            notify_hook_event_with_telemetry(project_root, event, telemetry).await;
        }
        None => {
            let _ = crate::daemon::notify_hook_event(project_root, event).await;
        }
    }
}

pub async fn hook_hermes_terminal_receipt() -> i32 {
    let event_json = read_hook_event!();
    let Ok(event) = serde_json::from_str::<DaemonHookEvent>(&event_json) else {
        return 0;
    };
    if event.agent != "hermes"
        || !matches!(
            event.event.as_str(),
            "terminalReceipt" | "turnCompleted" | "turnIngested"
        )
        || event.receipt.is_none()
    {
        return 0;
    }
    let cwd = event
        .route
        .as_ref()
        .and_then(|route| route.cwd.clone().or_else(|| route.worktree.clone()))
        .or_else(|| event.cwd.clone());
    let project_root = match cwd {
        Some(cwd) => crate::config::discover_project_root_with_identity(&cwd).await,
        None => None,
    };
    let hook_telemetry = record_hook_invoked(
        project_root.as_deref(),
        HintAgent::Hermes,
        event.event.as_str(),
        &event_json,
    );
    if let Some(project_root) = project_root.as_ref() {
        if let Some(guidance) = v2::dispatch(
            tracedecay_hooks::HookHostV1::Hermes,
            &event_json,
            project_root,
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
        notify_hook_event_with_telemetry(project_root, event, &hook_telemetry).await;
    } else if let Err(error) = daemon_hook_action(
        None,
        serde_json::json!({ "action": "hermes_receipt", "event": event }),
        Some(&hook_telemetry),
    )
    .await
    {
        eprintln!("[tracedecay] user Hermes receipt daemon call failed: {error}");
    }
    0
}

pub(crate) fn schedule_user_session_review(provider: &str, session_id: Option<&str>) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let payload = serde_json::json!({ "provider": provider, "session_id": session_id }).to_string();
    let mut command = std::process::Command::new(exe);
    command.arg("hook-user-session-review");
    let _ = spawn_reaped_hook_child(command, payload.as_bytes());
}

fn spawn_reaped_hook_child(
    mut command: std::process::Command,
    payload: &[u8],
) -> std::io::Result<u32> {
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = child.id();
    let write_result = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(payload));
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    write_result?;
    Ok(pid)
}

pub async fn hook_user_session_review() -> i32 {
    let event = read_hook_event!();
    let Ok(payload) = serde_json::from_str::<Value>(&event) else {
        return 0;
    };
    let Some(provider) = payload.get("provider").and_then(Value::as_str) else {
        return 0;
    };
    let session_id = payload.get("session_id").and_then(Value::as_str);
    if let Err(error) = daemon_hook_action(
        None,
        serde_json::json!({
            "action": "user_review",
            "provider": provider,
            "session_id": session_id,
        }),
        None,
    )
    .await
    {
        eprintln!("[tracedecay] {provider} user session review daemon call failed: {error}");
    }
    0
}

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

#[cfg(test)]
pub(crate) fn run_with_test_env_lock<T>(future: impl std::future::Future<Output = T>) -> T {
    let _lock = lock_test_env();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build hook test runtime")
        .block_on(future)
}

#[cfg(test)]
#[derive(Default)]
struct TestDaemonHookActionState {
    owner: Option<std::thread::ThreadId>,
    responses: std::collections::VecDeque<Value>,
    calls: Vec<(Option<PathBuf>, Value)>,
}

#[cfg(test)]
static TEST_DAEMON_HOOK_ACTION: std::sync::LazyLock<std::sync::Mutex<TestDaemonHookActionState>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(TestDaemonHookActionState::default()));

#[cfg(test)]
pub(crate) struct TestDaemonHookActionGuard {
    owner: std::thread::ThreadId,
}

#[cfg(test)]
impl TestDaemonHookActionGuard {
    pub(crate) fn install(responses: impl IntoIterator<Item = Value>) -> Self {
        let owner = std::thread::current().id();
        let mut state = TEST_DAEMON_HOOK_ACTION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            state.owner.is_none(),
            "daemon hook test responder is in use"
        );
        state.owner = Some(owner);
        state.responses = responses.into_iter().collect();
        state.calls.clear();
        Self { owner }
    }

    pub(crate) fn calls(&self) -> Vec<(Option<PathBuf>, Value)> {
        let state = TEST_DAEMON_HOOK_ACTION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.owner, Some(self.owner));
        state.calls.clone()
    }
}

#[cfg(test)]
impl Drop for TestDaemonHookActionGuard {
    fn drop(&mut self) {
        let mut state = TEST_DAEMON_HOOK_ACTION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.owner == Some(self.owner) {
            *state = TestDaemonHookActionState::default();
        }
    }
}

#[cfg(test)]
fn take_test_daemon_hook_action(
    project_root: Option<&Path>,
    arguments: &Value,
) -> Option<crate::errors::Result<Value>> {
    let mut state = TEST_DAEMON_HOOK_ACTION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.owner != Some(std::thread::current().id()) {
        return None;
    }
    state
        .calls
        .push((project_root.map(Path::to_path_buf), arguments.clone()));
    Some(
        state
            .responses
            .pop_front()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "daemon hook test responder has no response".to_string(),
            }),
    )
}

fn hook_route_metadata_from_event(
    event_json: &str,
    project_root: &Path,
) -> Option<HookRouteMetadata> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    Some(hook_route_metadata_from_parsed(&parsed, project_root))
}

fn hook_route_metadata_from_parsed(parsed: &Value, project_root: &Path) -> HookRouteMetadata {
    let cwd = event_cwd_from_parsed(parsed);
    let route_root = cwd.as_deref().unwrap_or(project_root);
    let worktree = crate::worktree::git_worktree_root(route_root)
        .unwrap_or_else(|| project_root.to_path_buf());
    let branch = crate::branch::current_branch(&worktree);
    HookRouteMetadata {
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
    root: Option<&Path>,
    agent: HintAgent,
    session_id: Option<String>,
    hint: ToolHint,
) -> Option<ToolHint> {
    let hint_id = mint_hint_id();
    deduped_project_hint_with_id(root, agent, session_id, &hint_id, hint)
}

fn deduped_project_hint_with_id(
    root: Option<&Path>,
    agent: HintAgent,
    session_id: Option<String>,
    hint_id: &str,
    hint: ToolHint,
) -> Option<ToolHint> {
    let Some(session_id) = session_id else {
        record_hint_emitted(root, agent, None, hint_id, &hint);
        return Some(hint);
    };

    // Hooks may carry a stable session id without a project root. Persist
    // those decisions in the user profile so one missing cwd does not turn
    // every prompt/tool event into the same repeated hint.
    let project_path = root
        .and_then(|root| crate::storage::resolve_layout_for_current_profile(root).ok())
        .filter(|layout| layout.data_root.is_dir())
        .map(|layout| layout.data_root.join("tool_hints_seen.json"));
    let path = project_path.or_else(|| {
        crate::storage::default_profile_root()
            .ok()
            .map(|profile| profile.join("tool_hints_seen.json"))
    });
    let Some(path) = path else {
        record_hint_emitted(root, agent, Some(&session_id), hint_id, &hint);
        return Some(hint);
    };
    let mut dedupe = tool_hints::ToolHintDedupe::load_or_default(&path);
    let decision = dedupe.decide(&session_id, hint.category);
    // Every decision — including the suppressed ones — advances the persisted
    // budget, so the save is unconditional.
    let _ = dedupe.save(&path);

    // A suppressed decision still records the hint as it stood; only the
    // escalation path reports the escalated wording.
    let (event, reported) = match decision {
        tool_hints::HintDecision::Emit => ("hint_emitted", hint),
        tool_hints::HintDecision::Escalate => ("hint_escalated", hint.escalated()),
        tool_hints::HintDecision::SuppressedBudget => ("suppressed_budget", hint),
        tool_hints::HintDecision::SuppressedDuplicate => ("suppressed_duplicate", hint),
    };
    record_hint_analytics(root, event, agent, Some(&session_id), hint_id, &reported);

    matches!(
        decision,
        tool_hints::HintDecision::Emit | tool_hints::HintDecision::Escalate
    )
    .then_some(reported)
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

/// Resolves the tracedecay project root named by a hook event's `cwd`.
///
/// Claude, Codex, and Kiro all send the session working directory under the
/// same key, so this resolver is host-neutral and lives beside the other
/// event-field readers rather than in any one host's module.
fn event_project_root(parsed: &Value) -> Option<PathBuf> {
    let cwd = event_cwd_from_parsed(parsed)?;
    crate::config::discover_project_root(&cwd)
}

/// [`event_project_root`] for callers that hold only the raw event JSON.
fn event_project_root_from_json(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    event_project_root(&parsed)
}

/// The project root of the hook process's own working directory. Used by the
/// surfaces whose payload carries no `cwd` at all.
fn process_cwd_project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    crate::config::discover_project_root(&cwd)
}

/// Resolves the project root from the event `cwd`, falling back to the hook
/// process's working directory when the event omits one entirely. A present but
/// non-project `cwd` still resolves to nothing: the event named a directory, and
/// silently re-attributing it to wherever the hook happens to run would route the
/// event into an unrelated project.
fn event_project_root_or_process_cwd(parsed: &Value) -> Option<PathBuf> {
    match event_cwd_from_parsed(parsed) {
        Some(cwd) => crate::config::discover_project_root(&cwd),
        None => process_cwd_project_root(),
    }
}

/// Identity-aware [`event_project_root`]: consults the registry so a
/// global-store-only checkout still resolves. Shared by every host whose session
/// events carry `cwd`.
async fn event_project_root_with_identity(parsed: &Value) -> Option<PathBuf> {
    let cwd = event_cwd_from_parsed(parsed)?;
    crate::config::discover_project_root_with_identity(&cwd).await
}

/// [`event_project_root_with_identity`] for callers that hold only the raw JSON.
async fn event_project_root_with_identity_from_json(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    event_project_root_with_identity(&parsed).await
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

pub(crate) enum HookStdinRead {
    Event(String),
    /// Stdin exceeded [`crate::application::host_admission::MAX_WIRE_MESSAGE_BYTES`].
    /// No payload bytes are retained.
    Oversized,
}

/// Read host-hook stdin with the PR6 wire message byte cap enforced before
/// whole-body materialization.
pub(crate) fn read_stdin_bounded() -> std::io::Result<HookStdinRead> {
    read_stdin_bounded_from(&mut std::io::stdin().lock())
}

/// Testable stdin reader: streams until EOF while retaining at most
/// [`crate::application::host_admission::MAX_WIRE_MESSAGE_BYTES`].
pub(crate) fn read_stdin_bounded_from(
    reader: &mut impl std::io::Read,
) -> std::io::Result<HookStdinRead> {
    use crate::application::host_admission::{
        MAX_WIRE_MESSAGE_BYTES, WireReadOutcome, read_bounded_to_string,
    };
    match read_bounded_to_string(reader, MAX_WIRE_MESSAGE_BYTES)? {
        WireReadOutcome::Ready(event) => Ok(HookStdinRead::Event(event)),
        WireReadOutcome::Oversized => Ok(HookStdinRead::Oversized),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod wire_stdin_bound_tests;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod hint_analytics_tests;

#[cfg(test)]
mod tests;

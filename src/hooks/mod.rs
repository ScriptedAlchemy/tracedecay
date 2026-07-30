//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! Each agent sends its own event schema and expects its own output shape, so
//! handlers stay agent-specific while shared plumbing lives here.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

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
    CURSOR_CATCH_UP_INGEST_MAX_BYTES, cursor_after_file_edit_decision,
    cursor_after_file_edit_rel_paths, cursor_before_submit_prompt_json,
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
pub use post_tool_use::{
    CLAUDE_POST_TOOL_USE_EDIT_TOOLS, CLAUDE_POST_TOOL_USE_SHELL_TOOLS, claude_post_tool_use_matcher,
};
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
    record_hook_invoked, record_other_hook_invoked, record_workspace_status_analytics,
};

pub(crate) fn aggregate_hook_completed_readiness(
    rows: &[Value],
) -> HookCompletedReadinessDistributions {
    analytics::aggregate_hook_completed_readiness(rows)
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
    event: crate::daemon::DaemonHookEvent,
    telemetry: &analytics::HookTimingSpan,
) {
    let payload_bytes = analytics::measure_json_payload_bytes(&event);
    crate::daemon::notify_hook_event(project_root, event).await;
    telemetry.note_completed_daemon_notification(payload_bytes);
}

pub(crate) async fn notify_hook_event_with_optional_telemetry(
    project_root: &Path,
    event: crate::daemon::DaemonHookEvent,
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
    let Ok(event) = serde_json::from_str::<crate::daemon::DaemonHookEvent>(&event_json) else {
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
    match dedupe.decide(&session_id, hint.category) {
        tool_hints::HintDecision::Emit => {
            let _ = dedupe.save(&path);
            record_hint_analytics(
                root,
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
                root,
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
                root,
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
                root,
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
mod wire_stdin_bound_tests {
    use super::{HookStdinRead, read_stdin_bounded_from};
    use crate::application::host_admission::MAX_WIRE_MESSAGE_BYTES;
    use std::io::{self, Read};

    struct ChunkedHostileReader {
        remaining: usize,
        chunk: Vec<u8>,
    }

    impl ChunkedHostileReader {
        fn new(total: usize, chunk_byte: u8, chunk_len: usize) -> Self {
            Self {
                remaining: total,
                chunk: vec![chunk_byte; chunk_len.max(1)],
            }
        }
    }

    impl Read for ChunkedHostileReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            let n = buf.len().min(self.chunk.len()).min(self.remaining);
            buf[..n].copy_from_slice(&self.chunk[..n]);
            self.remaining -= n;
            Ok(n)
        }
    }

    #[test]
    fn hook_stdin_streams_hostile_input_and_returns_oversized_without_payload() {
        let mut hostile =
            ChunkedHostileReader::new(MAX_WIRE_MESSAGE_BYTES + 512 * 1024, b'h', 4096);
        let outcome = read_stdin_bounded_from(&mut hostile).unwrap();
        assert!(matches!(outcome, HookStdinRead::Oversized));
        assert!(hostile.remaining < MAX_WIRE_MESSAGE_BYTES + 512 * 1024);
    }

    #[test]
    fn hook_stdin_accepts_exact_wire_cap() {
        let body = vec![b'a'; MAX_WIRE_MESSAGE_BYTES];
        let outcome = read_stdin_bounded_from(&mut body.as_slice()).unwrap();
        match outcome {
            HookStdinRead::Event(event) => assert_eq!(event.len(), MAX_WIRE_MESSAGE_BYTES),
            HookStdinRead::Oversized => panic!("exact cap must be accepted"),
        }
    }
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
        // Hook telemetry is fail-closed: the timing span only records a
        // `hook_completed` row once a runtime configuration snapshot is
        // published. Bootstrap the default snapshot so duration telemetry and
        // hint dedupe rows are observable in these tests.
        crate::config::bootstrap_runtime_configuration(project_root, &layout)
            .expect("publish hook test runtime configuration");
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
        for forbidden in [
            "tool_name",
            "session_id",
            "project_root",
            "event_cwd",
            "command",
        ] {
            assert!(
                row.get(forbidden).is_none(),
                "hook telemetry must omit {forbidden}"
            );
        }
        assert!(row["duration_us"].as_u64().is_some());
        assert!(row["duration_ms"].as_u64().is_some());
        assert!(row["hook_wall_time_us"].as_u64().is_some());
        assert_eq!(row["coverage"], "host_measured");
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

        let project_key =
            crate::application::host_admission::HostAdmissionTestRuntimeV1::canonical_project_key(
                &project_root,
            );

        // Branch: root known, session known → on-disk dedupe emits once.
        let emit_id = mint_hint_id();
        assert!(
            deduped_project_hint_with_id(
                Some(&project_root),
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
                Some(&project_root),
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
                Some(&project_root),
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
                    .map(|root| {
                        crate::application::host_admission::HostAdmissionTestRuntimeV1::canonical_project_key(
                            Path::new(root),
                        )
                    });
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

    #[test]
    fn hints_without_project_root_dedupe_in_the_user_profile() {
        let _lock = super::lock_test_env();
        let profile = tempfile::tempdir().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let session = Some("session-without-project-root".to_string());

        assert!(
            deduped_project_hint_with_id(
                None,
                HintAgent::Codex,
                session.clone(),
                &mint_hint_id(),
                test_hint(),
            )
            .is_some()
        );
        assert!(
            deduped_project_hint_with_id(
                None,
                HintAgent::Codex,
                session,
                &mint_hint_id(),
                test_hint(),
            )
            .is_none(),
            "a missing project root must not turn every prompt into a fresh hint"
        );
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
                    Some(&project_root),
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
            Some(&project_root),
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
                Some(&project_root),
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
    use super::{
        hook_route_metadata_from_event, parse_daemon_tool_json_content, spawn_reaped_hook_child,
    };

    #[test]
    fn daemon_tool_json_ignores_notices_and_returns_one_payload() {
        let response = serde_json::json!({
            "content": [
                { "type": "text", "text": "write already accepted by daemon" },
                { "type": "text", "text": r#"{"status":"ok"}"# },
                { "type": "text", "text": "informational notice" }
            ]
        });

        assert_eq!(
            parse_daemon_tool_json_content(&response, "test").unwrap(),
            serde_json::json!({ "status": "ok" })
        );
    }

    #[test]
    fn daemon_tool_json_rejects_zero_or_multiple_payloads() {
        let no_payload = serde_json::json!({
            "content": [{ "type": "text", "text": "notice only" }]
        });
        let error = parse_daemon_tool_json_content(&no_payload, "test").unwrap_err();
        assert!(error.to_string().contains("returned no JSON payload"));

        let multiple = serde_json::json!({
            "content": [
                { "type": "text", "text": "{}" },
                { "type": "text", "text": "[]" }
            ]
        });
        let error = parse_daemon_tool_json_content(&multiple, "test").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("returned multiple JSON payloads (2)")
        );
    }

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

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_hook_child_is_reaped_after_exit() {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("exit 0");
        let pid = spawn_reaped_hook_child(command, b"").expect("spawn disposable hook child");
        let process_path = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while process_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            !process_path.exists(),
            "the exited hook child remained as an unreaped process"
        );
    }
}

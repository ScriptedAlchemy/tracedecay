//! Cursor hook handlers: native lifecycle admission plus legacy pass-through
//! telemetry and daemon notifications.
//!
//! Cursor expects Cursor-shaped stdout, separate from Claude, Codex, and Kiro.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_hooks::DaemonHookEvent;

use super::tool_hints::HintAgent;
use super::{
    hook_route_metadata_from_event, read_hook_event, record_hook_invoked,
    record_hook_invoked_parsed,
};

/// Ceiling used by daemon-owned Cursor catch-up work.
///
/// The hook adapter itself never reads a transcript; this remains a runtime
/// port for the daemon's bounded backlog policy.
pub const CURSOR_CATCH_UP_INGEST_MAX_BYTES: u64 =
    crate::sessions::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;

fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

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
/// This unsupported legacy surface remains passive so it cannot become a
/// local hint or storage authority when native admission is unavailable.
pub async fn hook_cursor_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let _hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "postToolUse", &event);
    0
}

/// Cursor `beforeSubmitPrompt` hook handler.
///
/// Cursor's legacy prompt surface is intentionally passive. Native lifecycle
/// admission owns host work; an unsupported prompt surface cannot substitute
/// local transcript, memory, or hint work.
pub async fn hook_cursor_before_submit_prompt() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event_with_identity(&event).await;
    let _hook_telemetry = record_hook_invoked(
        root.as_deref(),
        HintAgent::Cursor,
        "beforeSubmitPrompt",
        &event,
    );
    println!("{}", cursor_before_submit_prompt_json());
    0
}

/// Builds the Cursor `beforeSubmitPrompt` fail-open response.
pub fn cursor_before_submit_prompt_json() -> String {
    serde_json::json!({ "continue": true }).to_string()
}

/// Cursor `sessionEnd` hook handler.
pub async fn hook_cursor_session_end() -> i32 {
    let event = read_hook_event!();
    println!(
        "{}",
        cursor_session_completion_response("sessionEnd", &event).await
    );
    0
}

async fn cursor_session_completion_response(hook_name: &str, event: &str) -> String {
    let root = cursor_project_root_from_event_with_identity(event).await;
    let hook_telemetry = record_hook_invoked(root.as_deref(), HintAgent::Cursor, hook_name, event);
    super::v2::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::CursorDesktop,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| serde_json::json!({ "additional_context": guidance }).to_string(),
    )
}

/// Cursor `stop` hook handler (fire-and-forget).
///
/// Fires at the end of an agent turn and submits its native session boundary to
/// the daemon. The adapter stays fail-open and emits an empty object when the
/// daemon has no immediate guidance.
pub async fn hook_cursor_stop() -> i32 {
    let event = read_hook_event!();
    println!(
        "{}",
        cursor_session_completion_response("stop", &event).await
    );
    0
}

/// Cursor `preCompact` hook handler.
///
/// Cursor's compaction event delegates all compaction work to the daemon.
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

/// Cursor `afterFileEdit` submits the native saved-edit event to V2.
pub async fn hook_cursor_after_file_edit() -> i32 {
    let event = read_hook_event!();
    if let Some(response) = cursor_after_file_edit_response(&event).await {
        println!("{response}");
    }
    0
}

async fn cursor_after_file_edit_response(event: &str) -> Option<String> {
    // One parse for the root, the analytics row, and the daemon notification.
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    let root = cursor_project_root_from_parsed_event_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Cursor,
        "afterFileEdit",
        event,
        &parsed,
    );
    super::v2::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::CursorDesktop,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map(|guidance| serde_json::json!({ "additional_context": guidance }).to_string())
}

/// Cursor `sessionStart` hook handler.
pub async fn hook_cursor_session_start() -> i32 {
    let event = read_hook_event!();
    println!("{}", cursor_session_start_response(&event).await);
    0
}

async fn cursor_session_start_response(event: &str) -> String {
    let root = cursor_project_root_from_event_with_identity(event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Cursor, "sessionStart", event);
    let guidance = super::v2::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::CursorDesktop,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    cursor_session_start_json(root.as_deref(), guidance.as_deref().unwrap_or(""))
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

pub fn cursor_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    cursor_project_root_from_parsed_event(&parsed)
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn cursor_before_submit_prompt_json_is_passive() {
        let empty = cursor_before_submit_prompt_json();
        let parsed: Value = serde_json::from_str(&empty).unwrap();
        assert_eq!(parsed["continue"], Value::Bool(true));
        assert!(parsed.get("additional_context").is_none());
    }

    fn bind_v2_project(project_root: &Path, project_id: &str) {
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        crate::hooks::v2::publish_test_binding(
            project_root,
            tracedecay_hooks::HookHostV1::CursorDesktop,
        )
        .unwrap();
    }

    fn accepted_admission() -> Value {
        serde_json::json!({
            "action": "hook_v2_admit",
            "status": "accepted",
            "disposition": tracedecay_hooks::HookTransportDispositionV1::Accepted,
            "orchestration": null,
            "ready_guidance": null,
            "feedback_notice": null,
            "reason": null,
        })
    }

    fn unavailable_admission() -> Value {
        serde_json::json!({
            "action": "hook_v2_admit",
            "status": "unavailable",
        })
    }

    fn assert_v2_admission(
        daemon: &crate::hooks::TestDaemonHookActionGuard,
        project_root: &Path,
        session_id: &str,
    ) {
        let calls = daemon.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.as_deref(), Some(project_root));
        assert_eq!(calls[0].1["action"], "hook_v2_admit");
        assert_eq!(calls[0].1["native_session_id"], session_id);
        assert_eq!(
            calls[0].1["envelope"]["producer"],
            serde_json::json!(tracedecay_hooks::HookHostV1::CursorDesktop)
        );
    }

    #[tokio::test]
    async fn stop_admits_the_native_boundary_within_hook_budget() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        bind_v2_project(&project_root, "proj_cursor_stop_admission");
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([accepted_admission()]);
        let event = serde_json::json!({
            "hook_event_name": "stop",
            "conversation_id": "cursor-stop-session",
            "generation_id": "cursor-stop-generation",
            "model": "cursor-test-model",
            "status": "completed",
            "loop_count": 0,
            "cwd": project_root,
            "workspace_roots": [project_root],
        })
        .to_string();

        let started = std::time::Instant::now();
        let response = cursor_session_completion_response("stop", &event).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "native stop admission exceeded the bounded hook path"
        );
        assert_eq!(response, serde_json::json!({}).to_string());
        assert_v2_admission(&daemon, &project_root, "cursor-stop-session");
    }

    #[tokio::test]
    async fn after_file_edit_admits_the_native_edit_within_hook_budget() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        bind_v2_project(&project_root, "proj_cursor_edit_admission");
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([accepted_admission()]);
        let event = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "conversation_id": "cursor-edit-session",
            "generation_id": "cursor-edit-generation",
            "model": "cursor-test-model",
            "file_path": project_root.join("src/lib.rs"),
            "edits": [{ "old_string": "", "new_string": "pub fn changed() {}" }],
            "session_id": "cursor-edit-session",
            "cursor_version": "test",
            "workspace_roots": [project_root],
            "user_email": null,
            "transcript_path": project_root.join("session.jsonl"),
        })
        .to_string();

        let started = std::time::Instant::now();
        assert_eq!(cursor_after_file_edit_response(&event).await, None);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "native edit admission exceeded the bounded hook path"
        );
        assert_v2_admission(&daemon, &project_root, "cursor-edit-session");
    }

    #[tokio::test]
    async fn session_start_without_event_identity_only_returns_workspace_identity() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        bind_v2_project(&project_root, "proj_cursor_start_admission");
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([]);
        let event = serde_json::json!({
            "hook_event_name": "sessionStart",
            "conversation_id": "cursor-start-session",
            "workspace_roots": [project_root],
        })
        .to_string();

        let started = std::time::Instant::now();
        let response = cursor_session_start_response(&event).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "native session-start admission exceeded the bounded hook path"
        );
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            response["env"]["TRACEDECAY_PROJECT_ROOT"],
            project_root.to_string_lossy().as_ref()
        );
        assert!(
            daemon.calls().is_empty(),
            "session start without a provider event key must not reach admission"
        );
    }

    #[tokio::test]
    async fn unavailable_native_events_make_one_daemon_attempt_without_fallback_work() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        bind_v2_project(&project_root, "proj_cursor_unavailable");

        let stop = serde_json::json!({
            "hook_event_name": "stop",
            "conversation_id": "cursor-stop-unavailable",
            "generation_id": "cursor-stop-generation",
            "model": "cursor-test-model",
            "status": "completed",
            "loop_count": 0,
            "cwd": project_root,
            "workspace_roots": [project_root],
        })
        .to_string();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([unavailable_admission()]);
        assert_eq!(
            cursor_session_completion_response("stop", &stop).await,
            serde_json::json!({}).to_string()
        );
        assert_eq!(daemon.calls().len(), 1, "stop must not ingest a transcript");
        drop(daemon);

        let edit = serde_json::json!({
            "hook_event_name": "afterFileEdit",
            "conversation_id": "cursor-edit-unavailable",
            "generation_id": "cursor-edit-generation",
            "model": "cursor-test-model",
            "file_path": project_root.join("src/lib.rs"),
            "edits": [{ "old_string": "", "new_string": "pub fn changed() {}" }],
            "session_id": "cursor-edit-unavailable",
            "cursor_version": "test",
            "workspace_roots": [project_root],
            "user_email": null,
            "transcript_path": project_root.join("session.jsonl"),
        })
        .to_string();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([unavailable_admission()]);
        assert_eq!(cursor_after_file_edit_response(&edit).await, None);
        assert_eq!(
            daemon.calls().len(),
            1,
            "edit must not notify a fallback route"
        );
        drop(daemon);

        let start = serde_json::json!({
            "hook_event_name": "sessionStart",
            "conversation_id": "cursor-start-unavailable",
            "workspace_roots": [project_root],
        })
        .to_string();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([]);
        let response = cursor_session_start_response(&start).await;
        assert_eq!(
            daemon.calls().len(),
            0,
            "start without provider event identity must not reach daemon admission"
        );
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["additional_context"], "");
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
}

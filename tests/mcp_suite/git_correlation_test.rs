//! End-to-end tests for the `tracedecay_sessions_for` session↔git correlation
//! query surface, driven through the real `handle_tool_call` dispatch against a
//! temp project with a linked git worktree and a seeded `sessions.db`.

#![cfg(feature = "test-transport")]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::storage::PrivateStoreIo;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_sessions::runtime::git_correlation::{
    DEFAULT_SPAN_MERGE_GAP_SECS, SpanObservation, SpanSource,
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::common;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new(common::git_program())
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} should spawn: {e}"));
    assert!(status.success(), "git {args:?} should succeed");
}

/// Initializes a project repo (`main`) plus a linked worktree checked out on
/// `feature/session` under `base`. Returns `(project_root, worktree)`.
fn setup_linked_worktree_under(base: &Path) -> (PathBuf, PathBuf) {
    let project_root = base.join("project");
    let worktree_root = base.join("session-worktree");
    std::fs::create_dir_all(project_root.join("src"))
        .unwrap_or_else(|e| panic!("project dirs: {e}"));
    std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n")
        .unwrap_or_else(|e| panic!("source: {e}"));
    run_git(&project_root, &["init", "-b", "main"]);
    run_git(&project_root, &["config", "user.email", "test@test.com"]);
    run_git(&project_root, &["config", "user.name", "Test"]);
    run_git(&project_root, &["add", "."]);
    run_git(&project_root, &["commit", "-m", "initial"]);
    let worktree_arg = worktree_root.to_string_lossy();
    run_git(
        &project_root,
        &[
            "worktree",
            "add",
            worktree_arg.as_ref(),
            "-b",
            "feature/session",
        ],
    );
    (project_root, worktree_root)
}

fn session(session_id: &str, project_key: &str, started_at: i64) -> SessionRecord {
    SessionRecord {
        provider: "claude".to_string(),
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        project_path: project_key.to_string(),
        title: Some(format!("Session {session_id}")),
        started_at: Some(started_at),
        ended_at: None,
        transcript_path: Some(format!("{session_id}.jsonl")),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn message(session_id: &str, message_id: &str, ts: i64, text: &str) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: "claude".to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp: Some(ts),
        ordinal: 1,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: Some("test-model".to_string()),
        tool_names: None,
        source_path: Some(format!("{session_id}.jsonl")),
        source_offset: Some(0),
        metadata_json: None,
    }
}

fn span(session_id: &str, branch: Option<&str>, worktree: &str, ts: i64) -> SpanObservation {
    SpanObservation {
        provider: "claude".to_string(),
        session_id: session_id.to_string(),
        thread_id: None,
        branch: branch.map(str::to_string),
        worktree: worktree.to_string(),
        ts,
        source: SpanSource::HookRoute,
    }
}

async fn record_span(runtime: &HostAdmissionTestRuntimeV1, observation: &SpanObservation) {
    runtime
        .record_project_span_for_test(observation, DEFAULT_SPAN_MERGE_GAP_SECS)
        .await
        .unwrap_or_else(|e| panic!("record span: {e}"));
}

fn extract_json(result: &tracedecay::mcp::ToolResult) -> Value {
    let text = result.value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result should carry text content: {}", result.value));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tool result should be JSON: {e}\n{text}"))
}

async fn call(
    runtime: &HostAdmissionTestRuntimeV1,
    cg: &TraceDecay,
    tool: &str,
    mut args: Value,
) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    let result = runtime
        .call_mcp_tool_for_test(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
    extract_json(&result)
}

/// An empty correlation index (sessions present, but no spans recorded) must be
/// reported distinctly from "no sessions matched", both through
/// `tracedecay_sessions_for` and the `tracedecay_diagnostics` health block, so
/// callers never mistake an unpopulated index for an answered-and-empty query.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn sessions_for_and_diagnostics_flag_empty_correlation_index() {
    let dir = common::tempdir_or_panic();
    #[cfg(windows)]
    let base = dir.path().to_path_buf();
    #[cfg(not(windows))]
    let base = dir.path().canonicalize().unwrap();
    let (project_root, _worktree_root) = setup_linked_worktree_under(&base);

    let profile_root = base.join("profile");
    PrivateStoreIo::create_dir_all(&profile_root)
        .unwrap_or_else(|e| panic!("create profile root: {e}"));
    let profile_root = profile_root
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize profile root: {e}"));
    let cg = TraceDecay::init_with_options(
        &project_root,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(base.join("global.db")),
        },
    )
    .await
    .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();
    let main_worktree = project_root.to_string_lossy().to_string();

    // Seed sessions and messages but record NO git spans: the correlation
    // index exists (schema is ensured on open) yet holds nothing.
    // Reuse the runtime retained by init — opening a second daemon-scoped
    // HostAdmissionTestRuntimeV1 against the same profile overlaps the
    // maintenance/daemon scope maps under default features and is redundant
    // under test-transport (init already mounted the project sessions).
    let runtime = cg
        .test_runtime_for_test()
        .expect("init retains registered project session runtime");
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &session("s1", &project_key, 1_000),
            )
            .await
            .unwrap_or_else(|e| panic!("seed session: {e}"))
    );
    assert!(
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &message("s1", "s1-m1", 1_050, "work on main"),
            )
            .await
            .unwrap_or_else(|e| panic!("seed session message: {e}"))
    );

    // sessions_for on an empty index: no results, explicitly flagged empty.
    let empty = call(
        &runtime,
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "main" }),
    )
    .await;
    assert_eq!(empty["count"], 0, "{empty}");
    assert_eq!(empty["index_empty"], true, "{empty}");
    assert_eq!(empty["index"]["span_count"], 0, "{empty}");
    assert!(
        empty["message"]
            .as_str()
            .is_some_and(|m| m.contains("empty")),
        "empty-index message should say the index is empty: {empty}"
    );

    // diagnostics surfaces the same empty-index health.
    let diag_empty = call(&runtime, &cg, "tracedecay_diagnostics", json!({})).await;
    assert_eq!(
        diag_empty["session_correlation"]["index_empty"], true,
        "{diag_empty}"
    );
    assert_eq!(
        diag_empty["session_correlation"]["span_count"], 0,
        "{diag_empty}"
    );

    // Record one span on main; the index is no longer empty.
    record_span(&runtime, &span("s1", Some("main"), &main_worktree, 1_000)).await;

    // A ref with no matching span now reads as "no match", not "empty index".
    let no_match = call(
        &runtime,
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "does-not-exist" }),
    )
    .await;
    assert_eq!(no_match["count"], 0, "{no_match}");
    assert_eq!(no_match["index_empty"], false, "{no_match}");
    assert!(
        no_match["message"]
            .as_str()
            .is_some_and(|m| m.contains("no sessions matched")),
        "populated index should report no-match, not empty: {no_match}"
    );

    // diagnostics now reports a populated index.
    let diag_populated = call(&runtime, &cg, "tracedecay_diagnostics", json!({})).await;
    assert_eq!(
        diag_populated["session_correlation"]["index_empty"], false,
        "{diag_populated}"
    );
    assert_eq!(
        diag_populated["session_correlation"]["span_count"], 1,
        "{diag_populated}"
    );
    assert_eq!(
        diag_populated["session_correlation"]["projection_available"], true,
        "{diag_populated}"
    );
}

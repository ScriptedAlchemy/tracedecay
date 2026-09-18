//! End-to-end tests for the `tracedecay_sessions_for` session↔git correlation
//! query surface, driven through the real `handle_tool_call` dispatch against a
//! temp project with a linked git worktree and a seeded `sessions.db`.

#![cfg(feature = "test-transport")]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use tracedecay::mcp::McpServer;
use tracedecay::project::{TraceDecay, TraceDecayOpenOptions};
use tracedecay::test_support::host_admission::{
    HostAdmissionTestRuntimeV1, ProjectScopedTestRuntimeV1,
};
use tracedecay_runtime_core::storage::PrivateStoreIo;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::git_correlation::{
    DEFAULT_SPAN_MERGE_GAP_SECS, SpanObservation, SpanSource,
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

use crate::common;
use crate::support::extract_tool_result_json as extract_json;

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

async fn call(server: &McpServer, tool: &str, mut args: Value) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    for _ in 0..60 {
        let result = server
            .call_tool_for_test(tool, args.clone())
            .await
            .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
        let envelope = extract_json(&result);
        if envelope.pointer("/problem/code").and_then(Value::as_str)
            == Some("application.surface.unavailable")
        {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        return envelope
            .pointer("/outcome/value/payload")
            .cloned()
            .unwrap_or(envelope);
    }
    panic!("{tool} project runtime did not finish mounting")
}

/// An empty correlation index (sessions present, but no spans recorded) must be
/// reported distinctly from "no sessions matched" through
/// `tracedecay_sessions_for`, so callers never mistake an unpopulated index
/// for an answered-and-empty query.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn sessions_for_distinguishes_empty_correlation_index_from_no_match() {
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
    // Reuse the runtime retained by init, opening a second daemon-scoped
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
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        cg,
        None,
        ProjectScopedTestRuntimeV1::new(runtime.clone())
            .expect("git-correlation runtime is project scoped"),
    )
    .await
    .unwrap_or_else(|error| panic!("construct git-correlation server: {error}"));

    // sessions_for on an empty index: no results, explicitly flagged empty.
    let empty = call(
        &server,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "main" }),
    )
    .await;
    assert_eq!(empty["count"], 0, "{empty}");
    assert_eq!(empty["index_empty"], true, "{empty}");
    assert_eq!(empty["index"]["projection_available"], false, "{empty}");
    assert_eq!(empty["index"]["spans_present"], false, "{empty}");
    assert_eq!(empty["index"]["span_count"], 0, "{empty}");
    assert_eq!(empty["index"]["count_mode"], "presence_only", "{empty}");
    assert_eq!(empty["message"], EMPTY_SPAN_INDEX_MESSAGE, "{empty}");

    // Record one span on main; the index is no longer empty.
    record_span(&runtime, &span("s1", Some("main"), &main_worktree, 1_000)).await;

    // A ref with no matching span now reads as "no match", not "empty index".
    let no_match = call(
        &server,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "does-not-exist" }),
    )
    .await;
    assert_eq!(no_match["count"], 0, "{no_match}");
    assert_eq!(no_match["index_empty"], false, "{no_match}");
    assert_eq!(
        no_match["index"]["projection_available"], true,
        "{no_match}"
    );
    assert_eq!(no_match["index"]["spans_present"], true, "{no_match}");
    assert_eq!(no_match["index"]["span_count"], Value::Null, "{no_match}");
    assert_eq!(
        no_match["index"]["count_mode"], "presence_only",
        "{no_match}"
    );
    assert_eq!(no_match["message"], NO_MATCH_MESSAGE, "{no_match}");

    server.shutdown().await;
}

/// `tracedecay_sessions_for` through MCP dispatch: the caller sees the session
/// that touched the ref, an explicit empty-index state, or a typed rejection.
/// Index generation and source watermark are content-addressed (they include
/// the temp worktree), so they are masked after a same-index equality check.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn sessions_for_names_the_sessions_that_touched_the_git_ref() {
    let dir = common::tempdir_or_panic();
    #[cfg(windows)]
    let base = dir.path().to_path_buf();
    #[cfg(not(windows))]
    let base = dir.path().canonicalize().unwrap();
    let (project_root, feature_root) = setup_linked_worktree_under(&base);
    let profile_root = base.join("profile");
    PrivateStoreIo::create_dir_all(&profile_root)
        .unwrap_or_else(|e| panic!("create profile root: {e}"));
    let profile_root = profile_root
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize profile root: {e}"));
    let cg = TraceDecay::init_with_options(
        &project_root,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root),
            global_db_path: Some(base.join("global.db")),
        },
    )
    .await
    .unwrap_or_else(|e| panic!("init project: {e}"));
    let runtime = cg
        .test_runtime_for_test()
        .expect("init retains registered project session runtime");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        cg,
        None,
        ProjectScopedTestRuntimeV1::new(runtime.clone())
            .expect("git-correlation runtime is project scoped"),
    )
    .await
    .unwrap_or_else(|error| panic!("construct git-correlation server: {error}"));

    let main_worktree = project_root.to_string_lossy().to_string();
    let feature_worktree = feature_root.to_string_lossy().to_string();

    assert_payload(
        call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "branch", "value": "main" }),
        )
        .await,
        answer(
            "branch",
            "main",
            "produced",
            json!([]),
            empty_span_index(),
            true,
            Some(EMPTY_SPAN_INDEX_MESSAGE),
            None,
            None,
        ),
    );

    record_span(
        &runtime,
        &span("s-early", Some("main"), &main_worktree, 1_000),
    )
    .await;
    record_span(
        &runtime,
        &span("s-late", Some("main"), &main_worktree, 2_000),
    )
    .await;
    record_span(
        &runtime,
        &span(
            "s-feature",
            Some("feature/session"),
            &feature_worktree,
            1_500,
        ),
    )
    .await;

    let main_hits = json!([
        correlation_hit("s-late", "main", &main_worktree, 2_000),
        correlation_hit("s-early", "main", &main_worktree, 1_000),
    ]);
    let main = call(
        &server,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "main" }),
    )
    .await;
    let generation = main["index"]["generation"].clone();
    let watermark = main["index"]["source_watermark"].clone();
    assert_payload(
        main,
        answer(
            "branch",
            "main",
            "produced",
            main_hits.clone(),
            populated_span_index(),
            false,
            None,
            None,
            None,
        ),
    );

    let feature = call(
        &server,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "feature/session" }),
    )
    .await;
    assert_eq!(feature["index"]["generation"], generation, "{feature}");
    assert_eq!(feature["index"]["source_watermark"], watermark, "{feature}");
    assert_payload(
        feature,
        answer(
            "branch",
            "feature/session",
            "produced",
            json!([correlation_hit(
                "s-feature",
                "feature/session",
                &feature_worktree,
                1_500
            )]),
            populated_span_index(),
            false,
            None,
            None,
            None,
        ),
    );

    // Branch and worktree queries ignore `relation`; the response still echoes it.
    let observed = call(
        &server,
        "tracedecay_sessions_for",
        json!({
            "git_ref": "branch",
            "value": "feature/session",
            "relation": "observed"
        }),
    )
    .await;
    assert_eq!(observed["index"]["generation"], generation, "{observed}");
    assert_payload(
        observed,
        answer(
            "branch",
            "feature/session",
            "observed",
            json!([correlation_hit(
                "s-feature",
                "feature/session",
                &feature_worktree,
                1_500
            )]),
            populated_span_index(),
            false,
            None,
            None,
            None,
        ),
    );

    assert_payload(
        call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "worktree", "value": feature_worktree }),
        )
        .await,
        answer(
            "worktree",
            &feature_worktree,
            "produced",
            json!([correlation_hit(
                "s-feature",
                "feature/session",
                &feature_worktree,
                1_500
            )]),
            populated_span_index(),
            false,
            None,
            None,
            None,
        ),
    );
    assert_payload(
        call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "branch", "value": "main", "limit": 1 }),
        )
        .await,
        answer(
            "branch",
            "main",
            "produced",
            json!([correlation_hit("s-late", "main", &main_worktree, 2_000)]),
            populated_span_index(),
            false,
            None,
            None,
            None,
        ),
    );
    assert_payload(
        call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "branch", "value": "main", "since": 1_500 }),
        )
        .await,
        answer(
            "branch",
            "main",
            "produced",
            json!([correlation_hit("s-late", "main", &main_worktree, 2_000)]),
            populated_span_index(),
            false,
            None,
            Some(1_500),
            None,
        ),
    );
    assert_payload(
        call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "branch", "value": "main", "until": 1_500 }),
        )
        .await,
        answer(
            "branch",
            "main",
            "produced",
            json!([correlation_hit("s-early", "main", &main_worktree, 1_000)]),
            populated_span_index(),
            false,
            None,
            None,
            Some(1_500),
        ),
    );
    assert_payload(
        call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "branch", "value": "does-not-exist" }),
        )
        .await,
        answer(
            "branch",
            "does-not-exist",
            "produced",
            json!([]),
            populated_span_index(),
            false,
            Some(NO_MATCH_MESSAGE),
            None,
            None,
        ),
    );
    // Spans do not populate commit evidence. A commit query stays index-empty.
    let commit = call(
        &server,
        "tracedecay_sessions_for",
        json!({ "git_ref": "commit", "value": "ABCD12" }),
    )
    .await;
    assert_eq!(commit["index"]["generation"], generation, "{commit}");
    assert_payload(
        commit,
        answer(
            "commit",
            "abcd12",
            "produced",
            json!([]),
            populated_span_index(),
            true,
            Some(EMPTY_COMMIT_INDEX_MESSAGE),
            None,
            None,
        ),
    );

    record_span(
        &runtime,
        &span("s-other", Some("other"), &main_worktree, 3_000),
    )
    .await;
    let after_other = call(
        &server,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "main" }),
    )
    .await;
    assert_ne!(
        after_other["index"]["generation"], generation,
        "new evidence must publish a new index generation: {after_other}"
    );
    assert_ne!(
        after_other["index"]["source_watermark"], watermark,
        "new evidence must move the source watermark: {after_other}"
    );
    assert_payload(
        after_other,
        answer(
            "branch",
            "main",
            "produced",
            main_hits,
            populated_span_index(),
            false,
            None,
            None,
            None,
        ),
    );

    assert_invalid_request(
        &call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "commit", "value": "abc" }),
        )
        .await,
    );
    assert_invalid_request(
        &call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "branch", "value": " " }),
        )
        .await,
    );
    assert_invalid_request(
        &call(
            &server,
            "tracedecay_sessions_for",
            json!({ "git_ref": "branch", "value": "main", "since": 20, "until": 10 }),
        )
        .await,
    );
    assert_schema_rejection(
        &server,
        json!({ "value": "main", "format": "json" }),
        "application surface request does not match its reviewed schema: missing field `git_ref`",
    )
    .await;
    assert_schema_rejection(
        &server,
        json!({ "git_ref": "tag", "value": "main", "format": "json" }),
        "application surface request does not match its reviewed schema: git_ref: unknown variant `tag`, expected one of `branch`, `worktree`, `commit`",
    )
    .await;

    server.shutdown().await;
}

const EMPTY_SPAN_INDEX_MESSAGE: &str = "correlation index empty (no git spans recorded yet). It will converge on the next daemon startup, or run `tracedecay sessions git-sync` to schedule it now";
const EMPTY_COMMIT_INDEX_MESSAGE: &str = "no commit evidence indexed yet. Run `tracedecay sync` to ingest direct host/tool evidence; `tracedecay sessions git-sync` adds weaker historical overlap evidence";
const NO_MATCH_MESSAGE: &str = "no sessions matched this git ref";

fn correlation_hit(session_id: &str, branch: &str, worktree: &str, ts: i64) -> Value {
    json!({
        "provider": "claude",
        "session_id": session_id,
        "branch": branch,
        "worktree": worktree,
        "first_ts": ts,
        "last_ts": ts,
        "event_count": 1,
        "span_count": 1,
        "sources": ["hookroute"],
        "commit_sha": null,
        "committed_at": null,
        "span_overlap_kind": null,
        "relation": null,
        "evidence": null,
        "confidence": null,
        "evidence_message_id": null
    })
}

fn empty_span_index() -> Value {
    json!({
        "projection_available": false,
        "generation": null,
        "source_watermark": null,
        "spans_present": false,
        "commits_present": false,
        "span_count": 0,
        "commit_count": 0,
        "backfill_watermark": null,
        "count_mode": "presence_only"
    })
}

fn populated_span_index() -> Value {
    json!({
        "projection_available": true,
        "generation": "INDEX_GENERATION",
        "source_watermark": "INDEX_WATERMARK",
        "spans_present": true,
        "commits_present": false,
        "span_count": null,
        "commit_count": 0,
        "backfill_watermark": null,
        "count_mode": "presence_only"
    })
}

fn answer(
    git_ref: &str,
    value: &str,
    relation: &str,
    results: Value,
    index: Value,
    index_empty: bool,
    message: Option<&str>,
    since: Option<i64>,
    until: Option<i64>,
) -> Value {
    let count = results
        .as_array()
        .expect("expected results are an array")
        .len();
    let mut payload = json!({
        "status": "ok",
        "git_ref": git_ref,
        "value": value,
        "relation": relation,
        "count": count,
        "results": results,
        "index_empty": index_empty,
        "index": index,
    });
    if let Some(message) = message {
        payload["message"] = json!(message);
    }
    if let Some(since) = since {
        payload["since"] = json!(since);
    }
    if let Some(until) = until {
        payload["until"] = json!(until);
    }
    payload
}

fn assert_payload(actual: Value, expected: Value) {
    let raw = actual.clone();
    assert_eq!(mask_index_identity(actual), expected, "raw payload: {raw}");
}

fn mask_index_identity(mut payload: Value) -> Value {
    let Some(index) = payload.get_mut("index").and_then(Value::as_object_mut) else {
        return payload;
    };
    if index.get("generation").and_then(Value::as_str).is_some() {
        index.insert("generation".to_owned(), json!("INDEX_GENERATION"));
    }
    if index
        .get("source_watermark")
        .and_then(Value::as_str)
        .is_some()
    {
        index.insert("source_watermark".to_owned(), json!("INDEX_WATERMARK"));
    }
    payload
}

fn assert_invalid_request(envelope: &Value) {
    assert_eq!(
        envelope["problem"]["kind"],
        json!("invalid_request"),
        "{envelope}"
    );
    assert_eq!(
        envelope["problem"]["code"],
        json!("application.retained.invalid-request"),
        "{envelope}"
    );
    assert_eq!(
        envelope["problem"]["message"],
        json!("The retained operation request is invalid."),
        "{envelope}"
    );
    assert_eq!(envelope["problem"]["retry"], json!("never"), "{envelope}");
    assert_eq!(envelope["problem"]["retryable"], json!(false), "{envelope}");
    assert_eq!(
        envelope["problem"]["legal_actions"],
        json!(["correct_request"]),
        "{envelope}"
    );
    assert!(
        envelope.get("count").is_none(),
        "invalid input must not be reported as an empty match: {envelope}"
    );
}

async fn assert_schema_rejection(server: &McpServer, args: Value, detail: &str) {
    let error = server
        .call_tool_for_test("tracedecay_sessions_for", args)
        .await
        .expect_err("malformed tracedecay_sessions_for arguments must be rejected");
    let (code, retryable, actual) = error
        .project_route_context()
        .unwrap_or_else(|| panic!("expected a typed project-route rejection, got {error}"));
    assert_eq!(code, "application_surface_invalid_request", "{error}");
    assert!(!retryable, "{error}");
    assert_eq!(actual, detail, "{error}");
}

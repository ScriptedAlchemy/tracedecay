//! Production MCP behavior of `tracedecay_session_refresh_begin`.
//!
//! Calls go through the daemon composition's JSON-RPC `tools/call` path, the
//! same entry a host uses. The store treats a running or completed operation
//! with the same request digest as joinable, so a repeat must report `joined`
//! and reuse the first call's handle, operation id, and acceptance time.

#![cfg(feature = "test-transport")]

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::common;
use crate::support::{GLOBAL_DB_ENV_LOCK, HomeEnvGuard, test_temp_dir};

const TOOL: &str = "tracedecay_session_refresh_begin";

fn init_git_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn refresh_begin_probe() {}\n",
    )
    .expect("project source");
    let init = Command::new(common::git_program())
        .args(["init", "-q"])
        .current_dir(project)
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    let add = Command::new(common::git_program())
        .args(["add", "."])
        .current_dir(project)
        .status()
        .expect("git add");
    assert!(add.success(), "git add must succeed");
    let commit = Command::new(common::git_program())
        .args([
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "session refresh begin fixture",
        ])
        .current_dir(project)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit must succeed");
}

fn profile_begin_arguments(session_id: &str) -> Value {
    json!({
        "scope": { "kind": "profile" },
        "session": { "id": session_id },
        "source": { "scope": "codex" },
        "target": {
            "temporal_mode": { "kind": "current" },
            "grain": "session",
            "frontier": { "observed_through": 0, "committed_through": 0 }
        },
        "format": "json"
    })
}

fn effect_payload(result: &Value) -> Value {
    assert_eq!(
        result["isError"],
        Value::Null,
        "{TOOL} must answer as a successful tool result, got {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{TOOL} returned no text content: {result}"));
    let envelope: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{TOOL} returned non-JSON text: {error}\n{text}"));
    assert_eq!(
        envelope["outcome"]["outcome"], "effect",
        "{TOOL} must answer an effect, got {envelope}"
    );
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("{TOOL} effect omitted its payload: {envelope}"))
}

fn assert_sha256_token(value: &str, prefix: &str) {
    let digest = value
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("expected {prefix} token, got {value}"));
    assert_eq!(
        digest.len(),
        64,
        "expected a sha256 hex digest after {prefix}, got {value}"
    );
    assert!(
        digest
            .chars()
            .all(|character| character.is_ascii_hexdigit()),
        "expected a hex digest after {prefix}, got {value}"
    );
}

fn assert_begin_payload(payload: &Value, outcome: &str) {
    assert_eq!(payload["outcome"], outcome, "{payload}");
    assert_eq!(payload["scope"], "profile", "{payload}");
    assert_eq!(payload["tool"], TOOL, "{payload}");
    let object = payload
        .as_object()
        .unwrap_or_else(|| panic!("{TOOL} payload must be an object: {payload}"));
    for field in ["progress", "receipt", "error"] {
        assert!(
            object.contains_key(field),
            "{TOOL} payload omitted {field}: {payload}"
        );
        assert_eq!(object[field], Value::Null, "{payload}");
    }
    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("{TOOL} omitted its opaque handle: {payload}"));
    let operation_id = payload["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{TOOL} omitted its durable operation id: {payload}"));
    assert_sha256_token(handle, "srh_");
    assert_sha256_token(operation_id, "refresh.");
    assert_ne!(
        handle, operation_id,
        "{TOOL} must not expose the durable operation id as the client handle"
    );
    assert!(
        payload["accepted_at"].as_i64().is_some(),
        "{TOOL} accepted_at must be integer microseconds, got {payload}"
    );
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "accepted_at",
            "error",
            "handle",
            "operation_id",
            "outcome",
            "progress",
            "receipt",
            "scope",
            "tool",
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_begin_starts_then_joins_the_same_refresh_and_refuses_an_untyped_scope() {
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let root = test_temp_dir();
    let isolation = root.path().join("composition");
    let home = root.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let project = isolation.join("project");
    init_git_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        &isolation,
        [project.clone()],
    )
    .await
    .expect("production composition harness");
    let arguments = profile_begin_arguments("session.refresh-begin.proof");

    let started = harness
        .call_tool(&project, TOOL, arguments.clone())
        .await
        .unwrap_or_else(|error| panic!("{TOOL} invocation failed: {error}"));
    assert!(
        started.error.is_none(),
        "{TOOL} returned a JSON-RPC error: {:?}",
        started.error
    );
    let started_payload = effect_payload(
        started
            .result
            .as_ref()
            .unwrap_or_else(|| panic!("{TOOL} returned no result: {started:?}")),
    );
    assert_begin_payload(&started_payload, "started");

    let joined = harness
        .call_tool(&project, TOOL, arguments)
        .await
        .unwrap_or_else(|error| panic!("{TOOL} repeat invocation failed: {error}"));
    assert!(
        joined.error.is_none(),
        "{TOOL} repeat returned a JSON-RPC error: {:?}",
        joined.error
    );
    let joined_payload = effect_payload(
        joined
            .result
            .as_ref()
            .unwrap_or_else(|| panic!("{TOOL} repeat returned no result: {joined:?}")),
    );
    let mut expected_join = started_payload.clone();
    expected_join["outcome"] = json!("joined");
    assert_eq!(
        joined_payload, expected_join,
        "repeating begin must join the same refresh instead of starting another"
    );

    let mut refused = profile_begin_arguments("session.refresh-begin.refused");
    refused["scope"] = json!({ "kind": "user" });
    let refused = harness
        .call_tool(&project, TOOL, refused)
        .await
        .unwrap_or_else(|error| panic!("{TOOL} refusal invocation failed: {error}"));
    let error = refused
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("{TOOL} accepted an untyped scope: {refused:?}"));
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "tool execution failed: config error: invalid retained application request for tracedecay_session_refresh_begin: scope: unknown variant `user`, expected `project` or `profile`"
    );
    assert!(
        refused.result.is_none(),
        "an untyped scope must not return a tool result: {refused:?}"
    );

    harness.shutdown().await;
}

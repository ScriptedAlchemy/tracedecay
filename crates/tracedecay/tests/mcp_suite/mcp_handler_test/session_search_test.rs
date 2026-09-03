use crate::support::*;
#[cfg(feature = "test-transport")]
use crate::{common, fixture};
#[cfg(feature = "test-transport")]
use serde_json::Value;
use serde_json::json;

#[cfg(feature = "test-transport")]
use std::path::Path;
#[cfg(feature = "test-transport")]
use std::process::Command;
#[cfg(feature = "test-transport")]
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

#[cfg(feature = "test-transport")]
fn write_production_codex_rollout(home: &Path, project: &Path) {
    let sessions = home.join(".codex/sessions/2026/08/02");
    std::fs::create_dir_all(&sessions).expect("create isolated Codex sessions directory");
    let records = [
        json!({
            "timestamp": "2026-08-02T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": "production-codex-reopen",
                "cwd": project,
                "model": "gpt-5.6",
            },
        }),
        json!({
            "timestamp": "2026-08-02T00:00:01.000Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "Find the cobalt orchard scheduler migration",
            },
        }),
        json!({
            "timestamp": "2026-08-02T00:00:02.000Z",
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": "The cobalt orchard scheduler migration is ready for review",
            },
        }),
    ];
    let rollout = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        sessions.join("rollout-production-codex-reopen.jsonl"),
        format!("{rollout}\n"),
    )
    .expect("write isolated Codex rollout");
}

#[cfg(feature = "test-transport")]
async fn production_codex_message_search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    let response = harness
        .call_tool(
            project,
            "tracedecay_message_search",
            json!({
                "query": "cobalt orchard scheduler migration",
                "provider": "codex",
                "format": "json",
            }),
        )
        .await
        .expect("production message search invocation");
    let result = response.result.expect("production message search result");
    assert_ne!(
        result["isError"], true,
        "production message search returned an error: {result}"
    );
    let envelope: Value = serde_json::from_str(
        result["content"][0]["text"]
            .as_str()
            .expect("production message search JSON content"),
    )
    .expect("production message search JSON");
    // Retained tools respond with the full evidence envelope; the search
    // payload the assertions consume lives under `outcome.value.payload`.
    let payload = envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or(envelope);
    assert!(
        payload["results"].as_array().is_some_and(|results| {
            results.iter().any(|result| {
                result["message"]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("cobalt orchard scheduler migration"))
            })
        }),
        "production Codex message search was empty after completed ingest: {payload}"
    );
    payload
}

/// Same contract for `tracedecay_message_search`: invalid scope values fail
/// closed instead of broadening the search to every session.
#[tokio::test]
async fn message_search_rejects_invalid_scope() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    for invalid in ["everything", "", "parents"] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                json!({"query": "anything", "scope": invalid}),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains("scope")
                && err.contains("expected one of `all`, `parents_only`, `subagents_only`"),
            "unexpected error for scope {invalid:?}: {err}"
        );
    }

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"query": "anything", "provider": "unknown-agent"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("unknown session provider 'unknown-agent'"),
        "unexpected provider error: {err}"
    );

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"query": "anything", "message_type": "promptish"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("message_type")
            && err.contains("expected one of `all`, `direct_user`, `tool_result`"),
        "unexpected message_type error: {err}"
    );
}

/// `project_scope` is closed: the mounted retained owner serves only its own
/// project, so any other scope value fails closed as not-found-or-not-
/// authorized rather than silently degrading to a broader search.
#[tokio::test]
async fn message_search_rejects_unsupported_project_scope() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    for invalid in ["everything", "all", "registered", "all_registered"] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                json!({"query": "anything", "project_scope": invalid}),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains("not found or is not authorized"),
            "unexpected error for project_scope {invalid:?}: {err}"
        );
    }

    // The owner's own scope stays served: the closed enum rejects foreign
    // scopes without breaking the supported one.
    handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({"query": "anything", "project_scope": "project"}),
        None,
        None,
    )
    .await
    .expect("project-scoped message search must stay served");
}

/// Cross-project selection has exactly one spelling —
/// `project_selector.project_id` — so top-level aliases are refused with the
/// typed invalid-selector route error, a foreign registered id fails closed
/// as not-found-or-not-authorized, and a malformed selector is a decode error
/// naming the argument.
#[tokio::test]
async fn message_search_rejects_foreign_project_selectors() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"query": "anything", "project_id": "proj_0123456789abcdef"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("project_route_invalid_selector")
            && err.contains("is not a registered-project selector"),
        "unexpected error for top-level project_id: {err}"
    );

    // `project_path` is a semantic message-search argument, not a route
    // selector; the mounted owner refuses a foreign path closed.
    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"query": "anything", "project_path": "/some/foreign/path"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("not found or is not authorized"),
        "unexpected error for foreign project_path: {err}"
    );

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({
                "query": "anything",
                "project_selector": {"project_id": "proj_0123456789abcdef"}
            }),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("not found") || err.contains("not registered"),
        "a foreign registered id must fail closed: {err}"
    );

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"query": "anything", "project_selector": {"path": "/some/path"}}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("project_selector"),
        "malformed selectors must fail decode naming the argument: {err}"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_codex_hook_ingest_survives_message_search_reopen() {
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let root = test_temp_dir();
    let isolation = root.path().join("composition");
    let home = root.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let project = isolation.join("project");
    std::fs::create_dir_all(&project).expect("production composition project");
    fixture::write_indexed_fixture_sources(&project);
    let init = Command::new(common::git_program())
        .args(["init", "-q"])
        .current_dir(&project)
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    let add = Command::new(common::git_program())
        .args(["add", "."])
        .current_dir(&project)
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
            "production Codex transcript fixture",
        ])
        .current_dir(&project)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit must succeed");
    write_production_codex_rollout(&home, &project);

    let harness =
        ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
            &isolation,
            [project.clone()],
        )
        .await
        .expect("production composition harness");
    let response = harness
        .call_tool(
            &project,
            "tracedecay_hook_runtime",
            json!({"action": "ingest_transcript", "provider": "codex", "format": "json"}),
        )
        .await
        .expect("production Codex hook ingest invocation");
    let result = match response.result {
        Some(result) => result,
        None => panic!("production Codex hook ingest failed: {:?}", response.error),
    };
    let ingest: Value = serde_json::from_str(
        result["content"][0]["text"]
            .as_str()
            .expect("production Codex hook ingest JSON content"),
    )
    .expect("production Codex hook ingest JSON");
    assert_eq!(ingest["completed"], true, "{ingest}");
    // The composition's background Codex catch-up may admit the rollout
    // before the hook pass reaches it, in which case the hook truthfully
    // reports zero new bytes. Either path must leave the rollout durable and
    // searchable, which the retrieval assertions below verify directly.
    assert!(
        ingest["admission"]["status"]
            .as_str()
            .is_some_and(|status| status != "unavailable" && status != "unknown"),
        "real Codex hook ingest was refused: {ingest}"
    );

    let initial = production_codex_message_search(&harness, &project).await;
    assert!(
        initial["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "production Codex retrieval was empty: {initial}"
    );

    harness.shutdown().await;

    let restarted =
        ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
            &isolation,
            [project.clone()],
        )
        .await
        .expect("reopen production composition");
    let resumed = production_codex_message_search(&restarted, &project).await;
    assert!(
        resumed["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "reopened production Codex retrieval was empty: {resumed}"
    );
    restarted.shutdown().await;
}

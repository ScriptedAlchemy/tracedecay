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
use std::time::Duration;
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
async fn demand_production_code_index(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) {
    // The first search demands demand-driven publication; poll the same tool
    // until a generation binds and the fixture symbol is retrievable.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let response = harness
            .call_tool(
                project,
                "tracedecay_search",
                json!({"query": "helper", "limit": 1, "format": "json"}),
            )
            .await
            .expect("production code-index search");
        let result = response
            .result
            .expect("production code-index search result");
        assert_ne!(
            result["isError"], true,
            "production code-index search returned an error: {result}"
        );
        let payload: Value = serde_json::from_str(
            result["content"][0]["text"]
                .as_str()
                .expect("production code-index search JSON content"),
        )
        .expect("production code-index search JSON");
        let generation_bound = payload["code_generation"].as_str().is_some();
        let helper_found = payload["results"].as_array().is_some_and(|results| {
            results.iter().any(|result| {
                result["display"]["name"]
                    .as_str()
                    .is_some_and(|name| name == "helper")
            })
        });
        if generation_bound && helper_found {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "production code index did not publish after search demand: {payload}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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
    let payload: Value = serde_json::from_str(
        result["content"][0]["text"]
            .as_str()
            .expect("production message search JSON content"),
    )
    .expect("production message search JSON");
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
            err.contains("scope must be one of all, parents_only, subagents_only"),
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
        err.contains("message_type must be one of all, direct_user, tool_result"),
        "unexpected message_type error: {err}"
    );
}

// ---------------------------------------------------------------------------
// Regression: catch-up flag ordering — transcript_ingest_done must lag
/// `project_scope` is a closed enum: any value other than `all_registered`
/// must fail closed rather than silently degrade to a single-project search.
#[tokio::test]
async fn message_search_rejects_unsupported_project_scope() {
    let (cg, _env, _dir) = setup_empty_project().await;
    for invalid in ["everything", "all", "registered", "ALL_REGISTERED"] {
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
            err.contains("project_scope must be omitted or all_registered"),
            "unexpected error for project_scope {invalid:?}: {err}"
        );
    }
}

/// The `all_registered` scope cannot be paired with a single-project
/// selector.
#[tokio::test]
async fn message_search_rejects_all_registered_with_project_selector() {
    let (cg, _env, _dir) = setup_empty_project().await;
    for selector in [
        json!({"project_id": "proj_x"}),
        json!({"project_selector": {"path": "/some/path"}}),
        json!({"project_selector": {"project_path": "/some/path"}}),
    ] {
        let mut args = json!({"query": "anything", "project_scope": "all_registered"});
        args.as_object_mut()
            .unwrap()
            .extend(selector.as_object().unwrap().clone());
        let err = expect_tool_error(
            handle_tool_call(&cg, "tracedecay_message_search", args, None, None).await,
        );
        assert!(
            err.contains("project_route_invalid_selector"),
            "unexpected error for selector {selector}: {err}"
        );
    }

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({
                "query": "anything",
                "project_scope": "all_registered",
                "project_path": "/some/path"
            }),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("project_scope cannot be combined"),
        "message-search project_path remains a semantic filter: {err}"
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

    let harness = ProductionProjectCompositionHarnessV1::open(&isolation, [project.clone()])
        .await
        .expect("production composition harness");
    demand_production_code_index(&harness, &project).await;
    let response = harness
        .call_tool(
            &project,
            "tracedecay_hook_runtime",
            json!({"action": "ingest_transcript", "provider": "codex", "format": "json"}),
        )
        .await
        .expect("production Codex hook ingest invocation");
    let ingest: Value = serde_json::from_str(
        response
            .result
            .expect("production Codex hook ingest result")["content"][0]["text"]
            .as_str()
            .expect("production Codex hook ingest JSON content"),
    )
    .expect("production Codex hook ingest JSON");
    assert_eq!(ingest["completed"], true, "{ingest}");
    assert!(
        ingest["messages_upserted"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "real Codex hook ingest did not project messages: {ingest}"
    );

    let initial = production_codex_message_search(&harness, &project).await;
    assert!(
        initial["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "production Codex retrieval was empty: {initial}"
    );

    harness.shutdown().await;

    let restarted = ProductionProjectCompositionHarnessV1::open(&isolation, [project.clone()])
        .await
        .expect("reopen production composition");
    demand_production_code_index(&restarted, &project).await;
    let resumed = production_codex_message_search(&restarted, &project).await;
    assert!(
        resumed["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "reopened production Codex retrieval was empty: {resumed}"
    );
    restarted.shutdown().await;
}

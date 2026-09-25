use crate::support::*;
#[cfg(feature = "test-transport")]
use crate::{common, fixture};
#[cfg(feature = "test-transport")]
use serde_json::Value;
use serde_json::json;

#[cfg(feature = "test-transport")]
use std::path::{Path, PathBuf};
#[cfg(feature = "test-transport")]
use std::process::Command;
#[cfg(feature = "test-transport")]
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_domain::SessionId;
#[cfg(feature = "test-transport")]
use tracedecay_project::project::TraceDecay;
#[cfg(feature = "test-transport")]
use tracedecay_session_temporal_store::SessionTemporalStore;
#[cfg(feature = "test-transport")]
use tracedecay_sessions::admission::HostAdmissionScope;

/// Where a composed journey seeds host transcripts.
///
/// The composition reads them from its own isolated layout rather than from
/// the ambient `$HOME`, so a rollout written under the process home is
/// invisible to it.
#[cfg(feature = "test-transport")]
fn composed_transcript_home(isolation: &Path) -> PathBuf {
    std::fs::create_dir_all(isolation).expect("production composition root");
    ProductionProjectCompositionHarnessV1::transcript_source_home(isolation)
        .expect("composed transcript source home")
}

#[cfg(feature = "test-transport")]
fn write_production_codex_rollout(home: &Path, project: &Path) {
    write_production_codex_rollouts(home, project, 1);
}

#[cfg(feature = "test-transport")]
fn write_production_codex_rollouts(home: &Path, project: &Path, count: usize) {
    let sessions = home.join(".codex/sessions/2026/08/02");
    std::fs::create_dir_all(&sessions).expect("create isolated Codex sessions directory");
    for index in 0..count {
        let target = index + 1 == count;
        let records = [
            json!({
                "timestamp": "2026-08-02T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": format!("production-codex-reopen-{index:03}"),
                    "cwd": project,
                    "model": "gpt-5.6",
                },
            }),
            json!({
                "timestamp": "2026-08-02T00:00:01.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "user_message",
                    "message": if target {
                        "Find the cobalt orchard scheduler migration".to_owned()
                    } else {
                        format!("Inspect isolated session fixture {index:03}")
                    },
                },
            }),
            json!({
                "timestamp": "2026-08-02T00:00:02.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "agent_message",
                    "message": if target {
                        "The cobalt orchard scheduler migration is ready for review".to_owned()
                    } else {
                        format!("Isolated session fixture {index:03} is ready")
                    },
                },
            }),
        ];
        let rollout = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            sessions.join(format!("rollout-production-codex-reopen-{index:03}.jsonl")),
            format!("{rollout}\n"),
        )
        .expect("write isolated Codex rollout");
    }
}

#[cfg(feature = "test-transport")]
async fn production_codex_message_search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    // A `partial` generation is the store saying "still converging", the same
    // not-ready contract as `stale`: re-read it. Every other outcome answers
    // now, so an empty `complete_zero` still fails the assertions below.
    let payload = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let payload = production_codex_message_search_once(harness, project).await;
            if payload["outcome"] != "partial"
                || payload["results"]
                    .as_array()
                    .is_some_and(|results| !results.is_empty())
            {
                break payload;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("production Codex message search convergence deadline");
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
    assert!(
        payload["results"].as_array().is_some_and(|results| {
            results.iter().any(|result| {
                result["message"]["text"].as_str()
                    == Some("The cobalt orchard scheduler migration is ready for review")
            })
        }),
        "production Codex message search did not hydrate the exact assistant message: {payload}"
    );
    payload
}

#[cfg(feature = "test-transport")]
async fn production_codex_message_search_once(
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
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or(envelope)
}

#[cfg(feature = "test-transport")]
fn production_tool_payload(response: serde_json::Value) -> Value {
    let envelope: Value = serde_json::from_str(
        response["content"][0]["text"]
            .as_str()
            .expect("production tool JSON content"),
    )
    .expect("production tool JSON");
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or(envelope)
}

#[cfg(feature = "test-transport")]
async fn call_production_tool(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> Value {
    let response = harness
        .call_tool(project, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} invocation failed: {error}"));
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool} returned transport error: {:?}", response.error));
    assert_ne!(
        result["isError"], true,
        "{tool} returned an error: {result}"
    );
    production_tool_payload(result)
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

/// The request has one spelling per control: `since`/`until` for time bounds,
/// `require_fresh` for the freshness precondition, and `project_selector` for
/// cross-project reads. Removed spellings fail decode instead of being
/// silently ignored or reinterpreted.
#[tokio::test]
async fn message_search_rejects_removed_request_spellings() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    for (field, value) in [
        ("project_scope", json!("project")),
        ("time_from", json!(0)),
        ("time_to", json!(0)),
        ("catch_up", json!(false)),
    ] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                json!({"query": "anything", field: value}),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains(&format!("unknown field `{field}`")),
            "unexpected error for removed field {field:?}: {err}"
        );
    }

    handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({"query": "anything", "since": 0, "until": 1, "require_fresh": false}),
        None,
        None,
    )
    .await
    .expect("canonical spellings must stay served");
}

/// Cross-project selection has exactly one spelling,
/// `project_selector.project_id`, so top-level aliases are refused with the
/// typed invalid-selector route error, a foreign registered id fails closed
/// as not-found-or-not-authorized, and a malformed selector is a decode error
/// naming the argument.
#[tokio::test]
async fn message_search_rejects_foreign_project_selectors() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    for (alias, value) in [
        ("project_id", "proj_0123456789abcdef"),
        ("project_path", "/some/foreign/path"),
    ] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                json!({"query": "anything", alias: value}),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains("project_route_invalid_selector")
                && err.contains("is not a registered-project selector"),
            "unexpected error for top-level {alias}: {err}"
        );
    }

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
#[tokio::test]
async fn message_search_limit_one_hydrates_a_bounded_multi_session_corpus() {
    const SESSION_COUNT: usize = 4;
    const MESSAGES_PER_SESSION: usize = 4;

    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    let runtime = open_active_project_session_db(&cg).await;
    for session_index in 0..SESSION_COUNT {
        let session_id = format!("bounded-search-session-{session_index}");
        for message_index in 0..MESSAGES_PER_SESSION {
            let message_id = format!("bounded-search-message-{session_index}-{message_index}");
            let unique_target = if session_index == 0 && message_index == 0 {
                " bounded retained target"
            } else {
                ""
            };
            seed_temporal_lcm_session_message(
                &cg,
                &session_id,
                &message_id,
                format!(
                    "workflow correction repeated skill tool pattern from {session_id} in {message_id}{unique_target}"
                ),
                i64::try_from(session_index * MESSAGES_PER_SESSION + message_index + 1)
                    .expect("fixture ordinal"),
            )
            .await;
        }
        SessionTemporalStore::new(
            runtime
                .registered_database(HostAdmissionScope::Project)
                .expect("registered project session database"),
        )
        .materialize_pending_session_refresh_for_test(
            &SessionId::new(session_id).expect("fixture session id"),
        )
        .await
        .expect("materialize canonical temporal session");
    }

    let result = handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "bounded retained target",
            "limit": 1,
        }),
        None,
        None,
    )
    .await
    .expect("a limit-one retained search must fit the admitted budget");
    let envelope: Value =
        serde_json::from_str(extract_text(&result.value)).expect("retained evidence envelope");
    let payload = envelope
        .pointer("/outcome/value/payload")
        .unwrap_or(&envelope);
    let hits = payload["results"].as_array().expect("message-search hits");
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(
        hit["message"]["session_id"], hit["session"]["session_id"],
        "the hydrated message must come from its owning session"
    );
    let session_id = hit["session"]["session_id"]
        .as_str()
        .expect("owning session id");
    let message_id = hit["message"]["message_id"]
        .as_str()
        .expect("owning message id");
    let text = hit["message"]["text"]
        .as_str()
        .expect("canonical message text");
    assert!(text.contains(session_id), "{text}");
    assert!(text.contains(message_id), "{text}");

    let broad = handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "workflow correction repeated skill tool pattern",
            "limit": 1,
        }),
        None,
        None,
    )
    .await
    .expect("the bounded record read must resolve duplicate candidate lanes once");
    let broad_envelope: Value =
        serde_json::from_str(extract_text(&broad.value)).expect("broad retained evidence envelope");
    let broad_payload = broad_envelope
        .pointer("/outcome/value/payload")
        .unwrap_or(&broad_envelope);
    assert_eq!(
        broad_payload["results"]
            .as_array()
            .expect("broad message-search hits")
            .len(),
        1
    );

    // A literal without a maintained-index token cannot be admitted through
    // the FTS prefilter, so the exact channel is a measured empty set: the
    // search stays bounded and answers zero results, never a budget refusal.
    let tokenless = handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "🚨 :: --",
            "limit": 1,
        }),
        None,
        None,
    )
    .await
    .expect("an exact query without a maintained-index token is a valid empty search");
    let tokenless_envelope: Value = serde_json::from_str(extract_text(&tokenless.value))
        .expect("token-free retained evidence envelope");
    let tokenless_payload = tokenless_envelope
        .pointer("/outcome/value/payload")
        .unwrap_or(&tokenless_envelope);
    assert_eq!(
        tokenless_payload["outcome"], "complete_zero",
        "{tokenless_payload}"
    );
    assert_eq!(
        tokenless_payload["results"],
        Value::Array(Vec::new()),
        "{tokenless_payload}"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_codex_hook_ingest_survives_message_search_reopen() {
    let env_lock = lock_process_env().await;
    let root = test_temp_dir();
    let isolation = root.path().join("composition");
    let home = root.path().join("home");
    let _home_guard = HomeEnvGuard::set(&env_lock, &home);
    let transcripts = composed_transcript_home(&isolation);
    let project = isolation.join("project");
    std::fs::create_dir_all(&project).expect("production composition project");
    fixture::write_indexed_fixture_sources(&project);
    commit_worktree(&project, "production Codex transcript fixture");
    write_production_codex_rollout(&transcripts, &project);

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
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
    // The composition's background Codex catch-up may admit the rollout before
    // the hook pass reaches it, in which case the hook persists no new frames
    // and reports the rollout as an exact duplicate. Both terminals prove the
    // transcript is durable; `accepted_for_replay` proves neither a commit nor
    // a duplicate and must not be reported for a rollout that is on disk and
    // admitted. Either path must also leave the rollout searchable, which the
    // retrieval assertions below verify directly.
    assert!(
        matches!(
            ingest["admission"]["status"].as_str(),
            Some("committed" | "exact_duplicate")
        ),
        "real Codex hook ingest proved neither a commit nor a duplicate: {ingest}"
    );

    let initial = production_codex_message_search(&harness, &project).await;
    assert!(
        initial["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "production Codex retrieval was empty: {initial}"
    );

    let session_id = "production-codex-reopen-000";
    let selectors = json!({
        "scope": {"kind": "profile"},
        "session": {"id": session_id},
        "source": {"scope": "codex"},
        "target": {
            "temporal_mode": {"kind": "current"},
            "grain": "session",
            "frontier": {"observed_through": 0, "committed_through": 0}
        },
        "format": "json"
    });
    let begun = call_production_tool(
        &harness,
        &project,
        "tracedecay_session_refresh_begin",
        selectors.clone(),
    )
    .await;
    assert!(
        matches!(begun["outcome"].as_str(), Some("started" | "joined")),
        "{begun}"
    );
    let handle = begun["handle"].as_str().expect("opaque refresh handle");
    let operation_id = begun["operation_id"]
        .as_str()
        .expect("durable refresh operation id");
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let mut arguments = selectors.clone();
            arguments["handle"] = json!(handle);
            let status = call_production_tool(
                &harness,
                &project,
                "tracedecay_session_refresh_status",
                arguments,
            )
            .await;
            if status["outcome"] == "complete" {
                break status["receipt"].clone();
            }
            assert_eq!(status["outcome"], "running", "{status}");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session refresh completion deadline");
    assert_eq!(receipt["operation_id"], operation_id, "{receipt}");
    assert_eq!(receipt["state"], "complete", "{receipt}");

    let loaded = call_production_tool(
        &harness,
        &project,
        "tracedecay_lcm_load_session",
        json!({"provider": "codex", "session_id": session_id, "limit": 10, "format": "json"}),
    )
    .await;
    let message = loaded["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["content"] == "Find the cobalt orchard scheduler migration")
        })
        .expect("lossless canonical Codex prompt");
    assert_eq!(message["storage_kind"], "canonical_occurrence", "{message}");
    assert!(message["store_id"].is_null(), "{message}");
    let message_id = message["message_id"]
        .as_str()
        .expect("canonical message identity");
    let expanded = call_production_tool(
        &harness,
        &project,
        "tracedecay_lcm_expand",
        json!({
            "provider": "codex",
            "session_id": session_id,
            "target": {"kind": "canonical_occurrence", "message_id": message_id},
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        expanded["expansion"]["content"], "Find the cobalt orchard scheduler migration",
        "{expanded}"
    );
    assert_eq!(
        expanded["expansion"]["raw_message"]["message_id"], message_id,
        "{expanded}"
    );
    let described = call_production_tool(
        &harness,
        &project,
        "tracedecay_lcm_describe",
        json!({
            "provider": "codex",
            "session_id": session_id,
            "target": {"kind": "session"},
            "format": "json"
        }),
    )
    .await;
    let captured = "Find the cobalt orchard scheduler migration";
    let overview = described["description"]["raw_messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["message_id"] == message_id)
        })
        .unwrap_or_else(|| panic!("describe omitted the captured prompt: {described}"));
    assert_eq!(
        overview["content_range"]["total_chars"],
        captured.chars().count() as u64,
        "{overview}"
    );
    let preview = overview["content_preview"]
        .as_str()
        .unwrap_or_else(|| panic!("describe preview missing: {overview}"));
    assert!(
        preview.contains("cobalt orchard"),
        "describe preview was empty: {preview:?}"
    );

    harness.shutdown().await;

    let restarted = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
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

/// Isolation is total: a composed daemon serves exactly one transcript home.
///
/// The composition pins that home, so no route it serves may reach a rollout
/// that only exists under the ambient process `$HOME`. The hook ingest route
/// resolved the process home on its own, which let a harness journey observe
/// two different readers behind one daemon.
#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_hook_ingest_reads_only_the_pinned_transcript_home() {
    let env_lock = lock_process_env().await;
    let root = test_temp_dir();
    let isolation = root.path().join("composition");
    let home = root.path().join("home");
    let _home_guard = HomeEnvGuard::set(&env_lock, &home);
    let transcripts = composed_transcript_home(&isolation);
    let project = isolation.join("project");
    std::fs::create_dir_all(&project).expect("production composition project");
    fixture::write_indexed_fixture_sources(&project);
    commit_worktree(&project, "production Codex transcript fixture");
    write_production_codex_rollout(&home, &project);
    assert!(
        !transcripts.join(".codex/sessions").exists(),
        "the rollout must exist only under the process home for this journey"
    );

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        &isolation,
        [project.clone()],
    )
    .await
    .expect("production composition harness");
    let ingest = call_production_tool(
        &harness,
        &project,
        "tracedecay_hook_runtime",
        json!({"action": "ingest_transcript", "provider": "codex", "format": "json"}),
    )
    .await;
    assert_eq!(ingest["completed"], true, "{ingest}");
    assert_eq!(
        ingest["messages_upserted"], 0,
        "hook ingest swept the ambient process home: {ingest}"
    );
    assert_ne!(
        ingest["admission"]["status"], "committed",
        "hook ingest committed a rollout outside the pinned transcript home: {ingest}"
    );

    let search = production_codex_message_search_once(&harness, &project).await;
    assert_eq!(
        search["results"],
        Value::Array(Vec::new()),
        "a rollout under the process home reached the composed daemon: {search}"
    );
    harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_session_import_immediately_searches_canonical_message() {
    let env_lock = lock_process_env().await;
    let root = test_temp_dir();
    let isolation = root.path().join("composition");
    let home = root.path().join("home");
    let _home_guard = HomeEnvGuard::set(&env_lock, &home);
    // `sessions_import` is the composition's own pass, so it reads the
    // isolated transcript layout rather than the process home.
    let transcripts = composed_transcript_home(&isolation);
    let project = isolation.join("project");
    std::fs::create_dir_all(&project).expect("production composition project");
    fixture::write_indexed_fixture_sources(&project);
    let init = Command::new(common::git_program())
        .args(["init", "-q"])
        .current_dir(&project)
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    // More than one bounded transcript pass admits. The searchable message is
    // in the final source, so Complete proves the production continuation
    // worker consumed every durable Codex frontier before returning.
    write_production_codex_rollouts(&transcripts, &project, 33);

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        &isolation,
        [project.clone()],
    )
    .await
    .expect("production composition harness");
    let import = harness
        .call_tool(
            &project,
            "tracedecay_admin_cli",
            json!({"action": "sessions_import", "format": "json"}),
        )
        .await
        .expect("production transcript import invocation");
    let import_result = import.result.expect("production transcript import result");
    assert_ne!(import_result["isError"], true, "{import_result}");
    let accepted = production_tool_payload(import_result);
    let idempotency_key = accepted["idempotency_key"]
        .as_str()
        .expect("session import idempotency key")
        .to_owned();

    let completed = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let status = harness
                .call_tool(
                    &project,
                    "tracedecay_admin_cli",
                    json!({
                        "action": "sessions_sync_status",
                        "idempotency_key": idempotency_key,
                        "format": "json",
                    }),
                )
                .await
                .expect("production transcript import status");
            let result = status.result.expect("production transcript status result");
            assert_ne!(result["isError"], true, "{result}");
            let payload = production_tool_payload(result);
            if payload["status"] == "complete" {
                break payload;
            }
            assert!(
                matches!(payload["status"].as_str(), Some("accepted" | "joined")),
                "session import did not remain active: {payload}"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session import completion deadline");
    assert_eq!(completed["termination"], "completed", "{completed}");
    assert!(
        completed["stats"]["sessions_imported"]
            .as_u64()
            .is_some_and(|count| count > 0)
            && completed["stats"]["messages_imported"]
                .as_u64()
                .is_some_and(|count| count > 0),
        "{completed}"
    );
    assert!(
        completed["failure_codes"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{completed}"
    );
    assert!(
        completed["coverage"].as_array().is_some_and(|coverage| {
            coverage
                .iter()
                .all(|entry| entry["coverage"]["outcome"] == "complete")
        }),
        "{completed}"
    );

    production_codex_message_search(&harness, &project).await;
    harness.shutdown().await;
}

/// `tracedecay_message_search` reads already-admitted messages through MCP
/// `tools/call`. A query that names a seeded message returns that message's
/// text, id, session, provider, and role. Those observations carry an unknown
/// valid time, so the hit is partial: one omitted record, coverage `unknown`
/// 1 and `visible` 0, not a complete answer. A query that matches nothing, the
/// wrong provider, an assistant message filtered as a tool result, and goals
/// with no goals are empty complete answers. Omitting `query` outside goals
/// mode, and naming an unknown provider, are typed invalid-request refusals.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn message_search_returns_literal_seeded_messages() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;

    seed_temporal_lcm_session_message(
        &cg,
        "proof-plum-session",
        "proof-plum-message",
        "The plum quartz regulator holds at 41 degrees",
        1,
    )
    .await;
    seed_temporal_lcm_session_message(
        &cg,
        "proof-amber-session",
        "proof-amber-message",
        "The amber lattice stays closed",
        1,
    )
    .await;
    seed_temporal_lcm_session_message_for_provider(
        &cg,
        "codex",
        "proof-orchid-session",
        "proof-orchid-message",
        "The orchid spool tension is 12 newtons",
        1,
    )
    .await;
    seed_temporal_lcm_tool_result_message(
        &cg,
        "proof-zinc-session",
        "proof-zinc-message",
        "zinc spindle torque reading 17",
        1,
    )
    .await;
    for session_id in [
        "proof-plum-session",
        "proof-amber-session",
        "proof-orchid-session",
        "proof-zinc-session",
    ] {
        materialize_proof_session(&cg, session_id).await;
    }

    let plum = message_search_payload(
        &cg,
        json!({
            "query": "plum quartz regulator",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(plum["query"], "plum quartz regulator");
    assert_eq!(plum["outcome"], "partial");
    assert_eq!(plum["status"], "partial");
    assert_eq!(plum["count"], 1);
    assert_eq!(plum["omitted"], 1);
    assert_eq!(plum["provider"], "all");
    assert_eq!(plum["requested_provider"], Value::Null);
    assert_eq!(plum["scope"], "all");
    assert_eq!(plum["message_type"], "all");
    assert_eq!(plum["goals"], false);
    assert_eq!(plum["require_fresh"], false);
    assert_eq!(plum["include_subagents"], true);
    assert_eq!(plum["refresh_required"], false);
    assert_eq!(plum["store_scope"], "project");
    assert_eq!(
        plum["temporal"]["coverage"],
        json!({"hidden": 0, "redacted": 0, "unknown": 1, "visible": 0})
    );
    assert_eq!(plum["temporal"]["freshness"], json!({"state": "fresh"}));
    assert_eq!(plum["results"].as_array().map(Vec::len), Some(1));
    let plum_hit = &plum["results"][0];
    assert_eq!(
        plum_hit["message"]["text"],
        "The plum quartz regulator holds at 41 degrees"
    );
    assert_eq!(plum_hit["message"]["message_id"], "proof-plum-message");
    assert_eq!(plum_hit["message"]["session_id"], "proof-plum-session");
    assert_eq!(plum_hit["message"]["provider"], "cursor");
    assert_eq!(plum_hit["message"]["role"], "assistant");
    assert_eq!(plum_hit["message"]["model"], "test-model");
    assert_eq!(plum_hit["session"]["session_id"], "proof-plum-session");
    assert_eq!(plum_hit["session"]["provider"], "cursor");
    assert_eq!(plum_hit["session"]["is_subagent"], false);

    let amber = message_search_payload(
        &cg,
        json!({
            "query": "amber lattice",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(amber["outcome"], "partial");
    assert_eq!(amber["status"], "partial");
    assert_eq!(amber["count"], 1);
    assert_eq!(amber["omitted"], 1);
    assert_eq!(
        amber["results"][0]["message"]["text"],
        "The amber lattice stays closed"
    );
    assert_eq!(
        amber["results"][0]["message"]["message_id"],
        "proof-amber-message"
    );
    assert_eq!(
        amber["results"][0]["message"]["session_id"],
        "proof-amber-session"
    );
    assert_eq!(amber["results"][0]["message"]["provider"], "cursor");
    assert_eq!(amber["results"][0]["message"]["role"], "assistant");

    let miss = message_search_payload(
        &cg,
        json!({
            "query": "no such nautilus phrase",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(miss["query"], "no such nautilus phrase");
    assert_eq!(miss["outcome"], "complete_zero");
    assert_eq!(miss["status"], "ok");
    assert_eq!(miss["count"], 0);
    assert_eq!(miss["results"], json!([]));
    assert_eq!(miss["provider"], "all");
    assert_eq!(miss["refresh_required"], false);
    assert_eq!(
        miss["temporal"]["coverage"],
        json!({"hidden": 0, "redacted": 0, "unknown": 0, "visible": 0})
    );

    let cursor_only = message_search_payload(
        &cg,
        json!({
            "query": "orchid spool tension",
            "provider": "cursor",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(cursor_only["provider"], "cursor");
    assert_eq!(cursor_only["requested_provider"], "cursor");
    assert_eq!(cursor_only["outcome"], "complete_zero");
    assert_eq!(cursor_only["count"], 0);
    assert_eq!(cursor_only["results"], json!([]));

    let codex_only = message_search_payload(
        &cg,
        json!({
            "query": "orchid spool tension",
            "provider": "codex",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(codex_only["provider"], "codex");
    assert_eq!(codex_only["requested_provider"], "codex");
    assert_eq!(codex_only["outcome"], "partial");
    assert_eq!(codex_only["status"], "partial");
    assert_eq!(codex_only["count"], 1);
    assert_eq!(codex_only["omitted"], 1);
    assert_eq!(
        codex_only["results"][0]["message"]["text"],
        "The orchid spool tension is 12 newtons"
    );
    assert_eq!(
        codex_only["results"][0]["message"]["message_id"],
        "proof-orchid-message"
    );
    assert_eq!(codex_only["results"][0]["message"]["provider"], "codex");
    assert_eq!(codex_only["results"][0]["message"]["role"], "assistant");
    assert_eq!(
        codex_only["results"][0]["message"]["session_id"],
        "proof-orchid-session"
    );
    assert_eq!(codex_only["results"][0]["session"]["provider"], "codex");

    let not_a_tool = message_search_payload(
        &cg,
        json!({
            "query": "plum quartz regulator",
            "message_type": "tool_result",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(not_a_tool["message_type"], "tool_result");
    assert_eq!(not_a_tool["outcome"], "complete_zero");
    assert_eq!(not_a_tool["count"], 0);
    assert_eq!(not_a_tool["results"], json!([]));

    let tool_hit = message_search_payload(
        &cg,
        json!({
            "query": "zinc spindle torque",
            "message_type": "tool_result",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(tool_hit["message_type"], "tool_result");
    assert_eq!(tool_hit["outcome"], "partial");
    assert_eq!(tool_hit["status"], "partial");
    assert_eq!(tool_hit["count"], 1);
    assert_eq!(tool_hit["omitted"], 1);
    assert_eq!(
        tool_hit["results"][0]["message"]["text"],
        "zinc spindle torque reading 17"
    );
    assert_eq!(
        tool_hit["results"][0]["message"]["message_id"],
        "proof-zinc-message"
    );
    assert_eq!(tool_hit["results"][0]["message"]["role"], "tool");
    assert_eq!(tool_hit["results"][0]["message"]["model"], Value::Null);
    assert_eq!(tool_hit["results"][0]["message"]["provider"], "cursor");
    assert_eq!(
        tool_hit["results"][0]["message"]["session_id"],
        "proof-zinc-session"
    );

    let goals = message_search_payload(
        &cg,
        json!({
            "goals": true,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(goals["goals"], true);
    assert_eq!(goals["query"], "");
    assert_eq!(goals["outcome"], "complete_zero");
    assert_eq!(goals["status"], "ok");
    assert_eq!(goals["count"], 0);
    assert_eq!(goals["results"], json!([]));

    let missing_query = refusal_problem(&expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({"format": "json"}),
            None,
            None,
        )
        .await,
    ));
    assert_eq!(missing_query["kind"], "invalid_request");
    assert_eq!(
        missing_query["code"],
        "application.retained.invalid-request"
    );
    assert_eq!(
        missing_query["message"],
        "The retained operation request is invalid."
    );
    assert_eq!(
        missing_query["diagnostic"]["code"],
        "application.retained.invalid-request"
    );
    assert_eq!(
        missing_query["diagnostic"]["message"],
        "The retained operation request is invalid."
    );
    assert_eq!(missing_query["retry"], "never");
    assert_eq!(missing_query["legal_actions"], json!(["correct_request"]));

    let unknown_provider = refusal_problem(&expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_message_search",
            json!({
                "query": "plum quartz regulator",
                "provider": "unknown-agent",
                "format": "json",
            }),
            None,
            None,
        )
        .await,
    ));
    assert_eq!(unknown_provider["kind"], "invalid_request");
    assert_eq!(
        unknown_provider["code"],
        "application.retained.message-search-provider-invalid"
    );
    assert_eq!(
        unknown_provider["message"],
        "unknown session provider 'unknown-agent' (expected all, cursor, claude, codex, vibe, cline, roo-code, kilo, kiro, kimi, opencode, hermes, or pi)"
    );
    assert_eq!(
        unknown_provider["diagnostic"]["message"],
        "unknown session provider 'unknown-agent' (expected all, cursor, claude, codex, vibe, cline, roo-code, kilo, kiro, kimi, opencode, hermes, or pi)"
    );
    assert_eq!(unknown_provider["retry"], "never");
    assert_eq!(
        unknown_provider["legal_actions"],
        json!(["correct_request"])
    );
}

#[cfg(feature = "test-transport")]
async fn materialize_proof_session(cg: &TraceDecay, session_id: &str) {
    let runtime = open_active_project_session_db(cg).await;
    SessionTemporalStore::new(
        runtime
            .registered_database(HostAdmissionScope::Project)
            .expect("registered project session database"),
    )
    .materialize_pending_session_refresh_for_test(
        &SessionId::new(session_id).expect("fixture session id"),
    )
    .await
    .expect("materialize canonical temporal session");
}

#[cfg(feature = "test-transport")]
async fn message_search_payload(cg: &TraceDecay, arguments: Value) -> Value {
    let result = handle_tool_call(cg, "tracedecay_message_search", arguments, None, None)
        .await
        .expect("tracedecay_message_search MCP call");
    let envelope = extract_json(&result.value);
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or(envelope)
}

#[cfg(feature = "test-transport")]
fn refusal_problem(error: &str) -> Value {
    const MARKER: &str = "answered with a retained refusal: ";
    let json = error
        .split_once(MARKER)
        .unwrap_or_else(|| panic!("expected a retained refusal, got {error}"))
        .1;
    let envelope: Value = serde_json::from_str(json)
        .unwrap_or_else(|parse_error| panic!("{parse_error} in retained refusal: {json}"));
    envelope
        .pointer("/Err/problem")
        .cloned()
        .or_else(|| envelope.get("problem").cloned())
        .unwrap_or_else(|| panic!("retained refusal has no problem record: {envelope}"))
}

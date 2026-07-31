//! Codex thread-goal and workflow-lifecycle ingestion: goal rows with dedupe,
//! latest-status surfacing, observation projection, and secret sanitization.

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::sessions::SessionProvider;
use tracedecay::sessions::codex::CodexSource;
use tracedecay_store::ObservationProjectionStore;
use tracedecay_store::ObservationReplayRequest;

use crate::codex::{
    write_codex_rollout_with_goal_context, write_codex_rollout_with_structured_events, write_jsonl,
};
use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    ProjectSessionTestRuntime, mark_test_project, open_project_session_db,
};
use crate::support::{init_git_repo, setup};

/// Writes a Codex rollout carrying a `thread_goal_updated` lifecycle: an
/// initial `active` goal, an identical follow-up (only token/time drift — must
/// be deduped), then a `paused` transition (a distinct state — must keep its
/// own row).
fn write_codex_rollout_with_goal_events(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/02");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-02T00-00-00-{session}.jsonl"));
    let mut goal_events: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/codex/thread_goal_updates.input.json"
    ))
    .expect("checked-in Codex goal update sequence");
    for event in &mut goal_events {
        event["payload"]["threadId"] = serde_json::Value::String(session.to_owned());
        event["payload"]["goal"]["threadId"] = serde_json::Value::String(session.to_owned());
    }
    let mut records = vec![
        serde_json::json!({
            "timestamp": "2026-01-02T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-02T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "start the overhaul"}
        }),
    ];
    records.append(&mut goal_events);
    records.push(serde_json::json!({
        "timestamp": "2026-01-02T00:00:05.000Z",
        "type": "event_msg",
        "payload": {"type": "agent_message", "message": "paused for review"}
    }));
    write_jsonl(&path, &records);
    path
}

async fn codex_observation_json_blobs(runtime: &ProjectSessionTestRuntime) -> Vec<String> {
    runtime
        .runtime()
        .replay_observations(
            HostAdmissionScope::Project,
            ObservationReplayRequest::new(0, 1_000).unwrap(),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| serde_json::to_string(row.observation()).unwrap())
        .collect()
}

async fn codex_observation_count(runtime: &ProjectSessionTestRuntime) -> u64 {
    runtime
        .runtime()
        .project_observation_table_count_for_test("observations")
        .await
        .unwrap()
}

async fn codex_workflow_fact_rows(
    runtime: &ProjectSessionTestRuntime,
) -> Vec<(String, Option<String>, Option<String>)> {
    runtime
        .runtime()
        .project_workflow_fact_rows_for_test()
        .await
        .unwrap()
}

#[tokio::test]
async fn recent_session_goals_surfaces_latest_status_per_session() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_goal_events(&home, &project, "codex-goal-events");

    mark_test_project(&project);
    let runtime = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    runtime
        .runtime()
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();

    let goals = runtime
        .runtime()
        .recent_project_session_goals_for_test(project.to_string_lossy().as_ref(), 10)
        .await
        .unwrap();
    // One row per session: the latest lifecycle state (paused).
    assert_eq!(goals.len(), 1);
    let goal = &goals[0];
    assert_eq!(goal.session.session_id, "codex-goal-events");
    assert_eq!(goal.message.kind.as_deref(), Some("goal"));
    assert_eq!(
        goal.message.text,
        "phlogiston pipeline rollout and verification"
    );
    let meta: serde_json::Value =
        serde_json::from_str(goal.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["status"], "paused");
    assert_eq!(meta["updated_at"], 1_782_880_661i64);

    // Re-ingest must be idempotent (upsert keyed by message_id): still one goal.
    runtime
        .runtime()
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();
    let goals_again = runtime
        .runtime()
        .recent_project_session_goals_for_test(project.to_string_lossy().as_ref(), 10)
        .await
        .unwrap();
    assert_eq!(goals_again.len(), 1);
}
#[tokio::test]
async fn codex_thread_goal_events_ingested_as_goal_rows_with_dedupe() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_goal_events(&home, &project, "codex-goal-events");

    mark_test_project(&project);
    let runtime = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let stats = runtime
        .runtime()
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();
    // user_message + agent_message + three goal transitions. The drift-only
    // active repeat is deduped; objective and status transitions remain.
    assert_eq!(stats.messages_upserted, 5);

    // Both distinct goal states are searchable by their shared objective text.
    let hits = runtime
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "phlogiston pipeline",
            10,
        )
        .await;
    let goal_hits: Vec<_> = hits
        .iter()
        .filter(|hit| hit.message.kind.as_deref() == Some("goal"))
        .collect();
    assert_eq!(
        goal_hits.len(),
        3,
        "objective/status transitions kept, drift deduped"
    );
    let mut statuses: Vec<String> = goal_hits
        .iter()
        .filter_map(|hit| {
            let meta: serde_json::Value =
                serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).ok()?;
            meta.get("status")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .collect();
    statuses.sort();
    assert_eq!(
        statuses,
        vec![
            "active".to_string(),
            "active".to_string(),
            "paused".to_string()
        ]
    );
    for hit in &goal_hits {
        assert_eq!(hit.message.role, "system");
        assert!(matches!(
            hit.message.text.as_str(),
            "phlogiston pipeline overhaul and reconciliation"
                | "phlogiston pipeline rollout and verification"
        ));
        let meta: serde_json::Value =
            serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["source"], "codex_thread_goal");
        assert_eq!(meta["source_event"], "thread_goal_updated");
        assert_eq!(meta["thread_id"], "codex-goal-events");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_workflow_lifecycle_goal_plan_task_persist_on_production_observation_path() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    // Fixture-backed rollouts already checked in via write helpers.
    write_codex_rollout_with_goal_events(&home, &project, "codex-wf-goal");
    write_codex_rollout_with_structured_events(&home, &project, "codex-wf-structured");
    write_codex_rollout_with_goal_context(&home, &project, "codex-wf-goal-context");

    let runtime = open_project_session_db(&project).await.unwrap();
    let _ = runtime
        .runtime()
        .ingest_project_provider_for_test(&project, Some(SessionProvider::Codex))
        .await
        .unwrap();

    let blobs = codex_observation_json_blobs(&runtime).await;
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"goal\"")
                && blob.contains("phlogiston pipeline overhaul")),
        "nested thread_goal_updated must persist WorkflowLifecycle Goal"
    );
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"plan\"")
                && blob.contains("sweep telemetry")
                && blob.contains("call-plan-1")),
        "update_plan arguments must persist on WorkflowLifecycle Plan"
    );
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"task\"")
                && blob.contains("task_complete")
                && !blob.contains("last_agent_message")),
        "exact task_complete must persist without last_agent_message"
    );
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"tool_invocation\"")
                && blob.contains("\"name\":\"update_plan\"")
                && blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"plan\"")
                && blob.contains("sweep telemetry")),
        "update_plan must co-locate ToolInvocation + WorkflowLifecycle Plan"
    );
    assert!(
        blobs.iter().any(|blob| {
            blob.contains("ensure all provider session messages are ingested")
                && blob.contains("\"kind\":\"message\"")
                && !blob.contains("\"semantic_kind\":\"goal\"")
        }),
        "goal-context response_item must remain Message-only (no WorkflowLifecycle Goal)"
    );
    assert!(
        !blobs
            .iter()
            .any(|blob| blob.contains("task_completed") || blob.contains("task_failed")),
        "lookalike task_completed/task_failed must not appear as lifecycle facts"
    );

    let workflow_rows = codex_workflow_fact_rows(&runtime).await;
    assert!(
        workflow_rows
            .iter()
            .any(|(kind, status, _)| kind == "goal" && status.as_deref() == Some("paused")),
        "projected goal status must carry native paused transition; got {workflow_rows:?}"
    );
    assert!(
        workflow_rows.iter().any(|(kind, _, _)| kind == "plan"),
        "projected plan row missing; got {workflow_rows:?}"
    );
    assert!(
        workflow_rows
            .iter()
            .any(|(kind, _, state)| kind == "task" && state.as_deref() == Some("task_complete")),
        "projected task_complete row missing; got {workflow_rows:?}"
    );

    // Exact-duplicate redelivery is a durable no-op (content-addressed ids).
    let observations_before = codex_observation_count(&runtime).await;
    let workflow_before = runtime
        .runtime()
        .project_observation_table_count_for_test("observation_workflow_facts")
        .await
        .unwrap();
    let _ = runtime
        .runtime()
        .ingest_project_provider_for_test(&project, Some(SessionProvider::Codex))
        .await
        .unwrap();
    assert_eq!(codex_observation_count(&runtime).await, observations_before);
    assert_eq!(
        runtime
            .runtime()
            .project_observation_table_count_for_test("observation_workflow_facts")
            .await
            .unwrap(),
        workflow_before
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_goal_token_ticks_retain_raw_observations_and_dedupe_projected_goal_state() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    // Checked-in production sequence: active → token/time tick → objective
    // transition → paused.
    write_codex_rollout_with_goal_events(&home, &project, "codex-goal-dedupe");

    let runtime = open_project_session_db(&project).await.unwrap();
    let _ = runtime
        .runtime()
        .ingest_project_provider_for_test(&project, Some(SessionProvider::Codex))
        .await
        .unwrap();

    let blobs = codex_observation_json_blobs(&runtime).await;
    let goal_observations = blobs
        .iter()
        .filter(|blob| {
            blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"goal\"")
        })
        .count();
    assert_eq!(
        goal_observations, 4,
        "all goal updates, including the token/time tick, must persist raw"
    );

    let goal_rows: Vec<_> = codex_workflow_fact_rows(&runtime)
        .await
        .into_iter()
        .filter(|(kind, _, _)| kind == "goal")
        .collect();
    assert_eq!(
        goal_rows.len(),
        3,
        "projected goal state must keep transitions only; got {goal_rows:?}"
    );
    assert_eq!(goal_rows[0].1.as_deref(), Some("active"));
    assert_eq!(goal_rows[1].1.as_deref(), Some("active"));
    assert_eq!(goal_rows[2].1.as_deref(), Some("paused"));

    let goals = runtime
        .runtime()
        .recent_project_session_goals_for_test(project.to_string_lossy().as_ref(), 10)
        .await
        .unwrap();
    assert_eq!(goals.len(), 1);
    assert_eq!(
        goals[0].message.text,
        "phlogiston pipeline rollout and verification"
    );
    let meta: serde_json::Value =
        serde_json::from_str(goals[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["status"], "paused");

    let observations = runtime
        .runtime()
        .project_observation_table_count_for_test("observations")
        .await
        .unwrap();
    let store = runtime
        .runtime()
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    loop {
        if store
            .rebuild_projection(observations)
            .await
            .unwrap()
            .is_complete()
        {
            break;
        }
    }

    let goal_rows_rebuilt: Vec<_> = codex_workflow_fact_rows(&runtime)
        .await
        .into_iter()
        .filter(|(kind, _, _)| kind == "goal")
        .collect();
    assert_eq!(goal_rows_rebuilt.len(), 3);
    assert_eq!(goal_rows_rebuilt[0].1.as_deref(), Some("active"));
    assert_eq!(goal_rows_rebuilt[1].1.as_deref(), Some("active"));
    assert_eq!(goal_rows_rebuilt[2].1.as_deref(), Some("paused"));

    // Restart reopen: latest goal remains paused with objective text.
    drop(runtime);
    let reopened = open_project_session_db(&project).await.unwrap();
    let goals_again = reopened
        .runtime()
        .recent_project_session_goals_for_test(project.to_string_lossy().as_ref(), 10)
        .await
        .unwrap();
    assert_eq!(goals_again.len(), 1);
    assert_eq!(
        goals_again[0].message.text,
        "phlogiston pipeline rollout and verification"
    );
    let meta_again: serde_json::Value =
        serde_json::from_str(goals_again[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta_again["status"], "paused");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_workflow_lifecycle_secret_content_is_sanitized_before_persistence() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    const SECRET: &str = "AKIACODEXLIFECYCLE01";
    let dir = home.join(".codex/sessions/2026/01/04");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-04T00-00-00-codex-wf-secret.jsonl");
    // Nested goal shape from write_codex_rollout_with_goal_events, with an
    // exact credential pattern embedded in the evidenced objective field.
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-04T00:00:00.000Z",
                "type": "session_meta",
                "payload": {"id": "codex-wf-secret", "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-04T00:00:01.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "thread_goal_updated",
                    "threadId": "codex-wf-secret",
                    "goal": {
                        "threadId": "codex-wf-secret",
                        "objective": format!("rotate access key {SECRET}"),
                        "status": "active",
                        "tokensUsed": 1,
                        "timeUsedSeconds": 1,
                        "createdAt": 1_783_500_000i64,
                        "updatedAt": 1_783_500_001i64
                    }
                }
            }),
        ],
    );

    let runtime = open_project_session_db(&project).await.unwrap();
    let _ = runtime
        .runtime()
        .ingest_project_provider_for_test(&project, Some(SessionProvider::Codex))
        .await
        .unwrap();

    let blobs = codex_observation_json_blobs(&runtime).await;
    let joined = blobs.join("\n");
    assert!(
        joined.contains("workflow_lifecycle"),
        "secret-bearing goal must still admit a WorkflowLifecycle fact"
    );
    assert!(
        !joined.contains(SECRET),
        "secret-bearing goal content must be sanitized before observation persistence"
    );
}

use super::*;
use crate::runtime::git_correlation::{
    SpanObservation, SpanSource, ensure_git_correlation_schema, record_span_observation,
};
use crate::runtime::workflow_index::WorkflowScopeFilter;

async fn mem_conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    db.connect().unwrap()
}

fn sample_run(run_id: &str, parent: &str) -> WorkflowRun {
    WorkflowRun {
        run_id: run_id.to_string(),
        parent_session_id: parent.to_string(),
        name: Some("triggering-evals".to_string()),
        description: Some("mine + run + score".to_string()),
        phase_json: Some(r#"[{"title":"Mine"},{"title":"Run"}]"#.to_string()),
        status: WorkflowStatus::Completed,
        started_ts: Some(1_700_000_000),
        ended_ts: Some(1_700_000_900),
        result_summary: Some("36 scenarios, 45 runs".to_string()),
        agent_count: 11,
    }
}

#[test]
fn status_from_disk_folds_known_and_unknown() {
    assert_eq!(
        WorkflowStatus::from_disk("completed"),
        WorkflowStatus::Completed
    );
    assert_eq!(WorkflowStatus::from_disk("done"), WorkflowStatus::Completed);
    assert_eq!(
        WorkflowStatus::from_disk("in_progress"),
        WorkflowStatus::Running
    );
    assert_eq!(WorkflowStatus::from_disk("blocked"), WorkflowStatus::Failed);
    assert_eq!(
        WorkflowStatus::from_disk("timed_out"),
        WorkflowStatus::Failed
    );
    assert_eq!(WorkflowStatus::from_disk("banana"), WorkflowStatus::Unknown);
}

#[tokio::test]
async fn queries_are_empty_before_schema_exists() {
    let conn = mem_conn().await;
    // No tables yet: readers must fail-open to empty/None.
    assert!(!tables_present(&conn).await.unwrap());
    assert!(
        runs_for_session(&conn, "sess", 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(run_for_id(&conn, "wf_x").await.unwrap().is_none());
    assert!(agents_for_run(&conn, "wf_x", 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn upsert_is_idempotent_and_updates_mutable_columns() {
    let conn = mem_conn().await;
    ensure_workflow_index_schema(&conn).await.unwrap();
    assert!(tables_present(&conn).await.unwrap());

    let mut run = sample_run("wf_alpha", "sess-1");
    run.status = WorkflowStatus::Running;
    run.result_summary = None;
    upsert_run(&conn, &run).await.unwrap();

    // Re-ingest the same run once it finished: overwrite, don't duplicate.
    let finished = sample_run("wf_alpha", "sess-1");
    upsert_run(&conn, &finished).await.unwrap();

    let all = runs_for_session(&conn, "sess-1", 10).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0], finished);
    assert_eq!(all[0].status, WorkflowStatus::Completed);
    assert_eq!(
        all[0].result_summary.as_deref(),
        Some("36 scenarios, 45 runs")
    );

    assert_eq!(run_for_id(&conn, "wf_alpha").await.unwrap(), Some(finished));
    assert!(run_for_id(&conn, "wf_missing").await.unwrap().is_none());
}

#[tokio::test]
async fn empty_run_id_is_rejected() {
    let conn = mem_conn().await;
    ensure_workflow_index_schema(&conn).await.unwrap();
    let mut run = sample_run("   ", "sess");
    run.run_id = "   ".to_string();
    let err = upsert_run(&conn, &run).await.unwrap_err();
    assert!(matches!(err, WorkflowIndexError::InvalidArgument(_)));
}

#[tokio::test]
async fn runs_for_session_orders_newest_first_and_scopes_by_parent() {
    let conn = mem_conn().await;
    ensure_workflow_index_schema(&conn).await.unwrap();

    let mut old = sample_run("wf_old", "sess-1");
    old.started_ts = Some(1_000);
    let mut new = sample_run("wf_new", "sess-1");
    new.started_ts = Some(2_000);
    let other = sample_run("wf_other", "sess-2");
    upsert_run(&conn, &old).await.unwrap();
    upsert_run(&conn, &new).await.unwrap();
    upsert_run(&conn, &other).await.unwrap();

    let s1 = runs_for_session(&conn, "sess-1", 10).await.unwrap();
    let ids: Vec<&str> = s1.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(ids, vec!["wf_new", "wf_old"]);

    let s2 = runs_for_session(&conn, "sess-2", 10).await.unwrap();
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].run_id, "wf_other");
}

#[tokio::test]
async fn agents_upsert_and_order_within_run() {
    let conn = mem_conn().await;
    ensure_workflow_index_schema(&conn).await.unwrap();
    upsert_run(&conn, &sample_run("wf_a", "sess"))
        .await
        .unwrap();

    let second = WorkflowAgent {
        run_id: "wf_a".to_string(),
        agent_label: "run:batch2".to_string(),
        agent_id: "a222".to_string(),
        phase: Some("Run".to_string()),
        transcript_path: Some("/tmp/agent-a222.jsonl".to_string()),
        agent_session_id: None,
        status: WorkflowStatus::Completed,
        model: Some("claude-fable-5".to_string()),
        tokens: 4200,
        started_ts: Some(2_000),
        ended_ts: Some(2_500),
    };
    let first = WorkflowAgent {
        agent_label: "mine:claude".to_string(),
        agent_id: "a111".to_string(),
        phase: Some("Mine".to_string()),
        started_ts: Some(1_000),
        ..second.clone()
    };
    upsert_agent(&conn, &second).await.unwrap();
    upsert_agent(&conn, &first).await.unwrap();
    // Idempotent re-ingest of the first agent.
    upsert_agent(&conn, &first).await.unwrap();

    let agents = agents_for_run(&conn, "wf_a", 10).await.unwrap();
    let labels: Vec<&str> = agents.iter().map(|a| a.agent_label.as_str()).collect();
    assert_eq!(labels, vec!["mine:claude", "run:batch2"]);
    assert_eq!(agents[0].tokens, 4200);
    assert_eq!(agents[1].model.as_deref(), Some("claude-fable-5"));
}

#[test]
fn workflow_scope_exists_predicate_includes_run_and_optional_label() {
    let run_only = WorkflowScopeFilter {
        run_id: "wf_alpha".to_string(),
        agent_label: None,
    };
    let (sql, params) = workflow_scope_exists_predicate(&run_only, "m.source_path", "m.session_id");
    assert!(sql.contains("workflow_agents"));
    assert!(sql.contains("wa.run_id = ?1"));
    assert!(sql.contains("wa.transcript_path = m.source_path"));
    assert!(sql.contains("wa.agent_session_id = m.session_id"));
    assert!(!sql.contains("agent_label"));
    assert_eq!(params.len(), 1);
    assert!(matches!(&params[0], libsql::Value::Text(id) if id == "wf_alpha"));

    let narrowed = WorkflowScopeFilter {
        run_id: "wf_beta".to_string(),
        agent_label: Some("mine:claude".to_string()),
    };
    let (sql, params) = workflow_scope_exists_predicate(&narrowed, "m.source_path", "m.session_id");
    assert!(sql.contains("workflow_agents"));
    assert!(sql.contains("wa.agent_label = ?2"));
    assert_eq!(params.len(), 2);
    assert!(matches!(&params[1], libsql::Value::Text(label) if label == "mine:claude"));
}

#[tokio::test]
async fn runs_for_git_scope_joins_through_parent_session_spans() {
    let conn = mem_conn().await;
    ensure_git_correlation_schema(&conn).await.unwrap();
    ensure_workflow_index_schema(&conn).await.unwrap();

    // A run owned by sess-branch, another owned by sess-other.
    upsert_run(&conn, &sample_run("wf_on_branch", "sess-branch"))
        .await
        .unwrap();
    upsert_run(&conn, &sample_run("wf_elsewhere", "sess-other"))
        .await
        .unwrap();
    // An orphan run with no resolvable parent must never leak into a
    // git-scoped result.
    upsert_run(&conn, &sample_run("wf_orphan", ""))
        .await
        .unwrap();

    // Record a span placing sess-branch on branch `feat/x`.
    record_span_observation(
        &conn,
        &SpanObservation {
            provider: "claude".to_string(),
            session_id: "sess-branch".to_string(),
            thread_id: None,
            branch: Some("feat/x".to_string()),
            worktree: "/repo".to_string(),
            ts: 1_700_000_100,
            source: SpanSource::Ingest,
        },
        super::super::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
    )
    .await
    .unwrap();

    let filter = GitScopeFilter::from_args(Some("feat/x"), None, None).unwrap();
    let hits = runs_for_git_scope(&conn, &filter, 10).await.unwrap();
    let ids: Vec<&str> = hits.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(ids, vec!["wf_on_branch"]);

    // A branch with no span yields nothing.
    let none = GitScopeFilter::from_args(Some("feat/absent"), None, None).unwrap();
    assert!(
        runs_for_git_scope(&conn, &none, 10)
            .await
            .unwrap()
            .is_empty()
    );

    // Empty filter is a caller error.
    let empty = GitScopeFilter::default();
    assert!(matches!(
        runs_for_git_scope(&conn, &empty, 10).await,
        Err(WorkflowIndexError::InvalidArgument(_))
    ));
}

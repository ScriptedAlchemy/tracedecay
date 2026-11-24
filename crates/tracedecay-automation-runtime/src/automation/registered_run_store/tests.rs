#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tracedecay_domain::ProjectId;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

use super::{AutomationValidationCursorV1, RegisteredAutomationRunStore};
use crate::automation::backend::AgentTaskKind;
use crate::automation::exact_run::ExactRunPublishOutcome;
use crate::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
};
use tracedecay_automation::evidence_budget::SESSION_EVIDENCE_BUDGET_EXHAUSTED;

/// A registered project-sessions store plus the runtime that keeps it open.
pub(crate) struct ProjectRunStoreFixture {
    pub(crate) runs: RegisteredAutomationRunStore,
    /// The same project-sessions shard, as the runner's activity source.
    pub(crate) db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    pub(crate) runtime: RegisteredGlobalDbTestRuntime,
}

static FIXTURE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Opens an isolated project shard under `root` and admits it as the run
/// store whose artifacts live in `dashboard_root`.
pub(crate) async fn project_run_store(
    root: &Path,
    dashboard_root: &Path,
) -> ProjectRunStoreFixture {
    let nonce = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let runtime = RegisteredGlobalDbTestRuntime::project(
        root.join(format!("profile-{nonce}")),
        root.join(format!("project-{nonce}")),
        ProjectId::new(format!("project.automation-runs-{nonce}")).unwrap(),
    )
    .await
    .expect("registered project test runtime");
    let db = runtime.project_database_arc().unwrap();
    let runs = RegisteredAutomationRunStore::admit(db.clone(), dashboard_root.to_path_buf())
        .expect("registered run store admission");
    ProjectRunStoreFixture { runs, db, runtime }
}

fn record(
    run_id: &str,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
    status: AutomationRunStatus,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    serde_json::from_value(json!({
        "schema_version": 2,
        "run_id": run_id,
        "trigger": trigger,
        "task": task,
        "task_key": crate::automation::backend::task_key(task),
        "backend": "codex_app_server",
        "status": status,
        "accepted_count": 0,
        "rejected_count": 0,
        "started_at": completed_at.to_string(),
        "completed_at": completed_at.to_string(),
        "completed_at_micros": completed_at * 1_000_000,
    }))
    .unwrap()
}

#[tokio::test]
async fn ordinary_and_exact_publication_replay_conflict_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = project_run_store(temp.path(), &temp.path().join("dashboard")).await;
    let runs = &fixture.runs;

    let queued = record(
        "run-ordinary",
        AgentTaskKind::MemoryCurator,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Queued,
        10,
    );
    runs.append_run_record(&queued).await.unwrap();
    let mut succeeded = queued.clone();
    succeeded.status = AutomationRunStatus::Succeeded;
    succeeded.completed_at = "11".to_owned();
    succeeded.completed_at_micros = Some(11_000_000);
    runs.append_run_record(&succeeded).await.unwrap();
    // A byte-identical replay is a no-op; changed same-status bytes refuse.
    runs.append_run_record(&succeeded).await.unwrap();
    let mut conflicting = succeeded.clone();
    conflicting.accepted_count = 9;
    runs.append_run_record(&conflicting)
        .await
        .expect_err("changed same-status bytes must refuse");
    // An identical older row refreshes physical ordering without becoming
    // the run's current terminal record again.
    runs.append_run_record(&queued).await.unwrap();
    assert_eq!(
        runs.find_run_record("run-ordinary").await.unwrap(),
        Some(succeeded.clone())
    );

    let exact = record(
        "run-exact",
        AgentTaskKind::UserJob,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Succeeded,
        12,
    );
    let (publication, bound) = runs
        .bind_staged_run_record_exact(&exact, |publication| Ok(publication.clone()))
        .await
        .unwrap();
    assert_eq!(bound, publication);
    assert_eq!(
        runs.find_run_record("run-exact").await.unwrap(),
        None,
        "a staged exact terminal is invisible before publication"
    );
    assert_eq!(
        runs.publish_staged_run_record_exact("run-exact", &publication)
            .await
            .unwrap(),
        ExactRunPublishOutcome::Published
    );
    // Replaying the committed publication succeeds without a new transition.
    assert_eq!(
        runs.publish_staged_run_record_exact("run-exact", &publication)
            .await
            .unwrap(),
        ExactRunPublishOutcome::Published
    );
    assert!(
        runs.is_exact_terminal_published("run-exact", &publication)
            .await
            .unwrap()
    );
    let mut changed_exact = exact.clone();
    changed_exact.accepted_count = 3;
    runs.bind_staged_run_record_exact(&changed_exact, |_| Ok(()))
        .await
        .expect_err("a different terminal for a published run must refuse");

    // A second admitted client of the same shard sees identical history.
    let remounted = RegisteredAutomationRunStore::admit(
        fixture
            .runtime
            .issue_project_database_lease_for_test()
            .unwrap(),
        runs.dashboard_root().to_path_buf(),
    )
    .unwrap();
    assert_eq!(
        remounted.find_run_record("run-exact").await.unwrap(),
        Some(exact)
    );
    let page = remounted.load_run_records_page(10).await.unwrap();
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.run_id.as_str())
            .collect::<Vec<_>>(),
        ["run-exact", "run-ordinary"]
    );
    assert!(page.is_complete());
    let first = remounted.load_run_records_page(1).await.unwrap();
    assert_eq!(first.records.len(), 1);
    assert!(first.has_more);
}

#[tokio::test]
async fn indexed_history_summary_cursor_and_scheduler_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = project_run_store(temp.path(), &temp.path().join("dashboard")).await;
    let runs = &fixture.runs;
    let task = AgentTaskKind::MemoryCurator;

    let scheduler_success = record(
        "curator-success",
        task,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Succeeded,
        100,
    );
    let mut manual_failure = record(
        "curator-manual-failure",
        task,
        AutomationTrigger::ManualCli,
        AutomationRunStatus::Failed,
        110,
    );
    manual_failure.error = Some("backend failed".to_owned());
    let mut exhausted = record(
        "curator-exhausted",
        task,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Skipped,
        120,
    );
    exhausted.error = Some(SESSION_EVIDENCE_BUDGET_EXHAUSTED.to_owned());
    exhausted.validation_report = Some(json!({
        "pagination": { "resume_after_fact_id": "fact-older" }
    }));
    let mut suppressed = record(
        "curator-suppressed",
        task,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Skipped,
        130,
    );
    suppressed.error = Some("not_due".to_owned());
    suppressed.validation_report = Some(json!({
        "pagination": { "resume_after_fact_id": null }
    }));
    let unrelated = record(
        "writer-success",
        AgentTaskKind::SkillWriter,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Succeeded,
        140,
    );
    for row in [
        &scheduler_success,
        &manual_failure,
        &exhausted,
        &suppressed,
        &unrelated,
    ] {
        runs.append_run_record(row).await.unwrap();
    }

    let summary = runs
        .load_run_ledger_task_summary(task, "memory_curator")
        .await
        .unwrap();
    let id = |record: Option<&AutomationRunLedgerRecord>| record.map(|r| r.run_id.clone());
    assert_eq!(
        id(summary.latest_logical_activity()).as_deref(),
        Some("curator-suppressed")
    );
    assert_eq!(
        id(summary.latest_successful()).as_deref(),
        Some("curator-success")
    );
    assert_eq!(
        id(summary.latest_effectful_any_trigger()).as_deref(),
        Some("curator-manual-failure")
    );
    assert_eq!(
        id(summary.latest_scheduler_effectful()).as_deref(),
        Some("curator-success")
    );
    assert_eq!(
        id(summary.latest_scheduler_activity()).as_deref(),
        Some("curator-suppressed")
    );
    assert_eq!(
        id(summary.latest_session_evidence_budget_exhausted()).as_deref(),
        Some("curator-exhausted")
    );
    assert_eq!(summary.records().len(), 4, "each selected run appears once");
    runs.load_run_ledger_task_summary(task, "user_job:other")
        .await
        .unwrap();
    runs.load_run_ledger_task_summary(task, "skill_writer")
        .await
        .expect_err("a summary requires the exact canonical task identity");

    // The newest run's explicit null cursor restarts pagination.
    assert_eq!(
        runs.load_latest_task_validation_cursor(
            "memory_curator",
            AutomationValidationCursorV1::ResumeAfterFactId
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        runs.load_run_records_for_task_key("skill_writer", 10)
            .await
            .unwrap(),
        vec![unrelated.clone()]
    );

    let mut job_success = record(
        "job-occurrence",
        AgentTaskKind::UserJob,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Succeeded,
        200,
    );
    job_success.task_key = "user_job:nightly".to_owned();
    runs.append_run_record(&job_success).await.unwrap();
    let mut diagnostic = record(
        "job-occurrence-skip",
        AgentTaskKind::UserJob,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Skipped,
        201,
    );
    diagnostic.task_key = "user_job:nightly".to_owned();
    diagnostic.error = Some("scheduler_lock_active".to_owned());
    let appended = runs
        .append_or_reuse_scheduler_diagnostic(&diagnostic, Some("job-occurrence"))
        .await
        .unwrap();
    let mut later = diagnostic.clone();
    later.completed_at = "205".to_owned();
    later.completed_at_micros = Some(205_000_000);
    let reused = runs
        .append_or_reuse_scheduler_diagnostic(&later, Some("job-occurrence"))
        .await
        .unwrap();
    assert_eq!(reused, appended, "an identical occurrence reuses its row");
    runs.append_or_reuse_scheduler_diagnostic(&diagnostic, Some("missing-anchor"))
        .await
        .expect_err("an unknown persistent anchor refuses");
    assert_eq!(
        runs.load_latest_scheduler_effectful_for_task_key("user_job:nightly")
            .await
            .unwrap()
            .map(|record| record.run_id),
        Some("job-occurrence".to_owned())
    );
}

#[tokio::test]
async fn growing_history_reports_indexed_read_write_and_lock_spans() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = project_run_store(temp.path(), &temp.path().join("dashboard")).await;
    let runs = &fixture.runs;
    for history_size in [2_000_i64, 8_000] {
        let started = std::time::Instant::now();
        for ordinal in 0..history_size {
            let run = record(
                &format!("run-{history_size}-{ordinal}"),
                AgentTaskKind::SessionReflector,
                AutomationTrigger::Scheduler,
                AutomationRunStatus::Succeeded,
                ordinal,
            );
            runs.append_run_record(&run).await.unwrap();
        }
        let write_elapsed = started.elapsed();
        let lookup_started = std::time::Instant::now();
        let latest = runs
            .find_run_record(&format!("run-{history_size}-{}", history_size - 1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.completed_at, (history_size - 1).to_string());
        let lookup_elapsed = lookup_started.elapsed();
        let summary_started = std::time::Instant::now();
        let summary = runs
            .load_run_ledger_task_summary(AgentTaskKind::SessionReflector, "session_reflector")
            .await
            .unwrap();
        assert_eq!(
            summary
                .latest_successful()
                .map(|record| record.run_id.as_str()),
            Some(format!("run-{history_size}-{}", history_size - 1).as_str())
        );
        let summary_elapsed = summary_started.elapsed();
        let page = runs.load_run_records_page(20).await.unwrap();
        assert_eq!(page.records.len(), 20);
        assert!(page.has_more);
        assert_eq!(page.records[0].run_id, latest.run_id);
        let snapshot = runs.database().read_snapshot().await.unwrap();
        let mut plan_rows = snapshot
            .query(
                "EXPLAIN QUERY PLAN
                     SELECT record_json
                       FROM automation_runs
                      WHERE namespace = ?1 AND run_id = ?2",
                tracedecay_runtime_core::db::engine::params![
                    fixture
                        .db
                        .binding()
                        .shard_id
                        .scope
                        .project_id()
                        .unwrap()
                        .as_str(),
                    latest.run_id.as_str()
                ],
            )
            .await
            .unwrap();
        let mut plan_lines = Vec::new();
        while let Some(row) = plan_rows.next().await.unwrap() {
            plan_lines.push(row.get::<String>(3).unwrap());
        }
        let namespace_plan = plan_lines.join("\n");
        assert!(
            namespace_plan.contains("SEARCH"),
            "an exact run lookup must use an indexed seek: {namespace_plan}"
        );
        tracing::info!(
            history_size,
            write_elapsed_micros = write_elapsed.as_micros(),
            exact_lookup_elapsed_micros = lookup_elapsed.as_micros(),
            task_summary_elapsed_micros = summary_elapsed.as_micros(),
            query_plan = %namespace_plan,
            "automation growing-history read/write and lock spans"
        );
    }
}

#[tokio::test]
async fn publication_commit_faults_leave_retryable_exact_state() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = project_run_store(temp.path(), &temp.path().join("dashboard")).await;
    let runs = &fixture.runs;
    let terminal = record(
        "run-fault",
        AgentTaskKind::SkillWriter,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Failed,
        50,
    );

    // The journal binding fails after staging: history stays unpublished
    // and the identical terminal binds again on retry.
    runs.bind_staged_run_record_exact(&terminal, |_| -> tracedecay_domain::errors::Result<()> {
        Err(crate::automation::config_error(
            "injected prepared-write fault",
        ))
    })
    .await
    .expect_err("the bind fault surfaces");
    assert_eq!(runs.find_run_record("run-fault").await.unwrap(), None);
    let (publication, ()) = runs
        .bind_staged_run_record_exact(&terminal, |_| Ok(()))
        .await
        .unwrap();

    // An abandoned-reservation sweep keeps a terminal its journal still owns.
    assert_eq!(
        runs.discard_unbound_staged_run_records_if("run-fault", || Ok(false))
            .await
            .unwrap(),
        crate::automation::exact_run::ExactRunUnboundDiscardOutcome::Retained
    );
    assert_eq!(
        runs.publish_staged_run_record_exact("run-fault", &publication)
            .await
            .unwrap(),
        ExactRunPublishOutcome::Published
    );
    assert_eq!(
        runs.find_run_record("run-fault").await.unwrap(),
        Some(terminal.clone())
    );
    // Recovery after the commit replays it without another transition.
    assert_eq!(
        runs.publish_staged_run_record_exact("run-fault", &publication)
            .await
            .unwrap(),
        ExactRunPublishOutcome::Published
    );
    assert_eq!(runs.load_run_records(10).await.unwrap(), vec![terminal]);

    // A reservation proven unbound loses its staged terminal; recovery then
    // reports the binding as missing rather than fabricating success.
    let abandoned = record(
        "run-abandoned",
        AgentTaskKind::SkillWriter,
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Succeeded,
        60,
    );
    let (abandoned_publication, ()) = runs
        .bind_staged_run_record_exact(&abandoned, |_| Ok(()))
        .await
        .unwrap();
    runs.discard_unbound_staged_run_records_if("run-abandoned", || Ok(true))
        .await
        .unwrap();
    assert_eq!(
        runs.publish_staged_run_record_exact("run-abandoned", &abandoned_publication)
            .await
            .unwrap(),
        ExactRunPublishOutcome::MissingPayload
    );
    assert_eq!(runs.find_run_record("run-abandoned").await.unwrap(), None);
}

#[tokio::test]
async fn legacy_jsonl_is_refused_without_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    std::fs::create_dir_all(&dashboard_root).unwrap();
    let fixture = project_run_store(temp.path(), &temp.path().join("fresh-dashboard")).await;
    let legacy = dashboard_root.join("automation_runs.jsonl");
    let bytes = b"{\"schema_version\":2,\"run_id\":\"retired\"}\n";
    std::fs::write(&legacy, bytes).unwrap();

    let refusal = RegisteredAutomationRunStore::admit(
        fixture.runtime.project_database_arc().unwrap(),
        dashboard_root.clone(),
    )
    .expect_err("legacy JSONL was served instead of refused");
    assert_eq!(
        refusal
            .project_route_context()
            .map(|(code, retryable, _)| (code, retryable)),
        Some(("automation_run_legacy_jsonl_refused", false))
    );
    assert_eq!(std::fs::read(&legacy).unwrap(), bytes);

    let spool_root = temp.path().join("spool-dashboard");
    std::fs::create_dir_all(spool_root.join("automation_run_spool")).unwrap();
    RegisteredAutomationRunStore::admit(
        fixture.runtime.project_database_arc().unwrap(),
        spool_root,
    )
    .expect_err("a retired exact spool refuses run-state admission");

    // A profile-only shard has no project namespace for run history.
    let profile = RegisteredGlobalDbTestRuntime::profile(temp.path().join("profile-only"))
        .await
        .unwrap();
    let projectless = RegisteredAutomationRunStore::admit(
        profile.profile_database_arc(),
        temp.path().join("projectless-dashboard"),
    )
    .unwrap();
    projectless
        .load_run_records(1)
        .await
        .expect_err("a store without project identity refuses run state");
}

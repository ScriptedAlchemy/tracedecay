//! Scheduler-only combined reflector+skill pass (Hermes combined-review
//! parity): one backend call serves both tasks when both are due in the same
//! tick, recording one ledger entry per task so per-task bookkeeping and the
//! dashboard scheduler status stay coherent.

use crate::support::*;
use tracedecay::automation::scheduler::{SessionActivity, schedule_decision};

fn combined_options(profile_root: &Path) -> CombinedReviewAutomationOptions {
    CombinedReviewAutomationOptions {
        run_id: Some("combined-run-1".to_string()),
        session_reflector: SessionReflectorAutomationOptions {
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            ..SessionReflectorAutomationOptions::default()
        },
        skill_writer: manual_skill_writer_options(profile_root),
        trigger: AutomationTrigger::Scheduler,
    }
}

fn combined_output_fixture() -> Value {
    json!({
        "facts": [
            {
                "content": "TraceDecay automation should manage durable session reflection facts directly",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.72,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence supports self-managed durable fact automation"
            }
        ],
        "skills": [
            {
                "id": "automation-run-review",
                "title": "Automation run review",
                "summary": "Review self-improvement automation run ledgers and apply policies.",
                "category": "workflow",
                "body_markdown": "Use when reviewing TraceDecay self-improvement runs.",
                "reason": "Session evidence repeats automation workflow outcome review."
            }
        ]
    })
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn combined_review_runner_records_both_tasks_from_one_backend_call() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let config = scheduler_config(Some(3600), None);
    let backend = CombinedJsonBackend::new(json!({"facts": [], "skills": []}));

    let dispatch =
        run_combined_review_with_backend(&cg, &config, &backend, combined_options(&profile_root))
            .await
            .unwrap();

    assert_eq!(backend.calls(), 1, "both tasks must share one backend call");
    let CombinedReviewDispatch::Ran(run) = dispatch else {
        panic!("expected combined dispatch to run, got {dispatch:?}");
    };
    assert_eq!(run.run_id, "combined-run-1");

    let reflector = &run.session_reflector.ledger_record;
    assert_eq!(reflector.run_id, "combined-run-1_facts");
    assert_eq!(reflector.task, AgentTaskKind::SessionReflector);
    assert_eq!(reflector.task_key.as_deref(), Some("session_reflector"));
    assert_eq!(reflector.trigger, AutomationTrigger::Scheduler);
    assert_eq!(reflector.status, AutomationRunStatus::Succeeded);
    assert_eq!(
        reflector.prompt_version.as_deref(),
        Some("combined_review:v1")
    );
    assert_eq!(reflector.accepted_count, 0);

    let skill = &run.skill_writer.ledger_record;
    assert_eq!(skill.run_id, "combined-run-1_skills");
    assert_eq!(skill.task, AgentTaskKind::SkillWriter);
    assert_eq!(skill.task_key.as_deref(), Some("skill_writer"));
    assert_eq!(skill.trigger, AutomationTrigger::Scheduler);
    assert_eq!(skill.status, AutomationRunStatus::Succeeded);
    assert_eq!(skill.prompt_version.as_deref(), Some("combined_review:v1"));
    assert_eq!(skill.accepted_count, 0);

    // Both halves share the combined request's input hash and correlate
    // through report_ref.combined_run_id.
    assert!(reflector.input_hash.is_some());
    assert_eq!(reflector.input_hash, skill.input_hash);
    for record in [reflector, skill] {
        let report_ref = record.report_ref.as_ref().unwrap();
        assert_eq!(report_ref["combined_run_id"], json!("combined-run-1"));
        assert_eq!(report_ref["combined_task_key"], json!("combined_review"));
    }

    // Empty combined proposals can commit both ledgers without cross-authority writes.
    let memory = tracedecay::application::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let pending = list_fact_proposals(
        &memory,
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::PendingApproval),
        10,
    )
    .await
    .unwrap();
    assert!(pending.is_empty());
    let proposals = list_fact_proposals(
        &memory,
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::Applied),
        10,
    )
    .await
    .unwrap();
    assert!(proposals.is_empty());
    assert!(
        !profile_root
            .join("agent_managed/skills/automation-run-review")
            .exists()
    );

    // Per-task last-run bookkeeping sees the combined run: both tasks are
    // now inside their scheduler interval.
    let records = load_run_records(&cg.store_layout().dashboard_root, 50)
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    for task in [AgentTaskKind::SessionReflector, AgentTaskKind::SkillWriter] {
        let now = current_timestamp();
        let decision = schedule_decision(&config, task, &records, SessionActivity::none(), now);
        assert_eq!(
            decision.skip_reason(),
            Some("scheduler_interval_not_elapsed"),
            "{task:?} must count the combined run as its last scheduler run"
        );
    }
}

#[tokio::test]
async fn combined_review_fails_closed_before_cross_authority_proposal_writes() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let _global_db = isolate_global_db(&cg);
    let config = scheduler_config(Some(3600), None);
    let backend = CombinedJsonBackend::new(combined_output_fixture());

    let dispatch =
        run_combined_review_with_backend(&cg, &config, &backend, combined_options(&profile_root))
            .await
            .unwrap();

    let CombinedReviewDispatch::RecordedFailure { error, .. } = dispatch else {
        panic!("non-atomic combined proposals must fail closed");
    };
    assert!(error.to_string().contains("atomic apply authority"));
    let memory = tracedecay::application::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    assert!(
        list_fact_proposals(&memory, &cg.store_layout().dashboard_root, None, 10,)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!profile_root.join("agent_managed").exists());
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.status == AutomationRunStatus::Failed)
    );
}

#[tokio::test]
async fn combined_review_not_dispatched_when_only_one_task_is_due() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_session_reflector_run",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = CombinedJsonBackend::new(combined_output_fixture());

    let dispatch =
        run_combined_review_with_backend(&cg, &config, &backend, combined_options(&profile_root))
            .await
            .unwrap();

    assert_eq!(backend.calls(), 0);
    let CombinedReviewDispatch::NotCombined { reason } = dispatch else {
        panic!("expected combined dispatch to fall back, got {dispatch:?}");
    };
    assert_eq!(reason, "session_reflector_not_due");
    let records = load_run_records(&cg.store_layout().dashboard_root, 50)
        .await
        .unwrap();
    assert_eq!(records.len(), 1, "fallback must not append ledger records");
}

#[tokio::test]
async fn combined_review_not_dispatched_when_skill_writer_is_not_due() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_skill_writer_run",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = CombinedJsonBackend::new(combined_output_fixture());

    let dispatch =
        run_combined_review_with_backend(&cg, &config, &backend, combined_options(&profile_root))
            .await
            .unwrap();

    assert_eq!(backend.calls(), 0);
    let CombinedReviewDispatch::NotCombined { reason } = dispatch else {
        panic!("expected combined dispatch to fall back, got {dispatch:?}");
    };
    assert_eq!(reason, "skill_writer_not_due");
}

#[tokio::test]
async fn combined_review_respects_escape_hatch_flag() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let mut config = scheduler_config(Some(3600), None);
    config.combine_due_tasks = false;
    let backend = CombinedJsonBackend::new(combined_output_fixture());

    let dispatch =
        run_combined_review_with_backend(&cg, &config, &backend, combined_options(&profile_root))
            .await
            .unwrap();

    assert_eq!(backend.calls(), 0);
    let CombinedReviewDispatch::NotCombined { reason } = dispatch else {
        panic!("expected combined dispatch to fall back, got {dispatch:?}");
    };
    assert_eq!(reason, "combined_mode_disabled");
}

#[tokio::test]
async fn combined_review_falls_back_when_evidence_is_unavailable() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    let _global_db = isolate_global_db(&cg);
    // No seeded session evidence: the reflector evidence bundle is empty, so
    // the combined path defers to the per-task runs (which record their own
    // skips).
    let config = scheduler_config(Some(3600), None);
    let backend = CombinedJsonBackend::new(combined_output_fixture());
    let retrieval = EmptyAutomationSessionRetrieval::new();

    let dispatch = tracedecay::automation::runner::run_combined_review_with_backend_and_retrieval(
        &cg,
        &config,
        &backend,
        &retrieval,
        combined_options(&profile_root),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    let CombinedReviewDispatch::NotCombined { reason } = dispatch else {
        panic!("expected combined dispatch to fall back, got {dispatch:?}");
    };
    assert_eq!(reason, "session_reflector_evidence_unavailable");
}

#[tokio::test]
async fn combined_review_terminal_evidence_matrix_has_zero_effects() {
    for reason in [
        "session_evidence_denied",
        "session_evidence_stale",
        "session_evidence_partial",
        "session_evidence_unavailable",
        "session_evidence_budget_exhausted",
        "session_evidence_cancelled",
    ] {
        let temp = tempdir().unwrap();
        let profile_root = temp.path().join("profile");
        let cg = init_project(temp.path()).await;
        let config = scheduler_config(Some(3600), None);
        let backend = CombinedJsonBackend::new(json!({"facts": [], "skills": []}));
        let retrieval = RejectedAutomationSessionRetrieval::new(reason);

        let dispatch =
            tracedecay::automation::runner::run_combined_review_with_backend_and_retrieval(
                &cg,
                &config,
                &backend,
                &retrieval,
                combined_options(&profile_root),
            )
            .await
            .unwrap();

        assert!(matches!(
            dispatch,
            CombinedReviewDispatch::NotCombined {
                reason: "session_reflector_evidence_unavailable"
            }
        ));
        assert_eq!(backend.calls(), 0);
        assert!(
            load_run_records(&cg.store_layout().dashboard_root, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(!profile_root.exists());
    }

    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("empty-profile");
    let cg = init_project(temp.path()).await;
    let config = scheduler_config(Some(3600), None);
    let backend = CombinedJsonBackend::new(json!({"facts": [], "skills": []}));
    let retrieval = EmptyAutomationSessionRetrieval::new();
    let dispatch = tracedecay::automation::runner::run_combined_review_with_backend_and_retrieval(
        &cg,
        &config,
        &backend,
        &retrieval,
        combined_options(&profile_root),
    )
    .await
    .unwrap();
    assert!(matches!(
        dispatch,
        CombinedReviewDispatch::NotCombined {
            reason: "session_reflector_evidence_unavailable"
        }
    ));
    assert_eq!(backend.calls(), 0);
    assert!(
        load_run_records(&cg.store_layout().dashboard_root, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!profile_root.exists());
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn combined_review_records_failures_for_both_tasks_when_an_array_is_missing() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let config = scheduler_config(Some(3600), None);
    let backend = CombinedJsonBackend::new(json!({ "facts": [] }));

    let dispatch =
        run_combined_review_with_backend(&cg, &config, &backend, combined_options(&profile_root))
            .await
            .unwrap();

    assert_eq!(backend.calls(), 1);
    let CombinedReviewDispatch::RecordedFailure { run, error } = dispatch else {
        panic!("expected combined dispatch to record failures, got {dispatch:?}");
    };
    let err = error.to_string();
    assert!(
        err.contains("must include facts and skills arrays"),
        "unexpected error: {err}"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 50)
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    for record in &records {
        assert_eq!(record.status, AutomationRunStatus::Failed);
        assert_eq!(record.trigger, AutomationTrigger::Scheduler);
        assert_eq!(record.prompt_version.as_deref(), Some("combined_review:v1"));
        assert!(
            record
                .error
                .as_deref()
                .is_some_and(|error| error.contains("facts and skills arrays"))
        );
    }
    let mut tasks: Vec<AgentTaskKind> = records.iter().map(|record| record.task).collect();
    tasks.sort_by_key(|task| format!("{task:?}"));
    assert_eq!(
        tasks,
        vec![AgentTaskKind::SessionReflector, AgentTaskKind::SkillWriter]
    );
    assert_eq!(
        run.session_reflector.ledger_record.status,
        AutomationRunStatus::Failed
    );
    assert_eq!(
        run.skill_writer.ledger_record.status,
        AutomationRunStatus::Failed
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn combined_review_records_noop_fallbacks_for_both_tasks_when_backend_fails() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let _global_db = isolate_global_db(&cg);
    let config = AutomationConfig {
        timeout_secs: 1,
        ..scheduler_config(Some(3600), None)
    };
    let backend = FailingBackend::new(AgentTaskKind::CombinedReview);

    let dispatch =
        run_combined_review_with_backend(&cg, &config, &backend, combined_options(&profile_root))
            .await
            .unwrap();

    // The backend failure is transient, but this test pins the noop-fallback
    // record, not retry semantics (covered by backend.rs retry tests) —
    // timeout_secs: 1 short-circuits the backoff so the test stays fast.
    assert_eq!(backend.calls(), 1);
    let CombinedReviewDispatch::Ran(run) = dispatch else {
        panic!("expected combined dispatch to record fallbacks, got {dispatch:?}");
    };
    assert_noop_fallback_record(
        &run.session_reflector.ledger_record,
        AgentTaskKind::SessionReflector,
        "session_reflector",
        json!({ "facts": [] }),
    );
    assert_noop_fallback_record(
        &run.skill_writer.ledger_record,
        AgentTaskKind::SkillWriter,
        "skill_writer",
        json!({ "skills": [] }),
    );
}

use crate::support::*;

#[path = "session_reflector/automatic_fact_receipts.rs"]
mod automatic_fact_receipts;

#[cfg(feature = "test-transport")]
use std::sync::atomic::Ordering;
#[cfg(feature = "test-transport")]
use tracedecay_agent_hosts::ports::session_evidence::{LcmGrepSort, LcmScope};
#[cfg(feature = "test-transport")]
use tracedecay_store::{ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchQuery};

#[test]
fn session_reflector_options_have_no_storage_selector() {
    let options = serde_json::to_value(SessionReflectorAutomationOptions::default()).unwrap();
    assert!(options.get("storage_scope").is_none());
    assert!(options.get("hermes_home").is_none());
    assert!(
        serde_json::from_value::<SessionReflectorAutomationOptions>(json!({
            "storage_scope": "hermes_profile"
        }))
        .is_err()
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn retained_session_reflector_preserves_retrieval_and_defers_ledger_publication() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let retrieval = FixtureAutomationSessionRetrieval::new(&cg);
    let backend = SessionJsonBackend::new(json!({"facts": []}));
    let retained = tracedecay_agent_hosts::automation::runner::run_session_reflector_with_backend_and_retrieval_for_retained_settlement(
        &cg,
        &scheduler_config(Some(3600), None),
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &test_configuration_revision(),
        &backend,
        &retrieval,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id: Some("retained-session-reflector".to_owned()),
            provider: "cursor".to_owned(),
            query: "durable session reflection".to_owned(),
            evidence_limit: 5,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await;
    let (result, guard) = retained.into_parts();
    let run = result.unwrap();

    assert_eq!(run.run_id, "retained-session-reflector");
    assert!(
        load_run_records(&cg.store_layout().dashboard_root, 10)
            .await
            .unwrap()
            .is_empty(),
        "an admitted retained run must not publish ahead of outer settlement"
    );
    drop(guard);
}

use tracedecay_agent_hosts::automation::automatic_facts::record_session_automatic_facts;

#[tokio::test]
async fn session_reflector_runner_skips_when_task_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = SessionJsonBackend::new(json!({"facts": []}));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: false,
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SessionReflector);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("session_reflector_disabled")
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_interrupts_validation_before_near_match_or_apply() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let interrupted = Arc::new(AtomicBool::new(true));
    let run_control = test_automation_run_control(Arc::clone(&interrupted));
    let backend = SessionJsonBackend::new(json!({
        "facts": [{
            "content": "Interrupted session reflection must never write a fact",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.8,
            "source_span": {
                "session_id": "session-reflect-1",
                "message_id": "session-reflect-1-message-001"
            },
            "reason": "bounded evidence describes the cancellation boundary"
        }]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let error = run_session_reflector_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        SessionReflectorAutomationOptions::default(),
    )
    .await
    .expect_err("interrupted validation must stop before near-match results");

    assert!(
        error.to_string().contains("cancelled"),
        "the canonical graph cancellation must stay typed through validation: {error}"
    );
    interrupted.store(false, Ordering::Release);
    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    assert!(
        list_automatic_fact_receipts(&memory, None, 10, run_control.read_control())
            .await
            .unwrap()
            .is_empty(),
        "interrupted validation must not write an automatic-fact receipt"
    );
}

#[tokio::test]
async fn session_reflector_fails_closed_on_stale_temporal_evidence() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = SessionJsonBackend::new(json!({"facts": []}));
    let retrieval = RejectedAutomationSessionRetrieval::new("session_evidence_stale");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = tracedecay_agent_hosts::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &test_configuration_revision(),
        &backend,
        &retrieval,
        SessionReflectorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("session_evidence_stale")
    );
    assert!(
        load_run_records(&cg.store_layout().dashboard_root, 10)
            .await
            .unwrap()
            .is_empty(),
        "rejected evidence must not write a ledger record"
    );
    assert!(
        !cg.store_layout()
            .dashboard_root
            .join("automation_outcomes.json")
            .exists(),
        "rejected evidence must not refresh fact outcomes"
    );
}

#[tokio::test]
async fn project_reflector_and_skill_writer_terminal_evidence_matrix_has_zero_writes() {
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
        let retrieval = RejectedAutomationSessionRetrieval::new(reason);
        let reflector_backend = SessionJsonBackend::new(json!({"facts": []}));
        let skill_backend = SkillJsonBackend::new(json!({"skills": []}));
        let config = AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            host_mode: AutomationHostMode::Standalone,
            tasks: AutomationTaskSet {
                session_reflector: AutomationTaskConfig {
                    enabled: true,
                    schedule: Some("manual".to_string()),
                    ..AutomationTaskConfig::default()
                },
                skill_writer: AutomationTaskConfig {
                    enabled: true,
                    schedule: Some("manual".to_string()),
                    ..AutomationTaskConfig::default()
                },
                ..AutomationTaskSet::default()
            },
            ..AutomationConfig::default()
        };

        let reflector =
            tracedecay_agent_hosts::automation::runner::run_session_reflector_with_backend_and_retrieval(
                &cg,
                &config,
                &test_automation_run_control(Arc::new(AtomicBool::new(false))),
                &test_configuration_revision(),
                &reflector_backend,
                &retrieval,
                SessionReflectorAutomationOptions::default(),
            )
            .await
            .unwrap();
        let skill = tracedecay_agent_hosts::automation::runner::run_skill_writer_with_backend_and_retrieval(
            &cg,
            &config,
            &test_configuration_revision(),
            &skill_backend,
            &retrieval,
            SkillWriterAutomationOptions {
                profile_root: Some(profile_root.clone()),
                ..SkillWriterAutomationOptions::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(reflector.ledger_record.error.as_deref(), Some(reason));
        assert_eq!(skill.ledger_record.error.as_deref(), Some(reason));
        assert_eq!(reflector_backend.calls(), 0);
        assert_eq!(skill_backend.calls(), 0);
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
    let retrieval = EmptyAutomationSessionRetrieval::new();
    let reflector_backend = SessionJsonBackend::new(json!({"facts": []}));
    let skill_backend = SkillJsonBackend::new(json!({"skills": []}));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };
    let reflector =
        tracedecay_agent_hosts::automation::runner::run_session_reflector_with_backend_and_retrieval(
            &cg,
            &config,
            &test_automation_run_control(Arc::new(AtomicBool::new(false))),
            &test_configuration_revision(),
            &reflector_backend,
            &retrieval,
            SessionReflectorAutomationOptions::default(),
        )
        .await
        .unwrap();
    let skill =
        tracedecay_agent_hosts::automation::runner::run_skill_writer_with_backend_and_retrieval(
            &cg,
            &config,
            &test_configuration_revision(),
            &skill_backend,
            &retrieval,
            SkillWriterAutomationOptions {
                profile_root: Some(profile_root.clone()),
                ..SkillWriterAutomationOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        reflector.ledger_record.error.as_deref(),
        Some("no_session_evidence")
    );
    assert_eq!(
        skill.ledger_record.error.as_deref(),
        Some("no_skill_writer_evidence")
    );
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
async fn session_reflector_runner_applies_valid_automatic_facts_by_default() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let seed_memory = tracedecay_usecases::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let seeded = record_session_automatic_facts(
        &seed_memory,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        "run.session-reflector-duplicate-seed",
        Some("evidence.session-reflector-duplicate-seed"),
        &[json!({
            "add_fact_request": {
                "content": "Cache invalidation policy must be explicit",
                "category": "project",
                "source_label": "session-reflector-test-seed",
                "tags": ["cache", "policy"],
                "entities": [],
                "trust": 0.97,
                "metadata": {},
            }
        })],
    )
    .await
    .unwrap();
    assert!(seeded.retry_error.is_none());
    assert_eq!(seeded.receipts.len(), 1);
    drop(seed_memory);
    let backend = SessionJsonBackend::new(json!({
        "facts": [
            {
                "content": "TraceDecay automation should manage durable session reflection facts directly",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.72,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence supports self-managed durable fact automation"
            },
            {
                "content": "Use the fact-store workflow only when the user explicitly asks to memorize or remember a subject",
                "category": "tool_guidance",
                "tags": ["memory", "workflow"],
                "entities": ["TraceDecay"],
                "trust": 0.74,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated assistant guidance describes durable fact-store tool use"
            },
            {
                "content": "Cache invalidation policy must be explicit",
                "category": "project",
                "tags": ["cache"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "duplicate should be quarantined"
            },
            {
                "content": "Uncited session reflection facts must not be accepted",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "reason": "missing citation is invalid"
            },
            {
                "content": "Session reflection citations must point at bounded evidence",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "source_span": {"session_id": "session-reflect-1", "message_id": "missing-message"},
                "reason": "bogus citation is invalid"
            },
            {
                "content": "Session reflection facts require calibrated trust",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "missing trust is invalid"
            },
            {
                "content": "Session reflection facts require a rationale",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"}
            },
            {
                "content": "Session reflector uses trust rather than confidence",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "confidence": 0.9,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "confidence is invalid"
            },
            {
                "content": "",
                "category": "project"
            },
            {
                "content": "Bucket trust labels emitted by backends map onto calibrated numeric scores",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": "high",
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "bucket trust labels should be accepted"
            },
            {
                "content": "Unknown trust labels must not be accepted",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": "sky-high",
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "unknown trust label is invalid"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        backend.calls(),
        2,
        "invalid candidates receive one repair turn"
    );
    assert_eq!(run.ledger_record.task, AgentTaskKind::SessionReflector);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.backend_attempt_count, 2);
    assert_eq!(run.ledger_record.accepted_count, 3);
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["source_label"],
        json!("session_reflector")
    );
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["category"],
        json!("project")
    );
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["metadata"]["source_span"],
        json!({"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"})
    );
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["metadata"]["trust_reason"],
        json!("Repeated session evidence supports self-managed durable fact automation")
    );
    assert_eq!(
        run.report["accepted_facts"][1]["add_fact_request"]["category"],
        json!("tool")
    );
    let quarantined = run.report["quarantined_facts"].as_array().unwrap();
    assert!(
        quarantined
            .iter()
            .any(|value| value["reason"].as_str().unwrap().contains("duplicate"))
    );
    let has_quarantine_reason = |reason: &str| {
        quarantined
            .iter()
            .any(|value| value["reason"] == json!(reason))
    };
    assert!(has_quarantine_reason("content is required"));
    assert!(has_quarantine_reason("source_span is required"));
    assert!(has_quarantine_reason(
        "source_span must cite a bounded session reflection evidence hit"
    ));
    assert!(has_quarantine_reason("trust is required"));
    assert!(has_quarantine_reason(
        "trust must be a number between 0 and 1, or one of low, medium, high"
    ));
    assert!(has_quarantine_reason("reason is required"));
    assert!(has_quarantine_reason(
        "confidence is not supported; use trust"
    ));
    assert_eq!(
        run.report["accepted_facts"][2]["add_fact_request"]["trust"],
        json!(0.85)
    );
    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let receipts: Vec<_> = list_automatic_fact_receipts(
        &memory,
        Some(AutomaticFactState::Applied),
        10,
        run_control.read_control(),
    )
    .await
    .unwrap()
    .into_iter()
    .filter(|receipt| receipt.run_id == run.run_id)
    .collect();
    assert_eq!(receipts.len(), 3);
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.add_fact_request.category
                == tracedecay_domain::FactCategoryV1::Tool)
            .count(),
        1
    );
    assert_eq!(
        run.report["status"],
        json!("partial"),
        "terminal applies can coexist with quarantined input"
    );
    assert_eq!(run.report["receipt"]["applied_count"], json!(3));
    assert_eq!(run.report["receipt"]["quarantined_count"], json!(8));
    assert_eq!(run.ledger_record.rejected_count, 8);
    assert_eq!(run.ledger_record.reviewed_count, 11);
    assert_eq!(
        run.report["curation_policy"]["effect"]["applied_count"],
        json!(3)
    );
    assert_eq!(
        run.report["curation_policy"]["effect"]["fully_applied"],
        json!(false)
    );
    assert_eq!(
        run.report["curation_policy"]["decision"]["authority"]["actor_id"],
        json!("automation:session-reflector")
    );
    assert_eq!(
        run.report["curation_policy"]["decision"]["authority"]["configuration_revision_id"],
        json!(test_configuration_revision())
    );
    assert!(run.ledger_record.applied_ops.is_some());
    let receipt_ids = run.report["receipt"]["automatic_fact_receipt_ids"]
        .as_array()
        .unwrap();
    assert_eq!(receipt_ids.len(), 3);
    assert!(
        receipts
            .iter()
            .all(|receipt| { receipt_ids.iter().any(|id| id == &json!(receipt.apply_id)) })
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["status"],
        json!("partial")
    );
    let artifact_kinds: Vec<&str> = run
        .ledger_record
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();
    assert_eq!(
        artifact_kinds,
        vec![
            "traces",
            "feedback",
            "generated_evals",
            "validation_gate",
            "optimizer_diagnosis",
            "codex_handoff"
        ]
    );
    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["task"], json!("session_reflector"));
    assert_eq!(eval_payload["summary"]["eval_count"], json!(11));
    assert!(
        eval_payload["eval_definitions"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |entry| entry["eval_id"] == json!("session_reflector:accepted:0")
                    && entry["harness"]["commands"][0]
                        == json!("cargo test --test automation_runner_test session_reflector")
            )
    );
    assert_eq!(
        eval_payload["runner"]["commands"].as_array().unwrap().len(),
        1
    );
    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["task"], json!("session_reflector"));
    assert_eq!(
        handoff_payload["next_actions"][0],
        json!("inspect automatically applied fact receipts and canonical fact ids")
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let after_apply = memory
        .search_project_memory_facts(
            ProjectMemoryFactSearchQuery::new(
                memory.owner().clone(),
                ProjectMemoryFactSearchKindV1::Search,
                Some("TraceDecay automation durable session reflection facts".to_owned()),
                None,
                10,
            )
            .unwrap(),
            run_control.read_control(),
        )
        .await
        .unwrap();
    assert!(
        after_apply.hits().iter().any(|hit| {
            hit.fact()
                .content()
                .contains("manage durable session reflection facts directly")
        }),
        "session reflector should persist terminal automatic effects"
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 3);
    assert!(records[0].applied_ops.is_some());
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_runner_auto_applies_validated_facts() {
    const HIGH_ENTROPY_RUN_ID: &str =
        "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE";

    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = SessionJsonBackend::new(json!({
        "facts": [
            {
                "content": "TraceDecay automation should make accepted session memories automatically",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.76,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence supports automatic durable memory capture"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: Some(HIGH_ENTROPY_RUN_ID.to_string()),
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.report["status"], json!("applied"));
    assert_eq!(run.report["receipt"]["applied_count"], json!(1));
    assert_eq!(run.report["receipt"]["quarantined_count"], json!(0));
    assert_eq!(
        run.report["curation_policy"]["effect"]["mutates_store"],
        json!(true)
    );
    assert!(run.ledger_record.applied_ops.is_some());
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["status"],
        json!("applied")
    );

    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let receipts = list_automatic_fact_receipts(
        &memory,
        Some(AutomaticFactState::Applied),
        10,
        run_control.read_control(),
    )
    .await
    .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].run_id, HIGH_ENTROPY_RUN_ID);
    assert!(receipts[0].applied_fact_id.is_some());
    assert_eq!(
        load_automatic_fact_receipt(&memory, &receipts[0].apply_id, run_control.read_control())
            .await
            .unwrap()
            .as_ref(),
        Some(&receipts[0])
    );

    let facts = memory
        .search_project_memory_facts(
            ProjectMemoryFactSearchQuery::new(
                memory.owner().clone(),
                ProjectMemoryFactSearchKindV1::Search,
                Some("automatic durable memory capture".to_owned()),
                None,
                10,
            )
            .unwrap(),
            run_control.read_control(),
        )
        .await
        .unwrap();
    assert!(
        facts.hits().iter().any(|hit| {
            hit.fact()
                .content()
                .contains("accepted session memories automatically")
        }),
        "the terminal receipt must correspond to a searchable canonical fact"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_runner_reports_partial_terminal_effects_for_duplicates() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = SessionJsonBackend::new(json!({
        "facts": [
            {
                "content": "TraceDecay automation should report partial session memory effects",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.76,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence supports a partial apply regression"
            },
            {
                "content": "TraceDecay automation should report partial session memory effects",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.76,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence supports a duplicate semantic no-op"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(run.report["status"], json!("partial"));
    assert_eq!(run.report["receipt"]["applied_count"], json!(1));
    assert_eq!(run.report["receipt"]["quarantined_count"], json!(0));
    assert_eq!(
        run.report["curation_policy"]["effect"]["fully_applied"],
        json!(false)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["status"],
        json!("partial")
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_records_terminal_quarantine_without_an_admitted_fact() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = SessionJsonBackend::new(json!({
        "facts": [{
            "content": "Terminal receipt input requires calibrated trust",
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "source_span": {
                "session_id": "session-reflect-1",
                "message_id": "session-reflect-1-message-001"
            },
            "reason": "The fixture deliberately omits trust"
        }]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.report["status"], json!("quarantined"));
    assert_eq!(run.report["receipt"]["applied_count"], json!(0));
    assert_eq!(run.report["receipt"]["quarantined_count"], json!(1));
    assert!(
        run.report["receipt"]["automatic_fact_receipt_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(run.report["quarantined_facts"].as_array().unwrap().len(), 1);

    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    assert!(
        list_automatic_fact_receipts(
            &memory,
            Some(AutomaticFactState::Quarantined),
            10,
            run_control.read_control(),
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn session_automatic_facts_replay_same_run_idempotently() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let owner = project_memory_owner(&cg);
    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        owner,
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let accepted = json!({
        "add_fact_request": {
            "content": "Repeated session evidence should produce one durable fact action",
            "category": "project",
            "source_label": "session_reflector",
            "tags": ["session-reflector"],
            "entities": ["session reflector"],
            "trust": 0.91,
            "metadata": {
                "source_span": {
                    "session_id": "session-a",
                    "message_id": "message-a"
                },
                "trust_reason": "same durable fact repeated"
            }
        },
        "validation": {
            "dedupe": {
                "nearest_existing_fact_id": null
            }
        }
    });

    let first = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-a",
        Some("evidence-a"),
        std::slice::from_ref(&accepted),
    )
    .await
    .unwrap();
    let second = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-a",
        Some("evidence-a"),
        std::slice::from_ref(&accepted),
    )
    .await
    .unwrap();
    let receipts = list_automatic_fact_receipts(
        &memory,
        Some(AutomaticFactState::Applied),
        10,
        run_control.read_control(),
    )
    .await
    .unwrap();

    assert!(first.retry_error.is_none());
    assert!(second.retry_error.is_none());
    assert_eq!(first.receipts, second.receipts);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].run_id, "run-a");
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_rejects_unsupported_source_role_and_time_filters_without_writes() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let project_db = project_session_runtime(&cg).await;
    seed_session_message_in_db(
        &project_db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "project-reflect-1",
            message_id: "project-reflect-1-message-001",
            role: "assistant",
            timestamp: 1_715_100_005,
            text: "Active project banana evidence should feed session reflection.",
            source: Some("project_lcm"),
        },
    )
    .await;
    seed_session_message_in_db(
        &project_db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "project-reflect-1",
            message_id: "project-reflect-1-message-002",
            role: "user",
            timestamp: 1_715_100_006,
            text: "Active project banana distractor has the wrong role.",
            source: Some("project_lcm"),
        },
    )
    .await;
    let _global_db = isolate_global_db(&cg);

    let backend = InspectSessionEvidenceBackend;
    for (host_mode, query) in [
        (AutomationHostMode::Standalone, "active project banana"),
        (AutomationHostMode::DelegatedHost, "project banana evidence"),
    ] {
        let config = AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            host_mode,
            tasks: AutomationTaskSet {
                session_reflector: AutomationTaskConfig {
                    enabled: true,
                    schedule: Some("manual".to_string()),
                    ..AutomationTaskConfig::default()
                },
                ..AutomationTaskSet::default()
            },
            ..AutomationConfig::default()
        };

        let run = run_session_reflector_with_backend(
            &cg,
            &config,
            &test_automation_run_control(Arc::new(AtomicBool::new(false))),
            &backend,
            SessionReflectorAutomationOptions {
                trigger: AutomationTrigger::ManualCli,
                provider: "cursor".to_string(),
                query: query.to_string(),
                scope: LcmScope::Session,
                session_id: Some("project-reflect-1".to_string()),
                include_summaries: false,
                evidence_limit: 5,
                sort: LcmGrepSort::Relevance,
                source: Some("project_lcm".to_string()),
                role: Some("assistant".to_string()),
                start_time: Some(1_715_100_000),
                end_time: Some(1_715_100_010),
                run_id: None,
                ..SessionReflectorAutomationOptions::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
        assert_eq!(
            run.ledger_record.error.as_deref(),
            Some("session_evidence_filter_unavailable")
        );
    }
    assert!(
        load_run_records(&cg.store_layout().dashboard_root, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_replays_recent_sessions_without_keyword_matches() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    // Deliberately avoids every keyword in the default reflection query so
    // the grep channel returns nothing and only session replay surfaces it.
    let db = project_session_runtime(&cg).await;
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "session-replay-1",
            message_id: "session-replay-1-message-001",
            role: "user",
            timestamp: 1_715_000_050,
            text: "Always pass the offline flag to cargo nextest on this machine.",
            source: None,
        },
    )
    .await;
    let backend = SessionReplayEvidenceBackend::new(
        json!({
            "facts": [
                {
                    "content": "Cargo nextest must run with the offline flag on this machine",
                    "category": "project",
                    "tags": ["testing"],
                    "entities": ["cargo-nextest"],
                    "trust": 0.7,
                    "source_span": {
                        "session_id": "session-replay-1",
                        "message_id": "session-replay-1-message-001"
                    },
                    "reason": "Replayed session turn states the requirement directly"
                }
            ]
        }),
        "session-replay-1",
        "session-replay-1-message-001",
    );
    let retrieval = StaticAutomationSessionRetrieval::message(
        "session-replay-1",
        "session-replay-1-message-001",
        "Always pass the offline flag to cargo nextest on this machine.",
    );
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = tracedecay_agent_hosts::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &test_configuration_revision(),
        &backend,
        &retrieval,
        SessionReflectorAutomationOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(
        run.ledger_record.accepted_count, 1,
        "a fact citing a replay-only turn should validate: {:?}",
        run.report["rejected_facts"]
    );
    assert_eq!(run.ledger_record.rejected_count, 0);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_suppresses_replay_for_filtered_runs() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let db = project_session_runtime(&cg).await;
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "session-replay-filtered",
            message_id: "session-replay-filtered-message-001",
            role: "user",
            timestamp: 1_715_000_070,
            text: "Always pass the offline flag to cargo nextest on this machine.",
            source: None,
        },
    )
    .await;
    let backend = SessionJsonBackend::new(json!({"facts": []}));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &backend,
        SessionReflectorAutomationOptions {
            role: Some("assistant".to_string()),
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("session_evidence_filter_unavailable")
    );
}

#[tokio::test]
async fn session_reflector_skips_when_replay_disabled_and_no_grep_hits() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = SessionJsonBackend::new(json!({"facts": []}));
    let retrieval = EmptyAutomationSessionRetrieval::new();
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = tracedecay_agent_hosts::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &test_configuration_revision(),
        &backend,
        &retrieval,
        SessionReflectorAutomationOptions {
            include_recent_sessions: false,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("no_session_evidence")
    );
}

#[cfg(feature = "test-transport")]
struct NoSummaryReplayBackend;

#[cfg(feature = "test-transport")]
impl AgentTaskBackend for NoSummaryReplayBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> std::result::Result<AgentTaskResponse, tracedecay_automation::backend::AgentTaskError>
    {
        assert_eq!(request.task, AgentTaskKind::SessionReflector);
        let evidence = &request.context["session_reflection_evidence"];
        assert_eq!(evidence["include_summaries"], json!(false));
        assert_eq!(evidence["evidence_mode"], json!("session_replay_with_grep"));
        let sessions = evidence["recent_session_slices"]["sessions"]
            .as_array()
            .expect("replay sessions should be present");
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0]["summary_nodes"]
                .as_array()
                .expect("summary nodes array")
                .is_empty(),
            "include_summaries=false must suppress replay summary nodes"
        );
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: json!({"facts": []}).to_string(),
            output_json: Some(json!({"facts": []})),
            model: Some("fixture-model".to_string()),
            provider: Some("fixture".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_replay_respects_include_summaries_false() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let db = project_session_runtime(&cg).await;
    let source = db
        .lcm_load_raw_message_for_test("cursor", "session-reflect-1-message-001")
        .await
        .expect("seeded raw message provides summary ownership");
    db.lcm_publish_immutable_summary_for_test(
        HostAdmissionScope::Project,
        tracedecay_sessions::runtime::lcm::types::LcmImmutableSummaryPublication {
            summary_id: "summary.session-reflect-1.no-replay".to_string(),
            predecessor_summary_id: None,
            draft: tracedecay_sessions::runtime::lcm::LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: "session-reflect-1".to_string(),
                session_id: "session-reflect-1".to_string(),
                depth: 0,
                summary_text: "summary that should not be replayed when summaries are disabled"
                    .to_string(),
                source_refs: vec![
                    tracedecay_sessions::runtime::lcm::LcmSourceRef::RawMessage {
                        store_id: source.store_id,
                    },
                ],
                source_token_count: 10,
                summary_token_count: 5,
                source_time_start: Some(1_715_000_001),
                source_time_end: Some(1_715_000_001),
                expand_hint: None,
                metadata_json: None,
            },
        },
    )
    .await
    .expect("summary fixture should publish through production constructor");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };
    let retrieval = StaticAutomationSessionRetrieval::message(
        "session-reflect-1",
        "session-reflect-1-message-001",
        "Remember TraceDecay automation should manage durable session reflection facts directly.",
    );

    let run = tracedecay_agent_hosts::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &test_configuration_revision(),
        &NoSummaryReplayBackend,
        &retrieval,
        SessionReflectorAutomationOptions {
            query: "does-not-match-any-grep-hit".to_string(),
            include_summaries: false,
            include_recent_sessions: true,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_runner_ledgers_malformed_backend_output() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = MalformedTextBackend::new(AgentTaskKind::SessionReflector, "not json");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_session_reflector_with_backend(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(
        err.to_string().contains("expected ident") || err.to_string().contains("expected value"),
        "unexpected error: {err}"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::SessionReflector);
    assert_eq!(records[0].task_key.as_deref(), Some("session_reflector"));
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert!(records[0].proposed_ops.is_none());
    assert!(records[0].error.as_deref().is_some_and(|error| {
        error.contains("expected ident") || error.contains("expected value")
    }));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_runner_ledgers_missing_facts_array() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let output = json!({"summary": "no facts"});
    let backend = SessionJsonBackend::new(output.clone());
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_session_reflector_with_backend(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(
        err.to_string()
            .contains("session reflector output must include a facts array")
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::SessionReflector);
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert_eq!(records[0].proposed_ops.as_ref(), Some(&output));
    assert!(records[0].error.as_deref().is_some_and(|error| {
        error.contains("session reflector output must include a facts array")
    }));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn session_reflector_runner_records_noop_fallback_when_backend_run_task_fails() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = FailingBackend::new(AgentTaskKind::SessionReflector);
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        timeout_secs: 1,
        tasks: AutomationTaskSet {
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            provider: "cursor".to_string(),
            query: "durable session reflection".to_string(),
            evidence_limit: 5,
            run_id: None,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    // The backend failure is transient, but this test pins the noop-fallback
    // record, not retry semantics (covered by backend.rs retry tests) —
    // timeout_secs: 1 short-circuits the backoff so the test stays fast.
    assert_eq!(backend.calls(), 1);
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::SessionReflector,
        "session_reflector",
        json!({ "facts": [] }),
    );
    assert!(
        run.ledger_record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("executable"))
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_noop_fallback_record(
        &records[0],
        AgentTaskKind::SessionReflector,
        "session_reflector",
        json!({ "facts": [] }),
    );
}

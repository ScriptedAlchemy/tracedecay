mod automation_runner_support;

use automation_runner_support::*;
use tracedecay::automation::fact_proposals::record_session_fact_proposals;

#[tokio::test]
async fn session_reflector_runner_skips_when_task_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = SessionJsonBackend::new(json!({"facts": []}));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions::default(),
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

#[tokio::test]
async fn session_reflector_runner_validates_fact_proposals_without_applying() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    seed_duplicate_facts(&cg).await;
    let backend = SessionJsonBackend::new(json!({
        "facts": [
            {
                "content": "The project requires durable session reflection facts to stay approval gated",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.72,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence describes the required approval gate"
            },
            {
                "content": "Cache invalidation policy must be explicit",
                "category": "project",
                "tags": ["cache"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "duplicate should be rejected"
            },
            {
                "content": "Uncited session reflection facts must not be accepted",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "reason": "missing citation should be rejected"
            },
            {
                "content": "Session reflection citations must point at bounded evidence",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.7,
                "source_span": {"session_id": "session-reflect-1", "message_id": "missing-message"},
                "reason": "bogus citation should be rejected"
            },
            {
                "content": "Session reflection facts require calibrated trust",
                "category": "project",
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "missing trust should be rejected"
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
                "reason": "confidence should be rejected"
            },
            {
                "content": "",
                "category": "project"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        model: Some("configured-model".to_string()),
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

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.task, AgentTaskKind::SessionReflector);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 7);
    assert_eq!(
        run.report["accepted_facts"][0]["add_fact_request"]["source"],
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
        json!("Repeated session evidence describes the required approval gate")
    );
    let rejected = run.report["rejected_facts"].as_array().unwrap();
    assert!(rejected
        .iter()
        .any(|value| value["reason"].as_str().unwrap().contains("duplicate")));
    let has_rejection_reason = |reason: &str| {
        rejected
            .iter()
            .any(|value| value["reason"] == json!(reason))
    };
    assert!(has_rejection_reason("content is required"));
    assert!(has_rejection_reason("source_span is required"));
    assert!(has_rejection_reason(
        "source_span must cite a bounded session reflection evidence hit"
    ));
    assert!(has_rejection_reason("trust is required"));
    assert!(has_rejection_reason("reason is required"));
    assert!(has_rejection_reason(
        "confidence is not supported; use trust"
    ));
    let proposals = list_fact_proposals(
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::PendingApproval),
        10,
    )
    .await
    .unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].run_id, run.run_id);
    assert_eq!(
        proposals[0].add_fact_request.as_ref().unwrap().content,
        "The project requires durable session reflection facts to stay approval gated"
    );
    assert_eq!(
        proposals[0].validation.as_ref().unwrap()["dedupe"]["near_duplicate_threshold"],
        json!(0.9)
    );
    assert_eq!(
        run.report["proposal_ids"][0],
        json!(proposals[0].proposal_id)
    );
    assert!(run.ledger_record.applied_ops.is_none());
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["pending_proposals"]["proposal_ids"]
            [0],
        json!(proposals[0].proposal_id)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["pending_proposals"]
            ["accepted_facts"][0]["add_fact_request"]["content"],
        json!("The project requires durable session reflection facts to stay approval gated")
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
    assert_eq!(eval_payload["summary"]["eval_count"], json!(8));
    assert!(eval_payload["eval_definitions"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |entry| entry["eval_id"] == json!("session_reflector:accepted:0")
                && entry["harness"]["commands"][0]
                    == json!("cargo test --test automation_runner_test session_reflector")
        ));
    assert_eq!(
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test session_reflector_runner_validates_fact_proposals_without_applying -- --nocapture"
        )
    );
    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["task"], json!("session_reflector"));
    assert_eq!(
        handoff_payload["next_actions"][0],
        json!("review pending fact proposals")
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test session_reflector_runner_validates_fact_proposals_without_applying -- --nocapture"
        )
    );
    let before_apply = cg
        .search_facts(tracedecay::memory::types::SearchFactsRequest {
            query: "durable session reflection facts approval gated".to_string(),
            category: Some(tracedecay::memory::types::MemoryCategory::Project),
            limit: Some(10),
            min_trust: Some(0.1),
            include_why: false,
        })
        .await
        .unwrap();
    assert!(
        before_apply
            .iter()
            .all(|hit| hit.fact.source.as_deref() != Some("session_reflector")),
        "session reflector should not write accepted facts before proposal approval"
    );

    let project_db = cg.open_project_store_db().await.unwrap();
    let applied = apply_fact_proposal(
        &cg.store_layout().dashboard_root,
        project_db.conn(),
        &proposals[0].proposal_id,
        Some("test".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(applied.state, FactProposalState::Applied);
    assert!(applied.apply_outcome.is_some());
    let after_apply = cg
        .search_facts(tracedecay::memory::types::SearchFactsRequest {
            query: "durable session reflection facts approval gated".to_string(),
            category: Some(tracedecay::memory::types::MemoryCategory::Project),
            limit: Some(10),
            min_trust: Some(0.1),
            include_why: false,
        })
        .await
        .unwrap();
    assert!(
        after_apply
            .iter()
            .any(|hit| hit.fact.source.as_deref() == Some("session_reflector")),
        "approving the proposal should apply it to the fact store"
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 7);
    assert!(records[0].applied_ops.is_none());
}

#[tokio::test]
async fn session_fact_proposals_dedupe_repeated_pending_facts_across_runs() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let accepted = json!({
        "add_fact_request": {
            "content": "Repeated session evidence should produce one pending fact proposal",
            "category": "project",
            "source": "session_reflector",
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
        "proposal": {
            "content": "Repeated session evidence should produce one pending fact proposal"
        },
        "validation": {
            "dedupe": {
                "nearest_existing_fact_id": null
            }
        }
    });

    let first = record_session_fact_proposals(
        &dashboard_root,
        "run-a",
        Some("evidence-a"),
        std::slice::from_ref(&accepted),
        &[],
    )
    .await
    .unwrap();
    let second = record_session_fact_proposals(
        &dashboard_root,
        "run-b",
        Some("evidence-b"),
        std::slice::from_ref(&accepted),
        &[],
    )
    .await
    .unwrap();
    let proposals = list_fact_proposals(
        &dashboard_root,
        Some(FactProposalState::PendingApproval),
        10,
    )
    .await
    .unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 0);
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].run_id, "run-a");
}

#[tokio::test]
async fn session_reflector_runner_reads_hermes_profile_lcm_with_filters() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;

    let hermes_home = tempdir().unwrap();
    let profile_db_path = resolve_hermes_profile_session_db_path(hermes_home.path()).unwrap();
    let profile_db = GlobalDb::open_at(&profile_db_path)
        .await
        .expect("hermes profile session db open");
    seed_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "hermes-reflect-1",
            message_id: "hermes-reflect-1-message-001",
            role: "assistant",
            timestamp: 1_715_100_005,
            text: "Hermes profile-only banana evidence should feed session reflection.",
            source: Some("hermes_profile_lcm"),
        },
    )
    .await;
    seed_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "hermes-reflect-1",
            message_id: "hermes-reflect-1-message-002",
            role: "user",
            timestamp: 1_715_100_006,
            text: "Hermes profile-only banana distractor has the wrong role.",
            source: Some("hermes_profile_lcm"),
        },
    )
    .await;

    let backend = InspectSessionEvidenceBackend;
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
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            storage_scope: "hermes_profile".to_string(),
            hermes_home: Some(hermes_home.path().to_path_buf()),
            provider: "cursor".to_string(),
            query: "profile-only banana".to_string(),
            scope: LcmScope::Session,
            session_id: Some("hermes-reflect-1".to_string()),
            include_summaries: false,
            evidence_limit: 5,
            sort: LcmGrepSort::Relevance,
            source: Some("hermes_profile_lcm".to_string()),
            role: Some("assistant".to_string()),
            start_time: Some(1_715_100_000),
            end_time: Some(1_715_100_010),
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.accepted_count, 0);
    assert_eq!(run.ledger_record.rejected_count, 0);
}

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
    assert!(err
        .to_string()
        .contains("session reflector output must include a facts array"));
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
    assert!(
        records[0].error.as_deref().is_some_and(
            |error| error.contains("session reflector output must include a facts array")
        )
    );
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
}

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

    assert_eq!(backend.calls(), 1);
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::SessionReflector,
        "session_reflector",
        json!({ "facts": [] }),
    );
    assert!(run
        .ledger_record
        .error
        .as_deref()
        .is_some_and(|error| error.contains("executable")));
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

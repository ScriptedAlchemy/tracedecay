use crate::support::*;
use tracedecay_agent_hosts::ports::session_evidence::{LcmGrepSort, LcmScope};

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

    let run = tracedecay::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
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
            tracedecay::automation::runner::run_session_reflector_with_backend_and_retrieval(
                &cg,
                &config,
                &reflector_backend,
                &retrieval,
                SessionReflectorAutomationOptions::default(),
            )
            .await
            .unwrap();
        let skill = tracedecay::automation::runner::run_skill_writer_with_backend_and_retrieval(
            &cg,
            &config,
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
        tracedecay::automation::runner::run_session_reflector_with_backend_and_retrieval(
            &cg,
            &config,
            &reflector_backend,
            &retrieval,
            SessionReflectorAutomationOptions::default(),
        )
        .await
        .unwrap();
    let skill = tracedecay::automation::runner::run_skill_writer_with_backend_and_retrieval(
        &cg,
        &config,
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

#[tokio::test]
async fn session_reflector_runner_auto_applies_valid_fact_proposals_by_default() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    seed_duplicate_facts(&cg).await;
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
                "reason": "unknown trust label should be rejected"
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
    assert_eq!(run.ledger_record.accepted_count, 3);
    assert_eq!(run.ledger_record.rejected_count, 8);
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
        json!("Repeated session evidence supports self-managed durable fact automation")
    );
    assert_eq!(
        run.report["accepted_facts"][1]["add_fact_request"]["category"],
        json!("tool")
    );
    let rejected = run.report["rejected_facts"].as_array().unwrap();
    assert!(
        rejected
            .iter()
            .any(|value| value["reason"].as_str().unwrap().contains("duplicate"))
    );
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
    assert!(has_rejection_reason(
        "trust must be a number between 0 and 1, or one of low, medium, high"
    ));
    assert!(has_rejection_reason("reason is required"));
    assert!(has_rejection_reason(
        "confidence is not supported; use trust"
    ));
    assert_eq!(
        run.report["accepted_facts"][2]["add_fact_request"]["trust"],
        json!(0.85)
    );
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
    assert_eq!(proposals.len(), 3);
    assert_eq!(proposals[0].run_id, run.run_id);
    assert_eq!(
        proposals[0].add_fact_request.as_ref().unwrap().content,
        "TraceDecay automation should manage durable session reflection facts directly"
    );
    assert_eq!(proposals[1].run_id, run.run_id);
    assert_eq!(
        proposals[1].add_fact_request.as_ref().unwrap().category,
        tracedecay::memory::types::MemoryCategory::Tool
    );
    assert_eq!(
        proposals[0].validation.as_ref().unwrap()["dedupe"]["near_duplicate_threshold"],
        json!(0.9)
    );
    assert_eq!(
        run.report["proposal_ids"][0],
        json!(proposals[0].proposal_id)
    );
    assert_eq!(run.report["status"], json!("auto_applied"));
    assert_eq!(run.report["dry_run"], json!(false));
    assert_eq!(
        run.report["session_fact_apply_policy"]["decision"],
        json!("auto_apply_allowed")
    );
    assert!(run.ledger_record.applied_ops.is_some());
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["applied_proposals"]["proposal_ids"]
            [0],
        json!(proposals[0].proposal_id)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["applied_proposals"]["accepted_facts"]
            [0]["add_fact_request"]["content"],
        json!("TraceDecay automation should manage durable session reflection facts directly")
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
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test session_reflector_runner_auto_applies_valid_fact_proposals_by_default -- --nocapture"
        )
    );
    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["task"], json!("session_reflector"));
    assert_eq!(
        handoff_payload["next_actions"][0],
        json!("inspect fact automation outcomes")
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test session_reflector_runner_auto_applies_valid_fact_proposals_by_default -- --nocapture"
        )
    );
    let after_apply = cg
        .search_facts(tracedecay::memory::types::SearchFactsRequest {
            query: "TraceDecay automation durable session reflection facts".to_string(),
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
        "session reflector should auto-apply accepted facts"
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 3);
    assert_eq!(records[0].rejected_count, 8);
    assert!(records[0].applied_ops.is_some());
}

#[tokio::test]
async fn session_reflector_runner_auto_apply_ignores_dashboard_approval_gate() {
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
        auto_apply_memory_ops: true,
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
            run_id: Some(HIGH_ENTROPY_RUN_ID.to_string()),
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.report["status"], json!("auto_applied"));
    assert_eq!(run.report["dry_run"], json!(false));
    assert_eq!(
        run.report["session_fact_apply_policy"]["decision"],
        json!("auto_apply_allowed")
    );
    assert_eq!(
        run.report["session_fact_apply_policy"]["mutates_store"],
        json!(true)
    );
    assert_eq!(
        run.report["session_fact_apply_policy"]["autonomous_memory_apply"],
        json!(true)
    );
    assert_eq!(
        run.report["session_fact_apply_policy"]["require_dashboard_approval"],
        json!(false)
    );
    assert_eq!(
        run.report["session_fact_apply_policy"]["approval_required"],
        json!(false)
    );
    assert!(run.ledger_record.applied_ops.is_some());
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["status"],
        json!("auto_applied")
    );

    let owner = project_memory_owner(&cg);
    let memory = tracedecay::application::memory::MemoryApplication::new(
        owner.clone(),
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
    let applied = list_fact_proposals(
        &memory,
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::Applied),
        10,
    )
    .await
    .unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].run_id, HIGH_ENTROPY_RUN_ID);

    let typed_proposal = memory
        .get_compatibility_fact_proposal(
            tracedecay_domain::ProvenanceId::new(applied[0].proposal_id.clone()).unwrap(),
        )
        .await
        .unwrap()
        .expect("auto-applied proposal remains in the canonical authority");
    assert_eq!(
        typed_proposal.automation_run_id(),
        Some(HIGH_ENTROPY_RUN_ID)
    );
    let canonical_fact_id = typed_proposal
        .applied_fact_id()
        .expect("auto-applied proposal has a canonical fact id")
        .clone();
    let projection = memory
        .get_compatibility_fact(tracedecay_store::CompatibilityFactTargetV1::Canonical(
            tracedecay_store::CompatibilityFactIdV1::new(owner, canonical_fact_id).unwrap(),
        ))
        .await
        .unwrap()
        .expect("auto-applied canonical fact is readable");
    let tracedecay_store::CompatibilityFactProjectionV1::Available(fact) = projection else {
        panic!("auto-applied fact must retain an available V1 projection");
    };
    assert_eq!(
        fact.fact().payload_access(),
        tracedecay_domain::PayloadAccessState::Eligible
    );
    assert!(
        fact.payload().is_some(),
        "canonical active payload is retained"
    );
    assert!(!fact.fact().active_assertion_id().as_str().is_empty());
    assert_eq!(fact.legacy_fact_id(), typed_proposal.legacy_fact_id());
    assert!(
        fact.legacy_fact_id().is_some(),
        "legacy mirror mapping is retained"
    );

    let facts = cg
        .search_facts(tracedecay::memory::types::SearchFactsRequest {
            query: "automatic durable memory capture".to_string(),
            category: Some(tracedecay::memory::types::MemoryCategory::Project),
            limit: Some(10),
            min_trust: Some(0.1),
            include_why: false,
        })
        .await
        .unwrap();
    assert!(
        facts
            .iter()
            .any(|hit| hit.fact.source.as_deref() == Some("session_reflector")),
        "dashboard approval must not block accepted session facts from being auto-applied"
    );
}

#[tokio::test]
async fn session_reflector_runner_self_manages_partial_noops_without_review_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let backend = SessionJsonBackend::new(json!({
        "facts": [
            {
                "content": "TraceDecay automation should keep partial session memory applies review gated",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.76,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence supports a partial apply regression"
            },
            {
                "content": "TraceDecay automation should keep partial session memory applies review gated",
                "category": "project",
                "tags": ["automation", "memory"],
                "entities": ["TraceDecay"],
                "trust": 0.76,
                "source_span": {"session_id": "session-reflect-1", "message_id": "session-reflect-1-message-001"},
                "reason": "Repeated session evidence supports a duplicate proposal no-op"
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_apply_memory_ops: true,
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

    assert_eq!(run.report["status"], json!("auto_applied"));
    assert_eq!(run.report["dry_run"], json!(false));
    assert_eq!(
        run.report["session_fact_apply_policy"]["applied_count"],
        json!(1)
    );
    assert_eq!(
        run.report["session_fact_apply_policy"]["fully_applied"],
        json!(false)
    );
    assert_eq!(
        run.report["session_fact_apply_policy"]["approval_required"],
        json!(false)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["status"],
        json!("auto_applied")
    );

    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(
        handoff_payload["readiness"]["approval_required"],
        json!(false)
    );
    assert_eq!(
        handoff_payload["readiness"]["auto_apply_allowed"],
        json!(true)
    );
}

#[tokio::test]
async fn session_fact_proposals_replay_same_run_idempotently() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let owner = project_memory_owner(&cg);
    let memory = tracedecay::application::memory::MemoryApplication::new(
        owner,
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let accepted = json!({
        "add_fact_request": {
            "content": "Repeated session evidence should produce one durable fact action",
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
            "content": "Repeated session evidence should produce one durable fact action"
        },
        "validation": {
            "dedupe": {
                "nearest_existing_fact_id": null
            }
        }
    });

    let first = record_session_fact_proposals(
        &memory,
        &dashboard_root,
        "run-a",
        Some("evidence-a"),
        std::slice::from_ref(&accepted),
        &[],
    )
    .await
    .unwrap();
    let second = record_session_fact_proposals(
        &memory,
        &dashboard_root,
        "run-a",
        Some("evidence-a"),
        std::slice::from_ref(&accepted),
        &[],
    )
    .await
    .unwrap();
    let proposals = memory
        .list_compatibility_fact_proposals(
            Some(tracedecay_store::CompatibilityFactProposalStateV1::PendingApproval),
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(proposals.proposals().len(), 1);
    assert_eq!(proposals.proposals()[0].automation_run_id(), Some("run-a"));
}

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

    let run = tracedecay::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
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

    let run = tracedecay::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
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

struct NoSummaryReplayBackend;

impl AgentTaskBackend for NoSummaryReplayBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
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
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

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
        tracedecay::sessions::lcm::types::LcmImmutableSummaryPublication {
            summary_id: "summary.session-reflect-1.no-replay".to_string(),
            predecessor_summary_id: None,
            draft: tracedecay::sessions::lcm::LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: "session-reflect-1".to_string(),
                session_id: "session-reflect-1".to_string(),
                depth: 0,
                summary_text: "summary that should not be replayed when summaries are disabled"
                    .to_string(),
                source_refs: vec![tracedecay::sessions::lcm::LcmSourceRef::RawMessage {
                    store_id: source.store_id,
                }],
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

    let run = tracedecay::automation::runner::run_session_reflector_with_backend_and_retrieval(
        &cg,
        &config,
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

#[tokio::test]
async fn session_fact_proposals_keep_paraphrases_distinct() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let owner = project_memory_owner(&cg);
    let memory = tracedecay::application::memory::MemoryApplication::new(
        owner,
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let fact = |content: &str| {
        json!({
            "add_fact_request": {
                "content": content,
                "category": "project",
                "source": "session_reflector",
                "tags": ["session-reflector"],
                "entities": ["merge discipline"],
                "trust": 0.9,
                "metadata": {
                    "source_span": { "session_id": "s", "message_id": "m" },
                    "trust_reason": "repeated evidence"
                }
            },
            "proposal": { "content": content }
        })
    };
    let batch = vec![
        fact(
            "Never merge a PR batch after a single flaky green pass; require stable \
             aggregate verification and a live PR-state recheck before merging",
        ),
        fact(
            "Before merging a PR batch, require stable aggregate verification and a \
             live PR-state recheck — a single flaky green pass is never enough to merge",
        ),
        fact(
            "A single flaky green pass is not enough: merging the PR batch needs \
             stable aggregate verification plus a live PR-state recheck first",
        ),
        fact(
            "Cursor composer ingestion reads cursorDiskKV with immutable read-only \
             SQLite opens and indexed primary-key lookups only",
        ),
    ];

    let recorded = record_session_fact_proposals(
        &memory,
        &dashboard_root,
        "run-a",
        Some("evidence-a"),
        &batch,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        recorded.len(),
        4,
        "each canonical proposal keeps its original evidence and identity"
    );

    let restated = vec![fact(
        "Require stable aggregate verification and live PR-state rechecks; never \
         merge the batch off one flaky green pass",
    )];
    let second = record_session_fact_proposals(
        &memory,
        &dashboard_root,
        "run-b",
        Some("evidence-b"),
        &restated,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(second.len(), 1);

    let proposals = memory
        .list_compatibility_fact_proposals(
            Some(tracedecay_store::CompatibilityFactProposalStateV1::PendingApproval),
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(proposals.proposals().len(), 5);
    assert_eq!(
        proposals
            .proposals()
            .iter()
            .filter(|proposal| proposal.request().content().contains("flaky green"))
            .count(),
        4,
        "paraphrases remain independently reviewable"
    );
    assert!(
        proposals
            .proposals()
            .iter()
            .any(|proposal| proposal.request().content().contains("cursorDiskKV")),
        "distinct proposal preserved"
    );
    assert_eq!(
        proposals
            .proposals()
            .iter()
            .filter(|proposal| proposal.automation_run_id() == Some("run-a"))
            .count(),
        4
    );
    assert_eq!(
        proposals
            .proposals()
            .iter()
            .filter(|proposal| proposal.automation_run_id() == Some("run-b"))
            .count(),
        1
    );
}

#[tokio::test]
async fn session_fact_proposals_never_mutate_applied_records() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let owner = project_memory_owner(&cg);
    let memory = tracedecay::application::memory::MemoryApplication::new(
        owner.clone(),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let applied = record_session_fact_proposals(
        &memory,
        &dashboard_root,
        "run-old",
        Some("evidence-old"),
        &[json!({
            "add_fact_request": {
                "content": "Never merge a PR batch after a single flaky green pass; require stable \
                            aggregate verification and a live PR-state recheck before merging",
                "category": "project",
                "source": "session_reflector",
                "tags": ["session-reflector"],
                "entities": ["merge discipline"],
                "trust": 0.9,
                "metadata": {
                    "source_span": { "session_id": "s", "message_id": "m" },
                    "trust_reason": "repeated evidence"
                }
            }
        })],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(applied.len(), 1);
    let applied_id = tracedecay_domain::ProvenanceId::new(applied[0].proposal_id.clone()).unwrap();
    let submitted = memory
        .get_compatibility_fact_proposal(applied_id.clone())
        .await
        .unwrap()
        .expect("submitted authority proposal");
    memory
        .promote_compatibility_fact_proposal(
            tracedecay_store::CompatibilityFactProposalPromotionV1::new(
                owner,
                applied_id.clone(),
                submitted.revision(),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let applied_before = memory
        .get_compatibility_fact_proposal(applied_id.clone())
        .await
        .unwrap()
        .expect("applied authority proposal");
    assert_eq!(
        applied_before.state(),
        tracedecay_store::CompatibilityFactProposalStateV1::Applied
    );

    let paraphrase = json!({
        "add_fact_request": {
            "content": "Before merging a PR batch, require stable aggregate verification and a \
                        live PR-state recheck — a single flaky green pass is never enough to merge",
            "category": "project",
            "source": "session_reflector",
            "tags": ["session-reflector"],
            "entities": ["merge discipline"],
            "trust": 0.9,
            "metadata": {
                "source_span": { "session_id": "s", "message_id": "m" },
                "trust_reason": "repeated evidence"
            }
        },
        "proposal": { "content": "paraphrase" }
    });
    let recorded = record_session_fact_proposals(
        &memory,
        &dashboard_root,
        "run-new",
        Some("evidence-new"),
        &[paraphrase],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "paraphrase of an applied fact enqueues as its own pending proposal"
    );
    assert_eq!(recorded[0].state, FactProposalState::PendingApproval);

    let proposals = memory
        .list_compatibility_fact_proposals(None, None, 10)
        .await
        .unwrap();
    assert_eq!(
        proposals.proposals().len(),
        2,
        "new pending proposal enqueued"
    );
    let applied_after = proposals
        .proposals()
        .iter()
        .find(|proposal| proposal.proposal_id() == &applied_id)
        .expect("applied record preserved");
    assert_eq!(
        applied_after, &applied_before,
        "recording a paraphrase must not mutate the applied authority record"
    );
}

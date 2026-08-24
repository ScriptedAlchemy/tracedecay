use crate::support::*;

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
    let pending = list_fact_proposals(
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::PendingApproval),
        10,
    )
    .await
    .unwrap();
    assert!(pending.is_empty());
    let proposals = list_fact_proposals(
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
            run_id: None,
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

    let pending = list_fact_proposals(
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::PendingApproval),
        10,
    )
    .await
    .unwrap();
    assert!(pending.is_empty());
    let applied = list_fact_proposals(
        &cg.store_layout().dashboard_root,
        Some(FactProposalState::Applied),
        10,
    )
    .await
    .unwrap();
    assert_eq!(applied.len(), 1);

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
async fn session_fact_proposals_dedupe_repeated_pending_facts_across_runs() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
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
async fn session_reflector_host_modes_do_not_select_alternate_lcm_storage() {
    let _env_lock = ENV_LOCK.lock().await;
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let project_db = GlobalDb::open_at(&cg.store_layout().sessions_db_path)
        .await
        .expect("project session db open");
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

        if host_mode == AutomationHostMode::Standalone {
            assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
            assert_eq!(run.ledger_record.accepted_count, 0);
            assert_eq!(run.ledger_record.rejected_count, 0);
        } else {
            assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
            assert_eq!(
                run.ledger_record.error.as_deref(),
                Some("delegated_host_mode")
            );
        }
    }
}

#[tokio::test]
async fn session_reflector_replays_recent_sessions_without_keyword_matches() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    // Deliberately avoids every keyword in the default reflection query so
    // the grep channel returns nothing and only session replay surfaces it.
    seed_session_message_in_db(
        &GlobalDb::open_at(&cg.store_layout().sessions_db_path)
            .await
            .expect("session db open"),
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
    seed_session_message_in_db(
        &GlobalDb::open_at(&cg.store_layout().sessions_db_path)
            .await
            .expect("session db open"),
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
        Some("no_session_evidence")
    );
}

#[tokio::test]
async fn session_reflector_skips_when_replay_disabled_and_no_grep_hits() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_message_in_db(
        &GlobalDb::open_at(&cg.store_layout().sessions_db_path)
            .await
            .expect("session db open"),
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "session-replay-2",
            message_id: "session-replay-2-message-001",
            role: "user",
            timestamp: 1_715_000_060,
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
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
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
    let db = GlobalDb::open_at(&cg.store_layout().sessions_db_path)
        .await
        .expect("session db open");
    db.lcm_insert_summary_node(tracedecay::sessions::lcm::LcmSummaryNodeDraft {
        provider: "cursor".to_string(),
        conversation_id: "session-reflect-1".to_string(),
        session_id: "session-reflect-1".to_string(),
        depth: 1,
        summary_text: "summary that should not be replayed when summaries are disabled".to_string(),
        source_refs: Vec::new(),
        source_token_count: 10,
        summary_token_count: 5,
        source_time_start: Some(1_715_000_001),
        source_time_end: Some(1_715_000_001),
        expand_hint: None,
        metadata_json: None,
    })
    .await
    .expect("summary fixture should insert");
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
        &NoSummaryReplayBackend,
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
async fn session_fact_proposals_fold_paraphrases_into_one_proposal() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
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

    let recorded =
        record_session_fact_proposals(&dashboard_root, "run-a", Some("evidence-a"), &batch, &[])
            .await
            .unwrap();
    assert_eq!(
        recorded.len(),
        2,
        "paraphrases must fold into the first proposal"
    );

    let restated = vec![fact(
        "Require stable aggregate verification and live PR-state rechecks; never \
         merge the batch off one flaky green pass",
    )];
    let second =
        record_session_fact_proposals(&dashboard_root, "run-b", Some("evidence-b"), &restated, &[])
            .await
            .unwrap();
    assert_eq!(second.len(), 0);

    let proposals = list_fact_proposals(
        &dashboard_root,
        Some(FactProposalState::PendingApproval),
        10,
    )
    .await
    .unwrap();
    assert_eq!(proposals.len(), 2);
    let folded = proposals
        .iter()
        .find(|p| {
            p.add_fact_request
                .as_ref()
                .is_some_and(|r| r.content.contains("flaky green"))
        })
        .expect("merge-discipline proposal present");
    assert_eq!(
        folded.duplicate_count, 3,
        "two in-batch paraphrases plus one cross-run restatement"
    );
    assert_eq!(folded.last_duplicate_run_id.as_deref(), Some("run-b"));
    assert_eq!(
        folded.folded_contents.len(),
        3,
        "each folded paraphrase is captured for reviewer recovery"
    );
    assert!(
        folded
            .folded_contents
            .iter()
            .any(|c| c.contains("Before merging a PR batch")),
        "first in-batch paraphrase captured verbatim"
    );
    assert!(
        folded
            .folded_contents
            .iter()
            .any(|c| c.contains("A single flaky green pass is not enough")),
        "second in-batch paraphrase captured verbatim"
    );
    assert!(
        folded
            .folded_contents
            .iter()
            .any(|c| c.contains("Require stable aggregate verification and live PR-state rechecks")),
        "cross-run restatement captured verbatim"
    );
    let distinct = proposals
        .iter()
        .find(|p| {
            p.add_fact_request
                .as_ref()
                .is_some_and(|r| r.content.contains("cursorDiskKV"))
        })
        .expect("distinct proposal preserved");
    assert_eq!(distinct.duplicate_count, 0);
    assert!(distinct.folded_contents.is_empty());
}

#[tokio::test]
async fn session_fact_proposals_never_mutate_applied_records() {
    use tracedecay::automation::fact_proposals::{
        FactProposalRecord, FactProposalStore, load_fact_proposal_store, save_fact_proposal_store,
    };

    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");

    let applied_request = serde_json::from_value(json!({
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
    }))
    .unwrap();
    let applied = FactProposalRecord {
        schema_version: 1,
        proposal_id: "fact_applied".to_string(),
        run_id: "run-old".to_string(),
        evidence_hash: Some("evidence-old".to_string()),
        state: FactProposalState::Applied,
        add_fact_request: Some(applied_request),
        proposal: None,
        validation_reason: None,
        validation: None,
        reviewer: Some("dashboard".to_string()),
        applied_fact_id: Some(42),
        apply_outcome: None,
        created_at: 1_000,
        updated_at: 1_000,
        duplicate_count: 0,
        last_duplicate_run_id: None,
        folded_contents: Vec::new(),
    };
    save_fact_proposal_store(
        &dashboard_root,
        &FactProposalStore {
            schema_version: 1,
            proposals: vec![applied],
        },
    )
    .await
    .unwrap();

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

    let store = load_fact_proposal_store(&dashboard_root).await.unwrap();
    assert_eq!(store.proposals.len(), 2, "new pending proposal enqueued");
    let applied = store
        .proposals
        .iter()
        .find(|p| p.proposal_id == "fact_applied")
        .expect("applied record preserved");
    assert_eq!(applied.state, FactProposalState::Applied);
    assert_eq!(
        applied.updated_at, 1_000,
        "applied record's updated_at must not be corrupted by the fold path"
    );
    assert_eq!(
        applied.duplicate_count, 0,
        "applied record must not be folded into"
    );
    assert!(applied.folded_contents.is_empty());
}

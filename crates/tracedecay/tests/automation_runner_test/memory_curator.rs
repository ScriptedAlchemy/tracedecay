use crate::support::*;
use std::sync::atomic::{AtomicUsize, Ordering};

mod backend_failures;
mod manual_trigger;
mod pagination;

struct TransientThenJsonBackend {
    calls: AtomicUsize,
}

impl AgentTaskBackend for TransientThenJsonBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> std::result::Result<AgentTaskResponse, tracedecay_automation::backend::AgentTaskError>
    {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= 2 {
            return Err(
                tracedecay_automation::backend::AgentTaskError::from_backend_message(
                    "timed out waiting for transient test backend",
                ),
            );
        }
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: "{\"ops\":[]}".to_string(),
            output_json: Some(json!({"ops": []})),
            model: Some("retry-test-model".to_string()),
            provider: Some("fixture".to_string()),
            input_tokens: None,
            output_tokens: None,
        })
    }
}

fn normalize_tags_op(facts: &SeededDuplicateFacts) -> Value {
    json!({
        "op": "normalize_tags",
        "target": exact_fact(&facts.loser_id, &facts.loser_event_id),
        "tags": ["cache", "policy", "curated"],
        "evidence_facts": [exact_fact(&facts.winner_id, &facts.winner_event_id)],
        "confidence": 0.98,
    })
}

fn exact_fact(fact_id: &str, expected_last_event_id: &str) -> Value {
    json!({
        "fact_id": fact_id,
        "expected_last_event_id": expected_last_event_id,
    })
}

#[tokio::test]
async fn memory_curator_repairs_then_applies_validated_ops_and_records_ledger() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let backend = SequentialJsonBackend::new(vec![
        json!({
            "ops": [{
                "op": "normalize_tags",
                "target": exact_fact(&facts.loser_id, &facts.loser_event_id),
                "tags": ["cache", "policy", "curated"],
                "evidence_facts": [exact_fact(&facts.winner_id, &facts.winner_event_id)],
                "confidence": 0.98,
            }, {
                "op": "normalize_tags",
                "target": {
                    "fact_id": "fact.missing",
                    "expected_last_event_id": facts.loser_event_id,
                },
                "tags": ["cache", "policy", "curated"],
                "evidence_facts": [exact_fact(&facts.winner_id, &facts.winner_event_id)],
                "confidence": 0.98,
            }]
        }),
        json!({
            "ops": [{
                "op": "normalize_tags",
                "target": exact_fact(&facts.loser_id, &facts.loser_event_id),
                "tags": ["cache", "policy", "curated"],
                "evidence_facts": [exact_fact(&facts.winner_id, &facts.winner_event_id)],
                "confidence": 0.98,
            }]
        }),
    ]);
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 2);
    assert_eq!(run.ledger_record.schema_version, 2);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(
        run.ledger_record.task_key.as_deref(),
        Some("memory_curator")
    );
    assert_eq!(
        run.ledger_record.prompt_version.as_deref(),
        Some("memory_curator:v1")
    );
    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 0);
    assert_eq!(run.ledger_record.reviewed_count, 1);
    assert_eq!(run.ledger_record.skipped_count, 0);
    assert_eq!(run.ledger_record.backend, "codex_app_server");
    assert_eq!(run.ledger_record.host_mode.as_deref(), Some("standalone"));
    assert_eq!(run.ledger_record.model.as_deref(), Some("fixture-model"));
    assert_eq!(run.ledger_record.backend_attempt_count, 2);
    assert_eq!(run.ledger_record.backend_attempts.len(), 2);
    assert!(run.ledger_record.backend_attempts[0].succeeded);
    assert!(
        run.ledger_record
            .evidence_hash
            .as_deref()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    assert!(
        run.ledger_record
            .input_hash
            .as_deref()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    assert!(
        run.ledger_record
            .output_hash
            .as_deref()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    assert_eq!(
        run.ledger_record.applied_ops.as_ref().unwrap()[0]["receipt"]["changed_fact_ids"][0],
        json!(facts.loser_id)
    );
    assert_eq!(run.ledger_record.rejected_ops.as_ref().unwrap(), &json!([]));
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["validation_repairs"][0]["attempt"],
        json!(1)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["facts_reviewed"],
        json!(2)
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["curation_policy"]["decision"]["disposition"],
        json!("allow")
    );
    assert_eq!(
        run.ledger_record.validation_report.as_ref().unwrap()["curation_policy"]["effect"]["mutates_store"],
        json!(true)
    );
    assert_eq!(
        run.report["curation_policy"]["decision"]["authority"]["actor_id"],
        json!("automation:memory-curator")
    );
    assert_eq!(
        run.report["curation_policy"]["decision"]["authority"]["configuration_revision_id"],
        json!(test_configuration_revision())
    );
    assert!(run.report["curation_policy"]["decision"]["authority"]["project_id"].is_string());
    assert!(run.report["curation_policy"]["decision"]["authority"]["profile_id"].is_string());
    assert!(
        run.report["curation_policy"]["decision"]["configuration_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
    assert!(fact_exists(&cg, &facts.loser_id, run_control.read_control()).await);
    assert_eq!(
        run.ledger_record.report_ref.as_ref().unwrap()["run_id"],
        json!(run.run_id)
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
    let validation_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "validation_gate")
        .unwrap();
    let validation_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        validation_artifact,
    )
    .await
    .unwrap();
    assert_eq!(
        validation_payload["task_validation"]["decision"],
        json!("passed")
    );
    assert_eq!(validation_payload["loop_stage"], json!("validation_gate"));
    assert_eq!(
        validation_payload["improvement_gate"]["decision"],
        json!("ready_for_handoff")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["feedback_status"],
        json!("derived_from_validation")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["generated_evals_status"],
        json!("passed")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["criteria"]["has_feedback"],
        json!(true)
    );
    assert_eq!(
        validation_payload["improvement_gate"]["criteria"]["has_generated_evals"],
        json!(true)
    );
    assert_eq!(
        validation_payload["improvement_gate"]["criteria"]["automatic_application"]["status"],
        json!("applied")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["source_refs"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        validation_payload["improvement_gate"]["optimizer_status"],
        json!("ready_for_handoff")
    );
    assert!(
        validation_payload["improvement_gate"]["artifact_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["kind"] == json!("generated_evals")
                && reference["sha256"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("sha256:")))
    );
    let feedback_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "feedback")
        .unwrap();
    let feedback_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        feedback_artifact,
    )
    .await
    .unwrap();
    assert_eq!(feedback_payload["status"], json!("derived_from_validation"));
    assert_eq!(feedback_payload["loop_stage"], json!("feedback"));
    assert_eq!(feedback_payload["source_refs"][0]["kind"], json!("traces"));
    assert_eq!(feedback_payload["summary"]["accepted_count"], json!(1));
    assert_eq!(feedback_payload["summary"]["rejected_count"], json!(0));
    assert_eq!(feedback_payload["summary"]["reviewed_count"], json!(1));
    assert_eq!(feedback_payload["human"], json!([]));
    assert!(
        feedback_payload["artifact_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["kind"] == json!("traces")
                && reference["sha256"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("sha256:")))
    );
    assert_eq!(feedback_payload["model"].as_array().unwrap().len(), 1);
    assert!(
        feedback_payload["model"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["outcome"] == json!("accepted"))
    );

    let eval_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "generated_evals")
        .unwrap();
    let eval_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        eval_artifact,
    )
    .await
    .unwrap();
    assert_eq!(eval_payload["status"], json!("generated_from_validation"));
    assert_eq!(eval_payload["loop_stage"], json!("generated_evals"));
    assert_eq!(
        eval_payload["automatic_application"]["status"],
        json!("applied")
    );
    assert_eq!(eval_payload["source_refs"][0]["kind"], json!("traces"));
    assert_eq!(eval_payload["source_refs"][1]["kind"], json!("feedback"));
    assert_eq!(
        eval_payload["eval_definitions"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        eval_payload["format"],
        json!("tracedecay_automation_eval:v1")
    );
    assert_eq!(eval_payload["runner"]["type"], json!("validation_replay"));
    assert_eq!(
        eval_payload["runner"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test memory_curator_repairs_then_applies_validated_ops_and_records_ledger -- --nocapture"
        )
    );
    assert_eq!(
        eval_payload["runner"]["artifact_api"],
        json!(format!(
            "/api/automation/runs/{}/artifacts/generated_evals",
            run.run_id
        ))
    );
    assert_eq!(
        eval_payload["runner"]["inputs"]["artifact_kind"],
        json!("generated_evals")
    );
    assert_eq!(
        eval_payload["runner"]["inputs"]["expected_eval_count"],
        json!(1)
    );
    assert!(
        eval_payload["runner"]["inputs"]["validation_report_hash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    assert_eq!(
        eval_payload["runner"]["checks"].as_array().unwrap().len(),
        3
    );
    assert_eq!(eval_payload["runner"]["status"], json!("passed"));
    assert_eq!(
        eval_payload["runner"]["results"][0]["check"],
        json!("accepted_count_matches")
    );
    assert_eq!(
        eval_payload["runner"]["results"][0]["status"],
        json!("passed")
    );
    assert_eq!(
        eval_payload["automatic_application"]["retry_required"],
        json!(false)
    );
    assert!(
        eval_payload["artifact_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["kind"] == json!("feedback")
                && reference["sha256"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("sha256:")))
    );
    assert!(
        eval_payload["eval_definitions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["expected_outcome"] == json!("accepted")
                && entry["eval_id"] == json!("memory_curator:accepted:0")
                && entry["source_feedback_ref"] == json!("accepted:0")
                && entry["schema_version"] == json!(1)
                && entry["kind"] == json!("automation_validation_regression")
                && entry["harness"]["type"] == json!("cargo_test_filter")
                && entry["harness"]["commands"][0]
                    == json!("cargo test --test automation_runner_test memory_curator")
                && entry["fixture"]["candidate"].is_object()
                && entry["source_feedback"]["artifact_kind"] == json!("feedback")
                && entry["source_feedback"]["feedback_id"] == json!("accepted:0")
                && entry["assertions"][0]["type"] == json!("outcome_equals"))
    );
    assert_eq!(
        eval_payload["result_refs"][0]["kind"],
        json!("validation_report")
    );

    let optimizer_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "optimizer_diagnosis")
        .unwrap();
    let optimizer_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        optimizer_artifact,
    )
    .await
    .unwrap();
    assert_eq!(optimizer_payload["status"], json!("generated"));
    assert_eq!(
        optimizer_payload["loop_stage"],
        json!("optimizer_diagnosis")
    );
    assert_eq!(optimizer_payload["signals"]["accepted_count"], json!(1));
    assert_eq!(optimizer_payload["signals"]["rejected_count"], json!(0));
    assert_eq!(optimizer_payload["signals"]["reviewed_count"], json!(1));
    assert_eq!(
        optimizer_payload["signals"]["validation_gate_decision"],
        json!("ready_for_handoff")
    );
    assert!(
        optimizer_payload["artifact_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["kind"] == json!("traces"))
    );
    assert!(
        optimizer_payload["diagnostic_inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["kind"] == json!("generated_evals")
                && reference["sha256"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("sha256:")))
    );
    assert_eq!(optimizer_payload["blockers"], json!([]));
    assert_eq!(
        optimizer_payload["recommendations"][0]["id"],
        json!("review_accepted_changes")
    );
    assert_eq!(
        optimizer_payload["ranked_changes"][0]["priority"],
        json!("medium")
    );
    assert_eq!(
        optimizer_payload["ranked_changes"][0]["ready_for_codex_handoff"],
        json!(true)
    );
    let handoff_artifact = run
        .ledger_record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "codex_handoff")
        .unwrap();
    let handoff_payload = read_run_artifact_payload(
        &cg.store_layout().dashboard_root,
        &run.run_id,
        handoff_artifact,
    )
    .await
    .unwrap();
    assert_eq!(handoff_payload["task"], json!("memory_curator"));
    assert_eq!(handoff_payload["loop_stage"], json!("codex_handoff"));
    assert_eq!(handoff_payload["status"], json!("ready_for_review"));
    assert_eq!(
        handoff_payload["readiness"]["validation_gate_decision"],
        json!("ready_for_handoff")
    );
    assert_eq!(handoff_payload["readiness"]["eval_count"], json!(1));
    assert_eq!(
        handoff_payload["readiness"]["automatic_application"]["status"],
        json!("applied")
    );
    assert_eq!(
        handoff_payload["machine_summary"]["next_stage"],
        json!("monitor_applied_outcomes")
    );
    assert_eq!(
        handoff_payload["source_refs"][0]["kind"],
        json!("validation_gate")
    );
    assert!(
        handoff_payload["artifact_manifest"]["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["kind"] == json!("optimizer_diagnosis"))
    );
    assert_eq!(
        handoff_payload["artifact_manifest"]["api_list"],
        json!(format!("/api/automation/runs/{}/artifacts", run.run_id))
    );
    assert_eq!(
        handoff_payload["artifact_manifest"]["api_payloads"]["generated_evals"],
        json!(format!(
            "/api/automation/runs/{}/artifacts/generated_evals",
            run.run_id
        ))
    );
    assert_eq!(
        handoff_payload["eval_replay"]["artifact_api"],
        json!(format!(
            "/api/automation/runs/{}/artifacts/generated_evals",
            run.run_id
        ))
    );
    assert_eq!(
        handoff_payload["eval_replay"]["commands"][0],
        json!(
            "cargo test --test automation_runner_test memory_curator_repairs_then_applies_validated_ops_and_records_ledger -- --nocapture"
        )
    );
    assert!(
        handoff_payload["request"]["evidence_hash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    assert_eq!(
        run.report["llm_apply"]["ops"][0]["target"]["fact_id"],
        json!(facts.loser_id)
    );
    assert_eq!(run.report["llm_apply"]["rejected_ops"], json!([]));
    assert_eq!(
        run.report["llm_apply"]["validation_repairs"][0]["attempt"],
        json!(1)
    );

    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].accepted_count, 1);
    assert_eq!(records[0].rejected_count, 0);
    assert_eq!(records[0].artifacts.len(), 6);
    assert_eq!(records[0].artifacts, run.ledger_record.artifacts);
    assert!(
        fact_exists(&cg, &facts.loser_id, run_control.read_control()).await,
        "normalizing tags must retain the curated fact"
    );
}

#[tokio::test]
async fn memory_curator_persists_transient_transient_success_retry_receipt() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = TransientThenJsonBackend {
        calls: AtomicUsize::new(0),
    };
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 3);
    assert_eq!(run.ledger_record.backend_attempt_count, 3);
    assert_eq!(
        run.ledger_record
            .backend_attempts
            .iter()
            .map(|attempt| attempt.failure_classification)
            .collect::<Vec<_>>(),
        vec![
            Some(AgentTaskFailureClass::Timeout),
            Some(AgentTaskFailureClass::Timeout),
            None,
        ]
    );
    assert_eq!(
        run.ledger_record
            .backend_attempts
            .iter()
            .map(|attempt| attempt.backoff_millis)
            .collect::<Vec<_>>(),
        vec![2_000, 5_000, 0]
    );
}

#[tokio::test]
async fn scheduler_memory_curator_applies_validated_ops_automatically() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [normalize_tags_op(&facts)]
    }));
    let mut config = scheduler_config(None, None);
    config.tasks.memory_curator.interval_secs = Some(1);

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(run.report["llm_apply"]["applied"], json!(1));
    assert_eq!(
        run.report["curation_policy"]["decision"]["disposition"],
        json!("allow")
    );
    assert!(fact_exists(&cg, &facts.loser_id, run_control.read_control()).await);
}

#[tokio::test]
async fn memory_curator_runner_artifacts_block_handoff_without_validation_examples() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({ "ops": [] }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.accepted_count, 0);
    assert_eq!(run.ledger_record.rejected_count, 0);
    assert_eq!(run.ledger_record.reviewed_count, 0);

    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["summary"]["eval_count"], json!(0));
    assert_eq!(
        eval_payload["automatic_application"]["status"],
        json!("no_candidate")
    );
    assert_eq!(eval_payload["eval_definitions"], json!([]));

    let validation_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "validation_gate").await;
    assert_eq!(
        validation_payload["task_validation"]["decision"],
        json!("no_valid_changes")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["decision"],
        json!("blocked_pending_feedback_or_evals")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["generated_evals_status"],
        json!("blocked_no_generated_evals")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["optimizer_status"],
        json!("blocked")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["handoff_status"],
        json!("blocked")
    );

    let optimizer_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "optimizer_diagnosis").await;
    assert_eq!(
        optimizer_payload["blockers"][0]["id"],
        json!("pending_feedback_or_evals")
    );

    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["status"], json!("blocked"));
    assert_eq!(
        handoff_payload["readiness"]["blockers"][0]["id"],
        json!("pending_feedback_or_evals")
    );
}

#[tokio::test]
async fn memory_curator_runner_artifacts_mark_handoff_ready_for_accepted_only_examples() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [normalize_tags_op(&facts)]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(run.ledger_record.accepted_count, 1);
    assert_eq!(run.ledger_record.rejected_count, 0);

    let eval_payload = read_artifact(&cg, &run.run_id, &run.ledger_record, "generated_evals").await;
    assert_eq!(eval_payload["runner"]["status"], json!("passed"));
    assert_eq!(
        eval_payload["automatic_application"]["status"],
        json!("applied")
    );

    let validation_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "validation_gate").await;
    assert_eq!(
        validation_payload["task_validation"]["decision"],
        json!("passed")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["decision"],
        json!("ready_for_handoff")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["handoff_status"],
        json!("ready")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["generated_evals_status"],
        json!("passed")
    );
    assert_eq!(
        validation_payload["improvement_gate"]["optimizer_status"],
        json!("ready_for_handoff")
    );

    let optimizer_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "optimizer_diagnosis").await;
    assert_eq!(optimizer_payload["blockers"], json!([]));

    let handoff_payload =
        read_artifact(&cg, &run.run_id, &run.ledger_record, "codex_handoff").await;
    assert_eq!(handoff_payload["status"], json!("ready_for_review"));
    assert_eq!(
        handoff_payload["readiness"]["validation_gate_decision"],
        json!("ready_for_handoff")
    );
    assert_eq!(
        handoff_payload["machine_summary"]["next_stage"],
        json!("monitor_applied_outcomes")
    );
}

#[tokio::test]
async fn memory_curator_runner_applies_validated_ops_under_apply_policy() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [normalize_tags_op(&facts)]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(
        run.report["curation_policy"]["decision"]["disposition"],
        json!("allow")
    );
    assert_eq!(
        run.report["curation_policy"]["effect"]["mutates_store"],
        json!(true)
    );
    assert_eq!(run.report["llm_apply"]["applied"], json!(1));
    assert!(
        fact_exists(&cg, &facts.loser_id, run_control.read_control()).await,
        "canonical curation retains the fact after normalizing its tags"
    );
}

#[tokio::test]
async fn memory_curator_quarantines_legacy_output_after_bounded_repair_exhaustion() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let invalid = json!({
        "ops": [
            {
                "op": "delete",
                "fact_id": facts.loser_id,
                "confidence": 0.98,
            }
        ]
    });
    let backend = SequentialJsonBackend::new(vec![invalid.clone(), invalid]);
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let error = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 2);
    assert!(error.to_string().contains("repair budget exhausted"));
    assert!(fact_exists(&cg, &facts.winner_id, run_control.read_control()).await);
    assert!(
        fact_exists(&cg, &facts.loser_id, run_control.read_control()).await,
        "quarantined validation failures must not mutate memory"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert!(
        records[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("output quarantined"))
    );
}

#[tokio::test]
async fn memory_curator_runner_auto_applies_validated_operations() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({
        "ops": [
            normalize_tags_op(&facts),
            {
                "op": "link_facts",
                "source": exact_fact(&facts.winner_id, &facts.winner_event_id),
                "target": exact_fact(&facts.loser_id, &facts.loser_event_id),
                "relation": "supports",
                "evidence_facts": [exact_fact(&facts.loser_id, &facts.loser_event_id)],
                "confidence": 0.98,
                "source_label": "automation:memory-curator",
                "metadata": {"review": "canonical-link"},
            }
        ]
    }));
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(
        run.report["curation_policy"]["decision"]["disposition"],
        json!("allow")
    );
    assert_eq!(
        run.report["curation_policy"]["effect"]["mutates_store"],
        json!(true)
    );
    assert_eq!(run.report["llm_apply"]["applied"], json!(2));
    assert_eq!(
        run.report["llm_apply"]["receipts"][0]["status"],
        json!("applied")
    );
    let receipt = &run.report["llm_apply"]["receipts"][0]["receipt"];
    assert_eq!(receipt["normalized_tags"], json!(1));
    assert_eq!(receipt["facts_linked"], json!(1));
    assert_eq!(
        receipt["operation_effects"].as_array().map(Vec::len),
        Some(2)
    );
    assert!(receipt["replay_fact_id"].is_string());
    assert!(receipt["replay_event_id"].is_string());
    let changed_fact_ids = receipt["changed_fact_ids"].as_array().unwrap();
    assert!(
        changed_fact_ids
            .iter()
            .any(|fact_id| fact_id.as_str() == Some(facts.winner_id.as_str()))
    );
    assert!(
        changed_fact_ids
            .iter()
            .any(|fact_id| fact_id.as_str() == Some(facts.loser_id.as_str()))
    );
    assert!(
        fact_exists(&cg, &facts.loser_id, run_control.read_control()).await,
        "automatic validation policy should retain the canonical fact"
    );
}

#[tokio::test]
async fn memory_curator_stops_before_backend_or_apply_when_caller_is_interrupted() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({"ops": [normalize_tags_op(&facts)]}));
    let owner = project_memory_owner(&cg);
    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        owner.clone(),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let winner_tags_before = memory
        .query_current_facts(
            tracedecay_store::CurrentFactsQuery::new(owner.clone(), None, 10).unwrap(),
        )
        .await
        .unwrap()
        .into_iter()
        .find(|fact| fact.fact_id().as_str() == facts.winner_id.as_str())
        .and_then(|fact| fact.payload().map(|payload| payload.tags().to_vec()))
        .expect("seeded winner must be available through canonical current-fact authority");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };
    let interrupted = Arc::new(AtomicBool::new(true));
    let run_control = test_automation_run_control(Arc::clone(&interrupted));

    let error = tracedecay_agent_hosts::automation::runner::run_memory_curator_with_backend(
        &cg,
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &run_control,
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("interrupted"));
    assert_eq!(backend.calls(), 0);
    interrupted.store(false, Ordering::Release);
    assert!(fact_exists(&cg, &facts.winner_id, run_control.read_control()).await);
    assert!(fact_exists(&cg, &facts.loser_id, run_control.read_control()).await);
    let winner_tags_after = memory
        .query_current_facts(tracedecay_store::CurrentFactsQuery::new(owner, None, 10).unwrap())
        .await
        .unwrap()
        .into_iter()
        .find(|fact| fact.fact_id().as_str() == facts.winner_id.as_str())
        .and_then(|fact| fact.payload().map(|payload| payload.tags().to_vec()))
        .expect("interrupted run must retain the seeded winner");
    assert_eq!(winner_tags_after, winner_tags_before);
    assert!(
        load_run_records(&cg.store_layout().dashboard_root, 10)
            .await
            .unwrap()
            .is_empty(),
        "an interrupted run must not emit an application receipt"
    );
}
